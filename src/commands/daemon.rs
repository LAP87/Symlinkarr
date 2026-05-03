use anyhow::Result;
use tracing::info;

use crate::config::Config;
use crate::db::Database;

async fn record_heartbeat(db: &Database, phase: &str, detail: Option<&str>) {
    if let Err(err) = db.record_daemon_heartbeat(phase, detail).await {
        tracing::warn!("Daemon heartbeat update failed (non-fatal): {}", err);
    }
}

pub(crate) async fn run_daemon(cfg: &Config, db: &Database) -> Result<()> {
    info!(
        "Symlinkarr daemon starting with live scheduler (legacy scan interval bootstrap: {} minutes)",
        cfg.daemon.interval_minutes
    );
    record_heartbeat(
        db,
        "starting",
        Some("Daemon loop booted and is preparing the first cycle"),
    )
    .await;

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

    record_heartbeat(db, "scheduler", Some("Starting live scheduler tick loop")).await;
    crate::scheduler::run_scheduler_loop(cfg, db).await
}
