//! HTTP handlers for the web UI

use askama::Template;
use axum::{
    extract::{Form, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Component, Path as StdPath, PathBuf};
use tracing::{error, info};

use crate::backup::BackupManager;
use crate::cleanup_audit::{self, CleanupScope};
use crate::commands::backup::ensure_backup_restore_runtime_healthy;
use crate::commands::cleanup::{
    apply_anime_remediation_plan_with_refresh, apply_cleanup_prune_with_refresh,
    assess_anime_remediation_group, preview_anime_remediation_plan,
    summarize_anime_remediation_blocked_reasons, CleanupPruneApplyArgs,
};
use crate::commands::config::validate_config_report;
use crate::commands::discover::load_discovery_snapshot;
use crate::commands::doctor::{collect_doctor_checks, DoctorCheckMode};
use crate::commands::report::build_anime_remediation_report;
use crate::commands::selected_libraries;
use crate::db::{AcquisitionJobCounts, LinkEventHistoryRecord, ScanHistoryRecord};
use crate::media_servers::deferred_refresh_summary;

use super::templates::*;
use super::{
    latest_cleanup_report_path, load_cleanup_report, should_surface_cleanup_audit_outcome,
    should_surface_scan_outcome, WebState,
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
}

fn dashboard_stats_from_web_stats(stats: crate::db::WebStats) -> DashboardStats {
    DashboardStats {
        active_links: stats.active_links,
        dead_links: stats.dead_links,
        total_scans: stats.total_scans,
        last_scan: stats.last_scan,
    }
}

fn queue_overview_from_counts(counts: AcquisitionJobCounts) -> QueueOverview {
    counts.into()
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

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(requested);
        return path.exists().then_some(path);
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
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

fn resolve_backup_restore_path(backup_dir: &StdPath, backup_file: &str) -> anyhow::Result<PathBuf> {
    let trimmed = backup_file.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Backup file must not be empty");
    }

    let requested = StdPath::new(trimmed);
    if requested.is_absolute() {
        anyhow::bail!("Backup restore only accepts files inside the configured backup directory");
    }

    if requested.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("Backup restore path escapes the configured backup directory");
    }

    let backup_root = backup_dir.canonicalize().map_err(|_| {
        anyhow::anyhow!(
            "Configured backup directory not found: {}",
            backup_dir.display()
        )
    })?;
    let canonical = backup_dir.join(requested).canonicalize().map_err(|_| {
        anyhow::anyhow!(
            "Backup restore file not found: {}",
            backup_dir.join(requested).display()
        )
    })?;
    if !canonical.starts_with(&backup_root) {
        anyhow::bail!("Backup restore path escapes the configured backup directory");
    }

    Ok(canonical)
}

