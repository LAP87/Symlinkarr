use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::{BackupConfig, Config};
use crate::db::Database;

pub(crate) use self::manifest::parse_backup_manifest;
use self::manifest::{
    compute_manifest_checksum, safety_snapshot_base_name, sanitize_backup_file_name_component,
    scheduled_backup_base_name, sha256_file, validate_managed_backup_file_name,
};
use self::restore::{
    backup_path_candidates, path_is_within_roots, resolve_restore_target_path,
    restore_managed_file_from_backup, restore_target_available, rollback_created_restore_symlinks,
    should_restore_db_record, upsert_restored_link, upsert_restored_link_in_tx,
    validate_restore_input_path,
};
use crate::models::{LinkStatus, MediaType};

// ─── Backup data structures ─────────────────────────────────────────

const BACKUP_MANIFEST_VERSION: u32 = 3;

/// A single symlink entry in a backup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub symlink_path: PathBuf,
    pub target_path: PathBuf,
    pub media_id: String,
    pub media_type: MediaType,
    #[serde(default = "default_db_tracked")]
    pub db_tracked: bool,
}

fn default_db_tracked() -> bool {
    true
}

/// Type of backup
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BackupType {
    /// Scheduled full backup
    Scheduled,
    /// Automatic safety snapshot before destructive operations
    Safety { operation: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupDatabaseSnapshot {
    pub filename: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManagedFile {
    pub filename: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub original_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BackupAppState {
    #[serde(default)]
    pub config_snapshot: Option<BackupManagedFile>,
    #[serde(default)]
    pub secret_snapshots: Vec<BackupManagedFile>,
}

/// Complete backup manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Format version for forward compatibility
    pub version: u32,
    /// When the backup was created
    pub timestamp: DateTime<Utc>,
    /// What triggered this backup
    pub backup_type: BackupType,
    /// Human-readable label
    pub label: String,
    /// All symlink mappings
    pub symlinks: Vec<BackupEntry>,
    /// Total count (redundant but convenient for quick inspection)
    pub total_count: usize,
    /// Optional SQLite snapshot captured alongside the manifest.
    #[serde(default)]
    pub database_snapshot: Option<BackupDatabaseSnapshot>,
    /// Optional config + secretfile snapshots for this install.
    #[serde(default)]
    pub app_state: Option<BackupAppState>,
    /// Integrity hash for the manifest content, excluding this field.
    #[serde(default)]
    pub content_sha256: Option<String>,
}

// ─── BackupManager ──────────────────────────────────────────────────

/// Manages symlink backup creation, rotation and restoration.
pub struct BackupManager {
    backup_dir: PathBuf,
    max_backups: usize,
    max_safety_backups: usize,
    enforce_secure_permissions: bool,
}

impl BackupManager {
    pub fn new(config: &BackupConfig) -> Self {
        Self {
            backup_dir: config.path.clone(),
            max_backups: config.max_backups,
            max_safety_backups: config.max_safety_backups,
            enforce_secure_permissions: true,
        }
    }

    /// Ensure the backup directory exists.
    fn ensure_dir(&self) -> Result<()> {
        if !self.backup_dir.exists() {
            std::fs::create_dir_all(&self.backup_dir)?;
            info!("Created backup directory: {:?}", self.backup_dir);
        }
        self.enforce_directory_permissions()?;
        Ok(())
    }

    fn canonical_backup_root(&self) -> Result<PathBuf> {
        self.ensure_dir()?;
        self.backup_dir.canonicalize().with_context(|| {
            format!(
                "Configured backup directory not found: {}",
                self.backup_dir.display()
            )
        })
    }

    fn resolve_managed_output_path(&self, file_name: &str) -> Result<PathBuf> {
        validate_managed_backup_file_name(file_name)?;
        Ok(self.canonical_backup_root()?.join(file_name))
    }

    pub fn resolve_restore_path(&self, backup_path: &Path) -> Result<PathBuf> {
        let backup_root = self.canonical_backup_root()?;
        validate_restore_input_path(backup_path)?;

        for candidate in backup_path_candidates(backup_path, &backup_root) {
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };

            if canonical.starts_with(&backup_root) {
                return Ok(canonical);
            }
            anyhow::bail!("Backup restore path escapes the configured backup directory");
        }

        anyhow::bail!(
            "Backup restore file not found: {}",
            backup_root.join(backup_path).display()
        );
    }

    fn enforce_directory_permissions(&self) -> Result<()> {
        if !self.enforce_secure_permissions {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perm = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&self.backup_dir, perm)?;
        }
        Ok(())
    }

    fn write_secure_json(&self, path: &Path, json: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                if self.enforce_secure_permissions {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let perm = std::fs::Permissions::from_mode(0o700);
                        let _ = std::fs::set_permissions(parent, perm);
                    }
                }
            }
        }
        std::fs::write(path, json)?;
        if self.enforce_secure_permissions {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = std::fs::Permissions::from_mode(0o600);
                std::fs::set_permissions(path, perm)?;
            }
        }
        Ok(())
    }

    fn copy_secure_file(
        &self,
        source_path: &Path,
        relative_output_path: &str,
    ) -> Result<BackupManagedFile> {
        let output_path = self.resolve_managed_output_path(relative_output_path)?;
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                if self.enforce_secure_permissions {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let perm = std::fs::Permissions::from_mode(0o700);
                        let _ = std::fs::set_permissions(parent, perm);
                    }
                }
            }
        }

        std::fs::copy(source_path, &output_path).with_context(|| {
            format!(
                "Failed to copy app-state file {} into backup",
                source_path.display()
            )
        })?;

        if self.enforce_secure_permissions {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = std::fs::Permissions::from_mode(0o600);
                let _ = std::fs::set_permissions(&output_path, perm);
            }
        }

        Ok(BackupManagedFile {
            filename: relative_output_path.to_string(),
            sha256: sha256_file(&output_path)?,
            size_bytes: std::fs::metadata(&output_path)?.len(),
            original_path: source_path.to_path_buf(),
        })
    }

    fn capture_app_state(&self, cfg: &Config, base_name: &str) -> Result<Option<BackupAppState>> {
        let bundle_dir = format!("{base_name}.app-state");
        let config_snapshot = cfg
            .loaded_from
            .as_ref()
            .filter(|path| path.exists() && path.is_file())
            .map(|config_path| {
                let config_name = config_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| sanitize_backup_file_name_component(name, "config.yaml"))
                    .unwrap_or_else(|| "config.yaml".to_string());
                self.copy_secure_file(config_path, &format!("{bundle_dir}/config/{config_name}"))
            })
            .transpose()?;

        let mut secret_snapshots = Vec::new();
        for (index, secret_path) in cfg.secret_files.iter().enumerate() {
            if !secret_path.exists() || !secret_path.is_file() {
                warn!(
                    "Skipping secretfile snapshot for {}: file missing or not a file",
                    secret_path.display()
                );
                continue;
            }

            let secret_name = secret_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| sanitize_backup_file_name_component(name, "secret"))
                .unwrap_or_else(|| "secret".to_string());
            let relative_path = format!("{bundle_dir}/secrets/{:02}-{}", index + 1, secret_name);
            secret_snapshots.push(self.copy_secure_file(secret_path, &relative_path)?);
        }

        if config_snapshot.is_none() && secret_snapshots.is_empty() {
            Ok(None)
        } else {
            Ok(Some(BackupAppState {
                config_snapshot,
                secret_snapshots,
            }))
        }
    }

    /// Create a full backup of active links, SQLite state, and restorable app-state files.
    pub async fn create_backup(&self, cfg: &Config, db: &Database, label: &str) -> Result<PathBuf> {
        self.ensure_dir()?;

        let active_links = db.get_links_by_status(LinkStatus::Active).await?;

        let entries: Vec<BackupEntry> = active_links
            .iter()
            .map(|link| BackupEntry {
                symlink_path: link.target_path.clone(),
                target_path: link.source_path.clone(),
                media_id: link.media_id.clone(),
                media_type: link.media_type,
                db_tracked: true,
            })
            .collect();

        let total_count = entries.len();
        let timestamp = Utc::now();
        let label = if label.trim().is_empty() {
            "Manual backup"
        } else {
            label.trim()
        };
        let base_name = scheduled_backup_base_name(timestamp, label);
        let app_state = self.capture_app_state(cfg, &base_name)?;
        let snapshot_filename = format!("{base_name}.sqlite3");
        let snapshot_path = self.resolve_managed_output_path(&snapshot_filename)?;
        db.export_snapshot(&snapshot_path)
            .await
            .context("Failed to capture SQLite snapshot for backup")?;

        let mut manifest = BackupManifest {
            version: BACKUP_MANIFEST_VERSION,
            timestamp,
            backup_type: BackupType::Scheduled,
            label: label.to_string(),
            symlinks: entries,
            total_count,
            database_snapshot: Some(BackupDatabaseSnapshot {
                filename: snapshot_filename,
                sha256: sha256_file(&snapshot_path)?,
                size_bytes: std::fs::metadata(&snapshot_path)?.len(),
            }),
            app_state,
            content_sha256: None,
        };
        manifest.content_sha256 = Some(compute_manifest_checksum(&manifest)?);

        let filename = format!("{base_name}.json");
        let path = self.resolve_managed_output_path(&filename)?;

        let json = serde_json::to_string_pretty(&manifest)?;
        self.write_secure_json(&path, &json)?;

        info!(
            "Backup created: {:?} ({} symlinks, database snapshot included)",
            path, total_count
        );

        // Auto-rotate old scheduled backups
        self.rotate()?;

        Ok(path)
    }

    /// Create a safety snapshot before a destructive operation.
    /// These are NEVER auto-rotated.
    pub async fn create_safety_snapshot(&self, db: &Database, operation: &str) -> Result<PathBuf> {
        self.create_safety_snapshot_with_extras(db, operation, &[])
            .await
    }

    pub async fn create_safety_snapshot_with_extras(
        &self,
        db: &Database,
        operation: &str,
        extra_symlink_paths: &[PathBuf],
    ) -> Result<PathBuf> {
        self.ensure_dir()?;

        let active_links = db.get_links_by_status(LinkStatus::Active).await?;

        let mut entries: Vec<BackupEntry> = active_links
            .iter()
            .map(|link| BackupEntry {
                symlink_path: link.target_path.clone(),
                target_path: link.source_path.clone(),
                media_id: link.media_id.clone(),
                media_type: link.media_type,
                db_tracked: true,
            })
            .collect();

        let mut seen_paths: std::collections::HashSet<_> = entries
            .iter()
            .map(|entry| entry.symlink_path.clone())
            .collect();
        for symlink_path in extra_symlink_paths {
            if seen_paths.contains(symlink_path) {
                continue;
            }

            match std::fs::symlink_metadata(symlink_path) {
                Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(symlink_path)
                {
                    Ok(target_path) => {
                        entries.push(BackupEntry {
                            symlink_path: symlink_path.clone(),
                            target_path,
                            media_id: String::new(),
                            media_type: MediaType::Tv,
                            db_tracked: false,
                        });
                        seen_paths.insert(symlink_path.clone());
                    }
                    Err(e) => warn!(
                        "Skipping extra safety snapshot entry for {:?}: {}",
                        symlink_path, e
                    ),
                },
                Ok(_) => warn!(
                    "Skipping extra safety snapshot entry for {:?}: not a symlink",
                    symlink_path
                ),
                Err(e) => warn!(
                    "Skipping extra safety snapshot entry for {:?}: {}",
                    symlink_path, e
                ),
            }
        }

        let total_count = entries.len();
        let now = Utc::now();
        let mut manifest = BackupManifest {
            version: BACKUP_MANIFEST_VERSION,
            timestamp: now,
            backup_type: BackupType::Safety {
                operation: operation.to_string(),
            },
            label: format!("Safety snapshot before {}", operation),
            symlinks: entries,
            total_count,
            database_snapshot: None,
            app_state: None,
            content_sha256: None,
        };
        manifest.content_sha256 = Some(compute_manifest_checksum(&manifest)?);

        let filename = format!("{}.json", safety_snapshot_base_name(now, operation));
        let path = self.resolve_managed_output_path(&filename)?;

        let json = serde_json::to_string_pretty(&manifest)?;
        self.write_secure_json(&path, &json)?;

        info!(
            "Safety snapshot created before '{}': {:?} ({} symlinks)",
            operation, path, total_count
        );

        if self.max_safety_backups > 0 {
            self.rotate_by_prefixes(
                &["symlinkarr-restore-point-", "safety-"],
                self.max_safety_backups,
            )?;
        }

        Ok(path)
    }

    /// Restore symlinks from a backup file.
    /// Returns (restored_count, skipped_count, error_count).
    pub async fn restore(
        &self,
        db: &Database,
        backup_path: &Path,
        dry_run: bool,
        allowed_symlink_roots: &[PathBuf],
        allowed_target_roots: &[PathBuf],
        enforce_roots: bool,
    ) -> Result<(usize, usize, usize)> {
        let backup_root = self.canonical_backup_root()?;
        let backup_path = if backup_path.is_absolute() {
            let canonical = backup_path.canonicalize().with_context(|| {
                format!("Backup restore file not found: {}", backup_path.display())
            })?;
            if !canonical.starts_with(&backup_root) {
                anyhow::bail!("Backup restore path escapes the configured backup directory");
            }
            canonical
        } else {
            self.resolve_restore_path(backup_path)?
        };
        let json = std::fs::read_to_string(&backup_path)?;
        let manifest = parse_backup_manifest(&json, &backup_path)?;
        let mut source_health_cache = std::collections::HashMap::new();
        let mut parent_health_cache = std::collections::HashMap::new();
        let mut restore_tx = if dry_run {
            None
        } else {
            Some(db.begin().await?)
        };

        info!(
            "Restoring from backup: {} ({} symlinks, {})",
            manifest.label,
            manifest.total_count,
            manifest.timestamp.format("%Y-%m-%d %H:%M:%S")
        );

        let mut restored = 0usize;
        let mut skipped = 0usize;
        let mut errors = 0usize;
        let mut created_symlinks = Vec::new();

        for entry in &manifest.symlinks {
            let resolved_target_path = resolve_restore_target_path(entry);
            if enforce_roots {
                if !path_is_within_roots(&entry.symlink_path, allowed_symlink_roots, false) {
                    warn!(
                        "Skipping restore outside allowed library roots: {:?}",
                        entry.symlink_path
                    );
                    skipped += 1;
                    continue;
                }
                if !path_is_within_roots(&resolved_target_path, allowed_target_roots, true) {
                    warn!(
                        "Skipping restore outside allowed source roots: {:?}",
                        resolved_target_path
                    );
                    skipped += 1;
                    continue;
                }
            }

            let target_available = restore_target_available(
                &resolved_target_path,
                &mut source_health_cache,
                &mut parent_health_cache,
            )
            .with_context(|| {
                format!(
                    "Backup restore from {} aborted while validating {:?} -> {:?}",
                    backup_path.display(),
                    entry.symlink_path,
                    resolved_target_path
                )
            })?;
            if !target_available {
                warn!(
                    "Skipping restore for {:?}: source target missing: {:?}",
                    entry.symlink_path, resolved_target_path
                );
                skipped += 1;
                continue;
            }

            if dry_run {
                let symlink_missing =
                    !entry.symlink_path.exists() && !entry.symlink_path.is_symlink();
                if symlink_missing {
                    info!(
                        "[DRY-RUN] Would restore: {:?} → {:?}",
                        entry.symlink_path, entry.target_path
                    );
                    restored += 1;
                } else if entry.db_tracked && should_restore_db_record(db, entry).await? {
                    info!(
                        "[DRY-RUN] Would restore DB record: {:?} → {:?}",
                        entry.symlink_path, entry.target_path
                    );
                    restored += 1;
                } else {
                    skipped += 1;
                }
                continue;
            }

            if entry.symlink_path.is_symlink() {
                match std::fs::read_link(&entry.symlink_path) {
                    Ok(current_target) if current_target == entry.target_path => {
                        if entry.db_tracked && should_restore_db_record(db, entry).await? {
                            if let Some(tx) = restore_tx.as_mut() {
                                upsert_restored_link_in_tx(db, tx, entry).await?;
                            }
                            restored += 1;
                        } else {
                            skipped += 1;
                        }
                        continue;
                    }
                    Ok(current_target) => {
                        warn!(
                            "Skipping restore for {:?}: existing symlink points to {:?}, expected {:?}",
                            entry.symlink_path, current_target, entry.target_path
                        );
                        skipped += 1;
                        continue;
                    }
                    Err(e) => {
                        warn!("Skipping restore for {:?}: {}", entry.symlink_path, e);
                        skipped += 1;
                        continue;
                    }
                }
            }

            if entry.symlink_path.exists() {
                skipped += 1;
                continue;
            }

            // Ensure parent directory exists (the *arr folder structure)
            if let Some(parent) = entry.symlink_path.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        warn!("Could not create directory {:?}: {}", parent, e);
                        errors += 1;
                        continue;
                    }
                }
            }

            // Create the symlink
            #[cfg(unix)]
            match std::os::unix::fs::symlink(&entry.target_path, &entry.symlink_path) {
                Ok(()) => {
                    if entry.db_tracked {
                        match restore_tx.as_mut() {
                            Some(tx) => match upsert_restored_link_in_tx(db, tx, entry).await {
                                Ok(()) => {
                                    created_symlinks.push(entry.symlink_path.clone());
                                    restored += 1;
                                }
                                Err(e) => {
                                    let _ = std::fs::remove_file(&entry.symlink_path);
                                    warn!(
                                        "Failed to restore database state for {:?}: {}",
                                        entry.symlink_path, e
                                    );
                                    errors += 1;
                                }
                            },
                            None => match upsert_restored_link(db, entry).await {
                                Ok(()) => {
                                    created_symlinks.push(entry.symlink_path.clone());
                                    restored += 1;
                                }
                                Err(e) => {
                                    let _ = std::fs::remove_file(&entry.symlink_path);
                                    warn!(
                                        "Failed to restore database state for {:?}: {}",
                                        entry.symlink_path, e
                                    );
                                    errors += 1;
                                }
                            },
                        }
                    } else {
                        created_symlinks.push(entry.symlink_path.clone());
                        restored += 1;
                    }
                }
                Err(e) => {
                    warn!("Failed to restore {:?}: {}", entry.symlink_path, e);
                    errors += 1;
                }
            }
        }

        if dry_run {
            info!(
                "[DRY-RUN] Would restore {} symlinks ({} already exist)",
                restored, skipped
            );
        } else {
            if let Some(tx) = restore_tx.take() {
                if let Err(err) = tx.commit().await {
                    rollback_created_restore_symlinks(&created_symlinks);
                    return Err(err).with_context(|| {
                        format!(
                            "Backup restore from {} finished filesystem work but failed to commit database updates",
                            backup_path.display()
                        )
                    });
                }
            }
            info!(
                "Restore complete: {} restored, {} skipped, {} errors",
                restored, skipped, errors
            );
        }

        Ok((restored, skipped, errors))
    }

    /// Rotate old backups beyond configured limits.
    /// Scheduled backups: always rotated to `max_backups`.
    /// Safety snapshots: rotated to `max_safety_backups` (0 = keep all).
    pub fn rotate(&self) -> Result<()> {
        if !self.backup_dir.exists() {
            return Ok(());
        }

        // Rotate scheduled backups
        self.rotate_by_prefixes(&["symlinkarr-backup-", "backup-"], self.max_backups)?;

        // Rotate safety snapshots (0 = keep all)
        if self.max_safety_backups > 0 {
            self.rotate_by_prefixes(
                &["symlinkarr-restore-point-", "safety-"],
                self.max_safety_backups,
            )?;
        }

        Ok(())
    }

    /// Rotate files matching a prefix, keeping only `max_count`.
    fn rotate_by_prefixes(&self, prefixes: &[&str], max_count: usize) -> Result<()> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.backup_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| {
                        prefixes.iter().any(|prefix| n.starts_with(prefix)) && n.ends_with(".json")
                    })
                    .unwrap_or(false)
            })
            .collect();

        // Sort by modification time ascending so oldest files are at the front.
        files.sort_by_key(|p| {
            p.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        while files.len() > max_count {
            let oldest = files.remove(0);
            info!("Rotating old backup: {:?}", oldest);
            remove_backup_artifacts(&oldest)?;
        }

        Ok(())
    }

    /// List all available backups with summary info.
    pub fn list(&self) -> Result<Vec<BackupSummary>> {
        if !self.backup_dir.exists() {
            return Ok(vec![]);
        }

        let mut summaries: Vec<BackupSummary> = Vec::new();

        for entry in std::fs::read_dir(&self.backup_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.extension().map(|e| e == "json").unwrap_or(false) {
                continue;
            }

            match std::fs::read_to_string(&path) {
                Ok(json) => match parse_backup_manifest(&json, &path) {
                    Ok(manifest) => {
                        let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        summaries.push(BackupSummary {
                            path: path.clone(),
                            filename: path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                            timestamp: manifest.timestamp,
                            backup_type: manifest.backup_type,
                            label: manifest.label,
                            symlink_count: manifest.total_count,
                            file_size,
                            database_snapshot: manifest.database_snapshot,
                            app_state: manifest.app_state,
                        });
                    }
                    Err(e) => {
                        warn!("Could not parse backup {:?}: {}", path, e);
                    }
                },
                Err(e) => {
                    warn!("Could not read {:?}: {}", path, e);
                }
            }
        }

        // Sort by timestamp, newest first
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.timestamp));

        Ok(summaries)
    }

    pub fn latest_scheduled_backup_timestamp(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|summary| matches!(summary.backup_type, BackupType::Scheduled))
            .map(|summary| summary.timestamp)
            .max())
    }

    pub fn restore_app_state(
        &self,
        cfg: &Config,
        backup_path: &Path,
        dry_run: bool,
    ) -> Result<BackupAppStateRestoreSummary> {
        let backup_root = self.canonical_backup_root()?;
        let backup_path = if backup_path.is_absolute() {
            let canonical = backup_path.canonicalize().with_context(|| {
                format!("Backup restore file not found: {}", backup_path.display())
            })?;
            if !canonical.starts_with(&backup_root) {
                anyhow::bail!("Backup restore path escapes the configured backup directory");
            }
            canonical
        } else {
            self.resolve_restore_path(backup_path)?
        };
        let json = std::fs::read_to_string(&backup_path)?;
        let manifest = parse_backup_manifest(&json, &backup_path)?;

        let Some(app_state) = manifest.app_state else {
            return Ok(BackupAppStateRestoreSummary::default());
        };

        let mut summary = BackupAppStateRestoreSummary {
            present: true,
            config_included: app_state.config_snapshot.is_some(),
            config_restored: false,
            secrets_included: app_state.secret_snapshots.len(),
            secrets_restored: 0,
            secrets_skipped: 0,
        };

        if let Some(config_snapshot) = app_state.config_snapshot.as_ref() {
            if let Some(current_config_path) = cfg.loaded_from.as_ref() {
                if !dry_run {
                    restore_managed_file_from_backup(self, current_config_path, config_snapshot)?;
                }
                summary.config_restored = true;
            }
        }

        for secret_snapshot in &app_state.secret_snapshots {
            let Some(current_secret_path) = cfg
                .secret_files
                .iter()
                .find(|candidate| **candidate == secret_snapshot.original_path)
            else {
                summary.secrets_skipped += 1;
                continue;
            };

            if !dry_run {
                restore_managed_file_from_backup(self, current_secret_path, secret_snapshot)?;
            }
            summary.secrets_restored += 1;
        }

        Ok(summary)
    }
}

