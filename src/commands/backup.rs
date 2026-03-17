use anyhow::Result;
use tracing::info;

use crate::backup;
use crate::commands::print_json;
use crate::config::Config;
use crate::db::Database;
use crate::OutputFormat;

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
