//! Askama templates for the web UI

use askama::Template;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::SystemTime;

#[allow(unused_imports)]
use super::filters;

use crate::cleanup_audit::{CleanupReport, CleanupScope};
use crate::config::Config;
use crate::db::{AcquisitionJobCounts, ScanHistoryRecord};
use crate::models::LinkRecord;

// ─── Dashboard ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct DashboardStats {
    pub active_links: i64,
    pub dead_links: i64,
    pub total_scans: i64,
    pub last_scan: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct QueueOverview {
    pub active_total: i64,
    pub queued: i64,
    pub downloading: i64,
    pub relinking: i64,
    pub blocked: i64,
    pub no_result: i64,
    pub failed: i64,
    pub completed_unlinked: i64,
}

impl From<AcquisitionJobCounts> for QueueOverview {
    fn from(value: AcquisitionJobCounts) -> Self {
        Self {
            active_total: value.active_total(),
            queued: value.queued,
            downloading: value.downloading,
            relinking: value.relinking,
            blocked: value.blocked,
            no_result: value.no_result,
            failed: value.failed,
            completed_unlinked: value.completed_unlinked,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanHistoryFilters {
    pub library: String,
    pub mode: String,
    pub search_missing: String,
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct ScanRunView {
    pub id: i64,
    pub started_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
    pub links_removed: i64,
    pub links_skipped: i64,
    pub ambiguous_skipped: i64,
    pub runtime_checks: String,
    pub library_scan: String,
    pub source_inventory: String,
    pub matching: String,
    pub title_enrichment: String,
    pub linking: String,
    pub plex_refresh: String,
    pub dead_link_sweep: String,
    pub total_runtime: String,
    pub cache_hit_ratio: String,
    pub candidate_slots: i64,
    pub scored_candidates: i64,
    pub exact_id_hits: i64,
    pub auto_acquire_requests: i64,
    pub auto_acquire_missing_requests: i64,
    pub auto_acquire_cutoff_requests: i64,
    pub auto_acquire_dry_run_hits: i64,
    pub auto_acquire_submitted: i64,
    pub auto_acquire_no_result: i64,
    pub auto_acquire_blocked: i64,
    pub auto_acquire_failed: i64,
    pub auto_acquire_completed_linked: i64,
    pub auto_acquire_completed_unlinked: i64,
    pub auto_acquire_successes: i64,
}

impl ScanRunView {
    pub fn from_record(record: ScanHistoryRecord) -> Self {
        let total_runtime_ms = record.runtime_checks_ms
            + record.library_scan_ms
            + record.source_inventory_ms
            + record.matching_ms
            + record.title_enrichment_ms
            + record.linking_ms
            + record.plex_refresh_ms
            + record.dead_link_sweep_ms;
        let auto_acquire_successes = record.auto_acquire_dry_run_hits
            + record.auto_acquire_submitted
            + record.auto_acquire_completed_linked
            + record.auto_acquire_completed_unlinked;

        Self {
            id: record.id,
            started_at: record.started_at,
            scope_label: record
                .library_filter
                .clone()
                .unwrap_or_else(|| "All Libraries".to_string()),
            dry_run: record.dry_run,
            search_missing: record.search_missing,
            library_items_found: record.library_items_found,
            source_items_found: record.source_items_found,
            matches_found: record.matches_found,
            links_created: record.links_created,
            links_updated: record.links_updated,
            dead_marked: record.dead_marked,
            links_removed: record.links_removed,
            links_skipped: record.links_skipped,
            ambiguous_skipped: record.ambiguous_skipped,
            runtime_checks: format_duration_ms(record.runtime_checks_ms),
            library_scan: format_duration_ms(record.library_scan_ms),
            source_inventory: format_duration_ms(record.source_inventory_ms),
            matching: format_duration_ms(record.matching_ms),
            title_enrichment: format_duration_ms(record.title_enrichment_ms),
            linking: format_duration_ms(record.linking_ms),
            plex_refresh: format_duration_ms(record.plex_refresh_ms),
            dead_link_sweep: format_duration_ms(record.dead_link_sweep_ms),
            total_runtime: format_duration_ms(total_runtime_ms),
            cache_hit_ratio: record
                .cache_hit_ratio
                .map(|ratio| format!("{:.0}%", ratio * 100.0))
                .unwrap_or_else(|| "n/a".to_string()),
            candidate_slots: record.candidate_slots,
            scored_candidates: record.scored_candidates,
            exact_id_hits: record.exact_id_hits,
            auto_acquire_requests: record.auto_acquire_requests,
            auto_acquire_missing_requests: record.auto_acquire_missing_requests,
            auto_acquire_cutoff_requests: record.auto_acquire_cutoff_requests,
            auto_acquire_dry_run_hits: record.auto_acquire_dry_run_hits,
            auto_acquire_submitted: record.auto_acquire_submitted,
            auto_acquire_no_result: record.auto_acquire_no_result,
            auto_acquire_blocked: record.auto_acquire_blocked,
            auto_acquire_failed: record.auto_acquire_failed,
            auto_acquire_completed_linked: record.auto_acquire_completed_linked,
            auto_acquire_completed_unlinked: record.auto_acquire_completed_unlinked,
            auto_acquire_successes,
        }
    }
}

fn format_duration_ms(value: i64) -> String {
    format!("{:.1}s", value as f64 / 1000.0)
}

fn format_cleanup_report_timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

fn cleanup_scope_label(scope: CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Anime => "Anime",
        CleanupScope::Tv => "TV",
        CleanupScope::Movie => "Movies",
        CleanupScope::All => "All Libraries",
    }
}

#[derive(Template)]
#[template(path = "web/ui/dashboard.html")]
pub struct DashboardTemplate {
    pub stats: DashboardStats,
    pub latest_run: Option<ScanRunView>,
    pub recent_runs: Vec<ScanRunView>,
    pub queue: QueueOverview,
}

// ─── Status ─────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/status.html")]
pub struct StatusTemplate {
    pub stats: DashboardStats,
    pub recent_links: Vec<LinkRecord>,
    pub queue: QueueOverview,
}

// ─── Health ─────────────────────────────────────────────────────────

pub struct HealthCheck {
    pub service: String,
    pub status: String,
    pub message: String,
}

#[derive(Template)]
#[template(path = "web/ui/health.html")]
pub struct HealthTemplate {
    pub checks: std::collections::HashMap<String, HealthCheck>,
}

// ─── Scan ───────────────────────────────────────────────────────────

use crate::config::LibraryConfig;

#[derive(Template)]
#[template(path = "web/ui/scan.html")]
pub struct ScanTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub latest_run: Option<ScanRunView>,
    pub history: Vec<ScanRunView>,
    pub queue: QueueOverview,
    pub filters: ScanHistoryFilters,
}

#[derive(Template)]
#[template(path = "web/ui/scan_result.html")]
pub struct ScanResultTemplate {
    pub success: bool,
    pub message: String,
    pub latest_run: Option<ScanRunView>,
    pub dry_run: bool,
}

#[derive(Template)]
#[template(path = "web/ui/scan_history.html")]
pub struct ScanHistoryTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub history: Vec<ScanRunView>,
    pub filters: ScanHistoryFilters,
}

#[derive(Template)]
#[template(path = "web/ui/scan_run.html")]
pub struct ScanRunDetailTemplate {
    pub run: ScanRunView,
}

// ─── Cleanup ────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/cleanup.html")]
pub struct CleanupTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub last_report: Option<CleanupReportSummaryView>,
    pub last_report_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CleanupReportSummaryView {
    pub path: PathBuf,
    pub created_at: String,
    pub scope_label: String,
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
}

impl CleanupReportSummaryView {
    pub fn from_report(path: PathBuf, report: CleanupReport) -> Self {
        Self {
            path,
            created_at: format_cleanup_report_timestamp(report.created_at),
            scope_label: cleanup_scope_label(report.scope).to_string(),
            total_findings: report.summary.total_findings,
            critical: report.summary.critical,
            high: report.summary.high,
            warning: report.summary.warning,
        }
    }
}

#[derive(Template)]
#[template(path = "web/ui/cleanup_result.html")]
pub struct CleanupResultTemplate {
    pub success: bool,
    pub message: String,
    pub report_path: Option<PathBuf>,
    pub report_summary: Option<CleanupReportSummaryView>,
}

#[derive(Template)]
#[template(path = "web/ui/prune_preview.html")]
pub struct PrunePreviewTemplate {
    pub findings: Vec<crate::cleanup_audit::CleanupFinding>,
    pub total: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub report_path: Option<PathBuf>,
    pub confirmation_token: Option<String>,
    pub error_message: Option<String>,
}

// ─── Links ──────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/links.html")]
pub struct LinksTemplate {
    pub links: Vec<LinkRecord>,
    pub filter: String,
}

#[derive(Template)]
#[template(path = "web/ui/dead_links.html")]
pub struct DeadLinksTemplate {
    pub links: Vec<LinkRecord>,
}

#[derive(Template)]
#[template(path = "web/ui/repair_result.html")]
pub struct RepairResultTemplate {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
}

// ─── Config ─────────────────────────────────────────────────────────

pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Template)]
#[template(path = "web/ui/config.html")]
pub struct ConfigTemplate {
    pub config: Config,
    pub validation_result: Option<ValidationResult>,
}

