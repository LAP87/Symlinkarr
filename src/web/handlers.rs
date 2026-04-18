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

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ScanHistoryQuery {
    pub library: Option<String>,
    pub mode: Option<String>,
    pub search_missing: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DiscoverQuery {
    pub library: Option<String>,
    #[serde(default)]
    pub refresh_cache: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct AnimeRemediationQuery {
    #[serde(default)]
    pub full: bool,
    pub plex_db: Option<String>,
    pub state: Option<String>,
    pub reason: Option<String>,
    pub title: Option<String>,
}

fn dashboard_stats_from_web_stats(stats: crate::db::WebStats) -> DashboardStats {
    DashboardStats {
        active_links: stats.active_links,
        dead_links: stats.dead_links,
        total_scans: stats.total_scans,
        last_scan: stats.last_scan,
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

fn scan_run_views(history: Vec<ScanHistoryRecord>) -> Vec<ScanRunView> {
    history.into_iter().map(ScanRunView::from_record).collect()
}

fn scan_history_filters_from_query(query: &ScanHistoryQuery) -> ScanHistoryFilters {
    ScanHistoryFilters {
        library: query.library.clone().unwrap_or_default(),
        mode: query.mode.clone().unwrap_or_else(|| "any".to_string()),
        search_missing: query
            .search_missing
            .clone()
            .unwrap_or_else(|| "any".to_string()),
        limit: query.limit.unwrap_or(25).clamp(1, 200),
    }
}

fn cleanup_report_summary_from_path(path: &StdPath) -> Option<CleanupReportSummaryView> {
    let report = load_cleanup_report(path)?;
    Some(CleanupReportSummaryView::from_report(
        path.to_path_buf(),
        report,
    ))
}

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

fn canonical_plex_db_path(path: PathBuf) -> Option<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    let canonical = path.canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }

    canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.eq_ignore_ascii_case("db"))?;

    Some(canonical)
}

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return canonical_plex_db_path(PathBuf::from(requested));
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find_map(canonical_plex_db_path)
}

async fn visible_last_scan_outcome(state: &WebState) -> Option<BackgroundScanOutcomeView> {
    let latest_run_started_at = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.into_iter().next().map(|run| run.started_at));

    state
        .last_scan_outcome()
        .await
        .filter(|outcome| should_surface_scan_outcome(outcome, latest_run_started_at.as_deref()))
        .map(Into::into)
}

async fn visible_last_cleanup_audit_outcome(
    state: &WebState,
) -> Option<BackgroundCleanupAuditOutcomeView> {
    let latest_report_created_at = latest_cleanup_report_path(&state.config.backup.path)
        .as_deref()
        .and_then(cleanup_report_summary_from_path)
        .map(|summary| summary.created_at);

    state
        .last_cleanup_audit_outcome()
        .await
        .filter(|outcome| {
            should_surface_cleanup_audit_outcome(outcome, latest_report_created_at.as_deref())
        })
        .map(Into::into)
}

async fn visible_last_repair_outcome(state: &WebState) -> Option<BackgroundRepairOutcomeView> {
    state.last_repair_outcome().await.map(Into::into)
}

async fn filtered_scan_history(
    state: &WebState,
    query: &ScanHistoryQuery,
) -> (ScanHistoryFilters, Vec<ScanRunView>) {
    let filters = scan_history_filters_from_query(query);
    let fetch_limit = (filters.limit * 5).clamp(50, 500);
    let history = match state.database.get_scan_history(fetch_limit).await {
        Ok(history) => history,
        Err(e) => {
            error!("Failed to get scan history: {}", e);
            Vec::new()
        }
    };

    let filtered = history
        .into_iter()
        .filter(|run| {
            if !filters.library.trim().is_empty()
                && run.library_filter.as_deref().unwrap_or_default() != filters.library
            {
                return false;
            }

            match filters.mode.as_str() {
                "dry" if !run.dry_run => return false,
                "live" if run.dry_run => return false,
                _ => {}
            }

            match filters.search_missing.as_str() {
                "only" if !run.search_missing => return false,
                "exclude" if run.search_missing => return false,
                _ => {}
            }

            true
        })
        .take(filters.limit as usize)
        .map(ScanRunView::from_record)
        .collect::<Vec<_>>();

    (filters, filtered)
}

