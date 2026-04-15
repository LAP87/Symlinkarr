//! Standalone restore command that works without an existing config.
//!
//! `symlinkarr restore <path>` — bootstrap a fresh installation from a backup
//! archive. Extracts config, secrets, and database snapshots without requiring
//! a running Symlinkarr setup or a config.yaml file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::backup;
use crate::config::{self, BackupConfig};
use crate::utils::path_under_roots;

/// Run a standalone restore from a backup file without needing config.
pub async fn run_standalone_restore(
    backup_path: &Path,
    target_config_dir: Option<&Path>,
    dry_run: bool,
    list_only: bool,
) -> Result<()> {
    info!("=== Symlinkarr Standalone Restore ===");

    if !backup_path.exists() {
        anyhow::bail!("Backup file not found: {}", backup_path.display());
    }

    let backup_dir = backup_path
        .parent()
        .context("Backup file has no parent directory")?
        .to_path_buf();
    let bm = backup::BackupManager::new(&BackupConfig::standalone(backup_dir));

    let resolved = bm.resolve_restore_path(backup_path)?;
    let json = std::fs::read_to_string(&resolved)
        .with_context(|| format!("Failed to read backup file: {}", resolved.display()))?;
    let manifest = backup::parse_backup_manifest(&json, &resolved)?;

    // ── List mode ──────────────────────────────────────────────────
    if list_only {
        println!("📦 {}\n", manifest.label);
        println!("   Created:  {}", manifest.timestamp.format("%Y-%m-%d %H:%M:%S"));
        println!("   Symlinks: {}", manifest.total_count);
        println!(
            "   Type:     {}",
            match &manifest.backup_type {
                backup::BackupType::Scheduled => "Scheduled".to_string(),
                backup::BackupType::Safety { operation } => format!("Safety ({})", operation),
            }
        );
        if let Some(ref db_snap) = manifest.database_snapshot {
            println!(
                "   🗄️  SQLite snapshot: {} ({} bytes)",
                db_snap.filename, db_snap.size_bytes
            );
        }
        if let Some(ref app_state) = manifest.app_state {
            if app_state.config_snapshot.is_some() {
                println!("   ⚙️  Config snapshot: yes");
            }
            if !app_state.secret_snapshots.is_empty() {
                println!(
                    "   🔐 Secret snapshots: {}",
                    app_state.secret_snapshots.len()
                );
            }
        }
        return Ok(());
    }

    // ── Determine target directory ────────────────────────────────
    let config_dir = target_config_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            // Try common locations for an existing config
            let candidates = vec![
                PathBuf::from("/app/config"),
                PathBuf::from("."),
            ];
            for dir in &candidates {
                if dir.join("config.yaml").exists() {
                    return dir.clone();
                }
            }
            // Default to /app/config if it exists as a directory, else current dir
            if Path::new("/app/config").is_dir() {
                PathBuf::from("/app/config")
            } else {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }
        });

    // ── Dry-run mode ──────────────────────────────────────────────
    if dry_run {
        println!("📋 Backup: {}", manifest.label);
        println!("   Created: {}", manifest.timestamp.format("%Y-%m-%d %H:%M:%S"));
        println!("   Symlinks: {}", manifest.total_count);
        println!("\n🔍 Dry run — no files will be written.");
        println!("   Would restore app state to: {}", config_dir.display());

        if let Some(ref db_snap) = manifest.database_snapshot {
            println!("   Would restore database: {}", db_snap.filename);
        }
        if let Some(ref app_state) = manifest.app_state {
            if app_state.config_snapshot.is_some() {
                println!("   Would restore config: config.yaml");
            }
            for secret in &app_state.secret_snapshots {
                println!("   Would restore secret: {} → {}", secret.filename, secret.original_path.display());
            }
        }
        return Ok(());
    }

    // ── Perform restore ───────────────────────────────────────────
    let result = restore_app_state_standalone(&bm, &manifest, &config_dir)?;

    println!("\n📋 Restore Summary:");
    if let Some(ref cfg_path) = result.config_snapshot_restored {
        println!("   ⚙️  Config restored: {}", cfg_path.display());
    } else if result.config_already_existed {
        println!("   ⏭️  Config already exists, skipped");
    } else {
        println!("   ℹ️  No config snapshot in backup");
    }
    if let Some(ref db_path) = result.db_snapshot_restored {
        println!("   🗄️  Database restored: {}", db_path.display());
    } else if result.db_already_existed {
        println!("   ⏭️  Database already exists, skipped");
    }
    if result.secrets_restored > 0 {
        println!("   🔐 Secrets restored: {}", result.secrets_restored);
    }
    if result.secrets_skipped > 0 {
        println!("   ⏭️  Secrets skipped (already exist): {}", result.secrets_skipped);
    }

    println!("\n💡 Next steps:");
    println!("   1. Edit the restored config.yaml to match your environment");
    println!("   2. Add any env-based secrets (environment variables) manually");
    println!("   3. Run `symlinkarr scan` or `symlinkarr daemon` to start");

    Ok(())
}