fn remove_backup_artifacts(manifest_path: &Path) -> Result<()> {
    let mut companion_paths = Vec::new();
    let mut companion_dirs = Vec::new();

    if let Ok(json) = std::fs::read_to_string(manifest_path) {
        if let Ok(manifest) = serde_json::from_str::<BackupManifest>(&json) {
            if let Some(snapshot) = manifest.database_snapshot.as_ref() {
                let path = manifest_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(&snapshot.filename);
                companion_paths.push(path);
            }
            if let Some(app_state) = manifest.app_state.as_ref() {
                for artifact in app_state
                    .config_snapshot
                    .iter()
                    .chain(app_state.secret_snapshots.iter())
                {
                    let artifact_path = manifest_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(&artifact.filename);
                    companion_paths.push(artifact_path.clone());
                    if let Some(Component::Normal(first_component)) =
                        Path::new(&artifact.filename).components().next()
                    {
                        let bundle_dir = manifest_path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(first_component);
                        if !companion_dirs.contains(&bundle_dir) {
                            companion_dirs.push(bundle_dir);
                        }
                    }
                }
            }
        }
    }

    let derived_snapshot = manifest_path.with_extension("sqlite3");
    if derived_snapshot != manifest_path && !companion_paths.contains(&derived_snapshot) {
        companion_paths.push(derived_snapshot);
    }

    std::fs::remove_file(manifest_path)?;
    for companion in companion_paths {
        if companion.exists() {
            std::fs::remove_file(companion)?;
        }
    }
    for companion_dir in companion_dirs {
        if companion_dir.exists() {
            std::fs::remove_dir_all(companion_dir)?;
        }
    }

    Ok(())
}

