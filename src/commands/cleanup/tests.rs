use super::*;

use crate::api::test_helpers::spawn_sequence_http_server;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Executor;
use std::str::FromStr;

use crate::commands::report::{AnimeRemediationSample, AnimeRootUsageSample};
use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig, MediaBrowserConfig,
    PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
    SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
};
use crate::db::Database;
use crate::models::{LinkRecord, MediaType};

fn test_config(root: &Path) -> Config {
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

fn tautulli_activity_json(file: &Path) -> String {
    serde_json::json!({
        "response": {
            "result": "success",
            "data": {
                "stream_count": "1",
                "sessions": [{
                    "title": "Episode 1",
                    "grandparent_title": "Show A",
                    "parent_title": "Season 01",
                    "year": "2024",
                    "media_type": "episode",
                    "friendly_name": "QA",
                    "file": file.display().to_string(),
                    "state": "playing",
                    "progress_percent": "42"
                }]
            }
        }
    })
    .to_string()
}

fn sample_group(root: &Path) -> AnimeRemediationSample {
    AnimeRemediationSample {
        normalized_title: "Show A".to_string(),
        recommended_tagged_root: AnimeRootUsageSample {
            path: root.join("anime/Show A (2024) {tvdb-1}"),
            filesystem_symlinks: 2,
            db_active_links: 2,
        },
        alternate_tagged_roots: vec![],
        legacy_roots: vec![AnimeRootUsageSample {
            path: root.join("anime/Show A"),
            filesystem_symlinks: 2,
            db_active_links: 0,
        }],
        plex_total_rows: 2,
        plex_live_rows: 2,
        plex_deleted_rows: 0,
        plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
        plex_guids: vec![],
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

#[test]
fn build_anime_remediation_plan_group_marks_simple_legacy_root_as_eligible() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = test_config(dir.path());

    let legacy_root = cfg.libraries[0].path.join("Show A");
    std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
    let source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
    std::fs::write(&source, b"video").unwrap();
    let legacy_symlink = legacy_root.join("Season 01/Show A - S01E01.mkv");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, &legacy_symlink).unwrap();

    let group = assess_anime_remediation_group(&sample_group(dir.path())).unwrap();
    assert!(group.eligible);
    assert_eq!(group.legacy_symlink_candidates, 1);
    assert!(group.block_reasons.is_empty());
}

#[test]
fn build_anime_remediation_plan_group_blocks_non_symlink_media_files() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = test_config(dir.path());

    let legacy_root = cfg.libraries[0].path.join("Show A");
    std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
    std::fs::write(legacy_root.join("Season 01/Show A - S01E01.mkv"), b"video").unwrap();

    let group = assess_anime_remediation_group(&sample_group(dir.path())).unwrap();
    assert!(!group.eligible);
    assert_eq!(group.legacy_media_files, 1);
    assert!(group
        .block_reasons
        .iter()
        .any(|reason| reason.message.contains("non-symlink media files")));
    assert!(group.block_reasons.iter().any(|reason| matches!(
        reason.code,
        AnimeRemediationBlockCode::LegacyRootsContainRealMedia
    )));
}

#[test]
fn build_anime_remediation_blocked_reason_summary_counts_groups_per_reason() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = test_config(dir.path());

    let legacy_root = cfg.libraries[0].path.join("Show A");
    std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
    std::fs::write(legacy_root.join("Season 01/Show A - S01E01.mkv"), b"video").unwrap();

    let blocked = assess_anime_remediation_group(&sample_group(dir.path())).unwrap();
    let summary = summarize_anime_remediation_blocked_reasons(&[blocked]);

    assert_eq!(summary.len(), 2);
    assert!(matches!(
        summary[0].code,
        AnimeRemediationBlockCode::LegacyRootsContainRealMedia
    ));
    assert_eq!(summary[0].groups, 1);
    assert_eq!(
        summary[0].recommended_action,
        "Manual migration required; move or relink real media files before remediation."
    );
}

