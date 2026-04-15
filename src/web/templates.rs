//! Askama templates for the web UI

use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use std::path::PathBuf;

#[allow(unused_imports)]
use super::filters;

use super::{
    ActiveCleanupAuditJob, ActiveRepairJob, ActiveScanJob, LastCleanupAuditOutcome,
    LastRepairOutcome, LastScanOutcome,
};
use crate::backup::BackupAppStateRestoreSummary;
use crate::cleanup_audit::{CleanupFinding, PrunePathAction};
use crate::cleanup_audit::{CleanupReport, CleanupScope};
use crate::commands::cleanup::{AnimeRemediationBlockedReasonSummary, AnimeRemediationPlanGroup};
use crate::commands::report::AnimeRemediationSample;
use crate::config::Config;
use crate::db::{AcquisitionJobCounts, ScanHistoryRecord};
use crate::media_servers::{DeferredRefreshSummary, LibraryInvalidationServerOutcome};
use crate::models::LinkRecord;

macro_rules! impl_template_into_response {
    ($($template:ty),+ $(,)?) => {
        $(
            impl IntoResponse for $template {
                fn into_response(self) -> Response {
                    match self.render() {
                        Ok(body) => Html(body).into_response(),
                        Err(err) => {
                            tracing::error!(
                                "failed to render {}: {}",
                                stringify!($template),
                                err
                            );
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Template render error",
                            )
                                .into_response()
                        }
                    }
                }
            }
        )+
    };
}

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
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeferredRefreshSummaryView {
    pub pending_targets: usize,
    pub servers: Vec<DeferredRefreshServerView>,
}

#[derive(Debug, Clone)]
pub struct DeferredRefreshServerView {
    pub server: String,
    pub queued_targets: usize,
}