struct StandaloneRestoreResult {
    config_snapshot_restored: Option<PathBuf>,
    config_already_existed: bool,
    db_snapshot_restored: Option<PathBuf>,
    db_already_existed: bool,
    secrets_restored: usize,
    secrets_skipped: usize,
}

fn default_db_restore_target(config_dir: &Path) -> PathBuf {
    config_dir.join("symlinkarr.db")
}

fn load_restore_targets(config_path: &Path) -> Option<config::RestoreConfigTargets> {
    match config::inspect_restore_targets(config_path) {
        Ok(targets) => Some(targets),
        Err(err) => {
            warn!(
                "Failed to inspect restored config {}: {}",
                config_path.display(),
                err
            );
            None
        }
    }
}

fn allowed_secret_restore_roots(config_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![config_dir.to_path_buf()];

    if config_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "config")
    {
        if let Some(parent) = config_dir.parent() {
            roots.push(parent.join("secrets"));
        }
    }

    roots
}

fn secret_restore_target_allowed(config_dir: &Path, target: &Path) -> bool {
    path_under_roots(target, &allowed_secret_restore_roots(config_dir))
}

fn ensure_parent_dir(path: &Path, what: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory for {what}: {}", parent.display()))?;
        }
    }
    Ok(())
}

fn restore_app_state_standalone(
    bm: &backup::BackupManager,
    manifest: &backup::BackupManifest,
    config_dir: &Path,
) -> Result<StandaloneRestoreResult> {
    let config_path = config_dir.join("config.yaml");
    let mut result = StandaloneRestoreResult {
        config_snapshot_restored: None,
        config_already_existed: false,
        db_snapshot_restored: None,
        db_already_existed: false,
        secrets_restored: 0,
        secrets_skipped: 0,
    };

    let has_app_state = manifest.app_state.is_some();
    let has_db_snapshot = manifest.database_snapshot.is_some();

    if !has_app_state && !has_db_snapshot {
        println!("ℹ️  Backup contains no app state or database snapshot.");
        println!("   To restore symlinks from this backup, run:");
        println!("   symlinkarr backup restore {}", manifest.label);
        println!("   (requires a running Symlinkarr with config.yaml)");
        return Ok(result);
    }

    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("Failed to create config directory: {}", config_dir.display()))?;

    // Restore config.yaml
    if let Some(ref app_state) = manifest.app_state {
        if let Some(ref cfg_snapshot) = app_state.config_snapshot {
            if config_path.exists() {
                warn!("Config already exists at {}, skipping", config_path.display());
                println!(
                    "   ⏭️  Config already exists at {}, skipping",
                    config_path.display()
                );
                result.config_already_existed = true;
            } else {
                let source_path = bm.resolve_restore_path(Path::new(&cfg_snapshot.filename))?;
                std::fs::copy(&source_path, &config_path).with_context(|| {
                    format!("Failed to restore config to {}", config_path.display())
                })?;
                info!("Restored config to {}", config_path.display());
                println!("   ⚙️  Config restored to {}", config_path.display());
                result.config_snapshot_restored = Some(config_path.clone());
            }
        }

        let restore_targets = if config_path.exists() {
            load_restore_targets(&config_path)
        } else {
            None
        };
        let configured_secret_targets = restore_targets
            .as_ref()
            .map(|targets| targets.secret_files.as_slice())
            .unwrap_or(&[]);

        // Restore secret files
        for (index, secret) in app_state.secret_snapshots.iter().enumerate() {
            let Some(target) = configured_secret_targets.get(index) else {
                warn!(
                    "Skipping secret snapshot {} because config {} does not expose a matching secretfile target",
                    secret.filename,
                    config_path.display()
                );
                println!(
                    "   ⏭️  Secret {} skipped: no matching secretfile target in {}",
                    secret.filename,
                    config_path.display()
                );
                result.secrets_skipped += 1;
                continue;
            };
            if !secret_restore_target_allowed(config_dir, target) {
                warn!(
                    "Skipping secret restore outside allowed config roots: {}",
                    target.display()
                );
                println!(
                    "   ⏭️  Secret {} skipped: target {} is outside allowed config roots",
                    secret.filename,
                    target.display()
                );
                result.secrets_skipped += 1;
                continue;
            }
            if target.exists() {
                warn!("Secret already exists at {}, skipping", target.display());
                println!("   ⏭️  Secret already exists at {}, skipping", target.display());
                result.secrets_skipped += 1;
                continue;
            }
            ensure_parent_dir(target, "secret restore target")?;
            let source_path = bm.resolve_restore_path(Path::new(&secret.filename))?;
            std::fs::copy(&source_path, target)
                .with_context(|| format!("Failed to restore secret to {}", target.display()))?;
            info!("Restored secret to {}", target.display());
            println!("   🔐 Secret restored to {}", target.display());
            result.secrets_restored += 1;
        }
    }

    let db_target = if config_path.exists() {
        load_restore_targets(&config_path)
            .map(|targets| targets.db_path)
            .unwrap_or_else(|| default_db_restore_target(config_dir))
    } else {
        default_db_restore_target(config_dir)
    };

    // Restore database snapshot
    if let Some(ref db_snap) = manifest.database_snapshot {
        let source_path = bm.resolve_restore_path(Path::new(&db_snap.filename))?;
        if db_target.exists() {
            warn!("Database already exists at {}, skipping", db_target.display());
            println!("   ⏭️  Database already exists at {}, skipping", db_target.display());
            result.db_already_existed = true;
        } else {
            ensure_parent_dir(&db_target, "database restore target")?;
            std::fs::copy(&source_path, &db_target)
                .with_context(|| format!("Failed to restore database to {}", db_target.display()))?;
            info!("Restored database snapshot to {}", db_target.display());
            println!("   🗄️  Database restored to {}", db_target.display());
            result.db_snapshot_restored = Some(db_target);
        }
    }

    Ok(result)
}

