use std::io::Read;
use std::path::{Component, Path};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    BackupAppState, BackupDatabaseSnapshot, BackupEntry, BackupManifest, BackupType,
    BACKUP_MANIFEST_VERSION,
};

pub(crate) fn parse_backup_manifest(json: &str, source: &Path) -> Result<BackupManifest> {
    let manifest: BackupManifest = serde_json::from_str(json)
        .with_context(|| format!("Failed to parse backup manifest {:?}", source))?;
    validate_backup_manifest(&manifest, source)?;
    Ok(manifest)
}

#[derive(Serialize)]
struct BackupManifestChecksumPayload<'a> {
    version: u32,
    timestamp: DateTime<Utc>,
    backup_type: &'a BackupType,
    label: &'a str,
    symlinks: &'a [BackupEntry],
    total_count: usize,
    database_snapshot: &'a Option<BackupDatabaseSnapshot>,
}

#[derive(Serialize)]
struct BackupManifestChecksumPayloadV3<'a> {
    version: u32,
    timestamp: DateTime<Utc>,
    backup_type: &'a BackupType,
    label: &'a str,
    symlinks: &'a [BackupEntry],
    total_count: usize,
    database_snapshot: &'a Option<BackupDatabaseSnapshot>,
    app_state: &'a Option<BackupAppState>,
}

fn validate_backup_manifest(manifest: &BackupManifest, source: &Path) -> Result<()> {
    match manifest.version {
        1 => return Ok(()),
        2 | BACKUP_MANIFEST_VERSION => {}
        other => {
            anyhow::bail!(
                "Unsupported backup manifest version {} in {:?}. Supported versions: 1-{}",
                other,
                source,
                BACKUP_MANIFEST_VERSION
            );
        }
    }

    let Some(expected) = manifest.content_sha256.as_deref() else {
        anyhow::bail!(
            "Backup manifest {:?} is missing content_sha256 for version {}",
            source,
            manifest.version
        );
    };
    let actual = compute_manifest_checksum(manifest)?;
    if actual != expected {
        anyhow::bail!("Backup manifest integrity check failed for {:?}", source);
    }
    Ok(())
}

pub(super) fn compute_manifest_checksum(manifest: &BackupManifest) -> Result<String> {
    let json = match manifest.version {
        2 => serde_json::to_vec(&BackupManifestChecksumPayload {
            version: manifest.version,
            timestamp: manifest.timestamp,
            backup_type: &manifest.backup_type,
            label: &manifest.label,
            symlinks: &manifest.symlinks,
            total_count: manifest.total_count,
            database_snapshot: &manifest.database_snapshot,
        })?,
        BACKUP_MANIFEST_VERSION => serde_json::to_vec(&BackupManifestChecksumPayloadV3 {
            version: manifest.version,
            timestamp: manifest.timestamp,
            backup_type: &manifest.backup_type,
            label: &manifest.label,
            symlinks: &manifest.symlinks,
            total_count: manifest.total_count,
            database_snapshot: &manifest.database_snapshot,
            app_state: &manifest.app_state,
        })?,
        other => anyhow::bail!(
            "Cannot compute checksum for unsupported backup manifest version {}",
            other
        ),
    };
    let mut hasher = Sha256::new();
    hasher.update(&json);
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn validate_managed_backup_file_name(file_name: &str) -> Result<()> {
    if file_name.trim().is_empty() {
        anyhow::bail!("Backup filename must not be empty");
    }

    let path = Path::new(file_name);
    if path.is_absolute() {
        anyhow::bail!("Backup filename must be relative to the configured backup directory");
    }

    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("Backup filename must stay inside the configured backup directory");
    }

    Ok(())
}

fn slugify_backup_name_part(value: &str) -> Option<String> {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

pub(super) fn sanitize_backup_file_name_component(value: &str, fallback: &str) -> String {
    let mut sanitized = String::new();
    let mut last_was_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            sanitized.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if matches!(ch, '.' | '_' | '-') {
            sanitized.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            sanitized.push('-');
            last_was_dash = true;
        }
    }

    let sanitized = sanitized
        .trim_matches(|ch: char| ch == '-' || ch == '.')
        .to_string();
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

pub(super) fn scheduled_backup_base_name(timestamp: DateTime<Utc>, label: &str) -> String {
    let ts = timestamp.format("%Y%m%d-%H%M%S");
    match slugify_backup_name_part(label) {
        Some(slug) if slug != "manual-backup" => format!("symlinkarr-backup-{slug}-{ts}"),
        _ => format!("symlinkarr-backup-{ts}"),
    }
}

pub(super) fn safety_snapshot_base_name(timestamp: DateTime<Utc>, operation: &str) -> String {
    let ts = timestamp.format("%Y%m%d-%H%M%S");
    let operation = slugify_backup_name_part(operation).unwrap_or_else(|| "restore-point".into());
    format!("symlinkarr-restore-point-{operation}-{ts}")
}