fn resolve_cleanup_report_path(backup_dir: &StdPath, report: &str) -> anyhow::Result<PathBuf> {
    let trimmed = report.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Cleanup report path is required");
    }

    let requested = StdPath::new(trimmed);
    let backup_root = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());

    if requested.is_absolute() {
        let canonical = requested
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Cleanup report not found: {}", requested.display()))?;
        if !canonical.starts_with(&backup_root) {
            anyhow::bail!("Cleanup report must be inside the configured backup directory");
        }
        return Ok(canonical);
    }

    if requested.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("Cleanup report path escapes the configured backup directory");
    }

    let joined = backup_dir.join(requested);
    let canonical = joined
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("Cleanup report not found: {}", joined.display()))?;
    if !canonical.starts_with(&backup_root) {
        anyhow::bail!("Cleanup report must be inside the configured backup directory");
    }

    Ok(canonical)
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
        .map(|event| SkipEventView {
            event_at: event.event_at,
            action: event.action,
            reason: event.note.unwrap_or_else(|| "unknown".to_string()),
            target_path: event.target_path.display().to_string(),
            source_path: event.source_path.map(|path| path.display().to_string()),
            media_id: event.media_id,
        })
        .collect()
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
    Html(template.render().unwrap_or_else(|e| e.to_string()))
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

    let template = StatusTemplate {
        stats,
        recent_links,
        queue,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /health - Health check page
pub async fn get_health(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving health page");

    let mut health_checks = HashMap::new();

    // Check database
    health_checks.insert(
        "database".to_string(),
        HealthCheck {
            service: "SQLite Database".to_string(),
            status: "healthy".to_string(),
            message: "Connected".to_string(),
        },
    );

    // Check external services
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

    let template = HealthTemplate {
        checks: health_checks,
        deferred_refresh: deferred_refresh_summary(&state.config)
            .map(DeferredRefreshSummaryView::from)
            .unwrap_or_default(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
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
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /scan/trigger - Trigger a scan
pub async fn post_scan_trigger(
    State(state): State<WebState>,
    Form(form): Form<ScanTriggerForm>,
) -> impl IntoResponse {
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
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /cleanup/anime-remediation - Read-only anime remediation backlog
pub async fn get_cleanup_anime_remediation(
    State(state): State<WebState>,
    Query(query): Query<AnimeRemediationQuery>,
) -> impl IntoResponse {
    let Some(plex_db_path) = resolve_plex_db_path(query.plex_db.as_deref()) else {
        return Html(
            AnimeRemediationTemplate {
                summary: None,
                groups: vec![],
                error_message: Some(
                    "Plex DB path is required or must exist at a standard local path".to_string(),
                ),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    };

    match build_anime_remediation_report(&state.config, &state.database, &plex_db_path, query.full)
        .await
    {
        Ok(Some(report)) => {
            let assessed_groups = report
                .groups
                .iter()
                .map(assess_anime_remediation_group)
                .collect::<Result<Vec<_>, _>>();

            match assessed_groups {
                Ok(assessed_groups) => {
                    let eligible_groups = assessed_groups
                        .iter()
                        .filter(|group| group.eligible)
                        .count();
                    let blocked_groups = assessed_groups.len().saturating_sub(eligible_groups);
                    let blocked_reason_summary =
                        summarize_anime_remediation_blocked_reasons(&assessed_groups)
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
                                eligible_groups,
                                blocked_groups,
                                blocked_reason_summary,
                            }),
                            groups: assessed_groups
                                .into_iter()
                                .map(AnimeRemediationGroupView::from_plan_group)
                                .collect(),
                            error_message: None,
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
    let Some(plex_db_path) = resolve_plex_db_path(form.plex_db.as_deref()) else {
        return Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: "Anime remediation preview failed: Plex DB path is required or must exist at a standard local path".to_string(),
                preview: None,
                apply: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
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
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        ),
        Err(err) => Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: format!("Anime remediation preview failed: {}", err),
                preview: None,
                apply: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        ),
    }
}

/// POST /cleanup/anime-remediation/apply - Apply a saved guarded remediation plan
pub async fn post_cleanup_anime_remediation_apply(
    State(state): State<WebState>,
    Form(form): Form<AnimeRemediationApplyForm>,
) -> impl IntoResponse {
    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &form.report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                AnimeRemediationResultTemplate {
                    success: false,
                    message: format!("Anime remediation apply failed: {}", err),
                    preview: None,
                    apply: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
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
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        ),
        Err(err) => Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: format!("Anime remediation apply failed: {}", err),
                preview: None,
                apply: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        ),
    }
}

/// POST /cleanup/audit - Run audit
pub async fn post_cleanup_audit(
    State(state): State<WebState>,
    Form(form): Form<CleanupAuditForm>,
) -> impl IntoResponse {
    let selected_libraries = form.selected_libraries();
    info!(
        "Running cleanup audit (scope={}, libraries={:?})",
        form.scope, selected_libraries
    );

    let scope = match CleanupScope::parse(&form.scope) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Html(
                    CleanupResultTemplate {
                        success: false,
                        message: format!("Invalid scope: {}", e),
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
                .into_response();
        }
    };

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
                error_message: None,
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
                    error_message: Some(err.to_string()),
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
                error_message: Some(format!(
                    "Cleanup report not found: {}",
                    report_path.display()
                )),
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
                    error_message: Some(format!("Failed to read report: {}", e)),
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
                    error_message: Some(format!("Failed to parse report: {}", e)),
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
        error_message: None,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /cleanup/prune - Apply prune
pub async fn post_cleanup_prune(
    State(state): State<WebState>,
    Form(form): Form<CleanupPruneForm>,
) -> impl IntoResponse {
    info!("Applying prune (token={})", form.token);

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
        );
    }

    if form.token.is_empty() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: "Confirmation token is required".to_string(),
                active_cleanup_audit: None,
                last_cleanup_audit_outcome: None,
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
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
            );
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
        );
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
            );
        }
    };

    let report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
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
            );
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
            );
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
            confirm_token: Some(&form.token),
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
            );
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

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /links - Links list
pub async fn get_links(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let filter = params.get("filter").map(|f| f.as_str());
    let limit: i64 = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(100);

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
    Html(template.render().unwrap_or_else(|e| e.to_string()))
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
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /links/repair - Repair dead links
pub async fn post_repair(State(state): State<WebState>) -> impl IntoResponse {
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
        ),
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
        }
    }
}