/// Restore app-state (config + secrets) from a backup manifest for auto-restore.
/// Unlike the full standalone restore, this is meant to be called from auto-restore
/// and skips any files that already exist on disk.
pub fn restore_app_state_auto(
    bm: &backup::BackupManager,
    manifest: &backup::BackupManifest,
    config_path: &Path,
) -> Result<()> {
    let Some(ref app_state) = manifest.app_state else {
        return Ok(());
    };
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));

    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("Failed to create config directory: {}", config_dir.display()))?;

    if let Some(ref cfg_snapshot) = app_state.config_snapshot {
        if !config_path.exists() {
            let source_path = bm.resolve_restore_path(Path::new(&cfg_snapshot.filename))?;
            std::fs::copy(&source_path, config_path)
                .with_context(|| format!("Failed to restore config to {}", config_path.display()))?;
            info!("Auto-restored config to {}", config_path.display());
        }
    }

    let restore_targets = if config_path.exists() {
        load_restore_targets(config_path)
    } else {
        None
    };
    let configured_secret_targets = restore_targets
        .as_ref()
        .map(|targets| targets.secret_files.as_slice())
        .unwrap_or(&[]);

    for (index, secret) in app_state.secret_snapshots.iter().enumerate() {
        let Some(target) = configured_secret_targets.get(index) else {
            warn!(
                "Auto-restore: skipping secret snapshot {} because config {} has no matching secretfile target",
                secret.filename,
                config_path.display()
            );
            continue;
        };
        if !secret_restore_target_allowed(config_dir, target) {
            warn!(
                "Auto-restore: skipping secret restore outside allowed config roots: {}",
                target.display()
            );
            continue;
        }
        if target.exists() {
            continue;
        }
        if let Err(e) = ensure_parent_dir(target, "secret restore target") {
            warn!("Auto-restore: {}", e);
            continue;
        }
        if let Ok(source_path) = bm.resolve_restore_path(Path::new(&secret.filename)) {
            if let Err(e) = std::fs::copy(&source_path, target) {
                warn!("Auto-restore: failed to restore secret to {}: {}", target.display(), e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_restore_target_allowed_accepts_config_tree() {
        assert!(secret_restore_target_allowed(
            Path::new("/srv/symlinkarr/config"),
            Path::new("/srv/symlinkarr/config/secrets/rd-token")
        ));
    }

    #[test]
    fn secret_restore_target_allowed_accepts_standard_docker_sibling_secrets() {
        assert!(secret_restore_target_allowed(
            Path::new("/app/config"),
            Path::new("/app/secrets/rd-token")
        ));
    }

    #[test]
    fn secret_restore_target_allowed_rejects_escaped_or_external_paths() {
        assert!(!secret_restore_target_allowed(
            Path::new("/app/config"),
            Path::new("/app/config/../outside/rd-token")
        ));
        assert!(!secret_restore_target_allowed(
            Path::new("/app/config"),
            Path::new("/etc/symlinkarr/rd-token")
        ));
    }
}
