//! HTTP handlers for the web UI

use askama::Template;
use axum::{
    body::Bytes,
    extract::{Form, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
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
use crate::db::{AcquisitionJobCounts, LinkEventHistoryRecord, ScanHistoryRecord};
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

    let template = DashboardTemplate {
        stats,
        activity_feed: dashboard_activity_feed(&state).await,
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
    let checks = collect_health_checks(&state);
    let deferred_refresh = deferred_refresh_summary(&state.config)
        .map(DeferredRefreshSummaryView::from)
        .unwrap_or_default();
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
        queue,
        checks,
        deferred_refresh,
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