#[test]
fn filter_anime_remediation_groups_respects_visibility_reason_and_title() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = test_config(dir.path());
    let eligible_root = cfg.libraries[0].path.join("Show A");
    std::fs::create_dir_all(eligible_root.join("Season 01")).unwrap();
    let source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
    std::fs::write(&source, b"video").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, eligible_root.join("Season 01/Show A - S01E01.mkv"))
        .unwrap();

    let blocked_root = cfg.libraries[0].path.join("Show B");
    std::fs::create_dir_all(blocked_root.join("Season 01")).unwrap();
    std::fs::write(blocked_root.join("Season 01/Show B - S01E01.mkv"), b"video").unwrap();

    let eligible_group = assess_anime_remediation_group(&sample_group(dir.path())).unwrap();
    let blocked_group = assess_anime_remediation_group(&AnimeRemediationSample {
        normalized_title: "Show B".to_string(),
        recommended_tagged_root: AnimeRootUsageSample {
            path: dir.path().join("anime/Show B (2024) {tvdb-2}"),
            filesystem_symlinks: 1,
            db_active_links: 1,
        },
        alternate_tagged_roots: vec![],
        legacy_roots: vec![AnimeRootUsageSample {
            path: blocked_root,
            filesystem_symlinks: 0,
            db_active_links: 0,
        }],
        plex_total_rows: 2,
        plex_live_rows: 2,
        plex_deleted_rows: 0,
        plex_guid_kinds: vec!["hama-tvdb".to_string()],
        plex_guids: vec![],
    })
    .unwrap();

    let filtered = filter_anime_remediation_groups(
        vec![eligible_group, blocked_group],
        &AnimeRemediationGroupFilters {
            visibility: AnimeRemediationVisibilityFilter::Blocked,
            block_code: Some(AnimeRemediationBlockCode::LegacyRootsContainRealMedia),
            title_contains: Some("show b".to_string()),
        },
    );

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].normalized_title, "Show B");
    assert!(!filtered[0].eligible);
}

#[test]
fn render_anime_remediation_groups_tsv_includes_block_codes_and_samples() {
    let group = AnimeRemediationPlanGroup {
        normalized_title: "Show B".to_string(),
        eligible: false,
        block_reasons: vec![make_anime_block_reason(
            AnimeRemediationBlockCode::LegacyRootsContainRealMedia,
            "legacy roots contain 1 non-symlink media files".to_string(),
        )],
        recommended_tagged_root: AnimeRootUsageSample {
            path: PathBuf::from("/plex/anime/Show B (2024) {tvdb-2}"),
            filesystem_symlinks: 1,
            db_active_links: 1,
        },
        alternate_tagged_roots: vec![],
        legacy_roots: vec![AnimeRootUsageSample {
            path: PathBuf::from("/plex/anime/Show B"),
            filesystem_symlinks: 0,
            db_active_links: 0,
        }],
        legacy_symlink_candidates: 0,
        broken_symlink_candidates: 1,
        legacy_media_files: 1,
        candidate_symlink_samples: vec![],
        broken_symlink_samples: vec![PathBuf::from(
            "/plex/anime/Show B/Season 01/Show B - S01E02.mkv",
        )],
        legacy_media_file_samples: vec![PathBuf::from(
            "/plex/anime/Show B/Season 01/Show B - S01E01.mkv",
        )],
        plex_live_rows: 2,
        plex_deleted_rows: 0,
        plex_guid_kinds: vec!["hama-tvdb".to_string()],
        plex_guids: vec!["com.plexapp.agents.hama://tvdb-2".to_string()],
    };

    let tsv = render_anime_remediation_groups_tsv(&[group]);
    assert!(tsv.contains("block_codes"));
    assert!(tsv.contains("legacy_roots_contain_real_media"));
    assert!(tsv.contains("/plex/anime/Show B/Season 01/Show B - S01E01.mkv"));
}

#[tokio::test]
async fn cleanup_remediate_anime_preview_then_apply_quarantines_legacy_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.cleanup.prune.quarantine_path = dir.path().join("quarantine");

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
        status: crate::models::LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let legacy_symlink = legacy_root.join("Season 01/Show A - S01E01.mkv");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&tracked_source, &legacy_symlink).unwrap();

    let plex_db_path = dir.path().join("plex.db");
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

    let report_path = dir.path().join("anime-remediation-plan.json");
    run_cleanup_anime_remediation(
        &cfg,
        &db,
        CleanupAnimeRemediationArgs {
            report: None,
            plex_db: Some(plex_db_path.to_str().unwrap()),
            apply: false,
            title: None,
            out: Some(report_path.to_str().unwrap()),
            confirm_token: None,
            max_delete: None,
            gate_mode: GateMode::Enforce,
            library_filter: Some("Anime"),
            output: OutputFormat::Json,
        },
    )
    .await
    .unwrap();

    let plan: AnimeRemediationPlanReport =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(plan.eligible_groups, 1);
    assert_eq!(plan.cleanup_candidates, 1);
    assert!(plan.blocked_reason_summary.is_empty());

    run_cleanup_anime_remediation(
        &cfg,
        &db,
        CleanupAnimeRemediationArgs {
            report: Some(report_path.to_str().unwrap()),
            plex_db: None,
            apply: true,
            title: None,
            out: None,
            confirm_token: Some(&plan.confirmation_token),
            max_delete: None,
            gate_mode: GateMode::Enforce,
            library_filter: Some("Anime"),
            output: OutputFormat::Json,
        },
    )
    .await
    .unwrap();

    assert!(!legacy_symlink.exists());
    let quarantined = cfg
        .cleanup
        .prune
        .quarantine_path
        .join("anime/Show A/Season 01/Show A - S01E01.mkv");
    assert!(quarantined.is_symlink());

    let backup_entries = std::fs::read_dir(&cfg.backup.path)
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    assert!(backup_entries
        .iter()
        .any(|name| name.starts_with("symlinkarr-restore-point-anime-remediation-")));
}

