use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::BackupConfig;
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MediaType};

// ─── Backup data structures ─────────────────────────────────────────

/// A single symlink entry in a backup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub symlink_path: PathBuf,
    pub target_path: PathBuf,
    pub media_id: String,
    pub media_type: MediaType,
}

/// Type of backup
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BackupType {
    /// Scheduled full backup
    Scheduled,
    /// Automatic safety snapshot before destructive operations
    Safety { operation: String },
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

    /// Create a full backup of all active symlinks.
    pub async fn create_backup(&self, db: &Database, label: &str) -> Result<PathBuf> {
        self.ensure_dir()?;

        let active_links = db.get_links_by_status(LinkStatus::Active).await?;

        let entries: Vec<BackupEntry> = active_links
            .iter()
            .map(|link| BackupEntry {
                symlink_path: link.target_path.clone(),
                target_path: link.source_path.clone(),
                media_id: link.media_id.clone(),
                media_type: link.media_type,
            })
            .collect();

        let total_count = entries.len();
        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: label.to_string(),
            symlinks: entries,
            total_count,
        };

        let filename = format!("backup-{}.json", manifest.timestamp.format("%Y%m%d-%H%M%S"));
        let path = self.backup_dir.join(&filename);

        let json = serde_json::to_string_pretty(&manifest)?;
        self.write_secure_json(&path, &json)?;

        info!("Backup created: {:?} ({} symlinks)", path, total_count);

        // Auto-rotate old scheduled backups
        self.rotate()?;

        Ok(path)
    }

    /// Create a safety snapshot before a destructive operation.
    /// These are NEVER auto-rotated.
    pub async fn create_safety_snapshot(&self, db: &Database, operation: &str) -> Result<PathBuf> {
        self.ensure_dir()?;

        let active_links = db.get_links_by_status(LinkStatus::Active).await?;

        let entries: Vec<BackupEntry> = active_links
            .iter()
            .map(|link| BackupEntry {
                symlink_path: link.target_path.clone(),
                target_path: link.source_path.clone(),
                media_id: link.media_id.clone(),
                media_type: link.media_type,
            })
            .collect();

        let total_count = entries.len();
        let now = Utc::now();
        let manifest = BackupManifest {
            version: 1,
            timestamp: now,
            backup_type: BackupType::Safety {
                operation: operation.to_string(),
            },
            label: format!("Safety snapshot before {}", operation),
            symlinks: entries,
            total_count,
        };

        let filename = format!("safety-{}-{}.json", operation, now.format("%Y%m%d-%H%M%S"));
        let path = self.backup_dir.join(&filename);

        let json = serde_json::to_string_pretty(&manifest)?;
        self.write_secure_json(&path, &json)?;

        info!(
            "Safety snapshot created before '{}': {:?} ({} symlinks)",
            operation, path, total_count
        );

        if self.max_safety_backups > 0 {
            self.rotate_by_prefix("safety-", self.max_safety_backups)?;
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
        let json = std::fs::read_to_string(backup_path)?;
        let manifest: BackupManifest = serde_json::from_str(&json)?;

        info!(
            "Restoring from backup: {} ({} symlinks, {})",
            manifest.label,
            manifest.total_count,
            manifest.timestamp.format("%Y-%m-%d %H:%M:%S")
        );

        let mut restored = 0usize;
        let mut skipped = 0usize;
        let mut errors = 0usize;

        for entry in &manifest.symlinks {
            if enforce_roots {
                if !path_is_within_roots(&entry.symlink_path, allowed_symlink_roots) {
                    warn!(
                        "Skipping restore outside allowed library roots: {:?}",
                        entry.symlink_path
                    );
                    skipped += 1;
                    continue;
                }
                if !path_is_within_roots(&entry.target_path, allowed_target_roots) {
                    warn!(
                        "Skipping restore outside allowed source roots: {:?}",
                        entry.target_path
                    );
                    skipped += 1;
                    continue;
                }
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
                } else if should_restore_db_record(db, entry).await? {
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
                        if should_restore_db_record(db, entry).await? {
                            upsert_restored_link(db, entry).await?;
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
                Ok(()) => match upsert_restored_link(db, entry).await {
                    Ok(()) => {
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
        self.rotate_by_prefix("backup-", self.max_backups)?;

        // Rotate safety snapshots (0 = keep all)
        if self.max_safety_backups > 0 {
            self.rotate_by_prefix("safety-", self.max_safety_backups)?;
        }

        Ok(())
    }

    /// Rotate files matching a prefix, keeping only `max_count`.
    fn rotate_by_prefix(&self, prefix: &str, max_count: usize) -> Result<()> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.backup_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(prefix) && n.ends_with(".json"))
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
            std::fs::remove_file(&oldest)?;
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
                Ok(json) => match serde_json::from_str::<BackupManifest>(&json) {
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
        summaries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok(summaries)
    }
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

fn path_is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
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
    let normalized_abs = normalize_path_for_root_check(&abs);

    roots.iter().any(|root| {
        let normalized_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let normalized_root = normalize_path_for_root_check(&normalized_root);
        normalized_abs.starts_with(&normalized_root)
    })
}

async fn should_restore_db_record(db: &Database, entry: &BackupEntry) -> Result<bool> {
    match db.get_link_by_target_path(&entry.symlink_path).await? {
        Some(existing) => Ok(existing.source_path != entry.target_path
            || existing.media_id != entry.media_id
            || existing.media_type != entry.media_type
            || existing.status != LinkStatus::Active),
        None => Ok(true),
    }
}

async fn upsert_restored_link(db: &Database, entry: &BackupEntry) -> Result<()> {
    let record = LinkRecord {
        id: None,
        source_path: entry.target_path.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn test_config(dir: &Path) -> BackupConfig {
        BackupConfig {
            enabled: true,
            path: dir.to_path_buf(),
            interval_hours: 24,
            max_backups: 3,
            max_safety_backups: 0, // keep all safety snapshots by default
        }
    }

    #[tokio::test]
    async fn test_backup_restore_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        // Create a fake manifest
        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "test".to_string(),
            symlinks: vec![
                BackupEntry {
                    symlink_path: dir.path().join("show/Season 01/ep1.mkv"),
                    target_path: dir.path().join("rd/ep1.mkv"),
                    media_id: "tvdb-12345".to_string(),
                    media_type: MediaType::Tv,
                },
                BackupEntry {
                    symlink_path: dir.path().join("show/Season 01/ep2.mkv"),
                    target_path: dir.path().join("rd/ep2.mkv"),
                    media_id: "tvdb-12345".to_string(),
                    media_type: MediaType::Tv,
                },
            ],
            total_count: 2,
        };

        // Write backup
        let backup_path = dir.path().join("test-backup.json");
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(&backup_path, json).unwrap();

        // Restore from backup (dry-run)
        let roots = vec![dir.path().to_path_buf()];
        let (restored, skipped, errors) = manager
            .restore(&db, &backup_path, true, &roots, &roots, true)
            .await
            .unwrap();
        assert_eq!(restored, 2);
        assert_eq!(skipped, 0);
        assert_eq!(errors, 0);

        // Restore for real
        let (restored, skipped, errors) = manager
            .restore(&db, &backup_path, false, &roots, &roots, true)
            .await
            .unwrap();
        assert_eq!(restored, 2);
        assert_eq!(skipped, 0);
        assert_eq!(errors, 0);

        // Verify symlinks exist
        assert!(dir.path().join("show/Season 01/ep1.mkv").is_symlink());
        assert!(dir.path().join("show/Season 01/ep2.mkv").is_symlink());

        // Restore again — should skip existing
        let (restored, skipped, errors) = manager
            .restore(&db, &backup_path, false, &roots, &roots, true)
            .await
            .unwrap();
        assert_eq!(restored, 0);
        assert_eq!(skipped, 2);
        assert_eq!(errors, 0);

        assert_eq!(db.get_stats().await.unwrap().0, 2);
    }

    #[tokio::test]
    async fn test_restore_backfills_database_for_existing_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let symlink_path = dir.path().join("show/Season 01/ep1.mkv");
        let target_path = dir.path().join("rd/ep1.mkv");
        std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::fs::write(&target_path, "video").unwrap();
        std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();

        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "existing symlink".to_string(),
            symlinks: vec![BackupEntry {
                symlink_path: symlink_path.clone(),
                target_path: target_path.clone(),
                media_id: "tvdb-1".to_string(),
                media_type: MediaType::Tv,
            }],
            total_count: 1,
        };
        let backup_path = dir.path().join("existing-symlink.json");
        std::fs::write(
            &backup_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let roots = vec![dir.path().to_path_buf()];
        let (restored, skipped, errors) = manager
            .restore(&db, &backup_path, false, &roots, &roots, true)
            .await
            .unwrap();

        assert_eq!(restored, 1);
        assert_eq!(skipped, 0);
        assert_eq!(errors, 0);
        assert!(db
            .get_link_by_target_path(&symlink_path)
            .await
            .unwrap()
            .is_some());
    }

    #[test]
    fn test_rotation_keeps_max_backups() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = test_config(dir.path());
        let manager = BackupManager::new(&config);

        // Create 5 scheduled backups (max is 3)
        for i in 0..5 {
            let filename = format!("backup-2026010{}-120000.json", i);
            let manifest = BackupManifest {
                version: 1,
                timestamp: Utc::now(),
                backup_type: BackupType::Scheduled,
                label: format!("test {}", i),
                symlinks: vec![],
                total_count: 0,
            };
            let json = serde_json::to_string_pretty(&manifest).unwrap();
            std::fs::write(dir.path().join(filename), json).unwrap();
        }

        // Also create a safety snapshot (should NOT be rotated)
        let safety_manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Safety {
                operation: "repair".to_string(),
            },
            label: "safety test".to_string(),
            symlinks: vec![],
            total_count: 0,
        };
        let json = serde_json::to_string_pretty(&safety_manifest).unwrap();
        std::fs::write(dir.path().join("safety-repair-20260101-120000.json"), json).unwrap();

        // Rotate
        manager.rotate().unwrap();

        // Count remaining scheduled backups
        let remaining: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("backup-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(remaining.len(), 3);

        // Safety snapshot should still exist
        assert!(dir
            .path()
            .join("safety-repair-20260101-120000.json")
            .exists());
    }

    #[test]
    fn test_list_backups() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));

        // Create a test backup
        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "full backup".to_string(),
            symlinks: vec![BackupEntry {
                symlink_path: PathBuf::from("/plex/tv/show/ep.mkv"),
                target_path: PathBuf::from("/mnt/rd/ep.mkv"),
                media_id: "tvdb-1".to_string(),
                media_type: MediaType::Tv,
            }],
            total_count: 1,
        };
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(dir.path().join("backup-20260212-120000.json"), json).unwrap();

        let list = manager.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].symlink_count, 1);
        assert_eq!(list[0].backup_type, BackupType::Scheduled);
    }

    #[test]
    fn test_path_is_within_roots_rejects_parent_escape() {
        let dir = tempfile::TempDir::new().unwrap();
        let allowed = dir.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();

        let escaped = allowed.join("..").join("escaped").join("file.mkv");
        assert!(!path_is_within_roots(&escaped, &[allowed]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_restore_skips_symlink_path_escape_even_with_lexical_prefix_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let backup_dir = dir.path().join("backups");
        let library_root = dir.path().join("library");
        let source_root = dir.path().join("sources");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let manager = BackupManager::new(&BackupConfig {
            enabled: true,
            path: backup_dir.clone(),
            interval_hours: 24,
            max_backups: 3,
            max_safety_backups: 3,
        });

        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "escape".to_string(),
            symlinks: vec![BackupEntry {
                symlink_path: library_root.join("..").join("escaped").join("outside.mkv"),
                target_path: source_root.join("video.mkv"),
                media_id: "tmdb-123".to_string(),
                media_type: MediaType::Movie,
            }],
            total_count: 1,
        };

        std::fs::write(source_root.join("video.mkv"), "video").unwrap();

        let backup_path = backup_dir.join("backup.json");
        std::fs::write(
            &backup_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let (restored, skipped, errors) = manager
            .restore(
                &db,
                &backup_path,
                false,
                std::slice::from_ref(&library_root),
                std::slice::from_ref(&source_root),
                true,
            )
            .await
            .unwrap();

        assert_eq!(restored, 0);
        assert_eq!(skipped, 1);
        assert_eq!(errors, 0);
        assert!(!dir.path().join("escaped").join("outside.mkv").exists());
    }
}
