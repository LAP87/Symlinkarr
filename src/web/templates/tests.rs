use super::*;

use crate::cleanup_audit::{
    AlternateMatchContext, CleanupFinding, CleanupOwnership, FindingReason, FindingSeverity,
    ParsedContext, PruneReasonCount,
};
use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig, MediaBrowserConfig,
    PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
    SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
};
use crate::models::{LinkStatus, MediaType};

fn sample_skip_reasons() -> Vec<SkipReasonView> {
    vec![
        SkipReasonView::from_reason("already_correct".to_string(), 6200),
        SkipReasonView::from_reason("source_missing_before_link".to_string(), 3044),
        SkipReasonView::from_reason("ambiguous_match".to_string(), 70),
        SkipReasonView::from_reason("auto_acquire_no_result_prowlarr_empty".to_string(), 6),
    ]
}

fn sample_scan_run_view() -> ScanRunView {
    let skip_reasons = sample_skip_reasons();
    let skip_reason_highlights = skip_reasons.iter().take(3).cloned().collect::<Vec<_>>();
    let skip_reason_extra_buckets = skip_reasons
        .len()
        .saturating_sub(skip_reason_highlights.len()) as i64;
    let skip_reason_groups = build_skip_reason_groups(&skip_reasons);
    let skip_reason_total = skip_reasons.iter().map(|reason| reason.count).sum();

    ScanRunView {
        id: 42,
        started_at: "2026-03-21 20:15:00".to_string(),
        scope_label: "Anime".to_string(),
        dry_run: false,
        search_missing: true,
        library_items_found: 3906,
        source_items_found: 101542,
        matches_found: 9924,
        links_created: 446,
        links_updated: 164,
        dead_marked: 15,
        links_removed: 2,
        links_skipped: 9314,
        ambiguous_skipped: 70,
        skip_reasons,
        skip_reason_highlights,
        skip_reason_groups,
        skip_reason_total,
        skip_reason_extra_buckets,
        runtime_checks: "0.2s".to_string(),
        library_scan: "12.4s".to_string(),
        source_inventory: "148.2s".to_string(),
        matching: "86.7s".to_string(),
        title_enrichment: "16.4s".to_string(),
        linking: "20.5s".to_string(),
        plex_refresh: "3.1s".to_string(),
        plex_refresh_requested_paths: 12,
        plex_refresh_unique_paths: 10,
        plex_refresh_planned_batches: 5,
        plex_refresh_coalesced_batches: 2,
        plex_refresh_coalesced_paths: 7,
        plex_refresh_refreshed_batches: 4,
        plex_refresh_refreshed_paths_covered: 12,
        plex_refresh_skipped_batches: 1,
        plex_refresh_unresolved_paths: 0,
        plex_refresh_capped_batches: 1,
        plex_refresh_aborted_due_to_cap: true,
        plex_refresh_failed_batches: 0,
        media_server_refresh: vec![MediaServerRefreshServerView {
            server: "Plex".to_string(),
            requested_targets: 12,
            refreshed_batches: 4,
            planned_batches: 5,
            skipped_batches: 1,
            failed_batches: 0,
            aborted_due_to_cap: true,
            deferred_due_to_lock: false,
        }],
        dead_link_sweep: "0.7s".to_string(),
        total_runtime: "288.2s".to_string(),
        cache_hit_ratio: "94%".to_string(),
        candidate_slots: 77_624_480,
        scored_candidates: 3_171,
        exact_id_hits: 0,
        auto_acquire_requests: 10,
        auto_acquire_missing_requests: 5,
        auto_acquire_cutoff_requests: 5,
        auto_acquire_dry_run_hits: 4,
        auto_acquire_submitted: 8,
        auto_acquire_no_result: 2,
        auto_acquire_blocked: 0,
        auto_acquire_failed: 0,
        auto_acquire_completed_linked: 6,
        auto_acquire_completed_unlinked: 2,
        auto_acquire_successes: 14,
    }
}

fn sample_activity_feed_view() -> DashboardActivityFeedView {
    DashboardActivityFeedView {
        active_items: vec![ActivityFeedItemView {
            kind_label: "Scan".to_string(),
            status_label: "Running".to_string(),
            status_badge_class: "badge-warning",
            scope_label: "Anime".to_string(),
            timestamp_label: "Started".to_string(),
            timestamp: "2026-04-19 21:15:00 UTC".to_string(),
            context: None,
            message: "Background scan is in progress.".to_string(),
            badges: vec![
                ActivityFeedBadgeView {
                    label: "Dry Run".to_string(),
                    badge_class: "badge-info",
                },
                ActivityFeedBadgeView {
                    label: "Search Missing".to_string(),
                    badge_class: "badge-warning",
                },
            ],
            link: Some(ActivityFeedLinkView {
                href: "/scan".to_string(),
                label: "Open Scan".to_string(),
            }),
        }],
        recent_items: vec![ActivityFeedItemView {
            kind_label: "Cleanup Audit".to_string(),
            status_label: "Completed".to_string(),
            status_badge_class: "badge-success",
            scope_label: "Anime".to_string(),
            timestamp_label: "Finished".to_string(),
            timestamp: "2026-04-19 21:18:00 UTC".to_string(),
            context: Some("Libraries: Anime".to_string()),
            message: "Cleanup report saved.".to_string(),
            badges: Vec::new(),
            link: Some(ActivityFeedLinkView {
                href: "/cleanup".to_string(),
                label: "Open Cleanup".to_string(),
            }),
        }],
    }
}