fn skip_event_views(events: Vec<LinkEventHistoryRecord>) -> Vec<SkipEventView> {
    events
        .into_iter()
        .map(|event| {
            let reason = event.note.unwrap_or_else(|| "unknown".to_string());
            SkipEventView {
                event_at: event.event_at,
                action: event.action,
                reason_label: skip_reason_label(&reason),
                reason_group: skip_reason_group_label(&reason),
                reason,
                target_path: event.target_path.display().to_string(),
                source_path: event.source_path.map(|path| path.display().to_string()),
                media_id: event.media_id,
            }
        })
        .collect()
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
        latest_run,
        recent_runs,
        queue,
        deferred_refresh,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
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

/// GET /scan - Scan page
pub async fn get_scan(
    State(state): State<WebState>,
    Query(query): Query<ScanHistoryQuery>,
) -> impl IntoResponse {
    info!("Serving scan page");

    let mut scan_query = query;
    if scan_query.limit.is_none() {
        scan_query.limit = Some(10);
    }
    let (filters, history) = filtered_scan_history(&state, &scan_query).await;
    let latest_run = history.first().cloned();
    let active_scan = state.active_scan().await.map(Into::into);
    let last_scan_outcome = if active_scan.is_none() {
        visible_last_scan_outcome(&state).await
    } else {
        None
    };
    let queue = match state.database.get_acquisition_job_counts().await {
        Ok(counts) => queue_overview_from_counts(counts),
        Err(e) => {
            error!("Failed to get acquisition queue counts: {}", e);
            QueueOverview::default()
        }
    };

    let template = ScanTemplate {
        libraries: state.config.libraries.clone(),
        active_scan,
        last_scan_outcome,
        latest_run,
        history,
        queue,
        filters,
        default_dry_run: state.config.symlink.dry_run,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /scan/trigger - Trigger a scan
pub async fn post_scan_trigger(
    State(state): State<WebState>,
    Form(form): Form<ScanTriggerForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/scan/trigger") {
        return response;
    }

    info!(
        "Triggering scan (dry_run={}, search_missing={})",
        form.dry_run, form.search_missing
    );

    let library_name = form.library.as_deref().filter(|l| !l.is_empty());

    match state
        .start_scan(
            form.dry_run,
            form.search_missing,
            library_name.map(|value| value.to_string()),
        )
        .await
    {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Html(
                ScanResultTemplate {
                    success: true,
                    message: format!(
                        "Scan started in background for {}. Refresh /scan or /scan/history for the finished run.",
                        job.scope_label
                    ),
                    active_scan: Some(job.into()),
                    last_scan_outcome: None,
                    latest_run: None,
                    dry_run: form.dry_run,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
        Err(e) => {
            error!("Scan rejected: {}", e);
            (
                StatusCode::CONFLICT,
                Html(
                    ScanResultTemplate {
                        success: false,
                        message: format!("Scan not started: {}", e),
                        active_scan: state.active_scan().await.map(Into::into),
                        last_scan_outcome: visible_last_scan_outcome(&state).await,
                        latest_run: None,
                        dry_run: form.dry_run,
                    }
                    .render()
                    .unwrap_or_else(|e| e.to_string()),
                ),
            )
                .into_response()
        }
    }
}

/// GET /scan/history - Scan history
pub async fn get_scan_history(
    State(state): State<WebState>,
    Query(query): Query<ScanHistoryQuery>,
) -> impl IntoResponse {
    let (filters, history) = filtered_scan_history(&state, &query).await;

    let template = ScanHistoryTemplate {
        libraries: state.config.libraries.clone(),
        history,
        filters,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /scan/history/:id - Scan run detail
pub async fn get_scan_run_detail(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match state.database.get_scan_run(id).await {
        Ok(Some(run)) => {
            let skip_events = match run.run_token.as_deref() {
                Some(token) => match state
                    .database
                    .get_skip_link_events_for_run_token(token, 25)
                    .await
                {
                    Ok(events) => skip_event_views(events),
                    Err(e) => {
                        error!("Failed to load skip events for scan run {}: {}", id, e);
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            let template = ScanRunDetailTemplate {
                run: ScanRunView::from_record(run),
                skip_events,
            };
            Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, format!("Scan run {} not found", id)).into_response(),
        Err(e) => {
            error!("Failed to load scan run {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load scan run {}: {}", id, e),
            )
                .into_response()
        }
    }
}

/// GET /cleanup - Cleanup page
pub async fn get_cleanup(State(state): State<WebState>) -> impl IntoResponse {
    let last_report = latest_cleanup_report_path(&state.config.backup.path);

    let last_report_summary = last_report
        .as_deref()
        .and_then(cleanup_report_summary_from_path);
    let active_cleanup_audit = state.active_cleanup_audit().await.map(Into::into);
    let last_cleanup_audit_outcome = if active_cleanup_audit.is_none() {
        visible_last_cleanup_audit_outcome(&state).await
    } else {
        None
    };

    let template = CleanupTemplate {
        libraries: state.config.libraries.clone(),
        active_cleanup_audit,
        last_cleanup_audit_outcome,
        last_report: last_report_summary,
        last_report_path: last_report,
        csrf_token: browser_csrf_token(&state),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /cleanup/anime-remediation - Read-only anime remediation backlog
pub async fn get_cleanup_anime_remediation(
    State(state): State<WebState>,
    Query(query): Query<AnimeRemediationQuery>,
) -> impl IntoResponse {
    let filters = match AnimeRemediationGroupFilters::parse(
        query.state.as_deref(),
        query.reason.as_deref(),
        query.title.as_deref(),
    ) {
        Ok(filters) => filters,
        Err(err) => {
            return Html(
                AnimeRemediationTemplate {
                    summary: None,
                    groups: vec![],
                    error_message: Some(format!("Invalid anime remediation filters: {}", err)),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
        }
    };

    let Some(plex_db_path) = resolve_plex_db_path(query.plex_db.as_deref()) else {
        return Html(
            AnimeRemediationTemplate {
                summary: None,
                groups: vec![],
                error_message: Some(
                    "Plex DB path is required or must exist at a standard local path".to_string(),
                ),
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    };

    match build_anime_remediation_report(&state.config, &state.database, &plex_db_path, query.full)
        .await
    {
        Ok(Some(report)) => {
            let assessed_groups = assess_anime_remediation_groups(&report.groups);

            match assessed_groups {
                Ok(assessed_groups) => {
                    let filtered_groups =
                        filter_anime_remediation_groups(assessed_groups.clone(), &filters);
                    let eligible_groups = filtered_groups
                        .iter()
                        .filter(|group| group.eligible)
                        .count();
                    let blocked_groups = filtered_groups.len().saturating_sub(eligible_groups);
                    let blocked_reason_summary =
                        summarize_anime_remediation_blocked_reasons(&filtered_groups)
                            .into_iter()
                            .map(Into::into)
                            .collect();
                    let available_blocked_reasons = anime_remediation_block_reason_catalog()
                        .into_iter()
                        .map(Into::into)
                        .collect();

                    Html(
                        AnimeRemediationTemplate {
                            summary: Some(AnimeRemediationSummaryView {
                                generated_at: report.generated_at,
                                plex_db_path: plex_db_path.display().to_string(),
                                full: query.full,
                                filesystem_mixed_root_groups: report.filesystem_mixed_root_groups,
                                plex_duplicate_show_groups: report.plex_duplicate_show_groups,
                                plex_hama_anidb_tvdb_groups: report.plex_hama_anidb_tvdb_groups,
                                correlated_hama_split_groups: report.correlated_hama_split_groups,
                                remediation_groups: report.remediation_groups,
                                returned_groups: report.returned_groups,
                                visible_groups: filtered_groups.len(),
                                eligible_groups,
                                blocked_groups,
                                state_filter: filters.visibility.as_str().to_string(),
                                reason_filter: filters
                                    .block_code
                                    .map(|code| code.as_str().to_string())
                                    .unwrap_or_default(),
                                title_filter: filters.title_contains.clone().unwrap_or_default(),
                                blocked_reason_summary,
                                available_blocked_reasons,
                            }),
                            groups: filtered_groups
                                .into_iter()
                                .map(AnimeRemediationGroupView::from_plan_group)
                                .collect(),
                            error_message: None,
                            csrf_token: browser_csrf_token(&state),
                        }
                        .render()
                        .unwrap_or_else(|e| e.to_string()),
                    )
                }
                Err(err) => {
                    error!("Failed to assess anime remediation backlog: {}", err);
                    Html(
                        AnimeRemediationTemplate {
                            summary: None,
                            groups: vec![],
                            error_message: Some(format!(
                                "Failed to assess anime remediation backlog: {}",
                                err
                            )),
                            csrf_token: browser_csrf_token(&state),
                        }
                        .render()
                        .unwrap_or_else(|e| e.to_string()),
                    )
                }
            }
        }
        Ok(None) => Html(
            AnimeRemediationTemplate {
                summary: None,
                groups: vec![],
                error_message: Some(
                    "No anime libraries are configured for remediation reporting".to_string(),
                ),
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        ),
        Err(err) => {
            error!("Failed to build anime remediation report: {}", err);
            Html(
                AnimeRemediationTemplate {
                    summary: None,
                    groups: vec![],
                    error_message: Some(format!(
                        "Failed to build anime remediation report: {}",
                        err
                    )),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
        }
    }
}

/// POST /cleanup/anime-remediation/preview - Build a guarded remediation plan
pub async fn post_cleanup_anime_remediation_preview(
    State(state): State<WebState>,
    Form(form): Form<AnimeRemediationPreviewForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(
        &state,
        &form.csrf_token,
        "/cleanup/anime-remediation/preview",
    ) {
        return response;
    }

    let Some(plex_db_path) = resolve_plex_db_path(form.plex_db.as_deref()) else {
        return Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: "Anime remediation preview failed: Plex DB path is required or must exist at a standard local path".to_string(),
                preview: None,
                apply: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response();
    };

    match preview_anime_remediation_plan(
        &state.config,
        &state.database,
        form.library.as_deref(),
        &plex_db_path,
        form.title.as_deref(),
        None,
    )
    .await
    {
        Ok((plan, report_path)) => Html(
            AnimeRemediationResultTemplate {
                success: true,
                message: format!(
                    "Anime remediation preview saved. Review {} before applying.",
                    report_path.display()
                ),
                preview: Some(AnimeRemediationPreviewResultView {
                    report_path,
                    plex_db_path: plan.plex_db_path.display().to_string(),
                    title_filter: plan.title_filter.unwrap_or_default(),
                    total_groups: plan.total_groups,
                    eligible_groups: plan.eligible_groups,
                    blocked_groups: plan.blocked_groups,
                    cleanup_candidates: plan.cleanup_candidates,
                    confirmation_token: plan.confirmation_token,
                    blocked_reason_summary: plan
                        .blocked_reason_summary
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    groups: plan
                        .groups
                        .into_iter()
                        .map(AnimeRemediationGroupView::from_plan_group)
                        .collect(),
                }),
                apply: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
        Err(err) => Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: format!("Anime remediation preview failed: {}", err),
                preview: None,
                apply: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
    }
}

/// POST /cleanup/anime-remediation/apply - Apply a saved guarded remediation plan
pub async fn post_cleanup_anime_remediation_apply(
    State(state): State<WebState>,
    Form(form): Form<AnimeRemediationApplyForm>,
) -> impl IntoResponse {
    if let Some(response) =
        require_browser_csrf_token(&state, &form.csrf_token, "/cleanup/anime-remediation/apply")
    {
        return response;
    }

    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &form.report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                AnimeRemediationResultTemplate {
                    success: false,
                    message: format!("Anime remediation apply failed: {}", err),
                    preview: None,
                    apply: None,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    match apply_anime_remediation_plan_with_refresh(
        &state.config,
        &state.database,
        form.library.as_deref(),
        &report_path,
        Some(form.token.trim()),
        form.max_delete,
        true,
    )
    .await
    {
        Ok((plan, outcome, safety_snapshot, invalidation)) => Html(
            AnimeRemediationResultTemplate {
                success: true,
                message: "Anime remediation applied.".to_string(),
                preview: None,
                apply: Some(AnimeRemediationApplyResultView {
                    report_path,
                    total_groups: plan.total_groups,
                    eligible_groups: plan.eligible_groups,
                    blocked_groups: plan.blocked_groups,
                    candidates: outcome.candidates,
                    quarantined: outcome.quarantined,
                    removed: outcome.removed,
                    skipped: outcome.skipped,
                    safety_snapshot,
                    media_server_invalidation_summary: invalidation.summary_suffix(),
                }),
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
        Err(err) => Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: format!("Anime remediation apply failed: {}", err),
                preview: None,
                apply: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
    }
}

/// POST /cleanup/audit - Run audit
pub async fn post_cleanup_audit(State(state): State<WebState>, body: Bytes) -> impl IntoResponse {
    let form = CleanupAuditForm::from_form_bytes(&body);
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/cleanup/audit") {
        return response;
    }

    let selected_libraries = form.selected_libraries();
    let scope = infer_cleanup_scope(&state.config, &selected_libraries);
    info!(
        "Running cleanup audit (scope={:?}, libraries={:?})",
        scope, selected_libraries
    );

    match state.start_cleanup_audit(scope, selected_libraries.clone()).await
    {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Html(
                CleanupResultTemplate {
                    success: true,
                    message: format!(
                        "Cleanup audit started in background for {} across {}. Refresh /cleanup for the finished report.",
                        job.scope_label, job.libraries_label
                    ),
                    active_cleanup_audit: Some(job.into()),
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
        Err(e) => {
            error!("Cleanup audit rejected: {}", e);
            (
                StatusCode::CONFLICT,
                Html(
                    CleanupResultTemplate {
                        success: false,
                        message: format!("Cleanup audit not started: {}", e),
                        active_cleanup_audit: state.active_cleanup_audit().await.map(Into::into),
                        last_cleanup_audit_outcome: visible_last_cleanup_audit_outcome(&state)
                            .await,
                        report_path: None,
                        report_summary: None,
                    }
                    .render()
                    .unwrap_or_else(|e| e.to_string()),
                ),
            )
                .into_response()
        }
    }
}

/// GET /cleanup/prune - Prune preview
pub async fn get_cleanup_prune(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(raw_report) = params.get("report").map(|p| p.as_str()) else {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                actionable_candidates: 0,
                critical: 0,
                high: 0,
                warning: 0,
                blocked_candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                reason_counts: vec![],
                blocked_reason_summary: vec![],
                legacy_anime_root_groups: vec![],
                report_path: None,
                confirmation_token: None,
                already_applied: false,
                error_message: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    };

    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, raw_report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    actionable_candidates: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    reason_counts: vec![],
                    blocked_reason_summary: vec![],
                    legacy_anime_root_groups: vec![],
                    report_path: None,
                    confirmation_token: None,
                    already_applied: false,
                    error_message: Some(err.to_string()),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    if !report_path.exists() {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                actionable_candidates: 0,
                critical: 0,
                high: 0,
                warning: 0,
                blocked_candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                reason_counts: vec![],
                blocked_reason_summary: vec![],
                legacy_anime_root_groups: vec![],
                report_path: None,
                confirmation_token: None,
                already_applied: false,
                error_message: Some(format!(
                    "Cleanup report not found: {}",
                    report_path.display()
                )),
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Parse the JSON report to show actual preview data
    let json = match std::fs::read_to_string(&report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    actionable_candidates: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    reason_counts: vec![],
                    blocked_reason_summary: vec![],
                    legacy_anime_root_groups: vec![],
                    report_path: None,
                    confirmation_token: None,
                    already_applied: false,
                    error_message: Some(format!("Failed to read report: {}", e)),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let mut report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    actionable_candidates: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    reason_counts: vec![],
                    blocked_reason_summary: vec![],
                    legacy_anime_root_groups: vec![],
                    report_path: None,
                    confirmation_token: None,
                    already_applied: false,
                    error_message: Some(format!("Failed to parse report: {}", e)),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let prune_plan =
        match cleanup_audit::hydrate_report_db_tracked_flags(&state.database, &mut report).await {
            Ok(()) => Some(cleanup_audit::build_prune_plan(
                &report,
                state.config.cleanup.prune.quarantine_foreign,
                false,
            )),
            Err(e) => {
                error!("Failed to hydrate cleanup report DB state: {}", e);
                None
            }
        };

    let template = PrunePreviewTemplate {
        findings: report
            .findings
            .clone()
            .into_iter()
            .map(|finding| {
                let action = prune_plan
                    .as_ref()
                    .map(|plan| plan.action_for_path(&finding.symlink_path))
                    .unwrap_or(crate::cleanup_audit::PrunePathAction::ObserveOnly);
                PruneFindingView::from_finding(finding, action)
            })
            .collect(),
        total: report.findings.len(),
        actionable_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.candidate_paths.len())
            .unwrap_or(0),
        critical: report.summary.critical,
        high: report.summary.high,
        warning: report.summary.warning,
        blocked_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.blocked_candidates)
            .unwrap_or(0),
        managed_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.managed_candidates)
            .unwrap_or(0),
        foreign_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.foreign_candidates)
            .unwrap_or(0),
        reason_counts: prune_plan
            .as_ref()
            .map(|plan| plan.reason_counts.clone())
            .unwrap_or_default(),
        blocked_reason_summary: prune_plan
            .as_ref()
            .map(|plan| plan.blocked_reason_summary.clone())
            .unwrap_or_default(),
        legacy_anime_root_groups: prune_plan
            .as_ref()
            .map(|plan| plan.legacy_anime_root_groups.clone())
            .unwrap_or_default(),
        report_path: Some(report_path.to_path_buf()),
        confirmation_token: prune_plan.map(|plan| plan.confirmation_token),
        already_applied: report.applied_at.is_some(),
        error_message: None,
        csrf_token: browser_csrf_token(&state),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /cleanup/prune - Apply prune
pub async fn post_cleanup_prune(
    State(state): State<WebState>,
    Form(form): Form<CleanupPruneForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/cleanup/prune") {
        return response;
    }

    info!("Applying prune from web UI");

    // Validate inputs
    if form.report.is_empty() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: "Report path is required".to_string(),
                active_cleanup_audit: None,
                last_cleanup_audit_outcome: None,
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response();
    }

    // Read the report
    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &form.report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: err.to_string(),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };
    if !report_path.exists() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: format!("Report not found: {}", report_path.display()),
                active_cleanup_audit: None,
                last_cleanup_audit_outcome: None,
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response();
    }

    let json = match std::fs::read_to_string(&report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to read report: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let _report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to parse report: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let selected = match selected_libraries(state.config.as_ref(), None) {
        Ok(selected) => selected,
        Err(e) => {
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let (outcome, invalidation) = match apply_cleanup_prune_with_refresh(
        &state.config,
        &state.database,
        CleanupPruneApplyArgs {
            libraries: &selected,
            report_path: &report_path,
            include_legacy_anime_roots: false,
            max_delete: None,
            confirm_token: None,
            emit_text: true,
        },
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("Prune operation failed: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let mut message = if outcome.removed > 0 {
        format!(
            "✅ Prune completed successfully: {} symlinks removed, {} skipped",
            outcome.removed, outcome.skipped
        )
    } else {
        "⚠️ Prune completed but no symlinks were removed".to_string()
    };
    if let Some(suffix) = invalidation.summary_suffix() {
        message.push_str(&format!(" ({})", suffix));
    }

    let template = CleanupResultTemplate {
        success: true,
        message,
        active_cleanup_audit: None,
        last_cleanup_audit_outcome: None,
        report_summary: cleanup_report_summary_from_path(&report_path),
        report_path: Some(report_path.to_path_buf()),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /links - Links list
pub async fn get_links(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let filter = params.get("filter").map(|f| f.as_str());
    let limit = clamp_link_list_limit(params.get("limit").and_then(|l| l.parse().ok()));

    let links = match filter {
        Some("dead") => state
            .database
            .get_dead_links_limited(limit)
            .await
            .unwrap_or_default(),
        Some("active") => state
            .database
            .get_active_links_limited(limit)
            .await
            .unwrap_or_default(),
        _ => state
            .database
            .get_active_links_limited(limit)
            .await
            .unwrap_or_default(),
    };

    let template = LinksTemplate {
        links,
        filter: filter.unwrap_or("active").to_string(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /links/dead - Dead links
pub async fn get_dead_links(State(state): State<WebState>) -> impl IntoResponse {
    let links = match state.database.get_dead_links().await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to get dead links: {}", e);
            vec![]
        }
    };

    let active_repair = state.active_repair().await.map(Into::into);
    let last_repair_outcome = if active_repair.is_none() {
        visible_last_repair_outcome(&state).await
    } else {
        None
    };

    let template = DeadLinksTemplate {
        links,
        active_repair,
        last_repair_outcome,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /links/repair - Repair dead links
pub async fn post_repair(
    State(state): State<WebState>,
    Form(form): Form<BrowserMutationForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/links/repair") {
        return response;
    }

    info!("Starting background auto repair");

    match state.start_repair().await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Html(
                RepairResultTemplate {
                    success: true,
                    message: format!(
                        "Repair started in background for {}. Refresh /links/dead for the finished outcome.",
                        job.scope_label
                    ),
                    repaired: 0,
                    failed: 0,
                    active_repair: Some(job.into()),
                    last_repair_outcome: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
        Err(err) => {
            let message = err.to_string();
            let active_repair = state.active_repair().await.map(Into::into);
            (
                StatusCode::CONFLICT,
                Html(
                    RepairResultTemplate {
                        success: false,
                        message: format!("Repair not started: {}", message),
                        repaired: 0,
                        failed: 0,
                        active_repair,
                        last_repair_outcome: visible_last_repair_outcome(&state).await,
                    }
                    .render()
                    .unwrap_or_else(|e| e.to_string()),
                ),
            )
                .into_response()
        }
    }
}

/// GET /config - Config page
pub async fn get_config(State(state): State<WebState>) -> impl IntoResponse {
    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: None,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /config/validate - Validate config
pub async fn post_config_validate(
    State(state): State<WebState>,
    Form(form): Form<BrowserMutationForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/config/validate")
    {
        return response;
    }

    let report = validate_config_report(&state.config).await;
    let result = Some(ValidationResult {
        valid: report.errors.is_empty(),
        errors: report.errors,
        warnings: report.warnings,
    });

    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: result,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /doctor - Doctor page
pub async fn get_doctor(State(state): State<WebState>) -> impl IntoResponse {
    let checks = collect_doctor_checks(&state.config, &state.database, DoctorCheckMode::ReadOnly)
        .await
        .into_iter()
        .map(|check| DoctorCheck {
            check: check.name,
            passed: check.ok,
            message: check.detail,
        })
        .collect::<Vec<_>>();

    let all_passed = checks.iter().all(|c| c.passed);

    let template = DoctorTemplate { checks, all_passed };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /discover - Discover page
pub async fn get_discover(
    State(state): State<WebState>,
    Query(query): Query<DiscoverQuery>,
) -> impl IntoResponse {
    let template = DiscoverTemplate {
        libraries: state.config.libraries.clone(),
        selected_library: query.library.unwrap_or_default(),
        refresh_cache: query.refresh_cache,
    };
    (
        StatusCode::OK,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
}

/// GET /discover/content - Discover content fragment
pub async fn get_discover_content(
    State(state): State<WebState>,
    Query(query): Query<DiscoverQuery>,
) -> impl IntoResponse {
    match load_discovery_snapshot(
        &state.config,
        &state.database,
        query.library.as_deref(),
        query.refresh_cache,
    )
    .await
    {
        Ok(snapshot) => {
            let template = DiscoverContentTemplate {
                discover_summary: snapshot.summary,
                folder_plans: snapshot.folders,
                discovered_items: snapshot.items,
                status_message: snapshot.status_message.or_else(|| {
                    (!query.refresh_cache).then(|| {
                        "Showing cached or on-disk discover results only. Enable refresh when you want a slower live cache sync first."
                            .to_string()
                    })
                }),
            };
            (
                StatusCode::OK,
                Html(template.render().unwrap_or_else(|e| e.to_string())),
            )
        }
        Err(err) => {
            let message = err.to_string();
            let template = DiscoverContentTemplate {
                discover_summary: DiscoverSummary::default(),
                folder_plans: vec![],
                discovered_items: vec![],
                status_message: Some(if message.contains("Unknown library filter") {
                    format!("Invalid library filter: {}", message)
                } else {
                    format!("Discover failed: {}", message)
                }),
            };
            (
                if message.contains("Unknown library filter") {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                Html(template.render().unwrap_or_else(|e| e.to_string())),
            )
        }
    }
}

/// GET /backup - Backup page
pub async fn get_backup(State(state): State<WebState>) -> impl IntoResponse {
    let backup_manager = BackupManager::new(&state.config.backup);
    let current_active_links = state
        .database
        .get_web_stats()
        .await
        .map(|stats| stats.active_links.max(0) as usize)
        .unwrap_or(0);
    let backups = backup_manager
        .list()
        .unwrap_or_default()
        .into_iter()
        .map(|backup| {
            let (kind_label, kind_badge_class) = match &backup.backup_type {
                crate::backup::BackupType::Scheduled => {
                    ("Symlinkarr Backup".to_string(), "badge-info")
                }
                crate::backup::BackupType::Safety { .. } => {
                    ("Restore Point".to_string(), "badge-warning")
                }
            };
            let link_delta_label = if backup.symlink_count == current_active_links {
                "Matches current tracked links".to_string()
            } else if backup.symlink_count > current_active_links {
                format!(
                    "{} more than current",
                    backup.symlink_count - current_active_links
                )
            } else {
                format!(
                    "{} fewer than current",
                    current_active_links - backup.symlink_count
                )
            };

            BackupInfo {
                filename: backup.filename,
                label: backup.label,
                kind_label,
                kind_badge_class,
                created_at: format_backup_timestamp(backup.timestamp),
                age_label: format_backup_age(backup.timestamp),
                recorded_links: backup.symlink_count,
                link_delta_label,
                manifest_size_bytes: backup.file_size,
                database_snapshot_size_bytes: backup
                    .database_snapshot
                    .map(|snapshot| snapshot.size_bytes),
                config_snapshot_present: backup
                    .app_state
                    .as_ref()
                    .and_then(|state| state.config_snapshot.as_ref())
                    .is_some(),
                secret_snapshot_count: backup
                    .app_state
                    .as_ref()
                    .map(|state| state.secret_snapshots.len())
                    .unwrap_or(0),
            }
        })
        .collect();

    let template = BackupTemplate {
        backups,
        backup_dir: state.config.backup.path.clone(),
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /backup/create - Create backup
pub async fn post_backup_create(
    State(state): State<WebState>,
    Form(form): Form<BackupCreateForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/backup/create") {
        return response;
    }

    info!("Creating backup (label={})", form.label);

    let backup_manager = BackupManager::new(&state.config.backup);

    let result = match backup_manager
        .create_backup(&state.config, &state.database, &form.label)
        .await
    {
        Ok(path) => Some(path),
        Err(e) => {
            error!("Backup failed: {}", e);
            None
        }
    };

    let created_summary = result.as_ref().and_then(|path| {
        backup_manager
            .list()
            .ok()
            .and_then(|items| items.into_iter().find(|backup| &backup.path == path))
    });
    let database_snapshot_path = result.as_ref().map(|path| path.with_extension("sqlite3"));
    let template = BackupResultTemplate {
        success: result.is_some(),
        message: if result.is_some() {
            "Backup created successfully".to_string()
        } else {
            "Backup failed".to_string()
        },
        backup_path: result,
        database_snapshot_path,
        config_snapshot_path: created_summary
            .as_ref()
            .and_then(|backup| backup.app_state.as_ref())
            .and_then(|state| state.config_snapshot.as_ref())
            .map(|file| state.config.backup.path.join(&file.filename)),
        secret_snapshot_count: created_summary
            .as_ref()
            .and_then(|backup| backup.app_state.as_ref())
            .map(|state| state.secret_snapshots.len())
            .unwrap_or(0),
        app_state_restore_summary: None,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /backup/restore - Restore backup
pub async fn post_backup_restore(
    State(state): State<WebState>,
    Form(form): Form<BackupRestoreForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/backup/restore")
    {
        return response;
    }

    info!("Restoring backup: {}", form.backup_file);

    let backup_manager = BackupManager::new(&state.config.backup);
    let backup_path = match backup_manager.resolve_restore_path(StdPath::new(&form.backup_file)) {
        Ok(path) => path,
        Err(e) => {
            let template = BackupResultTemplate {
                success: false,
                message: format!("Restore failed: {}", e),
                backup_path: None,
                database_snapshot_path: None,
                config_snapshot_path: None,
                secret_snapshot_count: 0,
                app_state_restore_summary: None,
            };
            return Html(
                template
                    .render()
                    .unwrap_or_else(|render_err| render_err.to_string()),
            )
            .into_response();
        }
    };

    if let Err(e) = ensure_backup_restore_runtime_healthy(&state.config, "backup restore").await {
        let template = BackupResultTemplate {
            success: false,
            message: format!("Restore failed: {}", e),
            backup_path: Some(backup_path),
            database_snapshot_path: None,
            config_snapshot_path: None,
            secret_snapshot_count: 0,
            app_state_restore_summary: None,
        };
        return Html(
            template
                .render()
                .unwrap_or_else(|render_err| render_err.to_string()),
        )
        .into_response();
    }

    let allowed_roots: Vec<PathBuf> = state
        .config
        .libraries
        .iter()
        .map(|l| l.path.clone())
        .collect();
    let allowed_source_roots: Vec<PathBuf> = state
        .config
        .sources
        .iter()
        .map(|s| s.path.clone())
        .collect();
    let result = backup_manager
        .restore(
            &state.database,
            &backup_path,
            false,
            &allowed_roots,
            &allowed_source_roots,
            true,
        )
        .await;
    let app_state_restore_result = match &result {
        Ok(_) => Some(backup_manager.restore_app_state(&state.config, &backup_path, false)),
        Err(_) => None,
    };

    let (success, message, app_state_restore_summary) = match result {
        Ok((restored, skipped, errors)) => {
            let summary = match app_state_restore_result {
                Some(Ok(summary)) => Some(summary),
                Some(Err(err)) => {
                    return Html(
                        BackupResultTemplate {
                            success: false,
                            message: format!(
                                "Links were restored, but app state restore failed: {}",
                                err
                            ),
                            backup_path: Some(backup_path),
                            database_snapshot_path: None,
                            config_snapshot_path: None,
                            secret_snapshot_count: 0,
                            app_state_restore_summary: None,
                        }
                        .render()
                        .unwrap_or_else(|render_err| render_err.to_string()),
                    )
                    .into_response();
                }
                None => None,
            };
            let app_state_message = summary
                .as_ref()
                .filter(|summary| summary.present)
                .map(|summary| {
                    format!(
                        " Links restored: {restored}, skipped: {skipped}, errors: {errors}. App state: config {}, secrets restored {}, secrets skipped {}.",
                        if summary.config_restored {
                            "restored"
                        } else if summary.config_included {
                            "skipped"
                        } else {
                            "not included"
                        },
                        summary.secrets_restored,
                        summary.secrets_skipped
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        " Links restored: {restored}, skipped: {skipped}, errors: {errors}."
                    )
                });
            (
                true,
                format!("Backup restored successfully.{app_state_message}"),
                summary,
            )
        }
        Err(e) => (false, format!("Restore failed: {}", e), None),
    };

    let template = BackupResultTemplate {
        success,
        message,
        backup_path: Some(backup_path),
        database_snapshot_path: None,
        config_snapshot_path: None,
        secret_snapshot_count: 0,
        app_state_restore_summary,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
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

#[cfg(test)]
mod tests;
