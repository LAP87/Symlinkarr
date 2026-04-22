//! HTTP handlers for the web UI

use askama::Template;
use axum::{
    body::Bytes,
    extract::{Form, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDateTime, Utc};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path as StdPath, PathBuf};
use tracing::{error, info};

#[cfg(test)]
use admin::DiscoverQuery;
pub(crate) use admin::{
    get_backup, get_config, get_discover, get_discover_content, get_doctor, post_backup_create,
    post_backup_restore, post_config_validate,
};
#[cfg(test)]
use cleanup::AnimeRemediationQuery;
pub(crate) use cleanup::{
    get_cleanup, get_cleanup_anime_remediation, get_cleanup_prune, get_dead_links, get_links,
    post_cleanup_anime_remediation_apply, post_cleanup_anime_remediation_preview,
    post_cleanup_audit, post_cleanup_prune, post_repair,
};
use scan::scan_run_views;
#[cfg(test)]
use scan::ScanHistoryQuery;
pub(crate) use scan::{get_scan, get_scan_history, get_scan_run_detail, post_scan_trigger};
pub(crate) use scan::{post_scan_anime_override, post_scan_anime_override_delete};

use crate::api::tautulli::TautulliClient;
use crate::backup::BackupManager;
use crate::cleanup_audit;
use crate::commands::backup::ensure_backup_restore_runtime_healthy;
use crate::commands::cleanup::{
    anime_remediation_block_reason_catalog, apply_anime_remediation_plan_with_refresh,
    apply_cleanup_prune_with_refresh, assess_anime_remediation_groups,
    filter_anime_remediation_groups, preview_anime_remediation_plan,
    summarize_anime_remediation_blocked_reasons, AnimeRemediationGroupFilters,
    CleanupPruneApplyArgs,
};
use crate::commands::config::validate_config_report;
use crate::commands::discover::load_discovery_snapshot;
use crate::commands::doctor::{collect_doctor_checks, DoctorCheckMode};
use crate::commands::report::build_anime_remediation_report;
use crate::commands::selected_libraries;
use crate::db::{
    AcquisitionJobCounts, AcquisitionJobRecord, AcquisitionJobStatus, LinkEventHistoryRecord,
    ScanHistoryRecord,
};
use crate::discovery::DiscoverSummary;
use crate::media_servers::deferred_refresh_summary;

use super::templates::*;
use super::{
    clamp_link_list_limit, infer_cleanup_scope, latest_cleanup_report_path, load_cleanup_report,
    resolve_cleanup_report_path, should_surface_cleanup_audit_outcome, should_surface_scan_outcome,
    WebState,
};

fn dashboard_stats_from_web_stats(stats: crate::db::WebStats) -> DashboardStats {
    DashboardStats {
        active_links: stats.active_links,
        dead_links: stats.dead_links,
        total_scans: stats.total_scans,
        last_scan: stats.last_scan,
    }
}

fn activity_badge(label: impl Into<String>, badge_class: &'static str) -> ActivityFeedBadgeView {
    ActivityFeedBadgeView {
        label: label.into(),
        badge_class,
    }
}

fn activity_link(href: impl Into<String>, label: impl Into<String>) -> ActivityFeedLinkView {
    ActivityFeedLinkView {
        href: href.into(),
        label: label.into(),
    }
}

fn scan_activity_badges(dry_run: bool, search_missing: bool) -> Vec<ActivityFeedBadgeView> {
    let mut badges = Vec::new();
    badges.push(activity_badge(
        if dry_run { "Dry Run" } else { "Live" },
        if dry_run {
            "badge-info"
        } else {
            "badge-success"
        },
    ));
    if search_missing {
        badges.push(activity_badge("Search Missing", "badge-warning"));
    }
    badges
}

fn activity_timestamp_rank(timestamp: &str) -> String {
    timestamp.replace(" UTC", "")
}

