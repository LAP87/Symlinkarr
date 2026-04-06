use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::config::BackupConfig;
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MediaType};
use crate::utils::{cached_source_health, PathHealth};

// ─── Backup data structures ─────────────────────────────────────────

const BACKUP_MANIFEST_VERSION: u32 = 2;

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
    /// Integrity hash for the manifest content, excluding this field.
    #[serde(default)]
    pub content_sha256: Option<String>,
}

fn parse_backup_manifest(json: &str, source: &Path) -> Result<BackupManifest> {
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

fn validate_backup_manifest(manifest: &BackupManifest, source: &Path) -> Result<()> {
    match manifest.version {
        1 => return Ok(()),
        BACKUP_MANIFEST_VERSION => {}
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

fn compute_manifest_checksum(manifest: &BackupManifest) -> Result<String> {
    let payload = BackupManifestChecksumPayload {
        version: manifest.version,
        timestamp: manifest.timestamp,
        backup_type: &manifest.backup_type,
        label: &manifest.label,
        symlinks: &manifest.symlinks,
        total_count: manifest.total_count,
        database_snapshot: &manifest.database_snapshot,
    };
    let json = serde_json::to_vec(&payload)?;
    let mut hasher = Sha256::new();
    hasher.update(&json);
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_file(path: &Path) -> Result<String> {
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

fn validate_managed_backup_file_name(file_name: &str) -> Result<()> {
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

fn backup_path_candidates(backup_path: &Path, backup_root: &Path) -> Vec<PathBuf> {
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

        for candidate in backup_path_candidates(backup_path, &backup_root) {
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };

            if canonical.starts_with(&backup_root) {
                return Ok(canonical);
            }
            anyhow::bail!("Backup restore path escapes the configured backup directory");
        }

        if backup_path.is_absolute() {
            anyhow::bail!(
                "Backup restore only accepts files inside the configured backup directory"
            );
        }

        if backup_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
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
                db_tracked: true,
            })
            .collect();

        let total_count = entries.len();
        let timestamp = Utc::now();
        let base_name = format!("backup-{}", timestamp.format("%Y%m%d-%H%M%S"));
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
            content_sha256: None,
        };
        manifest.content_sha256 = Some(compute_manifest_checksum(&manifest)?);

        let filename = format!("safety-{}-{}.json", operation, now.format("%Y%m%d-%H%M%S"));
        let path = self.resolve_managed_output_path(&filename)?;

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
        let backup_path = self.resolve_restore_path(backup_path)?;
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

        for entry in &manifest.symlinks {
            let resolved_target_path = resolve_restore_target_path(entry);
            if enforce_roots {
                if !path_is_within_roots(&entry.symlink_path, allowed_symlink_roots) {
                    warn!(
                        "Skipping restore outside allowed library roots: {:?}",
                        entry.symlink_path
                    );
                    skipped += 1;
                    continue;
                }
                if !path_is_within_roots(&resolved_target_path, allowed_target_roots) {
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
                tx.commit().await.with_context(|| {
                    format!(
                        "Backup restore from {} finished filesystem work but failed to commit database updates",
                        backup_path.display()
                    )
                })?;
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

fn restore_target_available(
    target_path: &Path,
    source_health_cache: &mut std::collections::HashMap<PathBuf, PathHealth>,
    parent_health_cache: &mut std::collections::HashMap<PathBuf, PathHealth>,
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
    let normalized_abs = normalize_path_for_root_check(&resolve_path_for_root_check(&abs));

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

async fn upsert_restored_link_in_tx(
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

fn resolve_restore_target_path(entry: &BackupEntry) -> PathBuf {
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

fn resolve_path_for_root_check(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }

    if let Some(parent) = path.parent() {
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            if let Some(name) = path.file_name() {
                return canonical_parent.join(name);
            }
            return canonical_parent;
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
        let source_root = dir.path().join("rd");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::write(source_root.join("ep1.mkv"), "video").unwrap();
        std::fs::write(source_root.join("ep2.mkv"), "video").unwrap();

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
                    db_tracked: true,
                },
                BackupEntry {
                    symlink_path: dir.path().join("show/Season 01/ep2.mkv"),
                    target_path: dir.path().join("rd/ep2.mkv"),
                    media_id: "tvdb-12345".to_string(),
                    media_type: MediaType::Tv,
                    db_tracked: true,
                },
            ],
            total_count: 2,
            database_snapshot: None,
            content_sha256: None,
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
    async fn test_create_backup_writes_database_snapshot_and_checksum() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let source_root = dir.path().join("rd");
        let library_root = dir.path().join("library");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&library_root).unwrap();
        let source_file = source_root.join("ep1.mkv");
        let symlink_file = library_root.join("ep1.mkv");
        std::fs::write(&source_file, "video").unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: source_file,
            target_path: symlink_file,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let backup_path = manager.create_backup(&db, "Manual backup").await.unwrap();
        let manifest = parse_backup_manifest(
            &std::fs::read_to_string(&backup_path).unwrap(),
            &backup_path,
        )
        .unwrap();

        let snapshot = manifest.database_snapshot.as_ref().unwrap();
        let snapshot_path = dir.path().join(&snapshot.filename);
        assert!(snapshot_path.exists());
        assert_eq!(snapshot.sha256, sha256_file(&snapshot_path).unwrap());
        assert!(manifest.content_sha256.is_some());
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
                db_tracked: true,
            }],
            total_count: 1,
            database_snapshot: None,
            content_sha256: None,
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

    #[tokio::test]
    async fn test_create_safety_snapshot_with_extras_captures_disk_only_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let manager = BackupManager::new(&test_config(&backup_dir));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let source_dir = dir.path().join("rd");
        let library_dir = dir.path().join("library");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&library_dir).unwrap();
        let source_file = source_dir.join("episode.mkv");
        std::fs::write(&source_file, "video").unwrap();
        let disk_only_symlink = library_dir.join("legacy.mkv");
        std::os::unix::fs::symlink(&source_file, &disk_only_symlink).unwrap();

        let snapshot_path = manager
            .create_safety_snapshot_with_extras(
                &db,
                "cleanup-prune",
                std::slice::from_ref(&disk_only_symlink),
            )
            .await
            .unwrap();

        let manifest: BackupManifest =
            serde_json::from_str(&std::fs::read_to_string(&snapshot_path).unwrap()).unwrap();
        assert_eq!(manifest.total_count, 1);
        assert_eq!(manifest.symlinks[0].symlink_path, disk_only_symlink);
        assert_eq!(manifest.symlinks[0].target_path, source_file);
        assert!(!manifest.symlinks[0].db_tracked);
    }

    #[tokio::test]
    async fn test_restore_disk_only_backup_entry_skips_db_backfill() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let symlink_path = dir.path().join("show/Season 01/legacy.mkv");
        let target_path = dir.path().join("rd/legacy.mkv");
        std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::fs::write(&target_path, "video").unwrap();

        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Safety {
                operation: "cleanup-prune".to_string(),
            },
            label: "disk only".to_string(),
            symlinks: vec![BackupEntry {
                symlink_path: symlink_path.clone(),
                target_path: target_path.clone(),
                media_id: String::new(),
                media_type: MediaType::Tv,
                db_tracked: false,
            }],
            total_count: 1,
            database_snapshot: None,
            content_sha256: None,
        };
        let backup_path = dir.path().join("disk-only-backup.json");
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
        assert!(symlink_path.is_symlink());
        assert!(db
            .get_link_by_target_path(&symlink_path)
            .await
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_restore_disk_only_relative_target_passes_root_validation() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let symlink_path = dir.path().join("library/show/Season 01/legacy.mkv");
        let target_path = dir.path().join("rd/legacy.mkv");
        std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::fs::write(&target_path, "video").unwrap();

        let relative_target = PathBuf::from("../../../rd/legacy.mkv");
        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Safety {
                operation: "cleanup-prune".to_string(),
            },
            label: "disk only relative".to_string(),
            symlinks: vec![BackupEntry {
                symlink_path: symlink_path.clone(),
                target_path: relative_target.clone(),
                media_id: String::new(),
                media_type: MediaType::Tv,
                db_tracked: false,
            }],
            total_count: 1,
            database_snapshot: None,
            content_sha256: None,
        };
        let backup_path = dir.path().join("disk-only-relative-backup.json");
        std::fs::write(
            &backup_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let library_roots = vec![dir.path().join("library")];
        let source_roots = vec![dir.path().join("rd")];
        let (restored, skipped, errors) = manager
            .restore(
                &db,
                &backup_path,
                false,
                &library_roots,
                &source_roots,
                true,
            )
            .await
            .unwrap();

        assert_eq!(restored, 1);
        assert_eq!(skipped, 0);
        assert_eq!(errors, 0);
        assert!(symlink_path.is_symlink());
        assert_eq!(std::fs::read_link(&symlink_path).unwrap(), relative_target);
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
                database_snapshot: None,
                content_sha256: None,
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
            database_snapshot: None,
            content_sha256: None,
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
                db_tracked: true,
            }],
            total_count: 1,
            database_snapshot: None,
            content_sha256: None,
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
    #[test]
    fn test_path_is_within_roots_rejects_symlink_escape() {
        let dir = tempfile::TempDir::new().unwrap();
        let allowed = dir.path().join("allowed");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let outside_file = outside.join("file.mkv");
        std::fs::write(&outside_file, "video").unwrap();

        let alias_dir = allowed.join("alias");
        std::os::unix::fs::symlink(&outside, &alias_dir).unwrap();

        assert!(!path_is_within_roots(
            &alias_dir.join("file.mkv"),
            &[allowed]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_path_is_within_roots_accepts_nested_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let nested = root.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(path_is_within_roots(&nested, &[root.to_path_buf()]));
        assert!(path_is_within_roots(
            &nested.join("file.mkv"),
            &[root.to_path_buf()]
        ));
    }

    #[test]
    fn test_restore_target_available_rejects_unhealthy_parent() {
        let path = PathBuf::from("/mnt/rd/file.mkv");
        let parent = path.parent().unwrap().to_path_buf();
        let mut source_cache = std::collections::HashMap::new();
        let mut parent_cache = std::collections::HashMap::new();
        parent_cache.insert(parent, PathHealth::TransportDisconnected);

        let err =
            restore_target_available(&path, &mut source_cache, &mut parent_cache).unwrap_err();

        assert!(err.to_string().contains("Aborting backup restore"));
    }

    #[cfg(unix)]
    #[test]
    fn test_path_is_within_roots_rejects_empty_roots() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("file.mkv");
        std::fs::write(&file, "video").unwrap();
        assert!(!path_is_within_roots(&file, &[]));
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
                db_tracked: true,
            }],
            total_count: 1,
            database_snapshot: None,
            content_sha256: None,
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

    #[tokio::test]
    async fn test_restore_skips_missing_target_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let library_root = dir.path().join("library");
        let source_root = dir.path().join("rd");
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();

        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "missing target".to_string(),
            symlinks: vec![BackupEntry {
                symlink_path: library_root.join("Show/Season 01/S01E01.mkv"),
                target_path: source_root.join("missing.mkv"),
                media_id: "tvdb-12345".to_string(),
                media_type: MediaType::Tv,
                db_tracked: true,
            }],
            total_count: 1,
            database_snapshot: None,
            content_sha256: None,
        };

        let backup_path = dir.path().join("missing-target.json");
        std::fs::write(
            &backup_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let roots = vec![dir.path().to_path_buf()];
        let source_roots = vec![source_root];
        let (restored, skipped, errors) = manager
            .restore(&db, &backup_path, false, &roots, &source_roots, true)
            .await
            .unwrap();

        assert_eq!(restored, 0);
        assert_eq!(skipped, 1);
        assert_eq!(errors, 0);
        assert!(!manifest.symlinks[0].symlink_path.exists());
        assert!(db
            .get_link_by_target_path(&manifest.symlinks[0].symlink_path)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_list_skips_unsupported_backup_manifest_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));

        let manifest = BackupManifest {
            version: BACKUP_MANIFEST_VERSION + 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "future backup".to_string(),
            symlinks: vec![],
            total_count: 0,
            database_snapshot: None,
            content_sha256: None,
        };
        std::fs::write(
            dir.path().join("backup-future.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let list = manager.list().unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_restore_rejects_unsupported_backup_manifest_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
            .await
            .unwrap();

        let backup_path = dir.path().join("future-backup.json");
        let manifest = BackupManifest {
            version: BACKUP_MANIFEST_VERSION + 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "future backup".to_string(),
            symlinks: vec![],
            total_count: 0,
            database_snapshot: None,
            content_sha256: None,
        };
        std::fs::write(
            &backup_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let err = manager
            .restore(&db, &backup_path, false, &[], &[], false)
            .await
            .unwrap_err();

        assert!(err
            .to_string()
            .contains("Unsupported backup manifest version"));
    }

    #[tokio::test]
    async fn test_restore_rejects_manifest_checksum_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let manager = BackupManager::new(&test_config(dir.path()));
        let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
            .await
            .unwrap();

        let backup_path = dir.path().join("tampered-backup.json");
        let mut manifest = BackupManifest {
            version: BACKUP_MANIFEST_VERSION,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: "tampered backup".to_string(),
            symlinks: vec![],
            total_count: 0,
            database_snapshot: None,
            content_sha256: None,
        };
        manifest.content_sha256 = Some(compute_manifest_checksum(&manifest).unwrap());
        manifest.label = "tampered after checksum".to_string();
        std::fs::write(
            &backup_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let err = manager
            .restore(&db, &backup_path, false, &[], &[], false)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("integrity check failed"));
    }
}