#[tokio::test]
async fn cleanup_remediate_anime_apply_requires_foreign_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.cleanup.prune.quarantine_foreign = false;

    let db = Database::new(&cfg.db_path).await.unwrap();
    let report_path = dir.path().join("anime-remediation-plan.json");
    std::fs::write(
        &report_path,
        serde_json::to_string(&AnimeRemediationPlanReport {
            version: ANIME_REMEDIATION_REPORT_VERSION,
            created_at: Utc::now(),
            plex_db_path: dir.path().join("plex.db"),
            title_filter: None,
            total_groups: 0,
            eligible_groups: 0,
            blocked_groups: 0,
            cleanup_candidates: 0,
            confirmation_token: "token".to_string(),
            blocked_reason_summary: Vec::new(),
            groups: Vec::new(),
            cleanup_report: cleanup_audit::CleanupReport {
                version: 1,
                created_at: Utc::now(),
                scope: CleanupScope::Anime,
                summary: cleanup_audit::CleanupSummary::default(),
                findings: Vec::new(),
                applied_at: None,
            },
        })
        .unwrap(),
    )
    .unwrap();

    let err = run_cleanup_anime_remediation(
        &cfg,
        &db,
        CleanupAnimeRemediationArgs {
            report: Some(report_path.to_str().unwrap()),
            plex_db: None,
            apply: true,
            title: None,
            out: None,
            confirm_token: Some("token"),
            max_delete: None,
            gate_mode: GateMode::Enforce,
            library_filter: Some("Anime"),
            output: OutputFormat::Json,
        },
    )
    .await
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("cleanup.prune.quarantine_foreign=true"));
}

#[tokio::test]
async fn cleanup_prune_apply_refuses_active_stream_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.backup.enabled = false;
    cfg.cleanup.prune.enforce_policy = false;

    let source_path = cfg.sources[0].path.join("Show.A.S01E01.mkv");
    std::fs::write(&source_path, b"video").unwrap();
    let symlink_path = cfg.libraries[0]
        .path
        .join("Show A (2024) {tvdb-1}/Season 01/Show A - S01E01.mkv");
    std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source_path, &symlink_path).unwrap();

    let Some((tautulli_url, _requests)) =
        spawn_sequence_http_server(&[("HTTP/1.1 200 OK", &tautulli_activity_json(&symlink_path))])
    else {
        panic!("failed to bind tautulli test server");
    };
    cfg.tautulli.url = tautulli_url;
    cfg.tautulli.api_key = "test-key".to_string();

    let db = Database::new(&cfg.db_path).await.unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_path.clone(),
        target_path: symlink_path.clone(),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        status: crate::models::LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let report_path = dir.path().join("cleanup-report.json");
    std::fs::write(
        &report_path,
        serde_json::to_string_pretty(&cleanup_audit::CleanupReport {
            version: 1,
            created_at: Utc::now(),
            scope: CleanupScope::Tv,
            findings: vec![cleanup_audit::CleanupFinding {
                symlink_path: symlink_path.clone(),
                source_path,
                media_id: "tvdb-1".to_string(),
                severity: cleanup_audit::FindingSeverity::Critical,
                confidence: 1.0,
                reasons: vec![cleanup_audit::FindingReason::BrokenSource],
                parsed: cleanup_audit::ParsedContext {
                    library_title: "Show A".to_string(),
                    parsed_title: "Show A".to_string(),
                    year: Some(2024),
                    season: Some(1),
                    episode: Some(1),
                },
                alternate_match: None,
                legacy_anime_root: None,
                db_tracked: true,
                ownership: cleanup_audit::CleanupOwnership::Managed,
            }],
            summary: cleanup_audit::CleanupSummary {
                total_findings: 1,
                critical: 1,
                high: 0,
                warning: 0,
            },
            applied_at: None,
        })
        .unwrap(),
    )
    .unwrap();

    let err = apply_cleanup_prune_with_refresh(
        &cfg,
        &db,
        CleanupPruneApplyArgs {
            libraries: &[&cfg.libraries[0]],
            report_path: &report_path,
            include_legacy_anime_roots: false,
            max_delete: None,
            confirm_token: None,
            emit_text: false,
        },
    )
    .await
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("currently active in Tautulli/Plex"));
    assert!(symlink_path.exists());
}

