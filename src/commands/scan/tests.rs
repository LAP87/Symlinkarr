use super::*;

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
fn scan_telemetry_summary_marks_aborted_refreshes() {
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
    let summary = format!(
        "refresh={}/{} skipped={} capped={}{}",
        telemetry.plex_refresh_stats.refreshed_batches,
        telemetry.plex_refresh_stats.planned_batches,
        telemetry.plex_refresh_stats.skipped_batches,
        telemetry.plex_refresh_stats.capped_batches,
        if telemetry.plex_refresh_stats.aborted_due_to_cap {
            " aborted"
        } else {
            ""
        }
    );

    assert!(summary.ends_with("capped=2 aborted"));
}

#[test]
fn scan_telemetry_summary_marks_deferred_refreshes() {
    let telemetry = ScanTelemetry {
        plex_refresh_stats: LibraryRefreshTelemetry {
            planned_batches: 1,
            deferred_due_to_lock: true,
            ..LibraryRefreshTelemetry::default()
        },
        ..ScanTelemetry::default()
    };
    let summary = format!(
        "refresh={}/{} skipped={} capped={}{}{}",
        telemetry.plex_refresh_stats.refreshed_batches,
        telemetry.plex_refresh_stats.planned_batches,
        telemetry.plex_refresh_stats.skipped_batches,
        telemetry.plex_refresh_stats.capped_batches,
        if telemetry.plex_refresh_stats.aborted_due_to_cap {
            " aborted"
        } else {
            ""
        },
        if telemetry.plex_refresh_stats.deferred_due_to_lock {
            " deferred"
        } else {
            ""
        }
    );

    assert!(summary.ends_with("capped=0 deferred"));
}