// ─── Doctor ─────────────────────────────────────────────────────────

pub struct DoctorCheck {
    pub check: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Template)]
#[template(path = "web/ui/doctor.html")]
pub struct DoctorTemplate {
    pub checks: Vec<DoctorCheck>,
    pub all_passed: bool,
}

// ─── Discover ───────────────────────────────────────────────────────

use crate::discovery::DiscoveredItem;

#[derive(Template)]
#[template(path = "web/ui/discover.html")]
pub struct DiscoverTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub selected_library: String,
    pub refresh_cache: bool,
    pub discovered_items: Vec<DiscoveredItem>,
    pub status_message: Option<String>,
}

#[derive(Template)]
#[template(path = "web/ui/discover_result.html")]
pub struct DiscoverResultTemplate {
    pub success: bool,
    pub message: String,
}

// ─── Backup ─────────────────────────────────────────────────────────

pub struct BackupInfo {
    pub filename: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Template)]
#[template(path = "web/ui/backup.html")]
pub struct BackupTemplate {
    pub backups: Vec<BackupInfo>,
    pub backup_dir: PathBuf,
}

#[derive(Template)]
#[template(path = "web/ui/backup_result.html")]
pub struct BackupResultTemplate {
    pub success: bool,
    pub message: String,
    pub backup_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cleanup_audit::{
        AlternateMatchContext, CleanupFinding, CleanupOwnership, FindingReason, FindingSeverity,
        ParsedContext,
    };
    use crate::models::{LinkStatus, MediaType};