#[tokio::test]
async fn cleanup_remediate_anime_apply_refuses_active_stream_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.backup.enabled = false;

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
        status: crate::models::LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let legacy_symlink = legacy_root.join("Season 01/Show A - S01E01.mkv");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&tracked_source, &legacy_symlink).unwrap();

    let Some((tautulli_url, _requests)) = spawn_sequence_http_server(&[(
        "HTTP/1.1 200 OK",
        &tautulli_activity_json(&legacy_symlink),
    )]) else {
        panic!("failed to bind tautulli test server");
    };
    cfg.tautulli.url = tautulli_url;
    cfg.tautulli.api_key = "test-key".to_string();

    let plex_db_path = dir.path().join("plex.db");
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

    let report_path = dir.path().join("anime-remediation-plan.json");
    run_cleanup_anime_remediation(
        &cfg,
        &db,
        CleanupAnimeRemediationArgs {
            report: None,
            plex_db: Some(plex_db_path.to_str().unwrap()),
            apply: false,
            title: None,
            out: Some(report_path.to_str().unwrap()),
            confirm_token: None,
            max_delete: None,
            gate_mode: GateMode::Enforce,
            library_filter: Some("Anime"),
            output: OutputFormat::Json,
        },
    )
    .await
    .unwrap();

    let plan: AnimeRemediationPlanReport =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();

    let err = run_cleanup_anime_remediation(
        &cfg,
        &db,
        CleanupAnimeRemediationArgs {
            report: Some(report_path.to_str().unwrap()),
            plex_db: None,
            apply: true,
            title: None,
            out: None,
            confirm_token: Some(&plan.confirmation_token),
            max_delete: None,
            gate_mode: GateMode::Enforce,
            library_filter: Some("Anime"),
            output: OutputFormat::Json,
        },
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("cleanup anime remediation apply"));
    assert!(err
        .to_string()
        .contains("currently active in Tautulli/Plex"));
    assert!(legacy_symlink.exists());
    assert!(!cfg
        .cleanup
        .prune
        .quarantine_path
        .join("anime/Show A/Season 01/Show A - S01E01.mkv")
        .exists());
}

#[test]
fn anime_remediation_plan_report_loads_legacy_string_block_reasons() {
    let report_json = serde_json::json!({
        "version": ANIME_REMEDIATION_REPORT_VERSION,
        "created_at": Utc::now(),
        "plex_db_path": "/tmp/plex.db",
        "title_filter": serde_json::Value::Null,
        "total_groups": 1,
        "eligible_groups": 0,
        "blocked_groups": 1,
        "cleanup_candidates": 0,
        "confirmation_token": "token",
        "groups": [{
            "normalized_title": "show a",
            "eligible": false,
            "block_reasons": [
                "legacy roots still contain 3 tracked DB links",
                "no legacy symlink candidates found under legacy roots"
            ],
            "recommended_tagged_root": {
                "path": "/anime/Show A (2024) {tvdb-1}",
                "filesystem_symlinks": 1,
                "db_active_links": 0
            },
            "alternate_tagged_roots": [],
            "legacy_roots": [{
                "path": "/anime/Show A",
                "filesystem_symlinks": 3,
                "db_active_links": 3
            }],
            "legacy_symlink_candidates": 0,
            "broken_symlink_candidates": 0,
            "legacy_media_files": 0,
            "candidate_symlink_samples": [],
            "plex_live_rows": 2,
            "plex_deleted_rows": 0,
            "plex_guid_kinds": ["anidb", "tvdb"],
            "plex_guids": ["anidb-100", "tvdb-1"]
        }],
        "cleanup_report": {
            "version": 1,
            "created_at": Utc::now(),
            "scope": "anime",
            "summary": {
                "total_findings": 0,
                "critical": 0,
                "high": 0,
                "warning": 0,
                "quarantine_candidates": 0
            },
            "findings": []
        }
    });

    let report: AnimeRemediationPlanReport = serde_json::from_value(report_json).unwrap();

    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].block_reasons.len(), 2);
    assert!(matches!(
        report.groups[0].block_reasons[0].code,
        AnimeRemediationBlockCode::LegacyRootsStillTracked
    ));
    assert_eq!(
        report.groups[0].block_reasons[0].recommended_action,
        "Do not auto-remediate yet; first move or prune the DB-tracked legacy links."
    );
    assert!(matches!(
        report.groups[0].block_reasons[1].code,
        AnimeRemediationBlockCode::NoLegacySymlinkCandidates
    ));
}
