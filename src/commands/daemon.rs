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
    let web_task = if cfg.has_web() {
        let web_cfg = cfg.clone();
        let web_db = db.clone();
        let port = cfg.web.port;
        Some(tokio::spawn(async move {
            if let Err(e) = crate::web::serve(web_cfg, web_db, port).await {
                tracing::error!("Web UI failed: {}", e);
            }
        }))
    } else {
        None
    };

    // Start handoff queue watcher in background if markers_dir is configured
    let handoff_task = if let Some(markers_dir) = cfg
        .handoff
        .markers_dir
        .as_ref()
        .map(std::path::PathBuf::from)
    {
        let handoff_cfg = cfg.clone();
        let handoff_db = db.clone();
        Some(tokio::spawn(async move {
            run_handoff_watcher(handoff_cfg, handoff_db, markers_dir).await;
        }))
    } else {
        None
    };

    record_heartbeat(db, "scheduler", Some("Starting live scheduler tick loop")).await;
    let scheduler_result = crate::scheduler::run_scheduler_loop(cfg, db).await;
    if let Some(handoff_task) = handoff_task {
        handoff_task.abort();
    }
    if let Some(mut web_task) = web_task {
        if scheduler_result.is_err() {
            web_task.abort();
        }
        if let Err(err) = (&mut web_task).await {
            if !err.is_cancelled() {
                tracing::warn!("Web task ended unexpectedly: {}", err);
            }
        }
    }
    scheduler_result
}

pub(crate) async fn process_handoff_markers_once(
    cfg: &Config,
    db: &Database,
    markers_dir: &std::path::Path,
) -> usize {
    if !markers_dir.exists() {
        return 0;
    }

    let has_markers = std::fs::read_dir(markers_dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                let path = e.path();
                path.extension().is_some_and(|ext| ext == "json")
                    && !path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            })
        })
        .unwrap_or(false);

    if !has_markers {
        return 0;
    }

    let import = crate::handoff::import_markers(db, markers_dir, cfg.handoff.consume_markers).await;
    if import.imported > 0 {
        info!(
            "Handoff watcher imported {} marker(s): {}",
            import.imported,
            import.summary_line()
        );
        for folder in &import.imported_folders {
            info!(
                "Handoff watcher: triggering targeted link for folder '{}'",
                folder
            );
            match crate::commands::scan::run_scan_with_origin(
                cfg,
                db,
                crate::db::ScanRunOrigin::Daemon,
                false,
                false,
                crate::OutputFormat::Text,
                None,
                Some(folder),
            )
            .await
            {
                Ok((added, _)) => {
                    info!("Handoff watcher: linked {} items for '{}'", added, folder);
                }
                Err(e) => {
                    tracing::warn!(
                        "Handoff watcher targeted link for '{}' failed: {}",
                        folder,
                        e
                    );
                }
            }
        }
    }
    import.imported
}

pub(crate) async fn run_handoff_watcher(
    cfg: Config,
    db: Database,
    markers_dir: std::path::PathBuf,
) {
    info!(
        "Handoff queue watcher started for: {}",
        markers_dir.display()
    );
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        process_handoff_markers_once(&cfg, &db, &markers_dir).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ContentType, LibraryConfig, SourceConfig};
    use crate::models::MediaType;
    use tempfile::tempdir;

    fn test_config(
        library: std::path::PathBuf,
        source: std::path::PathBuf,
        markers_dir: std::path::PathBuf,
        db_path: std::path::PathBuf,
    ) -> Config {
        Config {
            libraries: vec![LibraryConfig {
                name: "TV".to_string(),
                path: library,
                media_type: MediaType::Tv,
                content_type: Some(ContentType::Tv),
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: source,
                media_type: "auto".to_string(),
            }],
            api: crate::config::ApiConfig::default(),
            realdebrid: crate::config::RealDebridConfig::default(),
            decypharr: crate::config::DecypharrConfig::default(),
            dmm: crate::config::DmmConfig::default(),
            backup: crate::config::BackupConfig::default(),
            db_path: db_path.display().to_string(),
            log_level: "info".to_string(),
            daemon: crate::config::DaemonConfig::default(),
            symlink: crate::config::SymlinkConfig::default(),
            matching: crate::config::MatchingConfig::default(),
            prowlarr: crate::config::ProwlarrConfig::default(),
            bazarr: crate::config::BazarrConfig::default(),
            tautulli: crate::config::TautulliConfig::default(),
            plex: crate::config::PlexConfig::default(),
            emby: crate::config::MediaBrowserConfig::default(),
            jellyfin: crate::config::MediaBrowserConfig::default(),
            radarr: crate::config::RadarrConfig::default(),
            sonarr: crate::config::SonarrConfig::default(),
            sonarr_anime: crate::config::SonarrConfig::default(),
            features: crate::config::FeaturesConfig::default(),
            security: crate::config::SecurityConfig::default(),
            cleanup: crate::config::CleanupPolicyConfig::default(),
            web: crate::config::WebConfig::default(),
            handoff: crate::config::HandoffConfig {
                markers_dir: Some(markers_dir),
                consume_markers: true,
            },
            loaded_from: None,
            secret_files: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_process_handoff_markers_once_imports_and_links() {
        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let markers_dir = tmp.path().join("markers");
        let library_dir = tmp.path().join("library");
        let source_dir = tmp.path().join("source");
        std::fs::create_dir_all(&markers_dir).unwrap();
        std::fs::create_dir_all(&library_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();

        let show_dir = library_dir.join("Show A {tvdb-12345}");
        std::fs::create_dir_all(&show_dir).unwrap();

        let source_folder = source_dir.join("Show.A.S01");
        std::fs::create_dir_all(&source_folder).unwrap();
        let source_file = source_folder.join("Show.A.S01E01.mkv");
        std::fs::write(&source_file, b"content").unwrap();

        let marker_file = markers_dir.join("Show.A.S01.json");
        std::fs::write(
            &marker_file,
            r#"{"materialized_relative_path":"Show.A.S01/Show.A.S01E01.mkv","tvdb_id":12345}"#,
        )
        .unwrap();

        let cfg = test_config(library_dir, source_dir, markers_dir.clone(), db_path);

        let imported = process_handoff_markers_once(&cfg, &db, &markers_dir).await;
        assert_eq!(imported, 1);

        assert!(!marker_file.exists());

        let pins = db.list_source_pins().await.unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].source_folder, "Show.A.S01");
        assert_eq!(pins[0].media_id, "tvdb-12345");

        let active = db.get_active_links().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].source_path, source_file);
    }
}
