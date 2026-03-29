//! HTTP handlers for the web UI

use askama::Template;
use axum::{
    extract::{Form, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
};
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Component, Path as StdPath, PathBuf};
use tracing::{error, info};

use crate::backup::BackupManager;
use crate::cleanup_audit::{self, CleanupAuditor, CleanupScope};
use crate::commands::discover::load_discovery_snapshot;
use crate::commands::repair::{execute_repair_auto, summarize_repair_results};
use crate::commands::scan::run_scan;
use crate::db::{AcquisitionJobCounts, ScanHistoryRecord};
use crate::OutputFormat;

use super::templates::*;
use super::WebState;

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
    let json = std::fs::read_to_string(path).ok()?;
    let report: cleanup_audit::CleanupReport = serde_json::from_str(&json).ok()?;
    Some(CleanupReportSummaryView::from_report(
        path.to_path_buf(),
        report,
    ))
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

    Ok(backup_dir.join(requested))
}

fn repair_error_status(message: &str) -> StatusCode {
    if message.contains("Refusing repair auto") || message.contains("Unknown library filter") {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
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

    let template = DashboardTemplate {
        stats,
        latest_run,
        recent_runs,
        queue,
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
    let queue = match state.database.get_acquisition_job_counts().await {
        Ok(counts) => queue_overview_from_counts(counts),
        Err(e) => {
            error!("Failed to get acquisition queue counts: {}", e);
            QueueOverview::default()
        }
    };

    let template = ScanTemplate {
        libraries: state.config.libraries.clone(),
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

    let (added, removed) = match run_scan(
        &state.config,
        &state.database,
        form.dry_run,
        form.search_missing,
        OutputFormat::Text,
        library_name,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("Scan failed: {}", e);
            return Html(
                ScanResultTemplate {
                    success: false,
                    message: format!("Scan failed: {}", e),
                    latest_run: None,
                    dry_run: form.dry_run,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let latest_run = match state.database.get_scan_history(1).await {
        Ok(mut history) => history.drain(..).next().map(ScanRunView::from_record),
        Err(e) => {
            error!("Failed to load latest scan history after scan: {}", e);
            None
        }
    };

    let template = ScanResultTemplate {
        success: true,
        message: format!(
            "Scan complete: {} added/updated, {} removed",
            added, removed
        ),
        latest_run,
        dry_run: form.dry_run,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
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
            let template = ScanRunDetailTemplate {
                run: ScanRunView::from_record(run),
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
    // Look for the most recent cleanup report
    let last_report = match std::fs::read_dir(&state.config.backup.path) {
        Ok(entries) => {
            let mut reports: Vec<_> = entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    name.to_string_lossy().starts_with("cleanup-audit-")
                        && name.to_string_lossy().ends_with(".json")
                })
                .collect();

            // Sort by modification time (newest first)
            reports.sort_by_key(|entry| {
                entry
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            reports.reverse();

            reports.first().map(|entry| entry.path())
        }
        Err(_) => None,
    };

    let last_report_summary = last_report
        .as_deref()
        .and_then(cleanup_report_summary_from_path);

    let template = CleanupTemplate {
        libraries: state.config.libraries.clone(),
        last_report: last_report_summary,
        last_report_path: last_report,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
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
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Invalid scope: {}", e),
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let auditor = CleanupAuditor::new_with_progress(&state.config, &state.database, true);

    // Use a scoped output path when a subset of libraries was selected.
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let output_path = if selected_libraries.len() == 1 {
        state.config.backup.path.join(format!(
            "cleanup-audit-{}-{}-{}.json",
            form.scope, selected_libraries[0], ts
        ))
    } else if !selected_libraries.is_empty() {
        state.config.backup.path.join(format!(
            "cleanup-audit-{}-multi-{}-{}.json",
            form.scope,
            selected_libraries.len(),
            ts
        ))
    } else {
        state
            .config
            .backup
            .path
            .join(format!("cleanup-audit-{}-{}.json", form.scope, ts))
    };

    let report_path = match auditor
        .run_audit_filtered(
            scope,
            (!selected_libraries.is_empty()).then_some(selected_libraries.as_slice()),
            Some(&output_path),
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            error!("Audit failed: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Audit failed: {}", e),
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let message = if selected_libraries.len() == 1 {
        format!(
            "Audit complete for library '{}': {}",
            selected_libraries[0],
            report_path.display()
        )
    } else if !selected_libraries.is_empty() {
        format!(
            "Audit complete for {} libraries ({}): {}",
            selected_libraries.len(),
            selected_libraries.join(", "),
            report_path.display()
        )
    } else {
        format!("Audit complete: {}", report_path.display())
    };

    let template = CleanupResultTemplate {
        success: true,
        message,
        report_summary: cleanup_report_summary_from_path(&report_path),
        report_path: Some(report_path),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /cleanup/prune - Prune preview
pub async fn get_cleanup_prune(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let report_path = params.get("report").map(|p| p.as_str());

    if report_path.is_none() {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                critical: 0,
                high: 0,
                warning: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                report_path: None,
                confirmation_token: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Read the report and show preview
    let report_path = std::path::Path::new(report_path.unwrap());
    if !report_path.exists() {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                critical: 0,
                high: 0,
                warning: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                report_path: None,
                confirmation_token: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Parse the JSON report to show actual preview data
    let json = match std::fs::read_to_string(report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    report_path: None,
                    confirmation_token: None,
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
                    critical: 0,
                    high: 0,
                    warning: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    report_path: None,
                    confirmation_token: None,
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
            )),
            Err(e) => {
                error!("Failed to hydrate cleanup report DB state: {}", e);
                None
            }
        };

    let template = PrunePreviewTemplate {
        findings: report.findings.clone(),
        total: report.findings.len(),
        critical: report.summary.critical,
        high: report.summary.high,
        warning: report.summary.warning,
        managed_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.managed_candidates)
            .unwrap_or(0),
        foreign_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.foreign_candidates)
            .unwrap_or(0),
        report_path: Some(report_path.to_path_buf()),
        confirmation_token: prune_plan.map(|plan| plan.confirmation_token),
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
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Read the report
    let report_path = std::path::Path::new(&form.report);
    if !report_path.exists() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: format!("Report not found: {}", form.report),
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    let json = match std::fs::read_to_string(report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to read report: {}", e),
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
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    // Apply the prune operation
    let outcome = match cleanup_audit::run_prune(
        &state.config,
        &state.database,
        report_path,
        true,              // apply
        None,              // max_delete
        Some(&form.token), // confirmation_token
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            error!("Prune operation failed: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let message = if outcome.removed > 0 {
        format!(
            "✅ Prune completed successfully: {} symlinks removed, {} skipped",
            outcome.removed, outcome.skipped
        )
    } else {
        "⚠️ Prune completed but no symlinks were removed".to_string()
    };

    let template = CleanupResultTemplate {
        success: true,
        message,
        report_summary: cleanup_report_summary_from_path(report_path),
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

    let template = DeadLinksTemplate { links };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /links/repair - Repair dead links
pub async fn post_repair(State(state): State<WebState>) -> impl IntoResponse {
    info!("Running auto repair");

    match execute_repair_auto(&state.config, &state.database, None, false).await {
        Ok(results) => {
            let (repaired, failed, skipped, stale) = summarize_repair_results(&results);
            let message = if repaired == 0 && failed == 0 && skipped == 0 && stale == 0 {
                "Repair completed. No dead links required action.".to_string()
            } else {
                format!(
                    "Repair completed: {} repaired, {} unrepairable, {} skipped, {} stale record(s).",
                    repaired, failed, skipped, stale
                )
            };
            let template = RepairResultTemplate {
                success: true,
                message,
                repaired,
                failed,
            };
            (
                StatusCode::OK,
                Html(template.render().unwrap_or_else(|e| e.to_string())),
            )
        }
        Err(err) => {
            let message = err.to_string();
            let template = RepairResultTemplate {
                success: false,
                message: message.clone(),
                repaired: 0,
                failed: 0,
            };
            (
                repair_error_status(&message),
                Html(template.render().unwrap_or_else(|e| e.to_string())),
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
    let report = state.config.validate();
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
    let mut checks = vec![];

    // Check libraries exist
    for lib in &state.config.libraries {
        let exists = lib.path.exists();
        checks.push(DoctorCheck {
            check: format!("Library '{}' exists", lib.name),
            passed: exists,
            message: if exists {
                format!("{}: exists", lib.path.display())
            } else {
                format!("{}: NOT FOUND", lib.path.display())
            },
        });
    }

    // Check sources exist
    for source in &state.config.sources {
        let exists = source.path.exists();
        checks.push(DoctorCheck {
            check: format!("Source '{}' exists", source.name),
            passed: exists,
            message: if exists {
                format!("{}: exists", source.path.display())
            } else {
                format!("{}: NOT FOUND", source.path.display())
            },
        });
    }

    // Check database
    let db_ok = state.database.get_web_stats().await.is_ok();
    checks.push(DoctorCheck {
        check: "Database connection".to_string(),
        passed: db_ok,
        message: if db_ok { "Connected" } else { "Failed" }.to_string(),
    });

    // Check API keys
    let has_tmdb = state.config.has_tmdb();
    checks.push(DoctorCheck {
        check: "TMDB API key".to_string(),
        passed: has_tmdb,
        message: if has_tmdb { "Configured" } else { "Missing" }.to_string(),
    });

    let has_tvdb = state.config.has_tvdb();
    checks.push(DoctorCheck {
        check: "TVDB API key".to_string(),
        passed: has_tvdb,
        message: if has_tvdb { "Configured" } else { "Missing" }.to_string(),
    });

    let has_rd = state.config.has_realdebrid();
    checks.push(DoctorCheck {
        check: "Real-Debrid API token".to_string(),
        passed: has_rd,
        message: if has_rd { "Configured" } else { "Missing" }.to_string(),
    });

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
        true,
    )
    .await
    {
        Ok(snapshot) => {
            let template = DiscoverTemplate {
                libraries: state.config.libraries.clone(),
                selected_library,
                discovered_items: snapshot.items,
                status_message: snapshot.status_message,
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
    use tempfile::TempDir;

    use crate::cleanup_audit::{CleanupReport, CleanupSummary};
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
        SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::{AcquisitionJobSeed, AcquisitionRelinkKind, Database, ScanRunRecord};
    use crate::models::{LinkRecord, LinkStatus, MediaType};

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
            runtime_checks_ms: 10,
            library_scan_ms: 20,
            source_inventory_ms: 30,
            matching_ms: 40,
            title_enrichment_ms: 50,
            linking_ms: 60,
            plex_refresh_ms: 70,
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
    async fn status_page_renders_queue_pressure_and_recent_links() {
        let ctx = test_context().await;
        let body = render_body(get_status(State(ctx.state.clone())).await).await;

        assert!(body.contains("Queue pressure"));
        assert!(body.contains("Recent Links"));
        assert!(body.contains("tvdb-1"));
        assert!(body.contains("Queued"));
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
        assert!(body.contains("cached results only"));
    }

    #[tokio::test]
    async fn discover_page_rejects_invalid_library_filter() {
        let ctx = test_context().await;
        let response = get_discover(
            State(ctx.state),
            Query(DiscoverQuery {
                library: Some("Nope".to_string()),
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
    async fn post_repair_runs_real_repair_flow() {
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
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("Repair completed: 1 repaired, 0 unrepairable"));

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
            created_at: Utc::now(),
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
        assert!(body.contains("Apply Cleanup"));
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
        let backup_dir = PathBuf::from("/srv/symlinkarr/backups");
        let path = resolve_backup_restore_path(&backup_dir, "backup-20260329.json").unwrap();
        assert_eq!(path, backup_dir.join("backup-20260329.json"));
    }
}