impl From<DeferredRefreshSummary> for DeferredRefreshSummaryView {
    fn from(value: DeferredRefreshSummary) -> Self {
        Self {
            pending_targets: value.pending_targets,
            servers: value
                .servers
                .into_iter()
                .map(|entry| DeferredRefreshServerView {
                    server: entry.server.to_string(),
                    queued_targets: entry.queued_targets,
                })
                .collect(),
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
pub struct ActiveScanView {
    pub started_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
}

impl From<ActiveScanJob> for ActiveScanView {
    fn from(value: ActiveScanJob) -> Self {
        Self {
            started_at: value.started_at,
            scope_label: value.scope_label,
            dry_run: value.dry_run,
            search_missing: value.search_missing,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActiveCleanupAuditView {
    pub started_at: String,
    pub scope_label: String,
    pub libraries_label: String,
}

impl From<ActiveCleanupAuditJob> for ActiveCleanupAuditView {
    fn from(value: ActiveCleanupAuditJob) -> Self {
        Self {
            started_at: value.started_at,
            scope_label: value.scope_label,
            libraries_label: value.libraries_label,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActiveRepairView {
    pub started_at: String,
    pub scope_label: String,
}

impl From<ActiveRepairJob> for ActiveRepairView {
    fn from(value: ActiveRepairJob) -> Self {
        Self {
            started_at: value.started_at,
            scope_label: value.scope_label,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackgroundScanOutcomeView {
    pub finished_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub success: bool,
    pub message: String,
}

impl From<LastScanOutcome> for BackgroundScanOutcomeView {
    fn from(value: LastScanOutcome) -> Self {
        Self {
            finished_at: value.finished_at,
            scope_label: value.scope_label,
            dry_run: value.dry_run,
            search_missing: value.search_missing,
            success: value.success,
            message: value.message,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackgroundCleanupAuditOutcomeView {
    pub finished_at: String,
    pub scope_label: String,
    pub libraries_label: String,
    pub success: bool,
    pub message: String,
    pub report_path: Option<String>,
}

impl From<LastCleanupAuditOutcome> for BackgroundCleanupAuditOutcomeView {
    fn from(value: LastCleanupAuditOutcome) -> Self {
        Self {
            finished_at: value.finished_at,
            scope_label: value.scope_label,
            libraries_label: value.libraries_label,
            success: value.success,
            message: value.message,
            report_path: value.report_path,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackgroundRepairOutcomeView {
    pub finished_at: String,
    pub success: bool,
    pub message: String,
}

impl From<LastRepairOutcome> for BackgroundRepairOutcomeView {
    fn from(value: LastRepairOutcome) -> Self {
        Self {
            finished_at: value.finished_at,
            success: value.success,
            message: value.message,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MediaServerRefreshServerView {
    pub server: String,
    pub requested_targets: i64,
    pub refreshed_batches: i64,
    pub planned_batches: i64,
    pub skipped_batches: i64,
    pub failed_batches: i64,
    pub aborted_due_to_cap: bool,
    pub deferred_due_to_lock: bool,
}

#[derive(Debug, Clone)]
pub struct SkipReasonView {
    pub reason: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct SkipEventView {
    pub event_at: String,
    pub action: String,
    pub reason: String,
    pub target_path: String,
    pub source_path: Option<String>,
    pub media_id: Option<String>,
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
    pub skip_reasons: Vec<SkipReasonView>,
    pub runtime_checks: String,
    pub library_scan: String,
    pub source_inventory: String,
    pub matching: String,
    pub title_enrichment: String,
    pub linking: String,
    pub plex_refresh: String,
    pub plex_refresh_requested_paths: i64,
    pub plex_refresh_unique_paths: i64,
    pub plex_refresh_planned_batches: i64,
    pub plex_refresh_coalesced_batches: i64,
    pub plex_refresh_coalesced_paths: i64,
    pub plex_refresh_refreshed_batches: i64,
    pub plex_refresh_refreshed_paths_covered: i64,
    pub plex_refresh_skipped_batches: i64,
    pub plex_refresh_unresolved_paths: i64,
    pub plex_refresh_capped_batches: i64,
    pub plex_refresh_aborted_due_to_cap: bool,
    pub plex_refresh_failed_batches: i64,
    pub media_server_refresh: Vec<MediaServerRefreshServerView>,
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
    fn skip_reasons_from_record(record: &ScanHistoryRecord) -> Vec<SkipReasonView> {
        let mut entries = record
            .skip_reason_json
            .as_deref()
            .and_then(|json| {
                serde_json::from_str::<std::collections::BTreeMap<String, i64>>(json).ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(reason, count)| SkipReasonView { reason, count })
            .collect::<Vec<_>>();

        entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.reason.cmp(&b.reason)));
        entries
    }

    fn media_server_refresh_from_record(
        record: &ScanHistoryRecord,
    ) -> Vec<MediaServerRefreshServerView> {
        record
            .media_server_refresh_json
            .as_deref()
            .and_then(|json| {
                serde_json::from_str::<Vec<LibraryInvalidationServerOutcome>>(json).ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|entry| MediaServerRefreshServerView {
                server: entry.server.to_string(),
                requested_targets: entry.requested_targets as i64,
                refreshed_batches: entry.refresh.refreshed_batches as i64,
                planned_batches: entry.refresh.planned_batches as i64,
                skipped_batches: entry.refresh.skipped_batches as i64,
                failed_batches: entry.refresh.failed_batches as i64,
                aborted_due_to_cap: entry.refresh.aborted_due_to_cap,
                deferred_due_to_lock: entry.refresh.deferred_due_to_lock,
            })
            .collect()
    }

    pub fn from_record(record: ScanHistoryRecord) -> Self {
        let skip_reasons = Self::skip_reasons_from_record(&record);
        let media_server_refresh = Self::media_server_refresh_from_record(&record);
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
            skip_reasons,
            runtime_checks: format_duration_ms(record.runtime_checks_ms),
            library_scan: format_duration_ms(record.library_scan_ms),
            source_inventory: format_duration_ms(record.source_inventory_ms),
            matching: format_duration_ms(record.matching_ms),
            title_enrichment: format_duration_ms(record.title_enrichment_ms),
            linking: format_duration_ms(record.linking_ms),
            plex_refresh: format_duration_ms(record.plex_refresh_ms),
            plex_refresh_requested_paths: record.plex_refresh_requested_paths,
            plex_refresh_unique_paths: record.plex_refresh_unique_paths,
            plex_refresh_planned_batches: record.plex_refresh_planned_batches,
            plex_refresh_coalesced_batches: record.plex_refresh_coalesced_batches,
            plex_refresh_coalesced_paths: record.plex_refresh_coalesced_paths,
            plex_refresh_refreshed_batches: record.plex_refresh_refreshed_batches,
            plex_refresh_refreshed_paths_covered: record.plex_refresh_refreshed_paths_covered,
            plex_refresh_skipped_batches: record.plex_refresh_skipped_batches,
            plex_refresh_unresolved_paths: record.plex_refresh_unresolved_paths,
            plex_refresh_capped_batches: record.plex_refresh_capped_batches,
            plex_refresh_aborted_due_to_cap: record.plex_refresh_aborted_due_to_cap,
            plex_refresh_failed_batches: record.plex_refresh_failed_batches,
            media_server_refresh,
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

pub(crate) fn format_backup_timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

pub(crate) fn format_backup_age(value: DateTime<Utc>) -> String {
    let age = Utc::now().signed_duration_since(value);
    if age.num_days() > 0 {
        format!("{}d ago", age.num_days())
    } else if age.num_hours() > 0 {
        format!("{}h ago", age.num_hours())
    } else if age.num_minutes() > 0 {
        format!("{}m ago", age.num_minutes())
    } else {
        "just now".to_string()
    }
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
    pub deferred_refresh: DeferredRefreshSummaryView,
}

// ─── Status ─────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/status.html")]
pub struct StatusTemplate {
    pub stats: DashboardStats,
    pub recent_links: Vec<LinkRecord>,
    pub tracked_dead_links: Vec<LinkRecord>,
    pub queue: QueueOverview,
    pub checks: std::collections::BTreeMap<String, HealthCheck>,
    pub deferred_refresh: DeferredRefreshSummaryView,
}

pub struct HealthCheck {
    pub service: String,
    pub status: String,
    pub message: String,
}

// ─── Scan ───────────────────────────────────────────────────────────

use crate::config::LibraryConfig;

#[derive(Template)]
#[template(path = "web/ui/scan.html")]
pub struct ScanTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub active_scan: Option<ActiveScanView>,
    pub last_scan_outcome: Option<BackgroundScanOutcomeView>,
    pub latest_run: Option<ScanRunView>,
    pub history: Vec<ScanRunView>,
    pub queue: QueueOverview,
    pub filters: ScanHistoryFilters,
    pub default_dry_run: bool,
    pub csrf_token: String,
}

#[derive(Template)]
#[template(path = "web/ui/scan_result.html")]
pub struct ScanResultTemplate {
    pub success: bool,
    pub message: String,
    pub active_scan: Option<ActiveScanView>,
    pub last_scan_outcome: Option<BackgroundScanOutcomeView>,
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
    pub skip_events: Vec<SkipEventView>,
}

// ─── Cleanup ────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/cleanup.html")]
pub struct CleanupTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub active_cleanup_audit: Option<ActiveCleanupAuditView>,
    pub last_cleanup_audit_outcome: Option<BackgroundCleanupAuditOutcomeView>,
    pub last_report: Option<CleanupReportSummaryView>,
    pub last_report_path: Option<PathBuf>,
    pub csrf_token: String,
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
    pub active_cleanup_audit: Option<ActiveCleanupAuditView>,
    pub last_cleanup_audit_outcome: Option<BackgroundCleanupAuditOutcomeView>,
    pub report_path: Option<PathBuf>,
    pub report_summary: Option<CleanupReportSummaryView>,
}

#[derive(Template)]
#[template(path = "web/ui/prune_preview.html")]
pub struct PrunePreviewTemplate {
    pub findings: Vec<PruneFindingView>,
    pub total: usize,
    pub actionable_candidates: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
    pub blocked_candidates: usize,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub reason_counts: Vec<crate::cleanup_audit::PruneReasonCount>,
    pub blocked_reason_summary: Vec<crate::cleanup_audit::PruneBlockedReasonSummary>,
    pub legacy_anime_root_groups: Vec<crate::cleanup_audit::LegacyAnimeRootGroupCount>,
    pub report_path: Option<PathBuf>,
    pub confirmation_token: Option<String>,
    pub already_applied: bool,
    pub error_message: Option<String>,
    pub csrf_token: String,
}

#[derive(Debug, Clone)]
pub struct PruneFindingView {
    pub finding: CleanupFinding,
    pub action_label: String,
    pub action_badge_class: &'static str,
    pub blocked_reason_label: Option<String>,
    pub blocked_reason_recommended_action: Option<String>,
    pub alternate_match_summary: Option<String>,
    pub legacy_anime_root_summary: Option<String>,
    pub legacy_anime_tagged_roots: Vec<PathBuf>,
}

impl PruneFindingView {
    pub fn from_finding(finding: CleanupFinding, action: PrunePathAction) -> Self {
        let alternate_match_summary = finding.alternate_match.as_ref().map(|alt| {
            format!(
                "Better Match: {} ({}) score {:.2}",
                alt.title, alt.media_id, alt.score
            )
        });
        let legacy_anime_root_summary = finding
            .legacy_anime_root
            .as_ref()
            .map(|legacy| format!("legacy root: {}", legacy.untagged_root.display()));
        let legacy_anime_tagged_roots = finding
            .legacy_anime_root
            .as_ref()
            .map(|legacy| legacy.tagged_roots.clone())
            .unwrap_or_default();
        let (
            action_label,
            action_badge_class,
            blocked_reason_label,
            blocked_reason_recommended_action,
        ) = match action {
            PrunePathAction::Delete => ("Delete", "badge-danger", None, None),
            PrunePathAction::Quarantine => ("Quarantine", "badge-warning", None, None),
            PrunePathAction::Blocked(code) => (
                "Blocked",
                "badge-secondary",
                Some(code.label().to_string()),
                Some(code.recommended_action().to_string()),
            ),
            PrunePathAction::ObserveOnly => ("Observe", "badge-info", None, None),
        };

        Self {
            finding,
            action_label: action_label.to_string(),
            action_badge_class,
            blocked_reason_label,
            blocked_reason_recommended_action,
            alternate_match_summary,
            legacy_anime_root_summary,
            legacy_anime_tagged_roots,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnimeRemediationSummaryView {
    pub generated_at: String,
    pub plex_db_path: String,
    pub full: bool,
    pub filesystem_mixed_root_groups: usize,
    pub plex_duplicate_show_groups: usize,
    pub plex_hama_anidb_tvdb_groups: usize,
    pub correlated_hama_split_groups: usize,
    pub remediation_groups: usize,
    pub returned_groups: usize,
    pub visible_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub state_filter: String,
    pub reason_filter: String,
    pub title_filter: String,
    pub blocked_reason_summary: Vec<AnimeRemediationBlockedReasonView>,
    pub available_blocked_reasons: Vec<AnimeRemediationBlockedReasonView>,
}

#[derive(Debug, Clone)]
pub struct AnimeRemediationBlockedReasonView {
    pub code: String,
    pub label: String,
    pub groups: usize,
    pub recommended_action: String,
}

#[derive(Debug, Clone)]
pub struct AnimeRemediationGroupView {
    pub normalized_title: String,
    pub recommended_tagged_root: PathBuf,
    pub recommended_filesystem_symlinks: usize,
    pub recommended_db_active_links: usize,
    pub alternate_tagged_roots: Vec<PathBuf>,
    pub legacy_roots: Vec<PathBuf>,
    pub legacy_symlink_total: usize,
    pub legacy_db_total: usize,
    pub plex_total_rows: usize,
    pub plex_live_rows: usize,
    pub plex_deleted_rows: usize,
    pub plex_guid_kinds: Vec<String>,
    pub eligible: bool,
    pub block_reasons: Vec<String>,
    pub recommended_action: Option<String>,
    pub candidate_symlink_samples: Vec<PathBuf>,
    pub broken_symlink_samples: Vec<PathBuf>,
    pub legacy_media_file_samples: Vec<PathBuf>,
}

impl From<AnimeRemediationSample> for AnimeRemediationGroupView {
    fn from(value: AnimeRemediationSample) -> Self {
        let legacy_symlink_total = value
            .legacy_roots
            .iter()
            .map(|root| root.filesystem_symlinks)
            .sum();
        let legacy_db_total = value
            .legacy_roots
            .iter()
            .map(|root| root.db_active_links)
            .sum();

        Self {
            normalized_title: value.normalized_title,
            recommended_tagged_root: value.recommended_tagged_root.path,
            recommended_filesystem_symlinks: value.recommended_tagged_root.filesystem_symlinks,
            recommended_db_active_links: value.recommended_tagged_root.db_active_links,
            alternate_tagged_roots: value
                .alternate_tagged_roots
                .into_iter()
                .map(|root| root.path)
                .collect(),
            legacy_roots: value
                .legacy_roots
                .into_iter()
                .map(|root| root.path)
                .collect(),
            legacy_symlink_total,
            legacy_db_total,
            plex_total_rows: value.plex_total_rows,
            plex_live_rows: value.plex_live_rows,
            plex_deleted_rows: value.plex_deleted_rows,
            plex_guid_kinds: value.plex_guid_kinds,
            eligible: false,
            block_reasons: Vec::new(),
            recommended_action: None,
            candidate_symlink_samples: Vec::new(),
            broken_symlink_samples: Vec::new(),
            legacy_media_file_samples: Vec::new(),
        }
    }
}

impl AnimeRemediationGroupView {
    pub fn from_plan_group(value: AnimeRemediationPlanGroup) -> Self {
        let legacy_symlink_total = value
            .legacy_roots
            .iter()
            .map(|root| root.filesystem_symlinks)
            .sum();
        let legacy_db_total = value
            .legacy_roots
            .iter()
            .map(|root| root.db_active_links)
            .sum();
        let recommended_action = value
            .block_reasons
            .first()
            .map(|reason| reason.recommended_action.clone());

        Self {
            normalized_title: value.normalized_title,
            recommended_tagged_root: value.recommended_tagged_root.path,
            recommended_filesystem_symlinks: value.recommended_tagged_root.filesystem_symlinks,
            recommended_db_active_links: value.recommended_tagged_root.db_active_links,
            alternate_tagged_roots: value
                .alternate_tagged_roots
                .into_iter()
                .map(|root| root.path)
                .collect(),
            legacy_roots: value
                .legacy_roots
                .into_iter()
                .map(|root| root.path)
                .collect(),
            legacy_symlink_total,
            legacy_db_total,
            plex_total_rows: value.plex_live_rows + value.plex_deleted_rows,
            plex_live_rows: value.plex_live_rows,
            plex_deleted_rows: value.plex_deleted_rows,
            plex_guid_kinds: value.plex_guid_kinds,
            eligible: value.eligible,
            block_reasons: value
                .block_reasons
                .into_iter()
                .map(|reason| reason.message)
                .collect(),
            recommended_action,
            candidate_symlink_samples: value.candidate_symlink_samples,
            broken_symlink_samples: value.broken_symlink_samples,
            legacy_media_file_samples: value.legacy_media_file_samples,
        }
    }
}

impl From<AnimeRemediationBlockedReasonSummary> for AnimeRemediationBlockedReasonView {
    fn from(value: AnimeRemediationBlockedReasonSummary) -> Self {
        Self {
            code: value.code.as_str().to_string(),
            label: value.label,
            groups: value.groups,
            recommended_action: value.recommended_action,
        }
    }
}

#[derive(Template)]
#[template(path = "web/ui/anime_remediation.html")]
pub struct AnimeRemediationTemplate {
    pub summary: Option<AnimeRemediationSummaryView>,
    pub groups: Vec<AnimeRemediationGroupView>,
    pub error_message: Option<String>,
    pub csrf_token: String,
}

#[derive(Debug, Clone)]
pub struct AnimeRemediationPreviewResultView {
    pub report_path: PathBuf,
    pub plex_db_path: String,
    pub title_filter: String,
    pub total_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub cleanup_candidates: usize,
    pub confirmation_token: String,
    pub blocked_reason_summary: Vec<AnimeRemediationBlockedReasonView>,
    pub groups: Vec<AnimeRemediationGroupView>,
}

#[derive(Debug, Clone)]
pub struct AnimeRemediationApplyResultView {
    pub report_path: PathBuf,
    pub total_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub candidates: usize,
    pub quarantined: usize,
    pub removed: usize,
    pub skipped: usize,
    pub safety_snapshot: Option<PathBuf>,
    pub media_server_invalidation_summary: Option<String>,
}

#[derive(Template)]
#[template(path = "web/ui/anime_remediation_result.html")]
pub struct AnimeRemediationResultTemplate {
    pub success: bool,
    pub message: String,
    pub preview: Option<AnimeRemediationPreviewResultView>,
    pub apply: Option<AnimeRemediationApplyResultView>,
    pub csrf_token: String,
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
    pub active_repair: Option<ActiveRepairView>,
    pub last_repair_outcome: Option<BackgroundRepairOutcomeView>,
    pub csrf_token: String,
}

#[derive(Template)]
#[template(path = "web/ui/repair_result.html")]
pub struct RepairResultTemplate {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
    pub active_repair: Option<ActiveRepairView>,
    pub last_repair_outcome: Option<BackgroundRepairOutcomeView>,
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
    pub csrf_token: String,
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

use crate::discovery::{DiscoverFolderPlan, DiscoverPlacement, DiscoverSummary};

#[derive(Template)]
#[template(path = "web/ui/discover.html")]
pub struct DiscoverTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub selected_library: String,
    pub refresh_cache: bool,
}

#[derive(Template)]
#[template(path = "web/ui/discover_content.html")]
pub struct DiscoverContentTemplate {
    pub discover_summary: DiscoverSummary,
    pub folder_plans: Vec<DiscoverFolderPlan>,
    pub discovered_items: Vec<DiscoverPlacement>,
    pub status_message: Option<String>,
}

// ─── Backup ─────────────────────────────────────────────────────────

pub struct BackupInfo {
    pub filename: String,
    pub label: String,
    pub kind_label: String,
    pub kind_badge_class: &'static str,
    pub created_at: String,
    pub age_label: String,
    pub recorded_links: usize,
    pub link_delta_label: String,
    pub manifest_size_bytes: u64,
    pub database_snapshot_size_bytes: Option<u64>,
    pub config_snapshot_present: bool,
    pub secret_snapshot_count: usize,
}

#[derive(Template)]
#[template(path = "web/ui/backup.html")]
pub struct BackupTemplate {
    pub backups: Vec<BackupInfo>,
    pub backup_dir: PathBuf,
    pub csrf_token: String,
}

#[derive(Template)]
#[template(path = "web/ui/backup_result.html")]
pub struct BackupResultTemplate {
    pub success: bool,
    pub message: String,
    pub backup_path: Option<PathBuf>,
    pub database_snapshot_path: Option<PathBuf>,
    pub config_snapshot_path: Option<PathBuf>,
    pub secret_snapshot_count: usize,
    pub app_state_restore_summary: Option<BackupAppStateRestoreSummary>,
}

impl_template_into_response!(
    DashboardTemplate,
    StatusTemplate,
    ScanTemplate,
    ScanResultTemplate,
    ScanHistoryTemplate,
    ScanRunDetailTemplate,
    CleanupTemplate,
    CleanupResultTemplate,
    PrunePreviewTemplate,
    AnimeRemediationTemplate,
    AnimeRemediationResultTemplate,
    LinksTemplate,
    DeadLinksTemplate,
    RepairResultTemplate,
    ConfigTemplate,
    DoctorTemplate,
    DiscoverTemplate,
    DiscoverContentTemplate,
    BackupTemplate,
    BackupResultTemplate,
);
 
 // ─── No-config setup page ──────────────────────────────────────────
 
 #[derive(Template)]
 #[template(path = "web/ui/noconfig.html")]
 pub struct NoConfigTemplate;
 
 impl_template_into_response!(
     NoConfigTemplate,
 );

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cleanup_audit::{
        AlternateMatchContext, CleanupFinding, CleanupOwnership, FindingReason, FindingSeverity,
        ParsedContext, PruneReasonCount,
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
            skip_reasons: vec![
                SkipReasonView {
                    reason: "already_correct".to_string(),
                    count: 6200,
                },
                SkipReasonView {
                    reason: "source_missing_before_link".to_string(),
                    count: 3044,
                },
                SkipReasonView {
                    reason: "ambiguous_match".to_string(),
                    count: 70,
                },
            ],
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
        assert!(html.contains("Operational summary"));
        assert!(html.contains("2 dead"));
        assert!(html.contains("Auto-Repair All"));
        assert!(html.contains("Cleanup"));
        assert!(html.contains("Background repair running"));
        assert!(html.contains("tv / movie"));
        assert!(html.contains("badge badge-info"));
    }

    #[test]
    fn scan_run_detail_template_renders_full_run_summary() {
        let template = ScanRunDetailTemplate {
            run: sample_scan_run_view(),
            skip_events: vec![SkipEventView {
                event_at: "2026-03-21 21:12:00".to_string(),
                action: "skipped".to_string(),
                reason: "source_missing_before_link".to_string(),
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
        assert!(html.contains("source_missing_before_link"));
        assert!(html.contains(">3044<"));
        assert!(html.contains("Recent concrete skip events"));
        assert!(html.contains("/library/Show A/Season 01/Show A - S01E01.mkv"));
        assert!(html.contains("Back to Scan History"));
        assert!(html.contains("77624480"));
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
        assert!(html.contains("Better Match"));
        assert!(html.contains("Chucky (2021)"));
        assert!(html.contains("tvdb-2"));
        assert!(html.contains("score 1.00"));
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
        assert!(html.contains("Legacy Anime Root Groups"));
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
        assert!(html.contains("Apply blocked"));
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
}
