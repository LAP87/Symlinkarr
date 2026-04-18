use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus};
use crate::utils::{cached_source_health, PathHealth};

use super::{BackupEntry, BackupManagedFile, BackupManager};

pub(super) fn validate_restore_input_path(backup_path: &Path) -> Result<()> {
    if backup_path.is_absolute() {
        anyhow::bail!("Backup restore only accepts files inside the configured backup directory");
    }

    if backup_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("Backup restore path escapes the configured backup directory");
    }

    Ok(())
}

pub(super) fn backup_path_candidates(backup_path: &Path, backup_root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if backup_path.is_absolute() {
        candidates.push(backup_path.to_path_buf());
        return candidates;
    }

    candidates.push(backup_root.join(backup_path));
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_candidate = cwd.join(backup_path);
        if cwd_candidate != candidates[0] {
            candidates.push(cwd_candidate);
        }
    }

    candidates
}

pub(super) fn rollback_created_restore_symlinks(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        if let Err(err) = std::fs::remove_file(path) {
            if path.exists() || path.is_symlink() {
                warn!(
                    "Failed to roll back restored symlink after backup restore commit error {:?}: {}",
                    path, err
                );
            }
        }
    }
}

pub(super) fn restore_managed_file_from_backup(
    manager: &BackupManager,
    destination_path: &Path,
    artifact: &BackupManagedFile,
) -> Result<()> {
    let source_path = manager.resolve_restore_path(Path::new(&artifact.filename))?;
    let actual_sha = super::sha256_file(&source_path)?;
    if actual_sha != artifact.sha256 {
        anyhow::bail!(
            "Backup app-state integrity check failed for {}",
            artifact.filename
        );
    }

    if let Some(parent) = destination_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
            if manager.enforce_secure_permissions {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perm = std::fs::Permissions::from_mode(0o700);
                    let _ = std::fs::set_permissions(parent, perm);
                }
            }
        }
    }

    if destination_path.exists() && destination_path.is_dir() {
        anyhow::bail!(
            "Refusing to overwrite directory during app-state restore: {}",
            destination_path.display()
        );
    }

    let file_name = destination_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("restored");
    let temp_name = format!(".{file_name}.symlinkarr-restore.tmp");
    let temp_path = destination_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(temp_name);

    if temp_path.exists() {
        let _ = std::fs::remove_file(&temp_path);
    }

    std::fs::copy(&source_path, &temp_path).with_context(|| {
        format!(
            "Failed to restore app-state file {} to {}",
            source_path.display(),
            destination_path.display()
        )
    })?;

    if manager.enforce_secure_permissions {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perm = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&temp_path, perm);
        }
    }

    if destination_path.exists() {
        std::fs::remove_file(destination_path)?;
    }
    std::fs::rename(&temp_path, destination_path)?;

    Ok(())
}

pub(super) fn restore_target_available(
    target_path: &Path,
    source_health_cache: &mut HashMap<PathBuf, PathHealth>,
    parent_health_cache: &mut HashMap<PathBuf, PathHealth>,
) -> Result<bool> {
    let health = cached_source_health(target_path, source_health_cache, parent_health_cache);
    if health.blocks_destructive_ops() {
        anyhow::bail!(
            "Aborting backup restore: source target became unhealthy: {}",
            health.describe(target_path)
        );
    }
    Ok(health.is_healthy())
}

pub(super) fn path_is_within_roots(
    path: &Path,
    roots: &[PathBuf],
    follow_leaf_symlink: bool,
) -> bool {
    if roots.is_empty() {
        return false;
    }

    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let normalized_abs =
        normalize_path_for_root_check(&resolve_path_for_root_check(&abs, follow_leaf_symlink));

    roots.iter().any(|root| {
        let normalized_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let normalized_root = normalize_path_for_root_check(&normalized_root);
        normalized_abs.starts_with(&normalized_root)
    })
}

pub(super) async fn should_restore_db_record(db: &Database, entry: &BackupEntry) -> Result<bool> {
    match db.get_link_by_target_path(&entry.symlink_path).await? {
        Some(existing) => Ok(existing.source_path != entry.target_path
            || existing.media_id != entry.media_id
            || existing.media_type != entry.media_type
            || existing.status != LinkStatus::Active),
        None => Ok(true),
    }
}

pub(super) async fn upsert_restored_link(db: &Database, entry: &BackupEntry) -> Result<()> {
    let record = LinkRecord {
        id: None,
        source_path: resolve_restore_target_path(entry),
        target_path: entry.symlink_path.clone(),
        media_id: entry.media_id.clone(),
        media_type: entry.media_type,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };
    db.insert_link(&record).await?;
    Ok(())
}

pub(super) async fn upsert_restored_link_in_tx(
    db: &Database,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    entry: &BackupEntry,
) -> Result<()> {
    let record = LinkRecord {
        id: None,
        source_path: resolve_restore_target_path(entry),
        target_path: entry.symlink_path.clone(),
        media_id: entry.media_id.clone(),
        media_type: entry.media_type,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };
    db.insert_link_in_tx(&record, tx).await?;
    Ok(())
}

pub(super) fn resolve_restore_target_path(entry: &BackupEntry) -> PathBuf {
    if entry.target_path.is_absolute() {
        entry.target_path.clone()
    } else {
        entry
            .symlink_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&entry.target_path)
    }
}

fn resolve_path_for_root_check(path: &Path, follow_leaf_symlink: bool) -> PathBuf {
    if follow_leaf_symlink {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            return canonical;
        }
    }

    if let Some(parent) = path.parent() {
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            if let Some(name) = path.file_name() {
                return canonical_parent.join(name);
            }
            return canonical_parent;
        }
    }

    if !follow_leaf_symlink {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            return canonical;
        }
    }

    path.to_path_buf()
}

fn normalize_path_for_root_check(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}