/// Summary of a backup file (for listing)
#[derive(Debug)]
pub struct BackupSummary {
    #[allow(dead_code)] // Available for restore operations
    pub path: PathBuf,
    pub filename: String,
    pub timestamp: DateTime<Utc>,
    pub backup_type: BackupType,
    #[allow(dead_code)] // Available for display in detailed views
    pub label: String,
    pub symlink_count: usize,
    pub file_size: u64,
    pub database_snapshot: Option<BackupDatabaseSnapshot>,
    pub app_state: Option<BackupAppState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupAppStateRestoreSummary {
    pub present: bool,
    pub config_included: bool,
    pub config_restored: bool,
    pub secrets_included: usize,
    pub secrets_restored: usize,
    pub secrets_skipped: usize,
}

impl std::fmt::Display for BackupSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let type_icon = match &self.backup_type {
            BackupType::Scheduled => "📅",
            BackupType::Safety { .. } => "🛡️",
        };
        let size_kb = self.file_size / 1024;
        write!(
            f,
            "{} {} | {} symlinks | {}KB | {}",
            type_icon,
            self.timestamp.format("%Y-%m-%d %H:%M:%S"),
            self.symlink_count,
            size_kb,
            self.filename,
        )
    }
}

mod manifest;
mod restore;
#[cfg(test)]
mod tests;