fn sample_needs_attention_view() -> DashboardNeedsAttentionView {
    DashboardNeedsAttentionView {
        items: vec![
            NeedsAttentionItemView {
                severity_label: "Critical".to_string(),
                severity_badge_class: "badge-danger",
                title: "Latest background scan failed".to_string(),
                message: "Anime finished 2026-04-19 21:20:00 UTC and reported: RD cache sync failed"
                    .to_string(),
                next_step: "Open Scan, compare the failure against the latest run detail, and verify provider or path health before retrying another background pass.".to_string(),
                link: Some(ActivityFeedLinkView {
                    href: "/scan".to_string(),
                    label: "Open Scan".to_string(),
                }),
            },
            NeedsAttentionItemView {
                severity_label: "High".to_string(),
                severity_badge_class: "badge-warning",
                title: "Dead links need cleanup or repair".to_string(),
                message: "12 dead link(s) are currently tracked and can surface stale media paths to users."
                    .to_string(),
                next_step: "Review Dead Links, then decide whether the safest next move is repair or cleanup before the next media refresh.".to_string(),
                link: Some(ActivityFeedLinkView {
                    href: "/links/dead".to_string(),
                    label: "Review Dead Links".to_string(),
                }),
            },
        ],
    }
}

fn sample_queue_jobs() -> Vec<QueueJobView> {
    vec![
        QueueJobView {
            label: "Queued Anime".to_string(),
            status_label: "Queued".to_string(),
            status_badge_class: "badge-info",
            arr_label: "Sonarr".to_string(),
            scope_label: "Anime".to_string(),
            query: "Queued Anime S01E01".to_string(),
            attempts: 1,
            detail: None,
            timing_label: "Queued".to_string(),
            timing_value: "Pending".to_string(),
        },
        QueueJobView {
            label: "Blocked Anime".to_string(),
            status_label: "Blocked".to_string(),
            status_badge_class: "badge-warning",
            arr_label: "Sonarr".to_string(),
            scope_label: "Anime".to_string(),
            query: "Blocked Anime S01E02".to_string(),
            attempts: 2,
            detail: Some("provider returned a hard block".to_string()),
            timing_label: "Next retry".to_string(),
            timing_value: "2026-04-22 02:15:00 UTC".to_string(),
        },
    ]
}