fn parse_scan_timestamp(input: &str) -> Option<DateTime<Utc>> {
    let normalized = input.trim().trim_end_matches(" UTC");
    let naive = NaiveDateTime::parse_from_str(normalized, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

fn humanize_duration(delta: ChronoDuration) -> String {
    let total_minutes = delta.num_minutes().max(0);
    if total_minutes < 60 {
        return format!("{}m", total_minutes);
    }

    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if minutes == 0 {
        format!("{}h", hours)
    } else {
        format!("{}h {}m", hours, minutes)
    }
}

fn daemon_schedule_view(
    config: &crate::config::Config,
    latest_run_started_at: Option<&str>,
) -> DaemonScheduleView {
    let interval_label = format!("Every {} min", config.daemon.interval_minutes);
    let search_missing_label = if config.daemon.search_missing {
        "Enabled".to_string()
    } else {
        "Off".to_string()
    };
    let vacuum_label = if config.daemon.vacuum_enabled {
        format!("Daily @ {:02}:00 local", config.daemon.vacuum_hour_local)
    } else {
        "Off".to_string()
    };
    let last_run_label = latest_run_started_at
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Never recorded")
        .to_string();

    if !config.daemon.enabled {
        return DaemonScheduleView {
            status_label: "Config only".to_string(),
            status_badge_class: "badge-secondary",
            interval_label,
            search_missing_label,
            vacuum_label,
            last_run_label,
            next_due_label: "Not scheduled by daemon".to_string(),
            detail: "Daemon mode is disabled in config, so recorded scans here come from manual triggers or an external scheduler.".to_string(),
        };
    }

    let Some(last_run_at) = latest_run_started_at.and_then(parse_scan_timestamp) else {
        return DaemonScheduleView {
            status_label: "Priming".to_string(),
            status_badge_class: "badge-info",
            interval_label,
            search_missing_label,
            vacuum_label,
            last_run_label,
            next_due_label: "After first recorded scan".to_string(),
            detail: "Daemon mode is enabled but this database has no recorded scan yet. The web UI can only estimate cadence after the first run lands.".to_string(),
        };
    };

    let next_due = last_run_at + ChronoDuration::minutes(config.daemon.interval_minutes as i64);
    let now = Utc::now();
    if now >= next_due {
        return DaemonScheduleView {
            status_label: "Due".to_string(),
            status_badge_class: "badge-warning",
            interval_label,
            search_missing_label,
            vacuum_label,
            last_run_label,
            next_due_label: format!("Due now ({} late)", humanize_duration(now - next_due)),
            detail: "This is a config-based estimate only. The web UI still cannot prove whether a daemon process is actually running on this machine right now.".to_string(),
        };
    }

    DaemonScheduleView {
        status_label: "Waiting".to_string(),
        status_badge_class: "badge-success",
        interval_label,
        search_missing_label,
        vacuum_label,
        last_run_label,
        next_due_label: format!(
            "{} (in {})",
            next_due.format("%Y-%m-%d %H:%M:%S UTC"),
            humanize_duration(next_due - now)
        ),
        detail: "This is a config-based estimate from the latest recorded scan. It is useful for cadence awareness, not as proof that the daemon loop is alive.".to_string(),
    }
}

const RECENT_QUEUE_JOB_LIMIT: usize = 6;

fn format_operator_name(raw: &str) -> String {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut formatted = String::new();
    formatted.push(first.to_ascii_uppercase());
    formatted.push_str(chars.as_str());
    formatted
}

fn queue_status_presenter(status: AcquisitionJobStatus) -> (&'static str, &'static str) {
    match status {
        AcquisitionJobStatus::Queued => ("Queued", "badge-info"),
        AcquisitionJobStatus::Downloading => ("Downloading", "badge-warning"),
        AcquisitionJobStatus::Relinking => ("Relinking", "badge-warning"),
        AcquisitionJobStatus::NoResult => ("No Result", "badge-info"),
        AcquisitionJobStatus::Blocked => ("Blocked", "badge-warning"),
        AcquisitionJobStatus::CompletedLinked => ("Linked", "badge-success"),
        AcquisitionJobStatus::CompletedUnlinked => ("Needs Relink", "badge-secondary"),
        AcquisitionJobStatus::Failed => ("Failed", "badge-danger"),
    }
}

fn queue_job_timing(record: &AcquisitionJobRecord) -> (String, String) {
    if let Some(next_retry_at) = record.next_retry_at {
        return (
            "Next retry".to_string(),
            next_retry_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        );
    }
    if let Some(completed_at) = record.completed_at {
        return (
            "Completed".to_string(),
            completed_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        );
    }
    if let Some(submitted_at) = record.submitted_at {
        return (
            "Submitted".to_string(),
            submitted_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        );
    }

    (
        match record.status {
            AcquisitionJobStatus::Queued => "Queued",
            AcquisitionJobStatus::Downloading | AcquisitionJobStatus::Relinking => "Active",
            AcquisitionJobStatus::NoResult
            | AcquisitionJobStatus::Blocked
            | AcquisitionJobStatus::CompletedUnlinked
            | AcquisitionJobStatus::Failed => "Pending",
            AcquisitionJobStatus::CompletedLinked => "Completed",
        }
        .to_string(),
        "Pending".to_string(),
    )
}

fn queue_job_detail(record: &AcquisitionJobRecord) -> Option<String> {
    match (&record.error, &record.release_title) {
        (Some(error), Some(release)) => Some(format!("{} | Release: {}", error, release)),
        (Some(error), None) => Some(error.clone()),
        (None, Some(release)) => Some(format!("Release: {}", release)),
        (None, None) => None,
    }
}

fn queue_job_view(record: AcquisitionJobRecord) -> QueueJobView {
    let (status_label, status_badge_class) = queue_status_presenter(record.status);
    let (timing_label, timing_value) = queue_job_timing(&record);
    let detail = queue_job_detail(&record);

    QueueJobView {
        label: record.label,
        status_label: status_label.to_string(),
        status_badge_class,
        arr_label: format_operator_name(&record.arr),
        scope_label: record
            .library_filter
            .unwrap_or_else(|| "All Libraries".to_string()),
        query: record.query,
        attempts: record.attempts,
        detail,
        timing_label,
        timing_value,
    }
}

fn queue_activity_message(record: &AcquisitionJobRecord) -> String {
    match record.status {
        AcquisitionJobStatus::Queued => {
            "Queued and waiting for submission or the next retry window.".to_string()
        }
        AcquisitionJobStatus::Downloading => {
            "Download has been handed off and is waiting to relink.".to_string()
        }
        AcquisitionJobStatus::Relinking => {
            "Download finished and is waiting for fresh symlink verification.".to_string()
        }
        AcquisitionJobStatus::NoResult => queue_job_detail(record)
            .unwrap_or_else(|| "No provider result matched the request.".to_string()),
        AcquisitionJobStatus::Blocked => queue_job_detail(record)
            .unwrap_or_else(|| "Queue guard blocked automatic progress.".to_string()),
        AcquisitionJobStatus::CompletedUnlinked => queue_job_detail(record).unwrap_or_else(|| {
            "Download completed, but Symlinkarr still has not created a fresh link.".to_string()
        }),
        AcquisitionJobStatus::Failed => queue_job_detail(record)
            .unwrap_or_else(|| "Submission or follow-up processing failed.".to_string()),
        AcquisitionJobStatus::CompletedLinked => queue_job_detail(record)
            .unwrap_or_else(|| "Queue job completed and linked successfully.".to_string()),
    }
}

fn queue_activity_badges(record: &AcquisitionJobRecord) -> Vec<ActivityFeedBadgeView> {
    let mut badges = vec![activity_badge(
        format_operator_name(&record.arr),
        "badge-secondary",
    )];
    if let Some(scope) = record
        .library_filter
        .as_deref()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
    {
        badges.push(activity_badge(scope, "badge-info"));
    }
    if record.attempts > 0 {
        badges.push(activity_badge(
            format!("Attempts {}", record.attempts),
            "badge-warning",
        ));
    }
    badges
}

fn queue_activity_item(record: AcquisitionJobRecord) -> ActivityFeedItemView {
    let (status_label, status_badge_class) = queue_status_presenter(record.status);
    let (timestamp_label, timestamp) = queue_job_timing(&record);

    ActivityFeedItemView {
        kind_label: "Auto-Acquire".to_string(),
        status_label: status_label.to_string(),
        status_badge_class,
        scope_label: record.label.clone(),
        timestamp_label,
        timestamp,
        context: Some(format!(
            "{} • {}",
            format_operator_name(&record.arr),
            record.query
        )),
        message: queue_activity_message(&record),
        badges: queue_activity_badges(&record),
        link: Some(activity_link("/status", "Open Status")),
    }
}

async fn recent_queue_jobs(state: &WebState, limit: usize) -> Vec<QueueJobView> {
    let statuses = [
        AcquisitionJobStatus::Queued,
        AcquisitionJobStatus::Downloading,
        AcquisitionJobStatus::Relinking,
        AcquisitionJobStatus::Blocked,
        AcquisitionJobStatus::NoResult,
        AcquisitionJobStatus::CompletedUnlinked,
        AcquisitionJobStatus::Failed,
    ];

    match state
        .database
        .list_acquisition_jobs(Some(&statuses), limit.max(1))
        .await
    {
        Ok(records) => records.into_iter().map(queue_job_view).collect(),
        Err(err) => {
            error!("Failed to list recent acquisition jobs: {}", err);
            Vec::new()
        }
    }
}

async fn streaming_guard_view(state: &WebState) -> Option<StreamingGuardView> {
    if !state.config.has_tautulli() {
        return None;
    }

    let tautulli = TautulliClient::new(&state.config.tautulli);
    match tautulli.get_active_file_paths().await {
        Ok(paths) => Some(StreamingGuardView {
            status_label: if paths.is_empty() {
                "Idle".to_string()
            } else {
                "Protecting".to_string()
            },
            status_badge_class: if paths.is_empty() {
                "badge-success"
            } else {
                "badge-warning"
            },
            active_streams: paths.len(),
            protected_paths: paths.into_iter().take(6).collect(),
            error_message: None,
        }),
        Err(err) => Some(StreamingGuardView {
            status_label: "Unavailable".to_string(),
            status_badge_class: "badge-danger",
            active_streams: 0,
            protected_paths: Vec::new(),
            error_message: Some(err.to_string()),
        }),
    }
}

async fn acquisition_feed_items(
    state: &WebState,
) -> (Vec<ActivityFeedItemView>, Vec<ActivityFeedItemView>) {
    let statuses = [
        AcquisitionJobStatus::Queued,
        AcquisitionJobStatus::Downloading,
        AcquisitionJobStatus::Relinking,
        AcquisitionJobStatus::Blocked,
        AcquisitionJobStatus::NoResult,
        AcquisitionJobStatus::CompletedUnlinked,
        AcquisitionJobStatus::Failed,
    ];
    let records = match state
        .database
        .list_acquisition_jobs(Some(&statuses), 8)
        .await
    {
        Ok(records) => records,
        Err(err) => {
            error!("Failed to list acquisition feed jobs: {}", err);
            return (Vec::new(), Vec::new());
        }
    };

    let active_items = records
        .iter()
        .filter(|record| {
            matches!(
                record.status,
                AcquisitionJobStatus::Queued
                    | AcquisitionJobStatus::Downloading
                    | AcquisitionJobStatus::Relinking
            )
        })
        .take(2)
        .cloned()
        .map(queue_activity_item)
        .collect::<Vec<_>>();

    let recent_items = records
        .iter()
        .filter(|record| {
            matches!(
                record.status,
                AcquisitionJobStatus::Blocked
                    | AcquisitionJobStatus::NoResult
                    | AcquisitionJobStatus::CompletedUnlinked
                    | AcquisitionJobStatus::Failed
            )
        })
        .take(3)
        .cloned()
        .map(queue_activity_item)
        .collect::<Vec<_>>();

    (active_items, recent_items)
}

fn needs_attention_item(
    severity_label: impl Into<String>,
    severity_badge_class: &'static str,
    title: impl Into<String>,
    message: impl Into<String>,
    next_step: impl Into<String>,
    link: Option<ActivityFeedLinkView>,
) -> NeedsAttentionItemView {
    NeedsAttentionItemView {
        severity_label: severity_label.into(),
        severity_badge_class,
        title: title.into(),
        message: message.into(),
        next_step: next_step.into(),
        link,
    }
}

struct DashboardAttentionInputs<'a> {
    latest_run: Option<&'a ScanRunView>,
    last_scan_outcome: Option<&'a BackgroundScanOutcomeView>,
    last_cleanup_outcome: Option<&'a BackgroundCleanupAuditOutcomeView>,
    last_repair_outcome: Option<&'a BackgroundRepairOutcomeView>,
    streaming_guard: Option<&'a StreamingGuardView>,
    daemon_schedule: Option<&'a DaemonScheduleView>,
}

fn dashboard_needs_attention(
    stats: &DashboardStats,
    queue: &QueueOverview,
    deferred_refresh: &DeferredRefreshSummaryView,
    inputs: &DashboardAttentionInputs<'_>,
) -> DashboardNeedsAttentionView {
    let mut items = Vec::new();

    if let Some(outcome) = inputs.last_scan_outcome.filter(|outcome| !outcome.success) {
        items.push(needs_attention_item(
            "Critical",
            "badge-danger",
            "Latest background scan failed",
            format!(
                "{} finished {} and reported: {}",
                outcome.scope_label, outcome.finished_at, outcome.message
            ),
            "Open Scan, compare the failure against the latest run detail, and verify provider or path health before retrying another background pass.",
            Some(activity_link("/scan", "Open Scan")),
        ));
    }

    if let Some(outcome) = inputs
        .last_cleanup_outcome
        .filter(|outcome| !outcome.success)
    {
        let link = outcome
            .report_path
            .as_ref()
            .map(|path| {
                activity_link(
                    format!("/cleanup/prune?report={}", path),
                    "Open Prune Preview",
                )
            })
            .or_else(|| Some(activity_link("/cleanup", "Open Cleanup")));
        items.push(needs_attention_item(
            "High",
            "badge-danger",
            "Latest cleanup audit failed",
            format!(
                "{} across {} finished {} and reported: {}",
                outcome.scope_label, outcome.libraries_label, outcome.finished_at, outcome.message
            ),
            "Open Cleanup and inspect the latest audit output before rerunning the audit or pruning anything.",
            link,
        ));
    }

    if let Some(outcome) = inputs
        .last_repair_outcome
        .filter(|outcome| !outcome.success)
    {
        items.push(needs_attention_item(
            "High",
            "badge-danger",
            "Latest repair failed",
            format!(
                "Finished {} and reported: {}",
                outcome.finished_at, outcome.message
            ),
            "Open Dead Links, confirm the source is really gone, then retry repair only after the replacement path is visible again.",
            Some(activity_link("/links/dead", "Open Dead Links")),
        ));
    }

    if stats.dead_links > 0 {
        items.push(needs_attention_item(
            "High",
            "badge-warning",
            "Dead links need cleanup or repair",
            format!(
                "{} dead link(s) are currently tracked and can surface stale media paths to users.",
                stats.dead_links
            ),
            "Review Dead Links, then decide whether the safest next move is repair or cleanup before the next media refresh.",
            Some(activity_link("/links/dead", "Review Dead Links")),
        ));
    }

    if let Some(guard) = inputs
        .streaming_guard
        .filter(|guard| guard.error_message.is_none() && guard.active_streams > 0)
        .filter(|_| stats.dead_links > 0 || queue.completed_unlinked > 0)
    {
        items.push(needs_attention_item(
            "Medium",
            "badge-warning",
            "Playback guard is deferring safe mutations",
            format!(
                "{} active stream(s) are currently protected, so repair or cleanup apply may intentionally wait on overlapping paths.",
                guard.active_streams
            ),
            "Open Status or the dashboard playback-protection panel and confirm the protected paths before retrying repair or cleanup apply.",
            Some(activity_link("/status", "Open Status")),
        ));
    }

    if let Some(schedule) = inputs
        .daemon_schedule
        .filter(|schedule| schedule.status_label == "Due")
    {
        items.push(needs_attention_item(
            "Medium",
            "badge-warning",
            "Daemon scan cadence looks overdue",
            format!(
                "Latest recorded scan is behind the configured cadence. {}",
                schedule.next_due_label
            ),
            "Open Status and verify the daemon/service is actually running before you assume scans are still happening on schedule.",
            Some(activity_link("/status", "Open Status")),
        ));
    }

    if queue.blocked > 0 || queue.failed > 0 {
        items.push(needs_attention_item(
            "High",
            "badge-warning",
            "Auto-acquire queue is blocked",
            format!(
                "{} blocked and {} failed job(s) need operator review before the backlog silently grows.",
                queue.blocked, queue.failed
            ),
            "Open Status to confirm queue pressure and provider health, then rerun a targeted scan if the backlog should move again.",
            Some(activity_link("/status", "Open Status")),
        ));
    } else if queue.completed_unlinked > 0 {
        items.push(needs_attention_item(
            "High",
            "badge-warning",
            "Auto-acquire finished without relinking",
            format!(
                "{} completed job(s) still need a fresh link before they become real library wins.",
                queue.completed_unlinked
            ),
            "Open Status and inspect the latest queue rows before rerunning another scan, so you can see whether relink checks, source visibility, or ownership rules are holding them back.",
            Some(activity_link("/status", "Open Status")),
        ));
    } else if queue.no_result > 0 {
        items.push(needs_attention_item(
            "Medium",
            "badge-info",
            "Auto-acquire is finding no results",
            format!(
                "{} job(s) ended with no result. Check matcher scope, provider health, or query quality.",
                queue.no_result
            ),
            "Open Status and Scan, then compare search scope, provider availability, and query quality before assuming acquisition is broken.",
            Some(activity_link("/status", "Open Status")),
        ));
    }

    if deferred_refresh.pending_targets > 0 {
        let server_label = deferred_refresh
            .servers
            .first()
            .map(|server| server.server.clone())
            .unwrap_or_else(|| "media servers".to_string());
        items.push(needs_attention_item(
            "Medium",
            "badge-warning",
            "Media refresh backlog is accumulating",
            format!(
                "{} deferred target(s) are still queued. {} is already waiting on refresh work.",
                deferred_refresh.pending_targets, server_label
            ),
            "Open Status and let the current media-server backlog clear before assuming fresh links are already visible to users.",
            Some(activity_link("/status", "Open Status")),
        ));
    }

    if let Some(run) = inputs.latest_run {
        if run.plex_refresh_capped_batches > 0 || run.plex_refresh_failed_batches > 0 {
            items.push(needs_attention_item(
                "Medium",
                "badge-info",
                "Latest run hit refresh guardrails",
                format!(
                    "{} capped batch(es) and {} failed batch(es) were recorded on the latest run.",
                    run.plex_refresh_capped_batches, run.plex_refresh_failed_batches
                ),
                "Open the latest run detail and inspect refresh caps, skips, or failures before you rerun another large scan.",
                Some(activity_link(
                    format!("/scan/history/{}", run.id),
                    "Open Latest Run",
                )),
            ));
        }
    }

    DashboardNeedsAttentionView { items }
}

fn build_dashboard_needs_attention_view(
    stats: &DashboardStats,
    queue: &QueueOverview,
    deferred_refresh: &DeferredRefreshSummaryView,
    inputs: &DashboardAttentionInputs<'_>,
) -> DashboardNeedsAttentionView {
    dashboard_needs_attention(stats, queue, deferred_refresh, inputs)
}

async fn dashboard_activity_feed(state: &WebState) -> DashboardActivityFeedView {
    let mut active_items = Vec::new();

    if let Some(job) = state.active_scan().await {
        active_items.push(ActivityFeedItemView {
            kind_label: "Scan".to_string(),
            status_label: "Running".to_string(),
            status_badge_class: "badge-warning",
            scope_label: job.scope_label,
            timestamp_label: "Started".to_string(),
            timestamp: job.started_at,
            context: None,
            message: "Background scan is in progress.".to_string(),
            badges: scan_activity_badges(job.dry_run, job.search_missing),
            link: Some(activity_link("/scan", "Open Scan")),
        });
    }

    if let Some(job) = state.active_cleanup_audit().await {
        active_items.push(ActivityFeedItemView {
            kind_label: "Cleanup Audit".to_string(),
            status_label: "Running".to_string(),
            status_badge_class: "badge-warning",
            scope_label: job.scope_label,
            timestamp_label: "Started".to_string(),
            timestamp: job.started_at,
            context: Some(format!("Libraries: {}", job.libraries_label)),
            message: "Audit is building a new cleanup report.".to_string(),
            badges: Vec::new(),
            link: Some(activity_link("/cleanup", "Open Cleanup")),
        });
    }

    if let Some(job) = state.active_repair().await {
        active_items.push(ActivityFeedItemView {
            kind_label: "Repair".to_string(),
            status_label: "Running".to_string(),
            status_badge_class: "badge-warning",
            scope_label: job.scope_label,
            timestamp_label: "Started".to_string(),
            timestamp: job.started_at,
            context: None,
            message: "Repair is checking tracked dead links.".to_string(),
            badges: Vec::new(),
            link: Some(activity_link("/links/dead", "Open Dead Links")),
        });
    }

    let mut recent_items = Vec::new();

    if let Some(outcome) = scan::visible_last_scan_outcome(state).await {
        recent_items.push(ActivityFeedItemView {
            kind_label: "Scan".to_string(),
            status_label: if outcome.success {
                "Completed".to_string()
            } else {
                "Failed".to_string()
            },
            status_badge_class: if outcome.success {
                "badge-success"
            } else {
                "badge-danger"
            },
            scope_label: outcome.scope_label,
            timestamp_label: "Finished".to_string(),
            timestamp: outcome.finished_at,
            context: None,
            message: outcome.message,
            badges: scan_activity_badges(outcome.dry_run, outcome.search_missing),
            link: Some(activity_link("/scan", "Open Scan")),
        });
    }

    if let Some(outcome) = cleanup::visible_last_cleanup_audit_outcome(state).await {
        recent_items.push(ActivityFeedItemView {
            kind_label: "Cleanup Audit".to_string(),
            status_label: if outcome.success {
                "Completed".to_string()
            } else {
                "Failed".to_string()
            },
            status_badge_class: if outcome.success {
                "badge-success"
            } else {
                "badge-danger"
            },
            scope_label: outcome.scope_label,
            timestamp_label: "Finished".to_string(),
            timestamp: outcome.finished_at,
            context: Some(format!("Libraries: {}", outcome.libraries_label)),
            message: outcome.message,
            badges: Vec::new(),
            link: Some(match outcome.report_path {
                Some(path) => activity_link(format!("/cleanup/prune?report={path}"), "Open Report"),
                None => activity_link("/cleanup", "Open Cleanup"),
            }),
        });
    }

    if let Some(outcome) = state.last_repair_outcome().await {
        recent_items.push(ActivityFeedItemView {
            kind_label: "Repair".to_string(),
            status_label: if outcome.success {
                "Completed".to_string()
            } else {
                "Failed".to_string()
            },
            status_badge_class: if outcome.success {
                "badge-success"
            } else {
                "badge-danger"
            },
            scope_label: outcome.scope_label,
            timestamp_label: "Finished".to_string(),
            timestamp: outcome.finished_at,
            context: Some(format!(
                "Repaired {} | Failed {} | Skipped {} | Stale {}",
                outcome.repaired, outcome.failed, outcome.skipped, outcome.stale
            )),
            message: outcome.message,
            badges: Vec::new(),
            link: Some(activity_link("/links/dead", "Open Dead Links")),
        });
    }

    let (queue_active_items, queue_recent_items) = acquisition_feed_items(state).await;
    active_items.extend(queue_active_items);
    recent_items.extend(queue_recent_items);

    recent_items.sort_by(|left, right| {
        activity_timestamp_rank(&right.timestamp)
            .cmp(&activity_timestamp_rank(&left.timestamp))
            .then_with(|| left.kind_label.cmp(&right.kind_label))
    });

    DashboardActivityFeedView {
        active_items,
        recent_items,
    }
}

// ─── No-config setup page ──────────────────────────────────────────

pub async fn get_noconfig() -> impl IntoResponse {
    use super::templates::NoConfigTemplate;
    let template = NoConfigTemplate;
    template.into_response()
}

fn queue_overview_from_counts(counts: AcquisitionJobCounts) -> QueueOverview {
    counts.into()
}

fn collect_health_checks(state: &WebState) -> BTreeMap<String, HealthCheck> {
    let mut health_checks = BTreeMap::new();

    health_checks.insert(
        "database".to_string(),
        HealthCheck {
            service: "SQLite Database".to_string(),
            status: "healthy".to_string(),
            message: "Connected".to_string(),
        },
    );

    if state.config.has_tmdb() {
        health_checks.insert(
            "tmdb".to_string(),
            HealthCheck {
                service: "TMDB API".to_string(),
                status: "configured".to_string(),
                message: "API key set".to_string(),
            },
        );
    } else {
        health_checks.insert(
            "tmdb".to_string(),
            HealthCheck {
                service: "TMDB API".to_string(),
                status: "missing".to_string(),
                message: "No API key configured".to_string(),
            },
        );
    }

    if state.config.has_tvdb() {
        health_checks.insert(
            "tvdb".to_string(),
            HealthCheck {
                service: "TVDB API".to_string(),
                status: "configured".to_string(),
                message: "API key set".to_string(),
            },
        );
    }

    if state.config.has_realdebrid() {
        health_checks.insert(
            "realdebrid".to_string(),
            HealthCheck {
                service: "Real-Debrid API".to_string(),
                status: "configured".to_string(),
                message: "API token set".to_string(),
            },
        );
    }

    health_checks
}

fn browser_csrf_token(state: &WebState) -> String {
    state.browser_session_token().to_string()
}

fn require_browser_csrf_token(
    state: &WebState,
    submitted_token: &str,
    path: &str,
) -> Option<Response> {
    if !state.browser_mutation_guard_enabled() {
        return None;
    }

    (!super::has_valid_browser_csrf_token(submitted_token, state))
        .then(|| super::invalid_browser_csrf_response(path))
}

/// GET / - Dashboard page
pub async fn get_dashboard(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving dashboard");

    let stats = match state.database.get_web_stats().await {
        Ok(s) => dashboard_stats_from_web_stats(s),
        Err(e) => {
            error!("Failed to get stats: {}", e);
            DashboardStats::default()
        }
    };

    let recent_runs = match state.database.get_scan_history(5).await {
        Ok(history) => scan_run_views(history),
        Err(e) => {
            error!("Failed to get scan history: {}", e);
            Vec::new()
        }
    };
    let latest_run = recent_runs.first().cloned();
    let last_scan_outcome = scan::visible_last_scan_outcome(&state).await;
    let last_cleanup_audit_outcome = cleanup::visible_last_cleanup_audit_outcome(&state).await;
    let last_repair_outcome = state.last_repair_outcome().await.map(Into::into);

    let queue = match state.database.get_acquisition_job_counts().await {
        Ok(counts) => queue_overview_from_counts(counts),
        Err(e) => {
            error!("Failed to get acquisition queue counts: {}", e);
            QueueOverview::default()
        }
    };
    let deferred_refresh = match deferred_refresh_summary(&state.config) {
        Ok(summary) => DeferredRefreshSummaryView::from(summary),
        Err(e) => {
            error!("Failed to read deferred refresh queue: {}", e);
            DeferredRefreshSummaryView::default()
        }
    };
    let streaming_guard = streaming_guard_view(&state).await;
    let recent_queue_jobs = recent_queue_jobs(&state, RECENT_QUEUE_JOB_LIMIT).await;
    let daemon_schedule = daemon_schedule_view(
        &state.config,
        latest_run.as_ref().map(|run| run.started_at.as_str()),
    );
    let attention_inputs = DashboardAttentionInputs {
        latest_run: latest_run.as_ref(),
        last_scan_outcome: last_scan_outcome.as_ref(),
        last_cleanup_outcome: last_cleanup_audit_outcome.as_ref(),
        last_repair_outcome: last_repair_outcome.as_ref(),
        streaming_guard: streaming_guard.as_ref(),
        daemon_schedule: Some(&daemon_schedule),
    };
    let needs_attention =
        build_dashboard_needs_attention_view(&stats, &queue, &deferred_refresh, &attention_inputs);

    let template = DashboardTemplate {
        stats,
        needs_attention,
        activity_feed: dashboard_activity_feed(&state).await,
        daemon_schedule,
        streaming_guard,
        recent_queue_jobs,
        latest_run,
        recent_runs,
        queue,
        deferred_refresh,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /dashboard/activity-feed - HTMX fragment for live operator activity
pub async fn get_dashboard_activity_feed(State(state): State<WebState>) -> impl IntoResponse {
    DashboardActivityFeedTemplate {
        activity_feed: dashboard_activity_feed(&state).await,
    }
}

/// GET /dashboard/needs-attention - HTMX fragment for live operator triage
pub async fn get_dashboard_needs_attention(State(state): State<WebState>) -> impl IntoResponse {
    let stats = match state.database.get_web_stats().await {
        Ok(stats) => dashboard_stats_from_web_stats(stats),
        Err(err) => {
            error!("Failed to get dashboard stats for needs-attention fragment: {}", err);
            DashboardStats::default()
        }
    };

    let latest_run = match state.database.get_scan_history(1).await {
        Ok(runs) => runs.into_iter().next().map(ScanRunView::from_record),
        Err(err) => {
            error!(
                "Failed to get latest scan history for needs-attention fragment: {}",
                err
            );
            None
        }
    };
    let last_scan_outcome = scan::visible_last_scan_outcome(&state).await;
    let last_cleanup_audit_outcome = cleanup::visible_last_cleanup_audit_outcome(&state).await;
    let last_repair_outcome = state.last_repair_outcome().await.map(Into::into);
    let queue = match state.database.get_acquisition_job_counts().await {
        Ok(counts) => queue_overview_from_counts(counts),
        Err(err) => {
            error!(
                "Failed to get acquisition queue counts for needs-attention fragment: {}",
                err
            );
            QueueOverview::default()
        }
    };
    let deferred_refresh = match deferred_refresh_summary(&state.config) {
        Ok(summary) => DeferredRefreshSummaryView::from(summary),
        Err(err) => {
            error!(
                "Failed to read deferred refresh queue for needs-attention fragment: {}",
                err
            );
            DeferredRefreshSummaryView::default()
        }
    };
    let streaming_guard = streaming_guard_view(&state).await;
    let daemon_schedule = daemon_schedule_view(
        &state.config,
        latest_run.as_ref().map(|run| run.started_at.as_str()),
    );
    let attention_inputs = DashboardAttentionInputs {
        latest_run: latest_run.as_ref(),
        last_scan_outcome: last_scan_outcome.as_ref(),
        last_cleanup_outcome: last_cleanup_audit_outcome.as_ref(),
        last_repair_outcome: last_repair_outcome.as_ref(),
        streaming_guard: streaming_guard.as_ref(),
        daemon_schedule: Some(&daemon_schedule),
    };

    DashboardNeedsAttentionTemplate {
        needs_attention: build_dashboard_needs_attention_view(
            &stats,
            &queue,
            &deferred_refresh,
            &attention_inputs,
        ),
    }
}

/// GET /status - Detailed status page
pub async fn get_status(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving status page");

    let stats = match state.database.get_web_stats().await {
        Ok(s) => dashboard_stats_from_web_stats(s),
        Err(e) => {
            error!("Failed to get stats: {}", e);
            DashboardStats::default()
        }
    };

    // Get recent links
    let recent_links = match state.database.get_active_links_limited(50).await {
        Ok(links) => links,
        Err(e) => {
            error!("Failed to get links: {}", e);
            vec![]
        }
    };

    let queue = match state.database.get_acquisition_job_counts().await {
        Ok(counts) => queue_overview_from_counts(counts),
        Err(e) => {
            error!("Failed to get acquisition queue counts: {}", e);
            QueueOverview::default()
        }
    };
    let latest_scan_started_at = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.into_iter().next().map(|run| run.started_at));
    let checks = collect_health_checks(&state);
    let deferred_refresh = deferred_refresh_summary(&state.config)
        .map(DeferredRefreshSummaryView::from)
        .unwrap_or_default();
    let streaming_guard = streaming_guard_view(&state).await;
    let recent_queue_jobs = recent_queue_jobs(&state, RECENT_QUEUE_JOB_LIMIT).await;
    let tracked_dead_links = match state.database.get_dead_links_limited(8).await {
        Ok(links) => links,
        Err(e) => {
            error!("Failed to get tracked dead links: {}", e);
            vec![]
        }
    };

    let template = StatusTemplate {
        stats,
        recent_links,
        tracked_dead_links,
        recent_queue_jobs,
        queue,
        daemon_schedule: daemon_schedule_view(&state.config, latest_scan_started_at.as_deref()),
        checks,
        deferred_refresh,
        streaming_guard,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /health - Compatibility alias for the status page
pub async fn get_health(State(state): State<WebState>) -> impl IntoResponse {
    let _ = state;
    info!("Redirecting /health to /status");
    Redirect::permanent("/status")
}

// ─── Form structs ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BrowserMutationForm {
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct ScanTriggerForm {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub search_missing: bool,
    pub library: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug)]
pub struct CleanupAuditForm {
    pub library: Option<String>,
    pub libraries: Vec<String>,
    pub csrf_token: String,
}

impl CleanupAuditForm {
    /// Parse directly from raw form bytes. `serde_urlencoded` cannot deserialize
    /// repeated HTML checkbox fields into `Vec<String>`, so we bypass it here.
    fn from_form_bytes(body: &[u8]) -> Self {
        let mut csrf_token = String::new();
        let mut library = None;
        let mut libraries = Vec::new();

        for (key, value) in form_urlencoded::parse(body) {
            match key.as_ref() {
                "csrf_token" => csrf_token = value.into_owned(),
                "library" => library = Some(value.into_owned()),
                "libraries" => {
                    let v = value.trim();
                    if !v.is_empty() {
                        libraries.push(v.to_string());
                    }
                }
                _ => {}
            }
        }

        Self {
            library,
            libraries,
            csrf_token,
        }
    }

    fn selected_libraries(&self) -> Vec<String> {
        let mut libraries = self
            .libraries
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(|name| name.to_string())
            .collect::<Vec<_>>();

        if let Some(single) = self.library.as_deref().map(str::trim) {
            if !single.is_empty() && !libraries.iter().any(|name| name == single) {
                libraries.push(single.to_string());
            }
        }

        libraries
    }
}

#[derive(Debug, Deserialize)]
pub struct CleanupPruneForm {
    pub report: String,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct AnimeRemediationPreviewForm {
    pub plex_db: Option<String>,
    pub title: Option<String>,
    pub library: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct AnimeRemediationApplyForm {
    pub report: String,
    pub token: String,
    pub max_delete: Option<usize>,
    pub library: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupCreateForm {
    pub label: String,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupRestoreForm {
    pub backup_file: String,
    #[serde(default)]
    pub csrf_token: String,
}

mod admin;
mod cleanup;
mod scan;

#[cfg(test)]
mod tests;
