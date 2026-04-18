    use super::*;
    use axum::body::to_bytes;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Executor;
    use std::path::Path;
    use std::str::FromStr;
    use tempfile::TempDir;

    use crate::cleanup_audit::{CleanupReport, CleanupScope, CleanupSummary};
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::{AcquisitionJobSeed, AcquisitionRelinkKind, Database, ScanRunRecord};
    use crate::models::{LinkRecord, LinkStatus, MediaType};
    use crate::web::{
        ActiveCleanupAuditJob, ActiveScanJob, LastCleanupAuditOutcome, LastScanOutcome,
    };

    struct TestWebContext {
        _dir: TempDir,
        state: WebState,
    }

    fn test_config(root: &std::path::Path) -> Config {
        let library = root.join("anime");
        let source = root.join("rd");
        let backups = root.join("backups");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&backups).unwrap();

        Config {
            libraries: vec![LibraryConfig {
                name: "Anime".to_string(),
                path: library,
                media_type: MediaType::Tv,
                content_type: Some(ContentType::Anime),
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: source,
                media_type: "auto".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            backup: BackupConfig {
                path: backups,
                ..BackupConfig::default()
            },
            db_path: root.join("test.db").display().to_string(),
            log_level: "info".to_string(),
            daemon: DaemonConfig::default(),
            symlink: SymlinkConfig::default(),
            matching: MatchingConfig::default(),
            prowlarr: ProwlarrConfig::default(),
            bazarr: BazarrConfig::default(),
            tautulli: TautulliConfig::default(),
            plex: PlexConfig::default(),
            emby: MediaBrowserConfig::default(),
            jellyfin: MediaBrowserConfig::default(),
            radarr: RadarrConfig::default(),
            sonarr: SonarrConfig::default(),
            sonarr_anime: SonarrConfig::default(),
            features: FeaturesConfig::default(),
            security: SecurityConfig::default(),
            cleanup: CleanupPolicyConfig::default(),
            web: WebConfig::default(),
            loaded_from: None,
            secret_files: Vec::new(),
        }
    }

    async fn test_context() -> TestWebContext {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: dir.path().join("rd").join("Show.S01E01.mkv"),
            target_path: dir
                .path()
                .join("anime")
                .join("Show (2024) {tvdb-1}")
                .join("Season 01")
                .join("S01E01.mkv"),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        db.record_scan_run(&ScanRunRecord {
            dry_run: true,
            library_filter: Some("Anime".to_string()),
            run_token: Some("scan-run-handler".to_string()),
            search_missing: true,
            library_items_found: 1,
            source_items_found: 42,
            matches_found: 11,
            links_created: 3,
            links_updated: 1,
            dead_marked: 2,
            links_removed: 0,
            links_skipped: 7,
            ambiguous_skipped: 1,
            skip_reason_json: Some(
                r#"{"already_correct":6200,"source_missing_before_link":3044,"ambiguous_match":70}"#
                    .to_string(),
            ),
            runtime_checks_ms: 10,
            library_scan_ms: 20,
            source_inventory_ms: 30,
            matching_ms: 40,
            title_enrichment_ms: 50,
            linking_ms: 60,
            plex_refresh_ms: 70,
            plex_refresh_requested_paths: 4,
            plex_refresh_unique_paths: 3,
            plex_refresh_planned_batches: 2,
            plex_refresh_coalesced_batches: 1,
            plex_refresh_coalesced_paths: 2,
            plex_refresh_refreshed_batches: 1,
            plex_refresh_refreshed_paths_covered: 3,
            plex_refresh_skipped_batches: 1,
            plex_refresh_unresolved_paths: 0,
            plex_refresh_capped_batches: 1,
            plex_refresh_aborted_due_to_cap: true,
            plex_refresh_failed_batches: 0,
            media_server_refresh_json: Some(
                r#"[{"server":"plex","requested_targets":4,"refresh":{"requested_paths":3,"unique_paths":2,"planned_batches":2,"coalesced_batches":1,"coalesced_paths":2,"refreshed_batches":1,"refreshed_paths_covered":3,"skipped_batches":1,"unresolved_paths":0,"capped_batches":1,"aborted_due_to_cap":true,"failed_batches":0}},{"server":"emby","requested_targets":4,"refresh":{"requested_paths":1,"unique_paths":1,"planned_batches":1,"coalesced_batches":0,"coalesced_paths":0,"refreshed_batches":1,"refreshed_paths_covered":1,"skipped_batches":0,"unresolved_paths":0,"capped_batches":0,"aborted_due_to_cap":false,"failed_batches":0}}]"#.to_string(),
            ),
            dead_link_sweep_ms: 80,
            cache_hit_ratio: Some(0.85),
            candidate_slots: 1024,
            scored_candidates: 24,
            exact_id_hits: 2,
            auto_acquire_requests: 6,
            auto_acquire_missing_requests: 4,
            auto_acquire_cutoff_requests: 2,
            auto_acquire_dry_run_hits: 4,
            auto_acquire_submitted: 0,
            auto_acquire_no_result: 1,
            auto_acquire_blocked: 1,
            auto_acquire_failed: 0,
            auto_acquire_completed_linked: 0,
            auto_acquire_completed_unlinked: 0,
        })
        .await
        .unwrap();

        db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
            request_key: "anime-queued-1".to_string(),
            label: "Queued Anime".to_string(),
            query: "Queued Anime S01E01".to_string(),
            query_hints: vec![],
            imdb_id: Some("tt1234567".to_string()),
            categories: vec![5070],
            arr: "sonarr".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_kind: AcquisitionRelinkKind::MediaId,
            relink_value: "tvdb-1".to_string(),
        }])
        .await
        .unwrap();

        TestWebContext {
            _dir: dir,
            state: WebState::new(cfg, db),
        }
    }

    async fn create_test_plex_duplicate_db(path: &Path) {
        let options = SqliteConnectOptions::from_str(path.to_str().unwrap())
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        pool.execute(
            "CREATE TABLE section_locations (
                id INTEGER PRIMARY KEY,
                library_section_id INTEGER,
                root_path TEXT,
                available BOOLEAN,
                scanned_at INTEGER,
                created_at INTEGER,
                updated_at INTEGER
            );",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE metadata_items (
                id INTEGER PRIMARY KEY,
                library_section_id INTEGER,
                metadata_type INTEGER,
                title TEXT,
                original_title TEXT,
                year INTEGER,
                guid TEXT,
                deleted_at INTEGER
            );",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE media_items (
                id INTEGER PRIMARY KEY,
                library_section_id INTEGER,
                section_location_id INTEGER,
                metadata_item_id INTEGER,
                deleted_at INTEGER
            );",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE media_parts (
                id INTEGER PRIMARY KEY,
                media_item_id INTEGER,
                file TEXT,
                deleted_at INTEGER
            );",
        )
        .await
        .unwrap();
    }

    async fn render_body(response: impl IntoResponse) -> String {
        let response = response.into_response();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn dashboard_renders_latest_run_and_queue_summary() {
        let ctx = test_context().await;
        let body = render_body(get_dashboard(State(ctx.state.clone())).await).await;

        assert!(body.contains("Current baseline"));
        assert!(body.contains("Anime"));
        assert!(body.contains("Auto-Acquire Queue"));
        assert!(body.contains("Queue 1"));
        assert!(body.contains("Cache Hit"));
        assert!(body.contains("Media refresh protections activated"));
        assert!(body.contains("Top Why-Not Signals"));
        assert!(body.contains("Already correct 6200"));
        assert!(body.contains("Source missing before link 3044"));
        assert!(body.contains("Plex guard abort"));
        assert!(body.contains("Emby 1/1"));
    }

    #[tokio::test]
    async fn dashboard_renders_deferred_refresh_backlog() {
        let ctx = test_context().await;
        std::fs::write(
            ctx.state
                .config
                .backup
                .path
                .join(".media-server-refresh.queue.json"),
            r#"{
              "servers": [
                { "server": "plex", "paths": ["/library/anime", "/library/anime-2"] },
                { "server": "jellyfin", "paths": ["/library/anime"] }
              ]
            }"#,
        )
        .unwrap();

        let body = render_body(get_dashboard(State(ctx.state.clone())).await).await;
        assert!(body.contains("Deferred refresh 3"));
        assert!(body.contains("Media Refresh Backlog"));
        assert!(body.contains("Plex"));
        assert!(body.contains("Jellyfin"));
    }

    #[tokio::test]
    async fn scan_page_renders_phase_telemetry_and_acquire_summary() {
        let ctx = test_context().await;
        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Start Scan"));
        assert!(body.contains("Search Missing"));
        assert!(!body.contains("name=\"dry_run\" value=\"true\" checked"));
        assert!(body.contains("Candidate Slots"));
        assert!(body.contains("1024"));
        assert!(body.contains("4/6"));
        assert!(body.contains("Media refresh protections activated"));
        assert!(body.contains("Plex guard abort"));
        assert!(body.contains("Emby 1/1"));
    }

    #[tokio::test]
    async fn scan_page_renders_active_background_scan_banner() {
        let ctx = test_context().await;
        ctx.state
            .set_active_scan_for_test(Some(ActiveScanJob {
                started_at: "2026-03-29 23:59:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                dry_run: true,
                search_missing: true,
            }))
            .await;

        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Background scan running"));
        assert!(body.contains("2026-03-29 23:59:00 UTC"));
        assert!(body.contains("Anime"));
        assert!(body.contains("Search missing enabled"));
    }

    #[tokio::test]
    async fn scan_page_renders_last_failed_background_scan_outcome() {
        let ctx = test_context().await;
        ctx.state
            .set_last_scan_outcome_for_test(Some(LastScanOutcome {
                finished_at: "2099-03-29 23:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                dry_run: false,
                search_missing: true,
                success: false,
                message: "RD cache sync failed".to_string(),
            }))
            .await;

        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Background scan failed"));
        assert!(body.contains("RD cache sync failed"));
        assert!(body.contains("2099-03-29 23:58:00 UTC"));
    }

    #[tokio::test]
    async fn scan_page_hides_stale_failed_background_outcome_when_newer_run_exists() {
        let ctx = test_context().await;
        ctx.state
            .set_last_scan_outcome_for_test(Some(LastScanOutcome {
                finished_at: "2026-03-29 09:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                dry_run: false,
                search_missing: true,
                success: false,
                message: "stale failure".to_string(),
            }))
            .await;

        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(!body.contains("Background scan failed"));
        assert!(!body.contains("stale failure"));
    }

    #[tokio::test]
    async fn cleanup_page_renders_active_background_audit_banner() {
        let ctx = test_context().await;
        ctx.state
            .set_active_cleanup_audit_for_test(Some(ActiveCleanupAuditJob {
                started_at: "2026-03-29 23:59:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                libraries_label: "Anime".to_string(),
            }))
            .await;

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(body.contains("Background cleanup audit running"));
        assert!(body.contains("2026-03-29 23:59:00 UTC"));
        assert!(body.contains("Anime across Anime"));
    }

    #[tokio::test]
    async fn cleanup_page_renders_last_failed_background_audit_outcome() {
        let ctx = test_context().await;
        ctx.state
            .set_last_cleanup_audit_outcome_for_test(Some(LastCleanupAuditOutcome {
                finished_at: "2026-03-29 23:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                libraries_label: "Anime".to_string(),
                success: false,
                message: "source root unhealthy".to_string(),
                report_path: None,
            }))
            .await;

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(body.contains("Background cleanup audit failed"));
        assert!(body.contains("source root unhealthy"));
        assert!(body.contains("2026-03-29 23:58:00 UTC"));
    }

    #[tokio::test]
    async fn cleanup_page_hides_stale_failed_background_audit_outcome_when_newer_report_exists() {
        let ctx = test_context().await;
        let report_path = ctx
            .state
            .config
            .backup
            .path
            .join("cleanup-audit-anime-20260329.json");
        let report = CleanupReport {
            version: 1,
            created_at: chrono::Utc::now(),
            scope: CleanupScope::Anime,
            findings: vec![],
            summary: CleanupSummary {
                total_findings: 1,
                critical: 0,
                high: 1,
                warning: 0,
            },
            applied_at: None,
        };
        std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        ctx.state
            .set_last_cleanup_audit_outcome_for_test(Some(LastCleanupAuditOutcome {
                finished_at: "2026-03-29 09:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                libraries_label: "Anime".to_string(),
                success: false,
                message: "stale cleanup failure".to_string(),
                report_path: None,
            }))
            .await;

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(!body.contains("Background cleanup audit failed"));
        assert!(!body.contains("stale cleanup failure"));
        assert!(body.contains("Last Report"));
    }

    #[tokio::test]
    async fn anime_remediation_page_renders_ranked_groups() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = test_config(&root);
        let anime_root = cfg.libraries[0].path.clone();
        let tagged_root = anime_root.join("Show A (2024) {tvdb-1}");
        let legacy_root = anime_root.join("Show A");

        std::fs::create_dir_all(tagged_root.join("Season 01")).unwrap();
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        let tracked_source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
        let tracked_target = tagged_root.join("Season 01/Show A - S01E01.mkv");
        std::fs::write(&tracked_source, b"video").unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: tracked_source,
            target_path: tracked_target,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_target = legacy_root.join("Season 01/Show A - S01E01.mkv");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/tmp/source-a.mkv", &legacy_target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file("C:\\source-a.mkv", &legacy_target).unwrap();

        let plex_db_path = root.join("plex.db");
        create_test_plex_duplicate_db(&plex_db_path).await;
        let options = SqliteConnectOptions::from_str(plex_db_path.to_str().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?)",
        )
        .bind(anime_root.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at)
             VALUES (1, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://anidb-100', NULL),
                    (2, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://tvdb-1', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = WebState::new(cfg, db);
        let body = render_body(
            get_cleanup_anime_remediation(
                State(state),
                Query(AnimeRemediationQuery {
                    full: true,
                    plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                    state: None,
                    reason: None,
                    title: None,
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains("Legacy Anime Cleanup"));
        assert!(body.contains("Show A"));
        assert!(body.contains("Show Full Backlog") || body.contains("Show Sample"));
        assert!(
            body.contains("com.plexapp.agents.hama://anidb-100") || body.contains("hama-anidb")
        );
        assert!(body.contains("/tmp") || body.contains("/Show A (2024) {tvdb-1}"));
    }

    #[tokio::test]
    async fn anime_remediation_preview_page_renders_saved_plan_and_apply_gate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = test_config(&root);
        let anime_root = cfg.libraries[0].path.clone();
        let tagged_root = anime_root.join("Show A (2024) {tvdb-1}");
        let legacy_root = anime_root.join("Show A");

        std::fs::create_dir_all(tagged_root.join("Season 01")).unwrap();
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        let tracked_source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
        let tracked_target = tagged_root.join("Season 01/Show A - S01E01.mkv");
        std::fs::write(&tracked_source, b"video").unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: tracked_source.clone(),
            target_path: tracked_target,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_target = legacy_root.join("Season 01/Show A - S01E01.mkv");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&tracked_source, &legacy_target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&tracked_source, &legacy_target).unwrap();

        let plex_db_path = root.join("plex.db");
        create_test_plex_duplicate_db(&plex_db_path).await;
        let options = SqliteConnectOptions::from_str(plex_db_path.to_str().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?)",
        )
        .bind(anime_root.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at)
             VALUES (1, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://anidb-100', NULL),
                    (2, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://tvdb-1', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = WebState::new(cfg, db);
        let csrf_token = state.browser_session_token().to_string();
        let body = render_body(
            post_cleanup_anime_remediation_preview(
                State(state),
                Form(AnimeRemediationPreviewForm {
                    plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                    title: None,
                    library: Some("Anime".to_string()),
                    csrf_token,
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains("Apply this saved plan"));
        assert!(body.contains("already bound to this saved plan"));
        assert!(body.contains("Apply Legacy Cleanup"));
        assert!(body.contains("Report file:"));
        assert!(body.contains("name=\"token\""));
        assert!(!body.contains("Confirmation token"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn anime_remediation_apply_page_renders_quarantine_result() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let root = dir.path().to_path_buf();
        let mut cfg = test_config(&root);
        cfg.backup.path = std::path::PathBuf::from(root.file_name().unwrap()).join("backups");
        cfg.cleanup.prune.quarantine_path = root.join("quarantine");
        let anime_root = cfg.libraries[0].path.clone();
        let tagged_root = anime_root.join("Show A (2024) {tvdb-1}");
        let legacy_root = anime_root.join("Show A");

        std::fs::create_dir_all(tagged_root.join("Season 01")).unwrap();
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        let tracked_source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
        let tracked_target = tagged_root.join("Season 01/Show A - S01E01.mkv");
        std::fs::write(&tracked_source, b"video").unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: tracked_source.clone(),
            target_path: tracked_target,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_target = legacy_root.join("Season 01/Show A - S01E01.mkv");
        std::os::unix::fs::symlink(&tracked_source, &legacy_target).unwrap();

        let plex_db_path = root.join("plex.db");
        create_test_plex_duplicate_db(&plex_db_path).await;
        let options = SqliteConnectOptions::from_str(plex_db_path.to_str().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?)",
        )
        .bind(anime_root.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at)
             VALUES (1, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://anidb-100', NULL),
                    (2, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://tvdb-1', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        cfg.libraries[0].path = anime_root;
        let state = WebState::new(cfg, db);
        let (plan, report_path) = preview_anime_remediation_plan(
            &state.config,
            &state.database,
            Some("Anime"),
            &plex_db_path,
            None,
            None,
        )
        .await
        .unwrap();

        let body = render_body(
            post_cleanup_anime_remediation_apply(
                State(state.clone()),
                Form(AnimeRemediationApplyForm {
                    report: report_path.to_string_lossy().to_string(),
                    token: plan.confirmation_token,
                    max_delete: None,
                    library: Some("Anime".to_string()),
                    csrf_token: state.browser_session_token().to_string(),
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains("Anime remediation applied"));
        assert!(body.contains("Safety snapshot"));
        assert!(body.contains("Quarantined"));
        assert!(!legacy_target.exists());
    }

    #[tokio::test]
    async fn scan_run_detail_renders_specific_run() {
        let ctx = test_context().await;
        let run = ctx
            .state
            .database
            .get_scan_history(1)
            .await
            .unwrap()
            .remove(0);
        let body =
            render_body(get_scan_run_detail(State(ctx.state.clone()), Path(run.id)).await).await;

        assert!(body.contains("Scan Run Detail"));
        assert!(body.contains("Outcome summary"));
        assert!(body.contains("#1") || body.contains(&format!("#{}", run.id)));
        assert!(body.contains("Recent concrete skip events"));
        assert!(body.contains("1024"));
    }

    #[tokio::test]
    async fn scan_history_applies_mode_and_missing_filters() {
        let ctx = test_context().await;
        ctx.state
            .database
            .record_scan_run(&ScanRunRecord {
                dry_run: false,
                library_filter: Some("Movies".to_string()),
                search_missing: false,
                library_items_found: 2,
                source_items_found: 8,
                matches_found: 4,
                links_created: 1,
                links_updated: 0,
                skip_reason_json: None,
                ..Default::default()
            })
            .await
            .unwrap();

        let runs = ctx.state.database.get_scan_history(5).await.unwrap();
        let movie_run_id = runs
            .iter()
            .find(|run| run.library_filter.as_deref() == Some("Movies"))
            .map(|run| run.id)
            .unwrap();
        let anime_run_id = runs
            .iter()
            .find(|run| run.library_filter.as_deref() == Some("Anime"))
            .map(|run| run.id)
            .unwrap();

        let body = render_body(
            get_scan_history(
                State(ctx.state.clone()),
                Query(ScanHistoryQuery {
                    mode: Some("live".to_string()),
                    search_missing: Some("exclude".to_string()),
                    limit: Some(25),
                    ..ScanHistoryQuery::default()
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains(&format!("/scan/history/{}", movie_run_id)));
        assert!(!body.contains(&format!("/scan/history/{}", anime_run_id)));
    }

    #[tokio::test]
    async fn scan_history_renders_refresh_backend_badges() {
        let ctx = test_context().await;
        let body = render_body(
            get_scan_history(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Plex guard abort"));
        assert!(body.contains("Emby 1/1"));
    }

    #[tokio::test]
    async fn status_page_renders_queue_pressure_and_recent_links() {
        let ctx = test_context().await;
        let body = render_body(get_status(State(ctx.state.clone())).await).await;

        assert!(body.contains("Queue pressure"));
        assert!(body.contains("Service connectivity"));
        assert!(body.contains("Deferred media refresh"));
        assert!(body.contains("Recent Links"));
        assert!(body.contains("tvdb-1"));
        assert!(body.contains("Queued"));
    }

    #[tokio::test]
    async fn doctor_page_uses_full_doctor_checks() {
        let ctx = test_context().await;
        let body = render_body(get_doctor(State(ctx.state)).await).await;

        assert!(body.contains("db_schema_version"));
        assert!(body.contains("config_validation"));
        assert!(body.contains("cleanup.prune.enforce_policy"));
    }

    #[tokio::test]
    async fn doctor_page_does_not_create_missing_backup_dir_in_read_only_mode() {
        let ctx = test_context().await;
        let backup_dir = ctx.state.config.backup.path.clone();
        std::fs::remove_dir(&backup_dir).unwrap();
        assert!(!backup_dir.exists());

        let body = render_body(get_doctor(State(ctx.state)).await).await;

        assert!(body.contains("backup_dir"));
        assert!(body.contains("write probe skipped in read-only mode"));
        assert!(!backup_dir.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn doctor_page_flags_existing_non_writable_backup_dir() {
        use std::os::unix::fs::PermissionsExt;

        let ctx = test_context().await;
        let backup_dir = ctx.state.config.backup.path.clone();
        std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let body = render_body(get_doctor(State(ctx.state)).await).await;

        assert!(body.contains("backup_dir"));
        assert!(body.contains("denies write or traverse"));
        assert!(body.contains("mode=555"));
    }

    #[tokio::test]
    async fn discover_page_shell_renders_async_loader() {
        let ctx = test_context().await;
        let response = get_discover(State(ctx.state), Query(DiscoverQuery::default()))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("Loading discover preview"));
        assert!(body.contains("hx-get=\"/discover/content\""));
        assert!(body.contains("Web discover is intentionally read-only"));
        assert!(body.contains("Refresh Discover"));
    }

    #[tokio::test]
    async fn discover_content_renders_cached_gap_items() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();
        std::fs::create_dir_all(dir.path().join("anime").join("Missing Show {tvdb-1}")).unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("rd")
                .join("Missing.Show.S01E01.1080p.WEB-DL"),
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("rd")
                .join("Missing.Show.S01E01.1080p.WEB-DL")
                .join("Missing.Show.S01E01.1080p.WEB-DL.mkv"),
            b"video",
        )
        .unwrap();

        db.upsert_rd_torrent(
            "rd-1",
            "hash-1",
            "Missing.Show.S01E01.1080p.WEB-DL.mkv",
            "downloaded",
            r#"{"files":[{"selected":1,"bytes":1073741824,"path":"Missing.Show.S01E01.1080p.WEB-DL.mkv"}]}"#,
        )
        .await
        .unwrap();

        let state = WebState::new(cfg, db);
        let response = get_discover_content(State(state), Query(DiscoverQuery::default()))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("Folder Plans"));
        assert!(body.contains("Placement Report"));
        assert!(body.contains("Missing Show"));
        assert!(body.contains("create"));
        assert!(body.contains("Season 01"));
        assert!(body.contains("Real-Debrid API key not configured"));
        assert!(body.contains("live refresh is unavailable"));
        assert!(!body.contains("name=\"torrent_id\""));
    }

    #[tokio::test]
    async fn discover_content_rejects_invalid_library_filter() {
        let ctx = test_context().await;
        let response = get_discover_content(
            State(ctx.state),
            Query(DiscoverQuery {
                library: Some("Nope".to_string()),
                refresh_cache: false,
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("Invalid library filter"));
        assert!(body.contains("Unknown library filter"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_repair_starts_background_repair_flow() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();

        let library_root = cfg.libraries[0].path.clone();
        let source_root = cfg.sources[0].path.clone();
        let target_path = library_root.join("Show/Season 01/Show - S01E01.mkv");
        let missing_source = source_root.join("missing/Show.S01E01.mkv");
        let replacement = source_root.join("Show.S01E01.mkv");

        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(missing_source.parent().unwrap()).unwrap();
        std::fs::write(&replacement, b"video").unwrap();
        std::os::unix::fs::symlink(&missing_source, &target_path).unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: missing_source.clone(),
            target_path: target_path.clone(),
            media_id: "tvdb-99".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Dead,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let state = WebState::new(cfg, db);
        let response = post_repair(
            State(state.clone()),
            Form(BrowserMutationForm {
                csrf_token: state.browser_session_token().to_string(),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("Repair started in background"));

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if state.last_repair_outcome().await.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background repair should finish");

        let outcome = state
            .last_repair_outcome()
            .await
            .expect("expected repair outcome");
        assert!(outcome.success);
        assert_eq!(outcome.repaired, 1);
        assert_eq!(outcome.failed, 0);

        let repaired = state.database.get_active_links().await.unwrap();
        let repaired = repaired
            .into_iter()
            .find(|link| link.target_path == target_path)
            .expect("expected repaired active link");
        assert_eq!(repaired.source_path, replacement);
    }

    #[tokio::test]
    async fn cleanup_page_renders_latest_report_summary() {
        let ctx = test_context().await;
        let report_path = ctx
            .state
            .config
            .backup
            .path
            .join("cleanup-audit-anime-20260321.json");
        let report = CleanupReport {
            version: 1,
            created_at: chrono::Utc::now(),
            scope: CleanupScope::Anime,
            findings: vec![],
            summary: CleanupSummary {
                total_findings: 12,
                critical: 3,
                high: 5,
                warning: 4,
            },
            applied_at: None,
        };
        std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(body.contains("Last Report"));
        assert!(body.contains("12"));
        assert!(body.contains("Open Prune Preview"));
        assert!(!body.contains("Apply Cleanup"));
        assert!(body.contains("Apply from the preview page"));
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_dedupes_legacy_and_multi_select_fields() {
        let form = CleanupAuditForm {
            library: Some("Anime".to_string()),
            libraries: vec!["Anime".to_string(), "Anime 2".to_string()],
            csrf_token: String::new(),
        };

        assert_eq!(
            form.selected_libraries(),
            vec!["Anime".to_string(), "Anime 2".to_string()]
        );
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_uses_single_when_multi_empty() {
        let form = CleanupAuditForm {
            library: Some("Anime".to_string()),
            libraries: vec![],
            csrf_token: String::new(),
        };

        assert_eq!(form.selected_libraries(), vec!["Anime".to_string()]);
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_ignores_empty_library() {
        let form = CleanupAuditForm {
            library: Some("".to_string()),
            libraries: vec!["Anime".to_string()],
            csrf_token: String::new(),
        };

        assert_eq!(form.selected_libraries(), vec!["Anime".to_string()]);
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_whitespace_trimmed() {
        // Single is appended after multi-select, whitespace is trimmed throughout
        let form = CleanupAuditForm {
            library: Some("  Anime  ".to_string()),
            libraries: vec!["  Anime 2  ".to_string()],
            csrf_token: String::new(),
        };

        let result = form.selected_libraries();
        assert!(result.contains(&"Anime".to_string()));
        assert!(result.contains(&"Anime 2".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn resolve_backup_restore_path_rejects_absolute_input() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let manager = BackupManager::new(&crate::config::BackupConfig {
            enabled: true,
            path: backup_dir,
            interval_hours: 24,
            max_backups: 5,
            max_safety_backups: 0,
        });
        let err = manager
            .resolve_restore_path(StdPath::new("/tmp/evil.json"))
            .unwrap_err();
        assert!(err.to_string().contains("configured backup directory"));
    }

    #[test]
    fn resolve_backup_restore_path_rejects_parent_escape() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let manager = BackupManager::new(&crate::config::BackupConfig {
            enabled: true,
            path: backup_dir,
            interval_hours: 24,
            max_backups: 5,
            max_safety_backups: 0,
        });
        let err = manager
            .resolve_restore_path(StdPath::new("../outside.json"))
            .unwrap_err();
        assert!(err.to_string().contains("escapes"));
    }

    #[test]
    fn resolve_backup_restore_path_accepts_plain_filename() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let backup_file = backup_dir.join("backup-20260329.json");
        std::fs::write(&backup_file, "{}").unwrap();

        let manager = BackupManager::new(&crate::config::BackupConfig {
            enabled: true,
            path: backup_dir.clone(),
            interval_hours: 24,
            max_backups: 5,
            max_safety_backups: 0,
        });
        let path = manager
            .resolve_restore_path(StdPath::new("backup-20260329.json"))
            .unwrap();
        assert_eq!(path, backup_file.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_backup_restore_path_rejects_symlink_escape_inside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        let outside_dir = dir.path().join("outside");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("backup.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside_dir, backup_dir.join("linked")).unwrap();

        let manager = BackupManager::new(&crate::config::BackupConfig {
            enabled: true,
            path: backup_dir.clone(),
            interval_hours: 24,
            max_backups: 5,
            max_safety_backups: 0,
        });
        let err = manager
            .resolve_restore_path(StdPath::new("linked/backup.json"))
            .unwrap_err();
        assert!(err.to_string().contains("escapes"));
    }

    #[tokio::test]
    async fn post_backup_restore_rejects_unhealthy_runtime_roots() {
        let ctx = test_context().await;
        let csrf_token = ctx.state.browser_session_token().to_string();
        let backup_file = "backup-20260330.json";
        let backup_path = ctx.state.config.backup.path.join(backup_file);
        std::fs::write(&backup_path, "{}").unwrap();
        std::fs::remove_dir_all(&ctx.state.config.sources[0].path).unwrap();

        let response = post_backup_restore(
            State(ctx.state),
            Form(BackupRestoreForm {
                backup_file: backup_file.to_string(),
                csrf_token,
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("Restore failed: Refusing backup restore"));
    }

    #[test]
    fn resolve_cleanup_report_path_rejects_absolute_outside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        let outside = dir.path().join("outside.json");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(&outside, "{}").unwrap();

        let err = resolve_cleanup_report_path(&backup_dir, outside.to_string_lossy().as_ref())
            .unwrap_err();
        assert!(err.to_string().contains("configured backup directory"));
    }

    #[test]
    fn resolve_cleanup_report_path_accepts_plain_filename() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let report = backup_dir.join("cleanup-audit-anime.json");
        std::fs::write(&report, "{}").unwrap();

        let path = resolve_cleanup_report_path(&backup_dir, "cleanup-audit-anime.json").unwrap();
        assert_eq!(path, report.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cleanup_report_path_rejects_symlink_escape_inside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        let outside_dir = dir.path().join("outside");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("report.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside_dir, backup_dir.join("linked")).unwrap();

        let err = resolve_cleanup_report_path(&backup_dir, "linked/report.json").unwrap_err();
        assert!(err
            .to_string()
            .contains("Cleanup report must be inside the configured backup directory"));
    }
