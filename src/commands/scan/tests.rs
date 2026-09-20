use super::telemetry::{aggregate_skip_reasons, format_scan_details_line};
use super::*;

use std::collections::BTreeMap;

use crate::config::ContentType;

#[test]
fn missing_auto_acquire_skips_tv_libraries() {
    let tv = LibraryItem {
        id: crate::models::MediaId::Tvdb(81189),
        path: std::path::PathBuf::from("/tmp/Breaking Bad {tvdb-81189}"),
        title: "Breaking Bad".to_string(),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    };
    let movie = LibraryItem {
        id: crate::models::MediaId::Tmdb(603),
        path: std::path::PathBuf::from("/tmp/The Matrix {tmdb-603}"),
        title: "The Matrix 1999".to_string(),
        library_name: "Movies".to_string(),
        media_type: MediaType::Movie,
        content_type: ContentType::Movie,
    };

    assert_eq!(build_missing_search_query(&tv), None);
    assert_eq!(
        build_missing_search_query(&movie),
        Some("The Matrix 1999".to_string())
    );
}

#[test]
fn cache_hit_ratio_is_based_on_item_mix() {
    let telemetry = SourceInventoryTelemetry {
        cached_items: 8,
        filesystem_items: 2,
        ..SourceInventoryTelemetry::default()
    };

    assert_eq!(telemetry.cache_hit_ratio(), Some(0.8));
}

#[test]
fn aggregate_skip_reasons_merges_match_link_dead_and_auto_acquire_counts() {
    let telemetry = ScanTelemetry {
        match_stats: MatchTelemetry {
            skip_reasons: BTreeMap::from([
                ("ambiguous_match".to_string(), 2),
                ("matcher_metadata_mismatch".to_string(), 3),
            ]),
            ..MatchTelemetry::default()
        },
        ..ScanTelemetry::default()
    };
    let link_summary = LinkProcessSummary {
        skip_reasons: BTreeMap::from([("already_correct".to_string(), 5)]),
        ..LinkProcessSummary::default()
    };
    let dead_summary = crate::linker::DeadLinkSummary {
        skip_reasons: BTreeMap::from([("not_symlink".to_string(), 1)]),
        ..crate::linker::DeadLinkSummary::default()
    };
    let auto_acquire_summary = AutoAcquireBatchSummary {
        reason_counts: BTreeMap::from([("auto_acquire_no_result_prowlarr_empty".to_string(), 4)]),
        ..AutoAcquireBatchSummary::default()
    };

    let reasons = aggregate_skip_reasons(
        &telemetry,
        &link_summary,
        &dead_summary,
        &auto_acquire_summary,
    );

    assert_eq!(reasons.get("ambiguous_match"), Some(&2));
    assert_eq!(reasons.get("matcher_metadata_mismatch"), Some(&3));
    assert_eq!(reasons.get("already_correct"), Some(&5));
    assert_eq!(reasons.get("not_symlink"), Some(&1));
    assert_eq!(
        reasons.get("auto_acquire_no_result_prowlarr_empty"),
        Some(&4)
    );
}

#[test]
fn scan_details_line_marks_aborted_refreshes() {
    let telemetry = ScanTelemetry {
        plex_refresh_stats: LibraryRefreshTelemetry {
            planned_batches: 4,
            refreshed_batches: 0,
            skipped_batches: 4,
            capped_batches: 2,
            aborted_due_to_cap: true,
            ..LibraryRefreshTelemetry::default()
        },
        ..ScanTelemetry::default()
    };
    let summary = format_scan_details_line(&telemetry, 0, &LinkProcessSummary::default());

    assert!(summary.contains("refresh=0/4 skipped=4 capped=2 aborted"));
}

#[test]
fn scan_details_line_marks_deferred_refreshes() {
    let telemetry = ScanTelemetry {
        plex_refresh_stats: LibraryRefreshTelemetry {
            planned_batches: 1,
            deferred_due_to_lock: true,
            ..LibraryRefreshTelemetry::default()
        },
        ..ScanTelemetry::default()
    };
    let summary = format_scan_details_line(&telemetry, 0, &LinkProcessSummary::default());

    assert!(summary.contains("refresh=0/1 skipped=0 capped=0 deferred"));
}

#[tokio::test]
async fn test_targeted_scan_restricts_to_folder() {
    let dir = tempfile::tempdir().unwrap();
    let library_dir = dir.path().join("library");
    let source_dir = dir.path().join("source");
    std::fs::create_dir_all(&library_dir).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();

    let show_folder = library_dir.join("Breaking Bad {tvdb-81189}");
    std::fs::create_dir_all(&show_folder).unwrap();

    let target_folder = source_dir.join("Breaking.Bad.S01.720p");
    let other_folder = source_dir.join("Other.Show.S01");
    std::fs::create_dir_all(&target_folder).unwrap();
    std::fs::create_dir_all(&other_folder).unwrap();

    let target_file = target_folder.join("Breaking.Bad.S01E01.mkv");
    let other_file = other_folder.join("Other.Show.S01E01.mkv");
    std::fs::write(&target_file, b"video1").unwrap();
    std::fs::write(&other_file, b"video2").unwrap();

    let db_path = dir.path().join("test.db");
    let cfg = test_config(library_dir, source_dir, db_path.clone());
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let (created, _) = run_scan(
        &cfg,
        &db,
        false,
        false,
        OutputFormat::Json,
        None,
        Some("Breaking.Bad.S01.720p"),
    )
    .await
    .unwrap();

    assert_eq!(created, 1);
    let active = db.get_active_links().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].source_path, target_file);
}

fn test_config(
    library: std::path::PathBuf,
    source: std::path::PathBuf,
    db_path: std::path::PathBuf,
) -> Config {
    Config {
        libraries: vec![crate::config::LibraryConfig {
            name: "TV".to_string(),
            path: library,
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Tv),
            depth: 1,
        }],
        sources: vec![crate::config::SourceConfig {
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
        handoff: crate::config::HandoffConfig::default(),
        loaded_from: None,
        secret_files: Vec::new(),
    }
}
