use std::time::Duration;

use anyhow::Result;
use tracing::info;

use crate::config::Config;
use crate::db::Database;
use crate::OutputFormat;

pub(crate) async fn run_daemon(cfg: &Config, db: &Database) -> Result<()> {
    let interval = Duration::from_secs(cfg.daemon.interval_minutes * 60);
    info!(
        "Symlinkarr daemon starting (interval: {} minutes)",
        cfg.daemon.interval_minutes
    );

    match db.housekeeping().await {
        Ok(stats) => {
            if stats.scan_runs_deleted + stats.link_events_deleted + stats.old_jobs_deleted > 0 {
                info!(
                    "Housekeeping: removed {} old scan_runs, {} link_events, {} completed jobs",
                    stats.scan_runs_deleted, stats.link_events_deleted, stats.old_jobs_deleted
                );
            }
        }
        Err(e) => tracing::warn!("Housekeeping failed (non-fatal): {}", e),
    }

    match db
        .recover_stale_downloading_jobs(cfg.decypharr.completion_timeout_minutes)
        .await
    {
        Ok(n) if n > 0 => info!("Recovered {} stale Downloading jobs after restart", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("Stale job recovery failed (non-fatal): {}", e),
    }

    // Start web UI in background if enabled
    if cfg.has_web() {
        let web_cfg = cfg.clone();
        let web_db = db.clone();
        let port = cfg.web.port;
        tokio::spawn(async move {
            if let Err(e) = crate::web::serve(web_cfg, web_db, port).await {
                tracing::error!("Web UI failed: {}", e);
            }
        });
    }

    loop {
        if cfg.backup.enabled {
            let bm = crate::backup::BackupManager::new(&cfg.backup);
            if let Err(e) = bm.create_safety_snapshot(db, "daemon-scan").await {
                tracing::warn!("Pre-scan backup failed: {}", e);
            }
        }

        if let Err(e) = super::scan::run_scan(
            cfg,
            db,
            false,
            cfg.daemon.search_missing,
            OutputFormat::Text,
            None,
        )
        .await
        {
            tracing::error!("Scan cycle failed: {}", e);
        }

        info!("Next scan in {} minutes...", cfg.daemon.interval_minutes);
        tokio::time::sleep(interval).await;
    }
}