/// GET /config - Config page
pub async fn get_config(State(state): State<WebState>) -> impl IntoResponse {
    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: None,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /config/validate - Validate config
pub async fn post_config_validate(State(state): State<WebState>) -> impl IntoResponse {
    let report = validate_config_report(&state.config).await;
    let result = Some(ValidationResult {
        valid: report.errors.is_empty(),
        errors: report.errors,
        warnings: report.warnings,
    });

    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: result,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
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
    let selected_library = query.library.clone().unwrap_or_default();
    match load_discovery_snapshot(
        &state.config,
        &state.database,
        query.library.as_deref(),
        query.refresh_cache,
    )
    .await
    {
        Ok(snapshot) => {
            let template = DiscoverTemplate {
                libraries: state.config.libraries.clone(),
                selected_library,
                refresh_cache: query.refresh_cache,
                discovered_items: snapshot.items,
                status_message: snapshot.status_message.or_else(|| {
                    (!query.refresh_cache).then(|| {
                        "Showing cached RD results only. Enable refresh when you want a slower live cache sync first."
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
            let template = DiscoverTemplate {
                libraries: state.config.libraries.clone(),
                selected_library,
                refresh_cache: query.refresh_cache,
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

/// POST /discover/add - Add torrent to library
pub async fn post_discover_add(
    State(_state): State<WebState>,
    Form(form): Form<DiscoverAddForm>,
) -> impl IntoResponse {
    info!(
        "Rejecting web discover-add for torrent {} (library='{}') until safe Arr selection is wired",
        form.torrent_id, form.library
    );
    let template = DiscoverResultTemplate {
        success: false,
        message: "Web discover/add is not wired to a safe Decypharr routing flow yet. Use the CLI: `symlinkarr discover add <torrent_id> --arr <arr-name>`."
            .to_string(),
    };

    (
        StatusCode::NOT_IMPLEMENTED,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
}

/// GET /backup - Backup page
pub async fn get_backup(State(state): State<WebState>) -> impl IntoResponse {
    let backup_manager = BackupManager::new(&state.config.backup);

    // List existing backups
    let mut backups = vec![];
    if let Ok(entries) = std::fs::read_dir(&state.config.backup.path) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".json") {
                    if let Ok(meta) = entry.metadata() {
                        backups.push(BackupInfo {
                            filename: name.to_string(),
                            size: meta.len(),
                            modified: meta.modified().ok(),
                        });
                    }
                }
            }
        }
    }

    backups.sort_by(|a, b| {
        b.modified
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .cmp(&a.modified.unwrap_or(std::time::SystemTime::UNIX_EPOCH))
    });

    let template = BackupTemplate {
        backups,
        backup_dir: state.config.backup.path.clone(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /backup/create - Create backup
pub async fn post_backup_create(
    State(state): State<WebState>,
    Form(form): Form<BackupCreateForm>,
) -> impl IntoResponse {
    info!("Creating backup (label={})", form.label);

    let backup_manager = BackupManager::new(&state.config.backup);

    let result = match backup_manager
        .create_backup(&state.database, &form.label)
        .await
    {
        Ok(path) => Some(path),
        Err(e) => {
            error!("Backup failed: {}", e);
            None
        }
    };

    let template = BackupResultTemplate {
        success: result.is_some(),
        message: if result.is_some() {
            "Backup created successfully".to_string()
        } else {
            "Backup failed".to_string()
        },
        backup_path: result,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /backup/restore - Restore backup
pub async fn post_backup_restore(
    State(state): State<WebState>,
    Form(form): Form<BackupRestoreForm>,
) -> impl IntoResponse {
    info!("Restoring backup: {}", form.backup_file);

    let backup_manager = BackupManager::new(&state.config.backup);
    let backup_path =
        match resolve_backup_restore_path(&state.config.backup.path, &form.backup_file) {
            Ok(path) => path,
            Err(e) => {
                let template = BackupResultTemplate {
                    success: false,
                    message: format!("Restore failed: {}", e),
                    backup_path: None,
                };
                return Html(
                    template
                        .render()
                        .unwrap_or_else(|render_err| render_err.to_string()),
                );
            }
        };

    if let Err(e) = ensure_backup_restore_runtime_healthy(&state.config, "backup restore").await {
        let template = BackupResultTemplate {
            success: false,
            message: format!("Restore failed: {}", e),
            backup_path: Some(backup_path),
        };
        return Html(
            template
                .render()
                .unwrap_or_else(|render_err| render_err.to_string()),
        );
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
        .await
        .map(|_| ());

    let (success, message) = match result {
        Ok(()) => (true, "Backup restored successfully".to_string()),
        Err(e) => (false, format!("Restore failed: {}", e)),
    };

    let template = BackupResultTemplate {
        success,
        message,
        backup_path: Some(backup_path),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

// ─── Form structs ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScanTriggerForm {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub search_missing: bool,
    pub library: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CleanupAuditForm {
    pub scope: String,
    pub library: Option<String>,
    #[serde(default)]
    pub libraries: Vec<String>,
}

impl CleanupAuditForm {
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
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct AnimeRemediationPreviewForm {
    pub plex_db: Option<String>,
    pub title: Option<String>,
    pub library: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnimeRemediationApplyForm {
    pub report: String,
    pub token: String,
    pub max_delete: Option<usize>,
    pub library: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DiscoverAddForm {
    pub torrent_id: String,
    pub library: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupCreateForm {
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupRestoreForm {
    pub backup_file: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Executor;
    use std::path::Path;
    use std::str::FromStr;
    use tempfile::TempDir;

    use crate::cleanup_audit::{CleanupReport, CleanupSummary};
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::{AcquisitionJobSeed, AcquisitionRelinkKind, Database, ScanRunRecord};
    use crate::models::{LinkRecord, LinkStatus, MediaType};
    use crate::web::{
        ActiveCleanupAuditJob, ActiveScanJob, LastCleanupAuditOutcome, LastScanOutcome,
    };

    struct TestWebContext {
        _dir: TempDir,
        state: WebState,
    }

    fn test_config(root: &std::path::Path) -> Config {
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

    async fn test_context() -> TestWebContext {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: dir.path().join("rd").join("Show.S01E01.mkv"),
            target_path: dir
                .path()
                .join("anime")
                .join("Show (2024) {tvdb-1}")
                .join("Season 01")
                .join("S01E01.mkv"),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        db.record_scan_run(&ScanRunRecord {
            dry_run: true,
            library_filter: Some("Anime".to_string()),
            run_token: Some("scan-run-handler".to_string()),
            search_missing: true,
            library_items_found: 1,
            source_items_found: 42,
            matches_found: 11,
            links_created: 3,
            links_updated: 1,
            dead_marked: 2,
            links_removed: 0,
            links_skipped: 7,
            ambiguous_skipped: 1,
            skip_reason_json: None,
            runtime_checks_ms: 10,
            library_scan_ms: 20,
            source_inventory_ms: 30,
            matching_ms: 40,
            title_enrichment_ms: 50,
            linking_ms: 60,
            plex_refresh_ms: 70,
            plex_refresh_requested_paths: 4,
            plex_refresh_unique_paths: 3,
            plex_refresh_planned_batches: 2,
            plex_refresh_coalesced_batches: 1,
            plex_refresh_coalesced_paths: 2,
            plex_refresh_refreshed_batches: 1,
            plex_refresh_refreshed_paths_covered: 3,
            plex_refresh_skipped_batches: 1,
            plex_refresh_unresolved_paths: 0,
            plex_refresh_capped_batches: 1,
            plex_refresh_aborted_due_to_cap: true,
            plex_refresh_failed_batches: 0,
            media_server_refresh_json: Some(
                r#"[{"server":"plex","requested_targets":4,"refresh":{"requested_paths":3,"unique_paths":2,"planned_batches":2,"coalesced_batches":1,"coalesced_paths":2,"refreshed_batches":1,"refreshed_paths_covered":3,"skipped_batches":1,"unresolved_paths":0,"capped_batches":1,"aborted_due_to_cap":true,"failed_batches":0}},{"server":"emby","requested_targets":4,"refresh":{"requested_paths":1,"unique_paths":1,"planned_batches":1,"coalesced_batches":0,"coalesced_paths":0,"refreshed_batches":1,"refreshed_paths_covered":1,"skipped_batches":0,"unresolved_paths":0,"capped_batches":0,"aborted_due_to_cap":false,"failed_batches":0}}]"#.to_string(),
            ),
            dead_link_sweep_ms: 80,
            cache_hit_ratio: Some(0.85),
            candidate_slots: 1024,
            scored_candidates: 24,
            exact_id_hits: 2,
            auto_acquire_requests: 6,
            auto_acquire_missing_requests: 4,
            auto_acquire_cutoff_requests: 2,
            auto_acquire_dry_run_hits: 4,
            auto_acquire_submitted: 0,
            auto_acquire_no_result: 1,
            auto_acquire_blocked: 1,
            auto_acquire_failed: 0,
            auto_acquire_completed_linked: 0,
            auto_acquire_completed_unlinked: 0,
        })
        .await
        .unwrap();

        db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
            request_key: "anime-queued-1".to_string(),
            label: "Queued Anime".to_string(),
            query: "Queued Anime S01E01".to_string(),
            query_hints: vec![],
            imdb_id: Some("tt1234567".to_string()),
            categories: vec![5070],
            arr: "sonarr".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_kind: AcquisitionRelinkKind::MediaId,
            relink_value: "tvdb-1".to_string(),
        }])
        .await
        .unwrap();

        TestWebContext {
            _dir: dir,
            state: WebState::new(cfg, db),
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

    async fn render_body(response: impl IntoResponse) -> String {
        let response = response.into_response();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn dashboard_renders_latest_run_and_queue_summary() {
        let ctx = test_context().await;
        let body = render_body(get_dashboard(State(ctx.state.clone())).await).await;

        assert!(body.contains("Current baseline"));
        assert!(body.contains("Anime"));
        assert!(body.contains("Auto-Acquire Queue"));
        assert!(body.contains("Queue 1"));
        assert!(body.contains("Cache Hit"));
        assert!(body.contains("Media refresh protections activated"));
        assert!(body.contains("Plex guard abort"));
        assert!(body.contains("Emby 1/1"));
    }

    #[tokio::test]
    async fn dashboard_renders_deferred_refresh_backlog() {
        let ctx = test_context().await;
        std::fs::write(
            ctx.state
                .config
                .backup
                .path
                .join(".media-server-refresh.queue.json"),
            r#"{
              "servers": [
                { "server": "plex", "paths": ["/library/anime", "/library/anime-2"] },
                { "server": "jellyfin", "paths": ["/library/anime"] }
              ]
            }"#,
        )
        .unwrap();

        let body = render_body(get_dashboard(State(ctx.state.clone())).await).await;
        assert!(body.contains("Deferred refresh 3"));
        assert!(body.contains("Media Refresh Backlog"));
        assert!(body.contains("Plex"));
        assert!(body.contains("Jellyfin"));
    }

    #[tokio::test]
    async fn scan_page_renders_phase_telemetry_and_acquire_summary() {
        let ctx = test_context().await;
        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Start Real Scan"));
        assert!(body.contains("Search Missing"));
        assert!(body.contains("Candidate Slots"));
        assert!(body.contains("1024"));
        assert!(body.contains("4/6"));
        assert!(body.contains("Media refresh protections activated"));
        assert!(body.contains("Plex guard abort"));
        assert!(body.contains("Emby 1/1"));
    }

    #[tokio::test]
    async fn scan_page_renders_active_background_scan_banner() {
        let ctx = test_context().await;
        ctx.state
            .set_active_scan_for_test(Some(ActiveScanJob {
                started_at: "2026-03-29 23:59:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                dry_run: true,
                search_missing: true,
            }))
            .await;

        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Background scan running"));
        assert!(body.contains("2026-03-29 23:59:00 UTC"));
        assert!(body.contains("Anime"));
        assert!(body.contains("Search missing enabled"));
    }

    #[tokio::test]
    async fn scan_page_renders_last_failed_background_scan_outcome() {
        let ctx = test_context().await;
        ctx.state
            .set_last_scan_outcome_for_test(Some(LastScanOutcome {
                finished_at: "2099-03-29 23:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                dry_run: false,
                search_missing: true,
                success: false,
                message: "RD cache sync failed".to_string(),
            }))
            .await;

        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Background scan failed"));
        assert!(body.contains("RD cache sync failed"));
        assert!(body.contains("2099-03-29 23:58:00 UTC"));
    }

    #[tokio::test]
    async fn scan_page_hides_stale_failed_background_outcome_when_newer_run_exists() {
        let ctx = test_context().await;
        ctx.state
            .set_last_scan_outcome_for_test(Some(LastScanOutcome {
                finished_at: "2026-03-29 09:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                dry_run: false,
                search_missing: true,
                success: false,
                message: "stale failure".to_string(),
            }))
            .await;

        let body = render_body(
            get_scan(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(!body.contains("Background scan failed"));
        assert!(!body.contains("stale failure"));
    }

    #[tokio::test]
    async fn cleanup_page_renders_active_background_audit_banner() {
        let ctx = test_context().await;
        ctx.state
            .set_active_cleanup_audit_for_test(Some(ActiveCleanupAuditJob {
                started_at: "2026-03-29 23:59:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                libraries_label: "Anime".to_string(),
            }))
            .await;

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(body.contains("Background cleanup audit running"));
        assert!(body.contains("2026-03-29 23:59:00 UTC"));
        assert!(body.contains("Anime across Anime"));
    }

    #[tokio::test]
    async fn cleanup_page_renders_last_failed_background_audit_outcome() {
        let ctx = test_context().await;
        ctx.state
            .set_last_cleanup_audit_outcome_for_test(Some(LastCleanupAuditOutcome {
                finished_at: "2026-03-29 23:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                libraries_label: "Anime".to_string(),
                success: false,
                message: "source root unhealthy".to_string(),
                report_path: None,
            }))
            .await;

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(body.contains("Background cleanup audit failed"));
        assert!(body.contains("source root unhealthy"));
        assert!(body.contains("2026-03-29 23:58:00 UTC"));
    }

    #[tokio::test]
    async fn cleanup_page_hides_stale_failed_background_audit_outcome_when_newer_report_exists() {
        let ctx = test_context().await;
        let report_path = ctx
            .state
            .config
            .backup
            .path
            .join("cleanup-audit-anime-20260329.json");
        let report = CleanupReport {
            version: 1,
            created_at: chrono::Utc::now(),
            scope: CleanupScope::Anime,
            findings: vec![],
            summary: CleanupSummary {
                total_findings: 1,
                critical: 0,
                high: 1,
                warning: 0,
            },
        };
        std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        ctx.state
            .set_last_cleanup_audit_outcome_for_test(Some(LastCleanupAuditOutcome {
                finished_at: "2026-03-29 09:58:00 UTC".to_string(),
                scope_label: "Anime".to_string(),
                libraries_label: "Anime".to_string(),
                success: false,
                message: "stale cleanup failure".to_string(),
                report_path: None,
            }))
            .await;

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(!body.contains("Background cleanup audit failed"));
        assert!(!body.contains("stale cleanup failure"));
        assert!(body.contains("Last Report"));
    }

    #[tokio::test]
    async fn anime_remediation_page_renders_ranked_groups() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = test_config(&root);
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
            source_path: tracked_source,
            target_path: tracked_target,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_target = legacy_root.join("Season 01/Show A - S01E01.mkv");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/tmp/source-a.mkv", &legacy_target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file("C:\\source-a.mkv", &legacy_target).unwrap();

        let plex_db_path = root.join("plex.db");
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

        let state = WebState::new(cfg, db);
        let body = render_body(
            get_cleanup_anime_remediation(
                State(state),
                Query(AnimeRemediationQuery {
                    full: true,
                    plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains("Anime Remediation Backlog"));
        assert!(body.contains("Show A"));
        assert!(body.contains("Show Full Backlog") || body.contains("Show Sample"));
        assert!(
            body.contains("com.plexapp.agents.hama://anidb-100") || body.contains("hama-anidb")
        );
        assert!(body.contains("/tmp") || body.contains("/Show A (2024) {tvdb-1}"));
    }

    #[tokio::test]
    async fn anime_remediation_preview_page_renders_saved_plan_and_apply_gate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = test_config(&root);
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
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_target = legacy_root.join("Season 01/Show A - S01E01.mkv");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&tracked_source, &legacy_target).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&tracked_source, &legacy_target).unwrap();

        let plex_db_path = root.join("plex.db");
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

        let state = WebState::new(cfg, db);
        let body = render_body(
            post_cleanup_anime_remediation_preview(
                State(state),
                Form(AnimeRemediationPreviewForm {
                    plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                    title: None,
                    library: Some("Anime".to_string()),
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains("Apply this exact saved plan"));
        assert!(body.contains("Confirmation token"));
        assert!(body.contains("Apply Guarded Remediation"));
        assert!(body.contains("Report file:"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn anime_remediation_apply_page_renders_quarantine_result() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let root = dir.path().to_path_buf();
        let mut cfg = test_config(&root);
        cfg.backup.path = std::path::PathBuf::from(root.file_name().unwrap()).join("backups");
        cfg.cleanup.prune.quarantine_path = root.join("quarantine");
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
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_target = legacy_root.join("Season 01/Show A - S01E01.mkv");
        std::os::unix::fs::symlink(&tracked_source, &legacy_target).unwrap();

        let plex_db_path = root.join("plex.db");
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

        cfg.libraries[0].path = anime_root;
        let state = WebState::new(cfg, db);
        let (plan, report_path) = preview_anime_remediation_plan(
            &state.config,
            &state.database,
            Some("Anime"),
            &plex_db_path,
            None,
            None,
        )
        .await
        .unwrap();

        let body = render_body(
            post_cleanup_anime_remediation_apply(
                State(state.clone()),
                Form(AnimeRemediationApplyForm {
                    report: report_path.to_string_lossy().to_string(),
                    token: plan.confirmation_token,
                    max_delete: None,
                    library: Some("Anime".to_string()),
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains("Anime remediation applied"));
        assert!(body.contains("Safety snapshot"));
        assert!(body.contains("Quarantined"));
        assert!(!legacy_target.exists());
    }

    #[tokio::test]
    async fn scan_run_detail_renders_specific_run() {
        let ctx = test_context().await;
        let run = ctx
            .state
            .database
            .get_scan_history(1)
            .await
            .unwrap()
            .remove(0);
        let body =
            render_body(get_scan_run_detail(State(ctx.state.clone()), Path(run.id)).await).await;

        assert!(body.contains("Scan Run Detail"));
        assert!(body.contains("Outcome summary"));
        assert!(body.contains("#1") || body.contains(&format!("#{}", run.id)));
        assert!(body.contains("Recent concrete skip events"));
        assert!(body.contains("1024"));
    }

    #[tokio::test]
    async fn scan_history_applies_mode_and_missing_filters() {
        let ctx = test_context().await;
        ctx.state
            .database
            .record_scan_run(&ScanRunRecord {
                dry_run: false,
                library_filter: Some("Movies".to_string()),
                search_missing: false,
                library_items_found: 2,
                source_items_found: 8,
                matches_found: 4,
                links_created: 1,
                links_updated: 0,
                skip_reason_json: None,
                ..Default::default()
            })
            .await
            .unwrap();

        let runs = ctx.state.database.get_scan_history(5).await.unwrap();
        let movie_run_id = runs
            .iter()
            .find(|run| run.library_filter.as_deref() == Some("Movies"))
            .map(|run| run.id)
            .unwrap();
        let anime_run_id = runs
            .iter()
            .find(|run| run.library_filter.as_deref() == Some("Anime"))
            .map(|run| run.id)
            .unwrap();

        let body = render_body(
            get_scan_history(
                State(ctx.state.clone()),
                Query(ScanHistoryQuery {
                    mode: Some("live".to_string()),
                    search_missing: Some("exclude".to_string()),
                    limit: Some(25),
                    ..ScanHistoryQuery::default()
                }),
            )
            .await,
        )
        .await;

        assert!(body.contains(&format!("/scan/history/{}", movie_run_id)));
        assert!(!body.contains(&format!("/scan/history/{}", anime_run_id)));
    }

    #[tokio::test]
    async fn scan_history_renders_refresh_backend_badges() {
        let ctx = test_context().await;
        let body = render_body(
            get_scan_history(State(ctx.state.clone()), Query(ScanHistoryQuery::default())).await,
        )
        .await;

        assert!(body.contains("Plex guard abort"));
        assert!(body.contains("Emby 1/1"));
    }

    #[tokio::test]
    async fn status_page_renders_queue_pressure_and_recent_links() {
        let ctx = test_context().await;
        let body = render_body(get_status(State(ctx.state.clone())).await).await;

        assert!(body.contains("Queue pressure"));
        assert!(body.contains("Recent Links"));
        assert!(body.contains("tvdb-1"));
        assert!(body.contains("Queued"));
    }

    #[tokio::test]
    async fn doctor_page_uses_full_doctor_checks() {
        let ctx = test_context().await;
        let body = render_body(get_doctor(State(ctx.state)).await).await;

        assert!(body.contains("db_schema_version"));
        assert!(body.contains("config_validation"));
        assert!(body.contains("cleanup.prune.enforce_policy"));
    }

    #[tokio::test]
    async fn doctor_page_does_not_create_missing_backup_dir_in_read_only_mode() {
        let ctx = test_context().await;
        let backup_dir = ctx.state.config.backup.path.clone();
        std::fs::remove_dir(&backup_dir).unwrap();
        assert!(!backup_dir.exists());

        let body = render_body(get_doctor(State(ctx.state)).await).await;

        assert!(body.contains("backup_dir"));
        assert!(body.contains("write probe skipped in read-only mode"));
        assert!(!backup_dir.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn doctor_page_flags_existing_non_writable_backup_dir() {
        use std::os::unix::fs::PermissionsExt;

        let ctx = test_context().await;
        let backup_dir = ctx.state.config.backup.path.clone();
        std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let body = render_body(get_doctor(State(ctx.state)).await).await;

        assert!(body.contains("backup_dir"));
        assert!(body.contains("denies write or traverse"));
        assert!(body.contains("mode=555"));
    }

    #[tokio::test]
    async fn discover_page_renders_cached_gap_items() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();

        db.upsert_rd_torrent(
            "rd-1",
            "hash-1",
            "Missing.Show.S01E01.1080p.WEB-DL.mkv",
            "downloaded",
            r#"{"files":[{"bytes":1073741824,"path":"Missing.Show.S01E01.1080p.WEB-DL.mkv"}]}"#,
        )
        .await
        .unwrap();

        let state = WebState::new(cfg, db);
        let response = get_discover(State(state), Query(DiscoverQuery::default()))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("Discovered Items (1)"));
        assert!(body.contains("Missing Show"));
        assert!(body.contains("Real-Debrid API key not configured"));
        assert!(body.contains("live refresh is unavailable"));
    }

    #[tokio::test]
    async fn discover_page_rejects_invalid_library_filter() {
        let ctx = test_context().await;
        let response = get_discover(
            State(ctx.state),
            Query(DiscoverQuery {
                library: Some("Nope".to_string()),
                refresh_cache: false,
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("Invalid library filter"));
        assert!(body.contains("Unknown library filter"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_repair_starts_background_repair_flow() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();

        let library_root = cfg.libraries[0].path.clone();
        let source_root = cfg.sources[0].path.clone();
        let target_path = library_root.join("Show/Season 01/Show - S01E01.mkv");
        let missing_source = source_root.join("missing/Show.S01E01.mkv");
        let replacement = source_root.join("Show.S01E01.mkv");

        std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(missing_source.parent().unwrap()).unwrap();
        std::fs::write(&replacement, b"video").unwrap();
        std::os::unix::fs::symlink(&missing_source, &target_path).unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: missing_source.clone(),
            target_path: target_path.clone(),
            media_id: "tvdb-99".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Dead,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let state = WebState::new(cfg, db);
        let response = post_repair(State(state.clone())).await.into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("Repair started in background"));

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if state.last_repair_outcome().await.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background repair should finish");

        let outcome = state
            .last_repair_outcome()
            .await
            .expect("expected repair outcome");
        assert!(outcome.success);
        assert_eq!(outcome.repaired, 1);
        assert_eq!(outcome.failed, 0);

        let repaired = state.database.get_active_links().await.unwrap();
        let repaired = repaired
            .into_iter()
            .find(|link| link.target_path == target_path)
            .expect("expected repaired active link");
        assert_eq!(repaired.source_path, replacement);
    }

    #[tokio::test]
    async fn post_discover_add_returns_not_implemented_failure() {
        let ctx = test_context().await;
        let response = post_discover_add(
            State(ctx.state),
            Form(DiscoverAddForm {
                torrent_id: "rd-123".to_string(),
                library: "Anime".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("not wired to a safe Decypharr routing flow"));
    }

    #[tokio::test]
    async fn cleanup_page_renders_latest_report_summary() {
        let ctx = test_context().await;
        let report_path = ctx
            .state
            .config
            .backup
            .path
            .join("cleanup-audit-anime-20260321.json");
        let report = CleanupReport {
            version: 1,
            created_at: chrono::Utc::now(),
            scope: CleanupScope::Anime,
            findings: vec![],
            summary: CleanupSummary {
                total_findings: 12,
                critical: 3,
                high: 5,
                warning: 4,
            },
        };
        std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        let body = render_body(get_cleanup(State(ctx.state.clone())).await).await;

        assert!(body.contains("Last Report"));
        assert!(body.contains("12"));
        assert!(body.contains("Open Prune Preview"));
        assert!(!body.contains("Apply Cleanup"));
        assert!(body.contains("Apply only from preview"));
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_dedupes_legacy_and_multi_select_fields() {
        let form = CleanupAuditForm {
            scope: "anime".to_string(),
            library: Some("Anime".to_string()),
            libraries: vec!["Anime".to_string(), "Anime 2".to_string()],
        };

        assert_eq!(
            form.selected_libraries(),
            vec!["Anime".to_string(), "Anime 2".to_string()]
        );
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_uses_single_when_multi_empty() {
        let form = CleanupAuditForm {
            scope: "anime".to_string(),
            library: Some("Anime".to_string()),
            libraries: vec![],
        };

        assert_eq!(form.selected_libraries(), vec!["Anime".to_string()]);
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_ignores_empty_library() {
        let form = CleanupAuditForm {
            scope: "anime".to_string(),
            library: Some("".to_string()),
            libraries: vec!["Anime".to_string()],
        };

        assert_eq!(form.selected_libraries(), vec!["Anime".to_string()]);
    }

    #[test]
    fn cleanup_audit_form_selected_libraries_whitespace_trimmed() {
        // Single is appended after multi-select, whitespace is trimmed throughout
        let form = CleanupAuditForm {
            scope: "anime".to_string(),
            library: Some("  Anime  ".to_string()),
            libraries: vec!["  Anime 2  ".to_string()],
        };

        let result = form.selected_libraries();
        assert!(result.contains(&"Anime".to_string()));
        assert!(result.contains(&"Anime 2".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn resolve_backup_restore_path_rejects_absolute_input() {
        let backup_dir = PathBuf::from("/srv/symlinkarr/backups");
        let err = resolve_backup_restore_path(&backup_dir, "/tmp/evil.json").unwrap_err();
        assert!(err.to_string().contains("configured backup directory"));
    }

    #[test]
    fn resolve_backup_restore_path_rejects_parent_escape() {
        let backup_dir = PathBuf::from("/srv/symlinkarr/backups");
        let err = resolve_backup_restore_path(&backup_dir, "../outside.json").unwrap_err();
        assert!(err.to_string().contains("escapes"));
    }

    #[test]
    fn resolve_backup_restore_path_accepts_plain_filename() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let backup_file = backup_dir.join("backup-20260329.json");
        std::fs::write(&backup_file, "{}").unwrap();

        let path = resolve_backup_restore_path(&backup_dir, "backup-20260329.json").unwrap();
        assert_eq!(path, backup_file.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_backup_restore_path_rejects_symlink_escape_inside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        let outside_dir = dir.path().join("outside");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("backup.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside_dir, backup_dir.join("linked")).unwrap();

        let err = resolve_backup_restore_path(&backup_dir, "linked/backup.json").unwrap_err();
        assert!(err.to_string().contains("escapes"));
    }

    #[tokio::test]
    async fn post_backup_restore_rejects_unhealthy_runtime_roots() {
        let ctx = test_context().await;
        let backup_file = "backup-20260330.json";
        let backup_path = ctx.state.config.backup.path.join(backup_file);
        std::fs::write(&backup_path, "{}").unwrap();
        std::fs::remove_dir_all(&ctx.state.config.sources[0].path).unwrap();

        let response = post_backup_restore(
            State(ctx.state),
            Form(BackupRestoreForm {
                backup_file: backup_file.to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("Restore failed: Refusing backup restore"));
    }

    #[test]
    fn resolve_cleanup_report_path_rejects_absolute_outside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        let outside = dir.path().join("outside.json");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(&outside, "{}").unwrap();

        let err = resolve_cleanup_report_path(&backup_dir, outside.to_string_lossy().as_ref())
            .unwrap_err();
        assert!(err.to_string().contains("configured backup directory"));
    }

    #[test]
    fn resolve_cleanup_report_path_accepts_plain_filename() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let report = backup_dir.join("cleanup-audit-anime.json");
        std::fs::write(&report, "{}").unwrap();

        let path = resolve_cleanup_report_path(&backup_dir, "cleanup-audit-anime.json").unwrap();
        assert_eq!(path, report.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cleanup_report_path_rejects_symlink_escape_inside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        let outside_dir = dir.path().join("outside");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("report.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside_dir, backup_dir.join("linked")).unwrap();

        let err = resolve_cleanup_report_path(&backup_dir, "linked/report.json").unwrap_err();
        assert!(err
            .to_string()
            .contains("Cleanup report must be inside the configured backup directory"));
    }
}
