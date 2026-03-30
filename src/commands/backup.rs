use anyhow::Result;
use tracing::info;

use crate::backup;
use crate::commands::{ensure_runtime_directories_healthy, print_json};
use crate::config::Config;
use crate::db::Database;
use crate::OutputFormat;

pub(crate) async fn ensure_backup_restore_runtime_healthy(
    cfg: &Config,
    operation: &str,
) -> Result<()> {
    let selected_libraries: Vec<_> = cfg.libraries.iter().collect();
    ensure_runtime_directories_healthy(&selected_libraries, &cfg.sources, operation).await
}

pub(crate) async fn run_backup(
    cfg: &Config,
    db: &Database,
    action: crate::BackupAction,
    output: OutputFormat,
) -> Result<()> {
    let bm = backup::BackupManager::new(&cfg.backup);

    match action {
        crate::BackupAction::Create => {
            info!("=== Symlinkarr Backup ===");
            let path = bm.create_backup(db, "Manual backup").await?;
            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "created": true,
                    "file": path,
                }));
            } else {
                println!("✅ Backup created: {}", path.display());
            }
        }
        crate::BackupAction::List => {
            let backups = bm.list()?;
            if output == OutputFormat::Json {
                let items: Vec<_> = backups
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "filename": b.filename,
                            "timestamp": b.timestamp,
                            "type": match &b.backup_type {
                                backup::BackupType::Scheduled => "scheduled",
                                backup::BackupType::Safety { .. } => "safety",
                            },
                            "symlink_count": b.symlink_count,
                            "file_size": b.file_size,
                        })
                    })
                    .collect();
                print_json(&serde_json::json!({
                    "count": items.len(),
                    "items": items,
                }));
            } else if backups.is_empty() {
                println!("No backups found in {:?}", cfg.backup.path);
            } else {
                println!("\n📦 Available backups ({}):\n", backups.len());
                for b in &backups {
                    println!("  {}", b);
                }
                println!();
            }
        }
        crate::BackupAction::Restore { file, dry_run } => {
            info!("=== Symlinkarr Restore ===");
            let path = std::path::Path::new(&file);
            if !path.exists() {
                anyhow::bail!("Backup file not found: {}", file);
            }
            ensure_backup_restore_runtime_healthy(cfg, "backup restore").await?;

            let library_roots: Vec<_> = cfg.libraries.iter().map(|l| l.path.clone()).collect();
            let source_roots: Vec<_> = cfg.sources.iter().map(|s| s.path.clone()).collect();
            let (restored, skipped, errors) = bm
                .restore(
                    db,
                    path,
                    dry_run,
                    &library_roots,
                    &source_roots,
                    cfg.security.enforce_roots,
                )
                .await?;

            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "restored": restored,
                    "skipped": skipped,
                    "errors": errors,
                    "dry_run": dry_run,
                }));
            } else {
                println!("\n📋 Restore Results:");
                println!("   ✅ Restored: {}", restored);
                println!("   ⏭️  Skipped: {}", skipped);
                if errors > 0 {
                    println!("   ❌ Errors: {}", errors);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
        SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    fn restore_test_config(root: &std::path::Path) -> Config {
        Config {
            libraries: vec![LibraryConfig {
                name: "Anime".to_string(),
                path: root.join("library"),
                media_type: crate::models::MediaType::Tv,
                content_type: Some(ContentType::Anime),
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: root.join("source"),
                media_type: "auto".to_string(),
            }],
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            api: ApiConfig::default(),
            db_path: root.join("test.db").display().to_string(),
            log_level: "info".to_string(),
            daemon: DaemonConfig::default(),
            symlink: SymlinkConfig::default(),
            matching: MatchingConfig::default(),
            prowlarr: ProwlarrConfig::default(),
            bazarr: BazarrConfig::default(),
            tautulli: TautulliConfig::default(),
            plex: PlexConfig::default(),
            radarr: RadarrConfig::default(),
            sonarr: SonarrConfig::default(),
            sonarr_anime: SonarrConfig::default(),
            features: FeaturesConfig::default(),
            security: SecurityConfig::default(),
            cleanup: CleanupPolicyConfig::default(),
            backup: BackupConfig::default(),
            web: WebConfig::default(),
            loaded_from: None,
            secret_files: Vec::new(),
        }
    }

    #[tokio::test]
    async fn ensure_backup_restore_runtime_healthy_rejects_missing_mounts() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = restore_test_config(dir.path());

        let err = ensure_backup_restore_runtime_healthy(&cfg, "backup restore")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Refusing backup restore"));
    }

    #[tokio::test]
    async fn ensure_backup_restore_runtime_healthy_accepts_existing_roots() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = restore_test_config(dir.path());
        std::fs::create_dir_all(dir.path().join("library")).unwrap();
        std::fs::create_dir_all(dir.path().join("source")).unwrap();

        cfg.libraries[0].path = dir.path().join("library");
        cfg.sources[0].path = dir.path().join("source");

        ensure_backup_restore_runtime_healthy(&cfg, "backup restore")
            .await
            .unwrap();
    }
}