    fn sample_scan_run_view() -> ScanRunView {
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
            runtime_checks: "0.2s".to_string(),
            library_scan: "12.4s".to_string(),
            source_inventory: "148.2s".to_string(),
            matching: "86.7s".to_string(),
            title_enrichment: "16.4s".to_string(),
            linking: "20.5s".to_string(),
            plex_refresh: "3.1s".to_string(),
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
        };

        let html = template.render().unwrap();
        assert!(html.contains("Operational summary"));
        assert!(html.contains("2 dead"));
        assert!(html.contains("Auto-Repair All"));
        assert!(html.contains("Cleanup"));
        assert!(html.contains("tv / movie"));
        assert!(html.contains("badge badge-info"));
    }

    #[test]
    fn scan_run_detail_template_renders_full_run_summary() {
        let template = ScanRunDetailTemplate {
            run: sample_scan_run_view(),
        };

        let html = template.render().unwrap();
        assert!(html.contains("Scan Run Detail"));
        assert!(html.contains("Anime"));
        assert!(html.contains("Phase Telemetry"));
        assert!(html.contains("Matcher Signals"));
        assert!(html.contains("Auto-Acquire"));
        assert!(html.contains("Back to Scan History"));
        assert!(html.contains("77624480"));
    }

    #[test]
    fn cleanup_result_template_renders_report_summary() {
        let template = CleanupResultTemplate {
            success: true,
            message: "Audit complete".to_string(),
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
    }

    #[test]
    fn prune_preview_template_renders_alternate_match_context() {
        let template = PrunePreviewTemplate {
            findings: vec![CleanupFinding {
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
                    season: Some(1),
                    episode: Some(1),
                },
                alternate_match: Some(AlternateMatchContext {
                    media_id: "tvdb-2".to_string(),
                    title: "Chucky (2021)".to_string(),
                    score: 1.0,
                }),
                db_tracked: true,
                ownership: CleanupOwnership::Managed,
            }],
            total: 1,
            critical: 1,
            high: 0,
            warning: 0,
            managed_candidates: 1,
            foreign_candidates: 0,
            report_path: Some(PathBuf::from("/tmp/cleanup-audit-all.json")),
            confirmation_token: Some("abcdef1234567890".to_string()),
            error_message: None,
        };

        let html = template.render().unwrap();
        assert!(html.contains("Better Match"));
        assert!(html.contains("Chucky (2021)"));
        assert!(html.contains("tvdb-2"));
        assert!(html.contains("score 1.00"));
    }
}