fn sample_config() -> Config {
    Config {
        libraries: vec![LibraryConfig {
            name: "Anime".to_string(),
            path: PathBuf::from("/library/anime"),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/source/rd"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig {
            path: PathBuf::from("/backups"),
            ..BackupConfig::default()
        },
        db_path: "/data/symlinkarr.db".to_string(),
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

#[test]
fn dead_links_template_renders_summary_and_actions() {
    let template = DeadLinksTemplate {
        links: vec![
            LinkRecord {
                id: None,
                source_path: PathBuf::from("/mnt/rd/show-a.mkv"),
                target_path: PathBuf::from("/plex/Show A/S01E01.mkv"),
                media_id: "tvdb-1".to_string(),
                media_type: MediaType::Tv,
                status: LinkStatus::Dead,
                created_at: Some("2026-03-21 10:00:00".to_string()),
                updated_at: Some("2026-03-21 11:00:00".to_string()),
            },
            LinkRecord {
                id: None,
                source_path: PathBuf::from("/mnt/rd/movie.mkv"),
                target_path: PathBuf::from("/plex/Movie.mkv"),
                media_id: "tmdb-2".to_string(),
                media_type: MediaType::Movie,
                status: LinkStatus::Dead,
                created_at: Some("2026-03-21 10:00:00".to_string()),
                updated_at: None,
            },
        ],
        active_repair: Some(ActiveRepairView {
            started_at: "2026-03-29 23:59:00 UTC".to_string(),
            scope_label: "All Libraries".to_string(),
        }),
        last_repair_outcome: None,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Triage baseline and recovery order"));
    assert!(html.contains("2 dead"));
    assert!(html.contains("Auto-Repair All"));
    assert!(html.contains("Cleanup"));
    assert!(html.contains("Background repair running"));
    assert!(html.contains("tv / movie"));
    assert!(html.contains("badge badge-info"));
    assert!(html.contains("/wiki/Repair-and-Dead-Links"));
}

#[test]
fn doctor_template_renders_results_without_redundant_metric_summary() {
    let template = DoctorTemplate {
        checks: vec![
            DoctorCheck {
                check: "db_schema".to_string(),
                passed: true,
                message: "database schema is current".to_string(),
            },
            DoctorCheck {
                check: "backup_dir".to_string(),
                passed: false,
                message: "backup directory is not writable".to_string(),
            },
        ],
        all_passed: false,
    };

    let html = template.render().unwrap();
    assert!(html.contains("Inspection checklist"));
    assert!(html.contains("Needs review"));
    assert!(html.contains("backup directory is not writable"));
    assert!(html.contains("Re-run Checks"));
    assert!(html.contains("/wiki/Configuration-and-Doctor"));
    assert!(html.contains("/wiki/Backup-and-Restore"));
    assert!(!html.contains("metric-label\">Checks"));
}

#[test]
fn scan_run_detail_template_renders_full_run_summary() {
    let template = ScanRunDetailTemplate {
        run: sample_scan_run_view(),
        skip_events: vec![SkipEventView {
            event_at: "2026-03-21 21:12:00".to_string(),
            action: "skipped".to_string(),
            reason: "source_missing_before_link".to_string(),
            reason_label: "Source missing before link".to_string(),
            reason_group: "Linking".to_string(),
            target_path: "/library/Show A/Season 01/Show A - S01E01.mkv".to_string(),
            source_path: Some("/rd/Show.A.S01E01.mkv".to_string()),
            media_id: Some("tvdb-1".to_string()),
        }],
    };

    let html = template.render().unwrap();
    assert!(html.contains("Scan Run Detail"));
    assert!(html.contains("Anime"));
    assert!(html.contains("Phase Telemetry"));
    assert!(html.contains("Matcher Signals"));
    assert!(html.contains("Queue and throttle signals"));
    assert!(html.contains("cap 1") || html.contains(">1<"));
    assert!(html.contains("Auto-Acquire"));
    assert!(html.contains("Skip Reasons"));
    assert!(html.contains("Already correct"));
    assert!(html.contains("Source missing before link"));
    assert!(html.contains("Auto-Acquire"));
    assert!(html.contains("No Prowlarr result"));
    assert!(html.contains("Linking"));
    assert!(html.contains("source_missing_before_link"));
    assert!(html.contains(">3044<"));
    assert!(html.contains("Recent concrete skip events"));
    assert!(html.contains("/library/Show A/Season 01/Show A - S01E01.mkv"));
    assert!(html.contains("Back to Scan History"));
    assert!(html.contains("77624480"));
    assert!(html.contains("/wiki/Scan-History-and-Why-Not-Signals"));
}

#[test]
fn scan_run_detail_template_renders_deferred_media_refresh_status() {
    let mut run = sample_scan_run_view();
    run.media_server_refresh[0].deferred_due_to_lock = true;

    let template = ScanRunDetailTemplate {
        run,
        skip_events: Vec::new(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("deferred"));
}

#[test]
fn scan_history_template_renders_humanized_skip_reason_highlights() {
    let template = ScanHistoryTemplate {
        libraries: Vec::new(),
        history: vec![sample_scan_run_view()],
        filters: ScanHistoryFilters::default(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("History filters and row limit"));
    assert!(html.contains("Why Not"));
    assert!(html.contains("Already correct 6200"));
    assert!(html.contains("Source missing before link 3044"));
    assert!(html.contains("+1 more bucket(s)"));
    assert!(html.contains("/wiki/Scan-History-and-Why-Not-Signals"));
}

#[test]
fn scan_template_renders_top_skip_reason_summary() {
    let template = ScanTemplate {
        libraries: Vec::new(),
        active_scan: None,
        last_scan_outcome: None,
        latest_run: Some(sample_scan_run_view()),
        history: vec![sample_scan_run_view()],
        queue: QueueOverview::default(),
        filters: ScanHistoryFilters::default(),
        default_dry_run: false,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Top Skip Reasons"));
    assert!(html.contains("Matcher: Ambiguous match 70"));
    assert!(html.contains("Open the detail view for grouped counts and raw reason codes."));
    assert!(html.contains("/wiki/Scan-History-and-Why-Not-Signals"));
}

#[test]
fn dashboard_activity_feed_template_renders_active_and_recent_items() {
    let template = DashboardActivityFeedTemplate {
        activity_feed: sample_activity_feed_view(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Live Activity"));
    assert!(html.contains("Running now"));
    assert!(html.contains("Latest outcomes"));
    assert!(html.contains("Background scan is in progress."));
    assert!(html.contains("Cleanup report saved."));
    assert!(html.contains("Refresh 5s"));
    assert!(html.contains("hx-get=\"/dashboard/activity-feed\""));
    assert!(html.contains("Open Scan"));
    assert!(html.contains("Open Cleanup"));
}

#[test]
fn dashboard_template_renders_needs_attention_section() {
    let template = DashboardTemplate {
        stats: DashboardStats::default(),
        needs_attention: sample_needs_attention_view(),
        activity_feed: sample_activity_feed_view(),
        recent_queue_jobs: sample_queue_jobs(),
        latest_run: Some(sample_scan_run_view()),
        recent_runs: vec![sample_scan_run_view()],
        queue: QueueOverview::default(),
        deferred_refresh: DeferredRefreshSummaryView::default(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Needs Attention"));
    assert!(html.contains("Latest background scan failed"));
    assert!(html.contains("Dead links need cleanup or repair"));
    assert!(html.contains("Next step:"));
    assert!(html.contains("compare the failure against the latest run detail"));
    assert!(html.contains("Review Dead Links"));
    assert!(html.contains("/wiki/Dashboard-and-Daily-Operations"));
    assert!(html.contains("Recent queue jobs"));
    assert!(html.contains("Queued Anime"));
    assert!(html.contains("Blocked Anime"));
}

#[test]
fn status_template_renders_recent_queue_jobs() {
    let template = StatusTemplate {
        stats: DashboardStats::default(),
        recent_links: Vec::new(),
        tracked_dead_links: Vec::new(),
        recent_queue_jobs: sample_queue_jobs(),
        queue: QueueOverview {
            active_total: 2,
            queued: 1,
            downloading: 0,
            relinking: 0,
            blocked: 1,
            no_result: 0,
            failed: 0,
            completed_unlinked: 0,
        },
        checks: std::collections::BTreeMap::new(),
        deferred_refresh: DeferredRefreshSummaryView::default(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Recent auto-acquire jobs"));
    assert!(html.contains("Queued Anime"));
    assert!(html.contains("Blocked Anime"));
    assert!(html.contains("Needs Relink"));
}

#[test]
fn config_template_renders_topology_and_defaults_disclosures() {
    let template = ConfigTemplate {
        config: sample_config(),
        validation_result: Some(ValidationResult {
            valid: true,
            errors: Vec::new(),
            warnings: vec!["Backup path is on a slow disk".to_string()],
        }),
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Check current configuration"));
    assert!(html.contains("Docs and recommended workflow"));
    assert!(html.contains("Libraries and ingestion roots"));
    assert!(html.contains("Low-level runtime defaults"));
    assert!(html.contains("1 libraries"));
    assert!(html.contains("1 sources"));
    assert!(html.contains("/library/anime"));
    assert!(html.contains("/backups"));
    assert!(html.contains("/wiki/Configuration-and-Doctor"));
    assert!(html.contains("/wiki/Backup-and-Restore"));
}

#[test]
fn discover_template_renders_guide_disclosure() {
    let template = DiscoverTemplate {
        libraries: vec![LibraryConfig {
            name: "Anime".to_string(),
            path: PathBuf::from("/library/anime"),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        }],
        selected_library: String::new(),
        refresh_cache: false,
    };

    let html = template.render().unwrap();
    assert!(html.contains("Discover preview rules"));
    assert!(html.contains("Target library"));
    assert!(html.contains("Refresh RD cache first"));
    assert!(html.contains("/wiki/Discover-and-Queue"));
}

#[test]
fn backup_template_renders_storage_disclosure_and_restore_history() {
    let template = BackupTemplate {
        backups: vec![BackupInfo {
            filename: "symlinkarr-backup-before-cleanup-20260415-220000.json".to_string(),
            label: "before-cleanup".to_string(),
            kind_label: "Symlinkarr Backup".to_string(),
            kind_badge_class: "badge-info",
            created_at: "2026-04-15 22:00:00 UTC".to_string(),
            age_label: "6 days ago".to_string(),
            recorded_links: 420,
            link_delta_label: "+12 vs current".to_string(),
            manifest_size_bytes: 1337,
            database_snapshot_size_bytes: Some(8192),
            config_snapshot_present: true,
            secret_snapshot_count: 2,
        }],
        backup_dir: PathBuf::from("/backups"),
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Create new Symlinkarr backup"));
    assert!(html.contains("Storage path and restore semantics"));
    assert!(html.contains("1 snapshots"));
    assert!(html.contains("Existing backups"));
    assert!(html.contains("Confirm backup restore"));
    assert!(html.contains("symlinkarr-backup-before-cleanup-20260415-220000.json"));
    assert!(html.contains("/wiki/Backup-and-Restore"));
}

#[test]
fn links_template_renders_dead_link_wiki_entrypoint() {
    let template = LinksTemplate {
        links: vec![LinkRecord {
            id: None,
            source_path: PathBuf::from("/mnt/rd/show-a.mkv"),
            target_path: PathBuf::from("/plex/Show A/S01E01.mkv"),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: Some("2026-03-21 10:00:00".to_string()),
            updated_at: Some("2026-03-21 11:00:00".to_string()),
        }],
        filter: "all".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Browse and manage symlinks in the database."));
    assert!(html.contains("/wiki/Repair-and-Dead-Links"));
}

#[test]
fn skip_reason_presenter_humanizes_matcher_and_auto_acquire_codes() {
    let matcher = SkipReasonView::from_reason("matcher_metadata_mismatch".to_string(), 3);
    let auto_acquire =
        SkipReasonView::from_reason("auto_acquire_no_result_prowlarr_empty".to_string(), 4);

    assert_eq!(matcher.group, "Matcher");
    assert_eq!(matcher.label, "Metadata mismatch");
    assert!(matcher.help.contains("metadata"));
    assert_eq!(auto_acquire.group, "Auto-Acquire");
    assert_eq!(auto_acquire.label, "No Prowlarr result");
    assert!(auto_acquire.help.contains("Prowlarr"));
}

#[test]
fn cleanup_result_template_renders_report_summary() {
    let template = CleanupResultTemplate {
        success: true,
        message: "Audit complete".to_string(),
        active_cleanup_audit: None,
        last_cleanup_audit_outcome: None,
        report_path: Some(PathBuf::from("/tmp/cleanup-audit-anime.json")),
        report_summary: Some(CleanupReportSummaryView {
            path: PathBuf::from("/tmp/cleanup-audit-anime.json"),
            created_at: "2026-03-21 21:30:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            total_findings: 18,
            critical: 4,
            high: 9,
            warning: 5,
        }),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Audit Report Generated"));
    assert!(html.contains("2026-03-21 21:30:00 UTC"));
    assert!(html.contains("Anime"));
    assert!(html.contains("18"));
    assert!(html.contains("4 / 9"));
    assert!(html.contains("Best follow-up"));
    assert!(html.contains("Open Prune Preview for this exact report file."));
    assert!(html.contains("/wiki/Cleanup-Audit-and-Prune-Preview"));
}

#[test]
fn cleanup_result_template_renders_background_audit_banner() {
    let template = CleanupResultTemplate {
        success: true,
        message: "Cleanup audit started in background for Anime across Anime.".to_string(),
        active_cleanup_audit: Some(ActiveCleanupAuditView {
            started_at: "2026-03-29 23:59:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            libraries_label: "Anime".to_string(),
        }),
        last_cleanup_audit_outcome: None,
        report_path: None,
        report_summary: None,
    };

    let html = template.render().unwrap();
    assert!(html.contains("Background cleanup audit running"));
    assert!(html.contains("Background Audit Accepted"));
    assert!(html.contains("2026-03-29 23:59:00 UTC"));
    assert!(html.contains("The audit is running in the background."));
}

#[test]
fn cleanup_result_template_renders_last_failed_audit_outcome() {
    let template = CleanupResultTemplate {
        success: false,
        message: "Cleanup audit not started".to_string(),
        active_cleanup_audit: None,
        last_cleanup_audit_outcome: Some(BackgroundCleanupAuditOutcomeView {
            finished_at: "2026-03-29 23:59:59 UTC".to_string(),
            scope_label: "Anime".to_string(),
            libraries_label: "Anime".to_string(),
            success: false,
            message: "source root unhealthy".to_string(),
            report_path: None,
        }),
        report_path: None,
        report_summary: None,
    };

    let html = template.render().unwrap();
    assert!(html.contains("Last background cleanup audit failed"));
    assert!(html.contains("source root unhealthy"));
    assert!(html.contains("Fix the underlying path or runtime issue"));
}

#[test]
fn scan_result_template_renders_guided_follow_up() {
    let template = ScanResultTemplate {
        success: true,
        message: "Background scan started for Anime.".to_string(),
        active_scan: Some(ActiveScanView {
            started_at: "2026-04-22 01:10:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            dry_run: false,
            search_missing: true,
        }),
        last_scan_outcome: None,
        latest_run: Some(sample_scan_run_view()),
        dry_run: false,
    };

    let html = template.render().unwrap();
    assert!(html.contains("What this result actually means"));
    assert!(html.contains("Best follow-up"));
    assert!(html.contains("Scan History"));
    assert!(html.contains("/wiki/Scan-History-and-Why-Not-Signals"));
}

#[test]
fn repair_result_template_renders_recovery_guidance() {
    let template = RepairResultTemplate {
        success: true,
        message: "Repair completed with unresolved rows.".to_string(),
        repaired: 3,
        failed: 2,
        active_repair: None,
        last_repair_outcome: Some(BackgroundRepairOutcomeView {
            finished_at: "2026-04-22 01:15:00 UTC".to_string(),
            success: false,
            message: "Some files were still missing".to_string(),
        }),
    };

    let html = template.render().unwrap();
    assert!(html.contains("How to read this result"));
    assert!(html.contains("Use Cleanup only for rows that really have no safe replacement left."));
    assert!(html.contains("/wiki/Repair-and-Dead-Links"));
}

#[test]
fn backup_result_template_renders_follow_up_guidance() {
    let template = BackupResultTemplate {
        success: true,
        message: "Backup created successfully".to_string(),
        backup_path: Some(PathBuf::from("/backups/symlinkarr-backup-20260422.json")),
        database_snapshot_path: Some(PathBuf::from("/backups/symlinkarr-backup-20260422.sqlite3")),
        config_snapshot_path: Some(PathBuf::from(
            "/backups/symlinkarr-backup-20260422.config.yaml",
        )),
        secret_snapshot_count: 2,
        app_state_restore_summary: Some(crate::backup::BackupAppStateRestoreSummary {
            present: true,
            config_included: true,
            config_restored: true,
            secrets_included: 2,
            secrets_restored: 2,
            secrets_skipped: 0,
        }),
    };

    let html = template.render().unwrap();
    assert!(html.contains("What this artifact gives you"));
    assert!(html.contains("Best follow-up"));
    assert!(html
        .contains("Return to Backup and confirm the artifact now appears in the inventory list."));
    assert!(html.contains("/wiki/Backup-and-Restore"));
}

#[test]
fn prune_preview_template_renders_alternate_match_context() {
    let template = PrunePreviewTemplate {
        findings: vec![PruneFindingView::from_finding(
            CleanupFinding {
                symlink_path: PathBuf::from("/plex/Chuck (2007)/Season 01/Chuck - S01E01.mkv"),
                source_path: PathBuf::from("/rd/Chucky.S01E01.mkv"),
                media_id: "tvdb-1".to_string(),
                severity: FindingSeverity::Critical,
                confidence: 0.98,
                reasons: vec![
                    FindingReason::ParserTitleMismatch,
                    FindingReason::AlternateLibraryMatch,
                ],
                parsed: ParsedContext {
                    library_title: "Chuck (2007)".to_string(),
                    parsed_title: "Chucky".to_string(),
                    year: None,
                    season: Some(1),
                    episode: Some(1),
                },
                alternate_match: Some(AlternateMatchContext {
                    media_id: "tvdb-2".to_string(),
                    title: "Chucky (2021)".to_string(),
                    score: 1.0,
                }),
                legacy_anime_root: None,
                db_tracked: true,
                ownership: CleanupOwnership::Managed,
            },
            PrunePathAction::Delete,
        )],
        total: 1,
        critical: 1,
        high: 0,
        warning: 0,
        actionable_candidates: 1,
        blocked_candidates: 0,
        managed_candidates: 1,
        foreign_candidates: 0,
        reason_counts: vec![PruneReasonCount {
            reason: FindingReason::AlternateLibraryMatch,
            total: 1,
            managed: 1,
            foreign: 0,
        }],
        blocked_reason_summary: vec![],
        legacy_anime_root_groups: vec![],
        report_path: Some(PathBuf::from("/tmp/cleanup-audit-all.json")),
        confirmation_token: Some("abcdef1234567890".to_string()),
        already_applied: false,
        error_message: None,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Row-level reasons and exact paths"));
    assert!(html.contains("Better Match"));
    assert!(html.contains("Chucky (2021)"));
    assert!(html.contains("tvdb-2"));
    assert!(html.contains("score 1.00"));
    assert!(html.contains("/wiki/Cleanup-Audit-and-Prune-Preview"));
}

#[test]
fn prune_preview_template_renders_legacy_anime_root_context() {
    let template = PrunePreviewTemplate {
        findings: vec![PruneFindingView::from_finding(
            CleanupFinding {
                symlink_path: PathBuf::from("/plex/Show/Season 01/Show - S01E01.mkv"),
                source_path: PathBuf::from("/rd/Show.S01E01.mkv"),
                media_id: String::new(),
                severity: FindingSeverity::Warning,
                confidence: 0.55,
                reasons: vec![FindingReason::LegacyAnimeRootDuplicate],
                parsed: ParsedContext {
                    library_title: "Show".to_string(),
                    parsed_title: "Show".to_string(),
                    year: None,
                    season: Some(1),
                    episode: Some(1),
                },
                alternate_match: None,
                legacy_anime_root: Some(crate::cleanup_audit::LegacyAnimeRootDetails {
                    normalized_title: "Show".to_string(),
                    untagged_root: PathBuf::from("/plex/Show"),
                    tagged_roots: vec![PathBuf::from("/plex/Show (2024) {tvdb-123}")],
                }),
                db_tracked: false,
                ownership: CleanupOwnership::Foreign,
            },
            PrunePathAction::Quarantine,
        )],
        total: 1,
        critical: 0,
        high: 0,
        warning: 1,
        actionable_candidates: 1,
        blocked_candidates: 0,
        managed_candidates: 0,
        foreign_candidates: 1,
        reason_counts: vec![PruneReasonCount {
            reason: FindingReason::LegacyAnimeRootDuplicate,
            total: 1,
            managed: 0,
            foreign: 1,
        }],
        blocked_reason_summary: vec![],
        legacy_anime_root_groups: vec![crate::cleanup_audit::LegacyAnimeRootGroupCount {
            normalized_title: "Show".to_string(),
            total: 1,
            tagged_roots: vec![PathBuf::from("/plex/Show (2024) {tvdb-123}")],
        }],
        report_path: Some(PathBuf::from("/tmp/cleanup-audit-anime.json")),
        confirmation_token: Some("abcdef1234567890".to_string()),
        already_applied: false,
        error_message: None,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Legacy anime root groups"));
    assert!(html.contains("/plex/Show (2024) {tvdb-123}"));
    assert!(html.contains("legacy root"));
}

#[test]
fn prune_preview_template_renders_blocked_reason_summary() {
    let template = PrunePreviewTemplate {
        findings: vec![PruneFindingView::from_finding(CleanupFinding {
            symlink_path: PathBuf::from("/plex/Show/Season 01/Show - S01E01.mkv"),
            source_path: PathBuf::from("/rd/Show.S01E01.mkv"),
            media_id: "tvdb-1".to_string(),
            severity: FindingSeverity::Warning,
            confidence: 0.75,
            reasons: vec![FindingReason::DuplicateEpisodeSlot],
            parsed: ParsedContext {
                library_title: "Show".to_string(),
                parsed_title: "Show".to_string(),
                year: None,
                season: Some(1),
                episode: Some(1),
            },
            alternate_match: None,
            legacy_anime_root: None,
            db_tracked: false,
            ownership: CleanupOwnership::Foreign,
        }, PrunePathAction::Blocked(
            crate::cleanup_audit::PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor,
        ))],
        total: 1,
        critical: 0,
        high: 0,
        warning: 1,
        actionable_candidates: 0,
        blocked_candidates: 3,
        managed_candidates: 0,
        foreign_candidates: 0,
        reason_counts: vec![],
        blocked_reason_summary: vec![crate::cleanup_audit::PruneBlockedReasonSummary {
            code: crate::cleanup_audit::PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor,
            label: "duplicate slots without a tracked anchor are blocked".to_string(),
            candidates: 3,
            recommended_action:
                "Keep scanning until one canonical tracked link owns the slot before auto-pruning the duplicates."
                    .to_string(),
        }],
        legacy_anime_root_groups: vec![],
        report_path: Some(PathBuf::from("/tmp/cleanup-audit-all.json")),
        confirmation_token: Some("abcdef1234567890".to_string()),
        already_applied: false,
        error_message: None,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("duplicate slots without a tracked anchor are blocked"));
    assert!(html.contains("Keep scanning until one canonical tracked link owns the slot"));
    assert!(html.contains("Apply blockers and trust gates"));
    assert!(!html.contains("Apply Prune"));
}

#[test]
fn anime_remediation_template_renders_backlog_summary() {
    let template = AnimeRemediationTemplate {
        summary: Some(AnimeRemediationSummaryView {
            generated_at: "2026-03-30T02:00:00Z".to_string(),
            plex_db_path: "/tmp/plex.db".to_string(),
            full: false,
            filesystem_mixed_root_groups: 582,
            plex_duplicate_show_groups: 373,
            plex_hama_anidb_tvdb_groups: 371,
            correlated_hama_split_groups: 106,
            remediation_groups: 106,
            returned_groups: 50,
            visible_groups: 49,
            eligible_groups: 1,
            blocked_groups: 49,
            state_filter: "blocked".to_string(),
            reason_filter: "legacy_roots_still_tracked".to_string(),
            title_filter: "Gundam".to_string(),
            blocked_reason_summary: vec![AnimeRemediationBlockedReasonView {
                code: "legacy_roots_still_tracked".to_string(),
                label: "legacy roots still contain tracked DB links".to_string(),
                groups: 32,
                recommended_action:
                    "Do not auto-remediate yet; first move or prune the DB-tracked legacy links."
                        .to_string(),
            }],
            available_blocked_reasons: vec![AnimeRemediationBlockedReasonView {
                code: "legacy_roots_still_tracked".to_string(),
                label: "legacy roots still contain tracked DB links".to_string(),
                groups: 32,
                recommended_action:
                    "Do not auto-remediate yet; first move or prune the DB-tracked legacy links."
                        .to_string(),
            }],
        }),
        groups: vec![AnimeRemediationGroupView {
            normalized_title: "Mobile Suit Gundam SEED".to_string(),
            recommended_tagged_root: PathBuf::from(
                "/plex/anime/Mobile Suit Gundam SEED (2002) {tvdb-123}",
            ),
            recommended_filesystem_symlinks: 49,
            recommended_db_active_links: 49,
            alternate_tagged_roots: vec![],
            legacy_roots: vec![PathBuf::from("/plex/anime/Mobile Suit Gundam SEED")],
            legacy_symlink_total: 99,
            legacy_db_total: 0,
            plex_total_rows: 2,
            plex_live_rows: 2,
            plex_deleted_rows: 0,
            plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            eligible: false,
            block_reasons: vec!["legacy roots still contain 3 tracked DB links".to_string()],
            recommended_action: Some(
                "Do not auto-remediate yet; first move or prune the DB-tracked legacy links."
                    .to_string(),
            ),
            candidate_symlink_samples: vec![PathBuf::from(
                "/plex/anime/Mobile Suit Gundam SEED/Season 01/Show - S01E01.mkv",
            )],
            broken_symlink_samples: vec![PathBuf::from(
                "/plex/anime/Mobile Suit Gundam SEED/Season 01/Show - S01E02.mkv",
            )],
            legacy_media_file_samples: vec![PathBuf::from(
                "/plex/anime/Mobile Suit Gundam SEED/Season 01/Show - S01E03.mkv",
            )],
        }],
        error_message: None,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Legacy Anime Cleanup"));
    assert!(html.contains("Mobile Suit Gundam SEED"));
    assert!(html.contains("Recommended tagged root"));
    assert!(html.contains("Sample View"));
    assert!(html.contains("hama-anidb"));
    assert!(html.contains("visible blocked"));
    assert!(html.contains("legacy roots still contain tracked DB links"));
    assert!(html.contains("Download Filtered TSV"));
    assert!(html.contains("Apply Filters"));
    assert!(html.contains("Candidate symlinks"));
    assert!(html.contains("Broken legacy symlinks"));
    assert!(html.contains("Real media files blocking automatic cleanup"));
    assert!(html.contains("Most users can ignore this page."));
    assert!(html.contains("/wiki/Anime-Remediation"));
}

#[test]
fn anime_remediation_result_template_renders_review_samples() {
    let template = AnimeRemediationResultTemplate {
        success: true,
        message: "preview built".to_string(),
        preview: Some(AnimeRemediationPreviewResultView {
            report_path: PathBuf::from("/tmp/anime-remediation.json"),
            plex_db_path: "/tmp/plex.db".to_string(),
            title_filter: String::new(),
            total_groups: 1,
            eligible_groups: 0,
            blocked_groups: 1,
            cleanup_candidates: 2,
            confirmation_token: "abc123".to_string(),
            blocked_reason_summary: vec![],
            groups: vec![AnimeRemediationGroupView {
                normalized_title: "Horimiya".to_string(),
                recommended_tagged_root: PathBuf::from(
                    "/plex/anime/Horimiya (2021) {tvdb-123}",
                ),
                recommended_filesystem_symlinks: 12,
                recommended_db_active_links: 12,
                alternate_tagged_roots: vec![],
                legacy_roots: vec![PathBuf::from("/plex/anime/Horimiya")],
                legacy_symlink_total: 3,
                legacy_db_total: 0,
                plex_total_rows: 2,
                plex_live_rows: 2,
                plex_deleted_rows: 0,
                plex_guid_kinds: vec!["hama-tvdb".to_string()],
                eligible: false,
                block_reasons: vec!["legacy roots contain 13 non-symlink media files".into()],
                recommended_action: Some(
                    "Manual migration required; move or relink real media files before remediation."
                        .into(),
                ),
                candidate_symlink_samples: vec![PathBuf::from(
                    "/plex/anime/Horimiya/Season 01/Horimiya - S01E01.mkv",
                )],
                broken_symlink_samples: vec![PathBuf::from(
                    "/plex/anime/Horimiya/Season 01/Horimiya - S01E02.mkv",
                )],
                legacy_media_file_samples: vec![PathBuf::from(
                    "/plex/anime/Horimiya/Season 01/Horimiya - S01E03.mkv",
                )],
            }],
        }),
        apply: None,
        csrf_token: "csrf-test-token".to_string(),
    };

    let html = template.render().unwrap();
    assert!(html.contains("Plan contents"));
    assert!(html.contains("Candidate symlinks"));
    assert!(html.contains("Broken legacy symlinks"));
    assert!(html.contains("Blocking real media files"));
    assert!(html.contains("Horimiya - S01E03.mkv"));
    assert!(html.contains("Apply Legacy Cleanup"));
    assert!(html.contains("name=\"token\""));
    assert!(!html.contains("Confirmation token"));
}
