use std::time::Duration;

use anyhow::Result;
use chrono::{Local, NaiveDate, Timelike};
use tracing::info;

use crate::config::Config;
use crate::db::Database;
use crate::OutputFormat;

async fn run_housekeeping(cfg: &Config, db: &Database, last_vacuum_date: &mut Option<NaiveDate>) {
    let now = Local::now();
    let today = now.date_naive();
    let should_vacuum = cfg.daemon.vacuum_enabled
        && now.hour() >= u32::from(cfg.daemon.vacuum_hour_local)
        && *last_vacuum_date != Some(today);

    match db.housekeeping_with_vacuum(should_vacuum).await {
        Ok(stats) => {
            if should_vacuum {
                *last_vacuum_date = Some(today);
                info!(
                    "Housekeeping: removed {} old scan_runs, {} link_events, {} completed jobs, {} expired API cache entries, and ran VACUUM",
                    stats.scan_runs_deleted,
                    stats.link_events_deleted,
                    stats.old_jobs_deleted,
                    stats.expired_api_cache_deleted
                );
            } else if stats.scan_runs_deleted
                + stats.link_events_deleted
                + stats.old_jobs_deleted
                + stats.expired_api_cache_deleted
                > 0
            {
                info!(
                    "Housekeeping: removed {} old scan_runs, {} link_events, {} completed jobs, {} expired API cache entries",
                    stats.scan_runs_deleted,
                    stats.link_events_deleted,
                    stats.old_jobs_deleted,
                    stats.expired_api_cache_deleted
                );
            }
        }
        Err(e) => tracing::warn!("Housekeeping failed (non-fatal): {}", e),
    }
}

pub(crate) async fn run_daemon(cfg: &Config, db: &Database) -> Result<()> {
    let interval = Duration::from_secs(cfg.daemon.interval_minutes * 60);
    let mut last_vacuum_date = None;
    info!(
        "Symlinkarr daemon starting (interval: {} minutes)",
        cfg.daemon.interval_minutes
    );

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
        run_housekeeping(cfg, db, &mut last_vacuum_date).await;

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
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown signal received; stopping daemon loop");
                break;
            }
        }
    }

    Ok(())
}
