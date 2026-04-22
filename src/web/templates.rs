//! Askama templates for the web UI

use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use std::{collections::BTreeMap, path::PathBuf};

mod skip_reasons;

pub(crate) use self::skip_reasons::{
    build_skip_reason_groups, skip_reason_group_label, skip_reason_help, skip_reason_label,
};
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
use crate::db::{AcquisitionJobCounts, AnimeSearchOverrideRecord, ScanHistoryRecord};
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
    pub label: String,
    pub group: String,
    pub help: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct SkipReasonGroupView {
    pub group: String,
    pub total: i64,
    pub reasons: Vec<SkipReasonView>,
}

#[derive(Debug, Clone)]
pub struct SkipEventView {
    pub event_at: String,
    pub action: String,
    pub reason: String,
    pub reason_label: String,
    pub reason_group: String,
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
    pub skip_reason_highlights: Vec<SkipReasonView>,
    pub skip_reason_groups: Vec<SkipReasonGroupView>,
    pub skip_reason_total: i64,
    pub skip_reason_extra_buckets: i64,
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
            .and_then(|json| serde_json::from_str::<BTreeMap<String, i64>>(json).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(reason, count)| SkipReasonView::from_reason(reason, count))
            .collect::<Vec<_>>();

        entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
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
        let skip_reason_total = skip_reasons.iter().map(|entry| entry.count).sum();
        let skip_reason_highlights = skip_reasons.iter().take(3).cloned().collect::<Vec<_>>();
        let skip_reason_extra_buckets = skip_reasons
            .len()
            .saturating_sub(skip_reason_highlights.len())
            as i64;
        let skip_reason_groups = build_skip_reason_groups(&skip_reasons);
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
            skip_reason_highlights,
            skip_reason_groups,
            skip_reason_total,
            skip_reason_extra_buckets,
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
    pub needs_attention: DashboardNeedsAttentionView,
    pub activity_feed: DashboardActivityFeedView,
    pub daemon_schedule: DaemonScheduleView,
    pub streaming_guard: Option<StreamingGuardView>,
    pub recent_queue_jobs: Vec<QueueJobView>,
    pub latest_run: Option<ScanRunView>,
    pub recent_runs: Vec<ScanRunView>,
    pub queue: QueueOverview,
    pub deferred_refresh: DeferredRefreshSummaryView,
}

#[derive(Debug, Clone, Default)]
pub struct DashboardNeedsAttentionView {
    pub items: Vec<NeedsAttentionItemView>,
}

#[derive(Debug, Clone)]
pub struct NeedsAttentionItemView {
    pub severity_label: String,
    pub severity_badge_class: &'static str,
    pub title: String,
    pub message: String,
    pub next_step: String,
    pub link: Option<ActivityFeedLinkView>,
}

#[derive(Debug, Clone, Default)]
pub struct DashboardActivityFeedView {
    pub active_items: Vec<ActivityFeedItemView>,
    pub recent_items: Vec<ActivityFeedItemView>,
}

#[derive(Debug, Clone)]
pub struct ActivityFeedItemView {
    pub kind_label: String,
    pub status_label: String,
    pub status_badge_class: &'static str,
    pub scope_label: String,
    pub timestamp_label: String,
    pub timestamp: String,
    pub context: Option<String>,
    pub message: String,
    pub badges: Vec<ActivityFeedBadgeView>,
    pub link: Option<ActivityFeedLinkView>,
}

#[derive(Debug, Clone)]
pub struct ActivityFeedBadgeView {
    pub label: String,
    pub badge_class: &'static str,
}

#[derive(Debug, Clone)]
pub struct ActivityFeedLinkView {
    pub href: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct QueueJobView {
    pub label: String,
    pub status_label: String,
    pub status_badge_class: &'static str,
    pub arr_label: String,
    pub scope_label: String,
    pub query: String,
    pub attempts: i64,
    pub detail: Option<String>,
    pub timing_label: String,
    pub timing_value: String,
}

#[derive(Debug, Clone)]
pub struct DaemonScheduleView {
    pub status_label: String,
    pub status_badge_class: &'static str,
    pub interval_label: String,
    pub search_missing_label: String,
    pub vacuum_label: String,
    pub last_run_label: String,
    pub next_due_label: String,
    pub detail: String,
}

#[derive(Template)]
#[template(path = "web/ui/partials/activity_feed.html")]
pub struct DashboardActivityFeedTemplate {
    pub activity_feed: DashboardActivityFeedView,
}

// ─── Status ─────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/status.html")]
pub struct StatusTemplate {
    pub stats: DashboardStats,
    pub recent_links: Vec<LinkRecord>,
    pub tracked_dead_links: Vec<LinkRecord>,
    pub recent_queue_jobs: Vec<QueueJobView>,
    pub queue: QueueOverview,
    pub daemon_schedule: DaemonScheduleView,
    pub checks: std::collections::BTreeMap<String, HealthCheck>,
    pub deferred_refresh: DeferredRefreshSummaryView,
    pub streaming_guard: Option<StreamingGuardView>,
}

pub struct HealthCheck {
    pub service: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct StreamingGuardView {
    pub status_label: String,
    pub status_badge_class: &'static str,
    pub active_streams: usize,
    pub protected_paths: Vec<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FormFeedbackView {
    pub success: bool,
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
    pub anime_search_overrides: Vec<AnimeSearchOverrideView>,
    pub anime_override_feedback: Option<FormFeedbackView>,
    pub anime_override_panel_open: bool,
    pub filters: ScanHistoryFilters,
    pub default_dry_run: bool,
    pub csrf_token: String,
}

#[derive(Debug, Clone)]
pub struct AnimeSearchOverrideView {
    pub media_id: String,
    pub preferred_title: Option<String>,
    pub extra_hints: Vec<String>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<AnimeSearchOverrideRecord> for AnimeSearchOverrideView {
    fn from(value: AnimeSearchOverrideRecord) -> Self {
        Self {
            media_id: value.media_id,
            preferred_title: value.preferred_title,
            extra_hints: value.extra_hints,
            note: value.note,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
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
    pub playback_guard: Option<MutationStreamingGuardView>,
    pub csrf_token: String,
}

#[derive(Debug, Clone)]
pub struct MutationStreamingGuardView {
    pub protected_count: usize,
    pub protected_paths: Vec<String>,
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
    pub playback_guard: Option<MutationStreamingGuardView>,
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
    DashboardActivityFeedTemplate,
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

impl_template_into_response!(NoConfigTemplate,);

#[cfg(test)]
mod tests;
