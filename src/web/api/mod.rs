//! JSON API endpoints for automation

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::backup::BackupManager;
use crate::cleanup_audit::{self, CleanupAuditor, CleanupReport, CleanupScope};
use crate::commands::discover::load_discovery_snapshot;
use crate::commands::repair::{execute_repair_auto, summarize_repair_results};
use crate::commands::scan::run_scan;
use crate::config::Config;
use crate::db::{Database, ScanHistoryRecord};
use crate::OutputFormat;

use super::WebState;

/// Create the API router
pub fn create_router(state: WebState) -> Router<WebState> {
    Router::new()
        .route("/status", get(api_get_status))
        .route("/health", get(api_get_health))
        .route("/discover", get(api_get_discover))
        .route("/scan", post(api_post_scan))
        .route("/scan/jobs", get(api_get_scan_jobs))
        .route("/scan/history", get(api_get_scan_history))
        .route("/scan/:id", get(api_get_scan_run))
        .route("/repair/auto", post(api_post_repair_auto))
        .route("/cleanup/audit", post(api_post_cleanup_audit))
        .route("/cleanup/prune", post(api_post_cleanup_prune))
        .route("/links", get(api_get_links))
        .route("/config/validate", get(api_get_config_validate))
        .route("/doctor", get(api_get_doctor))
        .with_state(state)
}

// ─── API Response types ─────────────────────────────────────────────

#[derive(Serialize)]
pub struct ApiStatus {
    pub active_links: i64,
    pub dead_links: i64,
    pub total_scans: i64,
    pub last_scan: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiDiscoverQuery {
    pub library: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDiscoverItem {
    pub rd_torrent_id: String,
    pub torrent_name: String,
    pub status: String,
    pub size: i64,
    pub parsed_title: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDiscoverResponse {
    pub items: Vec<ApiDiscoverItem>,
    pub status_message: Option<String>,
}

#[derive(Serialize)]
pub struct ApiHealth {
    pub database: String,
    pub tmdb: String,
    pub tvdb: String,
    pub realdebrid: String,
}

#[derive(Serialize)]
pub struct ApiScanResponse {
    pub success: bool,
    pub message: String,
    pub created: u64,
    pub updated: u64,
    pub skipped: u64,
}

#[derive(Serialize)]
pub struct ApiScanJob {
    pub id: i64,
    pub started_at: String,
    pub dry_run: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiScanHistoryQuery {
    pub library: Option<String>,
    pub mode: Option<String>,
    pub search_missing: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiScanAutoAcquireSummary {
    pub requests: i64,
    pub missing_requests: i64,
    pub cutoff_requests: i64,
    pub dry_run_hits: i64,
    pub submitted: i64,
    pub no_result: i64,
    pub blocked: i64,
    pub failed: i64,
    pub completed_linked: i64,
    pub completed_unlinked: i64,
    pub successes: i64,
}

#[derive(Serialize, Deserialize)]
pub struct ApiScanHistoryEntry {
    pub id: i64,
    pub started_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub total_runtime_ms: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub cache_hit_ratio: Option<f64>,
    pub dead_count: i64,
    pub auto_acquire: ApiScanAutoAcquireSummary,
}

#[derive(Serialize, Deserialize)]
pub struct ApiScanRunDetail {
    pub id: i64,
    pub started_at: String,
    pub library_filter: Option<String>,
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
    pub runtime_checks_ms: i64,
    pub library_scan_ms: i64,
    pub source_inventory_ms: i64,
    pub matching_ms: i64,
    pub title_enrichment_ms: i64,
    pub linking_ms: i64,
    pub plex_refresh_ms: i64,
    pub dead_link_sweep_ms: i64,
    pub total_runtime_ms: i64,
    pub cache_hit_ratio: Option<f64>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: String,
}

#[derive(Serialize, Deserialize)]
pub struct ApiRepairResponse {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
}

fn repair_error_status(message: &str) -> StatusCode {
    if message.contains("Refusing repair auto") || message.contains("Unknown library filter") {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

#[derive(Serialize, Deserialize)]
pub struct ApiCleanupAuditResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
}

#[derive(Serialize, Deserialize)]
pub struct ApiCleanupPruneResponse {
    pub success: bool,
    pub message: String,
    pub candidates: usize,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub removed: usize,
    pub quarantined: usize,
    pub skipped: usize,
}

#[derive(Serialize, Deserialize)]
pub struct ApiLink {
    pub id: i64,
    pub source_path: String,
    pub target_path: String,
    pub media_id: String,
    pub media_type: String,
    pub status: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Serialize)]
pub struct ApiConfigValidateResponse {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ApiDoctorCheck {
    pub check: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct ApiDoctorResponse {
    pub all_passed: bool,
    pub checks: Vec<ApiDoctorCheck>,
}

#[derive(Deserialize)]
pub struct ApiScanRequest {
    pub dry_run: Option<bool>,
    pub library: Option<String>,
    pub search_missing: Option<bool>,
}

#[derive(Deserialize)]
pub struct ApiCleanupAuditRequest {
    pub scope: String,
}

#[derive(Deserialize)]
pub struct ApiCleanupPruneRequest {
    pub report_path: String,
    pub token: String,
    pub max_delete: Option<usize>,
}

// ─── API Handlers ───────────────────────────────────────────────────

/// GET /api/v1/status
pub async fn api_get_status(State(state): State<WebState>) -> Json<ApiStatus> {
    let stats = state.database.get_web_stats().await.unwrap_or_default();

    Json(ApiStatus {
        active_links: stats.active_links,
        dead_links: stats.dead_links,
        total_scans: stats.total_scans,
        last_scan: stats.last_scan,
    })
}

/// GET /api/v1/health
pub async fn api_get_health(State(state): State<WebState>) -> Json<ApiHealth> {
    let db_status = if state.database.get_web_stats().await.is_ok() {
        "healthy"
    } else {
        "unhealthy"
    };

    let tmdb_status = if state.config.has_tmdb() {
        "configured"
    } else {
        "missing"
    };

    let tvdb_status = if state.config.has_tvdb() {
        "configured"
    } else {
        "missing"
    };

    let rd_status = if state.config.has_realdebrid() {
        "configured"
    } else {
        "missing"
    };

    Json(ApiHealth {
        database: db_status.to_string(),
        tmdb: tmdb_status.to_string(),
        tvdb: tvdb_status.to_string(),
        realdebrid: rd_status.to_string(),
    })
}

/// GET /api/v1/discover
pub async fn api_get_discover(
    State(state): State<WebState>,
    Query(query): Query<ApiDiscoverQuery>,
) -> Result<Json<ApiDiscoverResponse>, (StatusCode, Json<ApiErrorResponse>)> {
    match load_discovery_snapshot(
        &state.config,
        &state.database,
        query.library.as_deref(),
        true,
    )
    .await
    {
        Ok(snapshot) => Ok(Json(ApiDiscoverResponse {
            items: snapshot
                .items
                .into_iter()
                .map(|item| ApiDiscoverItem {
                    rd_torrent_id: item.rd_torrent_id,
                    torrent_name: item.torrent_name,
                    status: item.status,
                    size: item.size,
                    parsed_title: item.parsed_title,
                })
                .collect(),
            status_message: snapshot.status_message,
        })),
        Err(err) => {
            let message = err.to_string();
            let status = if message.contains("Unknown library filter") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err((status, Json(ApiErrorResponse { error: message })))
        }
    }
}

/// POST /api/v1/scan
pub async fn api_post_scan(
    State(state): State<WebState>,
    Json(req): Json<ApiScanRequest>,
) -> impl IntoResponse {
    info!("API: Triggering scan");

    let dry_run = req.dry_run.unwrap_or(false);
    let library_name = req.library.filter(|l| !l.is_empty());
    let search_missing = req.search_missing.unwrap_or(false);
    let before_latest_id = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.first().map(|record| record.id));

    if let Err(e) = run_scan(
        &state.config,
        &state.database,
        dry_run,
        search_missing,
        OutputFormat::Json,
        library_name.as_deref(),
    )
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiScanResponse {
                success: false,
                message: format!("Scan failed: {}", e),
                created: 0,
                updated: 0,
                skipped: 0,
            }),
        );
    }

    let latest_run = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.into_iter().next())
        .filter(|record| before_latest_id.is_none_or(|before| record.id > before));

    if let Some(record) = latest_run {
        (
            StatusCode::OK,
            Json(ApiScanResponse {
                success: true,
                message: format!(
                    "Scan complete: {} created, {} updated, {} skipped",
                    record.links_created, record.links_updated, record.links_skipped
                ),
                created: record.links_created as u64,
                updated: record.links_updated as u64,
                skipped: record.links_skipped as u64,
            }),
        )
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiScanResponse {
                success: false,
                message: "Scan completed, but no new history row was recorded".to_string(),
                created: 0,
                updated: 0,
                skipped: 0,
            }),
        )
    }
}

/// GET /api/v1/scan/jobs
pub async fn api_get_scan_jobs(State(state): State<WebState>) -> Json<Vec<ApiScanJob>> {
    let history = state
        .database
        .get_scan_history(50)
        .await
        .unwrap_or_default();

    let jobs = history
        .into_iter()
        .map(|h| ApiScanJob {
            id: h.id,
            started_at: h.started_at.to_string(),
            dry_run: h.dry_run,
            library_items_found: h.library_items_found,
            source_items_found: h.source_items_found,
            matches_found: h.matches_found,
            links_created: h.links_created,
            links_updated: h.links_updated,
            dead_marked: h.dead_marked,
        })
        .collect();

    Json(jobs)
}

fn scan_scope_label(record: &ScanHistoryRecord) -> String {
    record
        .library_filter
        .clone()
        .unwrap_or_else(|| "All Libraries".to_string())
}

fn scan_total_runtime_ms(record: &ScanHistoryRecord) -> i64 {
    record.runtime_checks_ms
        + record.library_scan_ms
        + record.source_inventory_ms
        + record.matching_ms
        + record.title_enrichment_ms
        + record.linking_ms
        + record.plex_refresh_ms
        + record.dead_link_sweep_ms
}

fn scan_auto_acquire_successes(record: &ScanHistoryRecord) -> i64 {
    record.auto_acquire_dry_run_hits
        + record.auto_acquire_submitted
        + record.auto_acquire_completed_linked
        + record.auto_acquire_completed_unlinked
}

fn scan_dead_count(record: &ScanHistoryRecord) -> i64 {
    record.dead_marked + record.links_removed
}

fn scan_history_matches_query(record: &ScanHistoryRecord, query: &ApiScanHistoryQuery) -> bool {
    if query
        .library
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|library| record.library_filter.as_deref().unwrap_or_default() != library)
    {
        return false;
    }

    match query.mode.as_deref() {
        Some("dry") if !record.dry_run => return false,
        Some("live") if record.dry_run => return false,
        _ => {}
    }

    match query.search_missing.as_deref() {
        Some("only") if !record.search_missing => return false,
        Some("exclude") if record.search_missing => return false,
        _ => {}
    }

    true
}

fn scan_history_query_limit(query: &ApiScanHistoryQuery) -> i64 {
    query.limit.unwrap_or(25).clamp(1, 200)
}

fn scan_history_fetch_limit(query: &ApiScanHistoryQuery) -> i64 {
    (scan_history_query_limit(query) * 10).clamp(50, 1_000)
}

fn scan_history_entry_from_record(record: ScanHistoryRecord) -> ApiScanHistoryEntry {
    let scope_label = scan_scope_label(&record);
    let total_runtime_ms = scan_total_runtime_ms(&record);
    let dead_count = scan_dead_count(&record);
    let auto_acquire_successes = scan_auto_acquire_successes(&record);
    let started_at = record.started_at.clone();

    ApiScanHistoryEntry {
        id: record.id,
        started_at,
        scope_label,
        dry_run: record.dry_run,
        search_missing: record.search_missing,
        total_runtime_ms,
        matches_found: record.matches_found,
        links_created: record.links_created,
        links_updated: record.links_updated,
        cache_hit_ratio: record.cache_hit_ratio,
        dead_count,
        auto_acquire: ApiScanAutoAcquireSummary {
            requests: record.auto_acquire_requests,
            missing_requests: record.auto_acquire_missing_requests,
            cutoff_requests: record.auto_acquire_cutoff_requests,
            dry_run_hits: record.auto_acquire_dry_run_hits,
            submitted: record.auto_acquire_submitted,
            no_result: record.auto_acquire_no_result,
            blocked: record.auto_acquire_blocked,
            failed: record.auto_acquire_failed,
            completed_linked: record.auto_acquire_completed_linked,
            completed_unlinked: record.auto_acquire_completed_unlinked,
            successes: auto_acquire_successes,
        },
    }
}

fn scan_run_detail_from_record(record: ScanHistoryRecord) -> ApiScanRunDetail {
    let scope_label = scan_scope_label(&record);
    let total_runtime_ms = scan_total_runtime_ms(&record);
    let auto_acquire_successes = scan_auto_acquire_successes(&record);
    let started_at = record.started_at.clone();

    ApiScanRunDetail {
        id: record.id,
        started_at,
        library_filter: record.library_filter.clone(),
        scope_label,
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
        runtime_checks_ms: record.runtime_checks_ms,
        library_scan_ms: record.library_scan_ms,
        source_inventory_ms: record.source_inventory_ms,
        matching_ms: record.matching_ms,
        title_enrichment_ms: record.title_enrichment_ms,
        linking_ms: record.linking_ms,
        plex_refresh_ms: record.plex_refresh_ms,
        dead_link_sweep_ms: record.dead_link_sweep_ms,
        total_runtime_ms,
        cache_hit_ratio: record.cache_hit_ratio,
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

/// GET /api/v1/scan/history
pub async fn api_get_scan_history(
    State(state): State<WebState>,
    Query(query): Query<ApiScanHistoryQuery>,
) -> Json<Vec<ApiScanHistoryEntry>> {
    let limit = scan_history_query_limit(&query);
    let fetch_limit = scan_history_fetch_limit(&query);

    let history = state
        .database
        .get_scan_history(fetch_limit)
        .await
        .unwrap_or_default();

    let items = history
        .into_iter()
        .filter(|record| scan_history_matches_query(record, &query))
        .take(limit as usize)
        .map(scan_history_entry_from_record)
        .collect();

    Json(items)
}

/// GET /api/v1/scan/:id
pub async fn api_get_scan_run(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> Result<Json<ApiScanRunDetail>, (StatusCode, Json<ApiErrorResponse>)> {
    match state.database.get_scan_run(id).await {
        Ok(Some(run)) => Ok(Json(scan_run_detail_from_record(run))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiErrorResponse {
                error: format!("Scan run {} not found", id),
            }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to load scan run {}: {}", id, e),
            }),
        )),
    }
}

/// POST /api/v1/repair/auto
pub async fn api_post_repair_auto(State(state): State<WebState>) -> impl IntoResponse {
    info!("API: Running auto repair");

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
            (
                StatusCode::OK,
                Json(ApiRepairResponse {
                    success: true,
                    message,
                    repaired,
                    failed,
                }),
            )
        }
        Err(err) => {
            let message = err.to_string();
            (
                repair_error_status(&message),
                Json(ApiRepairResponse {
                    success: false,
                    message,
                    repaired: 0,
                    failed: 0,
                }),
            )
        }
    }
}

/// POST /api/v1/cleanup/audit
pub async fn api_post_cleanup_audit(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupAuditRequest>,
) -> impl IntoResponse {
    info!("API: Running cleanup audit");

    let scope = match CleanupScope::parse(&req.scope) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Invalid scope: {}", e),
                    report_path: String::new(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                }),
            );
        }
    };

    let auditor = CleanupAuditor::new_with_progress(&state.config, &state.database, false);

    let default_output = state.config.backup.path.join(format!(
        "cleanup-audit-{}-{}.json",
        req.scope,
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    let report_path = match auditor.run_audit(scope, Some(&default_output)).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Audit failed: {}", e),
                    report_path: String::new(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                }),
            );
        }
    };

    let report_json = match std::fs::read_to_string(&report_path) {
        Ok(json) => json,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Audit report read failed: {}", e),
                    report_path: report_path.to_string_lossy().to_string(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                }),
            );
        }
    };

    let report: CleanupReport = match serde_json::from_str(&report_json) {
        Ok(report) => report,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Audit report parse failed: {}", e),
                    report_path: report_path.to_string_lossy().to_string(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                }),
            );
        }
    };

    (
        StatusCode::OK,
        Json(ApiCleanupAuditResponse {
            success: true,
            message: "Audit complete".to_string(),
            report_path: report_path.to_string_lossy().to_string(),
            total_findings: report.summary.total_findings,
            critical: report.summary.critical,
            high: report.summary.high,
            warning: report.summary.warning,
        }),
    )
}

/// POST /api/v1/cleanup/prune
pub async fn api_post_cleanup_prune(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupPruneRequest>,
) -> impl IntoResponse {
    info!("API: Applying prune");

    match cleanup_audit::run_prune(
        &state.config,
        &state.database,
        std::path::Path::new(&req.report_path),
        true,
        req.max_delete,
        Some(&req.token),
    )
    .await
    {
        Ok(outcome) => (
            StatusCode::OK,
            Json(ApiCleanupPruneResponse {
                success: true,
                message: "Prune applied".to_string(),
                candidates: outcome.candidates,
                managed_candidates: outcome.managed_candidates,
                foreign_candidates: outcome.foreign_candidates,
                removed: outcome.removed,
                quarantined: outcome.quarantined,
                skipped: outcome.skipped,
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiCleanupPruneResponse {
                success: false,
                message: format!("Prune failed: {}", e),
                candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                removed: 0,
                quarantined: 0,
                skipped: 0,
            }),
        ),
    }
}

/// GET /api/v1/links
pub async fn api_get_links(
    State(state): State<WebState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<ApiLink>> {
    let limit: i64 = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(100);
    let status_filter = params.get("status").map(|s| s.as_str());

    let links = match status_filter {
        Some("dead") => state
            .database
            .get_dead_links_limited(limit)
            .await
            .unwrap_or_default(),
        _ => state
            .database
            .get_active_links_limited(limit)
            .await
            .unwrap_or_default(),
    }
    .into_iter()
    .map(|l| ApiLink {
        id: l.id.unwrap_or(0),
        source_path: l.source_path.to_string_lossy().to_string(),
        target_path: l.target_path.to_string_lossy().to_string(),
        media_id: l.media_id,
        media_type: format!("{:?}", l.media_type),
        status: format!("{:?}", l.status),
        created_at: l.created_at,
        updated_at: l.updated_at,
    })
    .collect();

    Json(links)
}

/// GET /api/v1/config/validate
pub async fn api_get_config_validate(
    State(state): State<WebState>,
) -> Json<ApiConfigValidateResponse> {
    let report = state.config.validate();

    Json(ApiConfigValidateResponse {
        valid: report.errors.is_empty(),
        errors: report.errors,
        warnings: report.warnings,
    })
}

/// GET /api/v1/doctor
pub async fn api_get_doctor(State(state): State<WebState>) -> Json<ApiDoctorResponse> {
    let mut checks = vec![];

    for lib in &state.config.libraries {
        let exists = lib.path.exists();
        checks.push(ApiDoctorCheck {
            check: format!("Library '{}' exists", lib.name),
            passed: exists,
            message: if exists {
                format!("{}: exists", lib.path.display())
            } else {
                format!("{}: NOT FOUND", lib.path.display())
            },
        });
    }

    for source in &state.config.sources {
        let exists = source.path.exists();
        checks.push(ApiDoctorCheck {
            check: format!("Source '{}' exists", source.name),
            passed: exists,
            message: if exists {
                format!("{}: exists", source.path.display())
            } else {
                format!("{}: NOT FOUND", source.path.display())
            },
        });
    }

    let db_ok = state.database.get_web_stats().await.is_ok();
    checks.push(ApiDoctorCheck {
        check: "Database connection".to_string(),
        passed: db_ok,
        message: if db_ok { "Connected" } else { "Failed" }.to_string(),
    });

    let all_passed = checks.iter().all(|c| c.passed);

    Json(ApiDoctorResponse { all_passed, checks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use axum::response::IntoResponse;
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
        SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::{Database, ScanRunRecord};
    use crate::models::{LinkRecord, LinkStatus, MediaType};

    fn test_config(root: &Path) -> Config {
        let library_root = root.join("library");
        let source_root = root.join("source");
        let backup_root = root.join("backups");
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&backup_root).unwrap();

        Config {
            libraries: vec![LibraryConfig {
                name: "Anime".to_string(),
                path: library_root,
                media_type: crate::models::MediaType::Tv,
                content_type: Some(ContentType::Anime),
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: source_root,
                media_type: "auto".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            backup: BackupConfig {
                path: backup_root,
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

    async fn test_state() -> WebState {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::mem::forget(dir);
        let cfg = test_config(&root);
        let db = Database::new(&cfg.db_path).await.unwrap();

        db.record_scan_run(&ScanRunRecord {
            dry_run: true,
            library_filter: Some("Anime".to_string()),
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
            runtime_checks_ms: 200,
            library_scan_ms: 12_400,
            source_inventory_ms: 148_200,
            matching_ms: 86_700,
            title_enrichment_ms: 16_400,
            linking_ms: 20_500,
            plex_refresh_ms: 3_100,
            dead_link_sweep_ms: 700,
            cache_hit_ratio: Some(0.94),
            candidate_slots: 77_624_480,
            scored_candidates: 3_171,
            exact_id_hits: 0,
            auto_acquire_requests: 10,
            auto_acquire_missing_requests: 5,
            auto_acquire_cutoff_requests: 5,
            auto_acquire_dry_run_hits: 4,
            auto_acquire_submitted: 0,
            auto_acquire_no_result: 2,
            auto_acquire_blocked: 0,
            auto_acquire_failed: 0,
            auto_acquire_completed_linked: 0,
            auto_acquire_completed_unlinked: 0,
        })
        .await
        .unwrap();

        WebState::new(cfg, db)
    }

    fn make_scan_history_query(
        library: Option<&str>,
        mode: Option<&str>,
        search_missing: Option<&str>,
        limit: Option<i64>,
    ) -> ApiScanHistoryQuery {
        ApiScanHistoryQuery {
            library: library.map(|value| value.to_string()),
            mode: mode.map(|value| value.to_string()),
            search_missing: search_missing.map(|value| value.to_string()),
            limit,
        }
    }

    #[tokio::test]
    async fn api_get_scan_run_returns_full_detail() {
        let ctx = test_state().await;
        let history = ctx.database.get_scan_history(1).await.unwrap();
        let run_id = history[0].id;

        let response = api_get_scan_run(State(ctx), Path(run_id)).await;
        let body = match response {
            Ok(json) => json.into_response(),
            Err((status, _json)) => panic!("unexpected error {}", status),
        };

        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let json: ApiScanRunDetail = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json.id, run_id);
        assert_eq!(json.library_filter.as_deref(), Some("Anime"));
        assert_eq!(json.scope_label, "Anime");
        assert_eq!(json.total_runtime_ms, 288_200);
        assert_eq!(json.auto_acquire_successes, 4);
        assert_eq!(json.auto_acquire_requests, 10);
        assert!(json.search_missing);
    }

    #[tokio::test]
    async fn api_get_scan_run_returns_not_found_for_missing_id() {
        let ctx = test_state().await;

        let response = api_get_scan_run(State(ctx), Path(9999)).await;
        let (status, body) = match response {
            Ok(_) => panic!("expected not found"),
            Err(err) => err,
        };

        assert_eq!(status, StatusCode::NOT_FOUND);
        let body = body.into_response();
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "Scan run 9999 not found");
    }

    #[tokio::test]
    async fn api_get_scan_history_filters_and_summarizes_runs() {
        let ctx = test_state().await;

        ctx.database
            .record_scan_run(&ScanRunRecord {
                dry_run: false,
                library_filter: Some("Movies".to_string()),
                search_missing: false,
                library_items_found: 12,
                source_items_found: 34,
                matches_found: 56,
                links_created: 7,
                links_updated: 8,
                dead_marked: 2,
                links_removed: 1,
                runtime_checks_ms: 10,
                library_scan_ms: 20,
                source_inventory_ms: 30,
                matching_ms: 40,
                title_enrichment_ms: 50,
                linking_ms: 60,
                plex_refresh_ms: 70,
                dead_link_sweep_ms: 80,
                cache_hit_ratio: Some(0.5),
                auto_acquire_requests: 9,
                auto_acquire_missing_requests: 4,
                auto_acquire_cutoff_requests: 3,
                auto_acquire_dry_run_hits: 1,
                auto_acquire_submitted: 2,
                auto_acquire_no_result: 1,
                auto_acquire_blocked: 1,
                auto_acquire_failed: 1,
                auto_acquire_completed_linked: 1,
                auto_acquire_completed_unlinked: 1,
                ..Default::default()
            })
            .await
            .unwrap();

        let response = api_get_scan_history(
            State(ctx),
            Query(make_scan_history_query(
                Some("Anime"),
                Some("dry"),
                Some("only"),
                Some(10),
            )),
        )
        .await;

        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let json: Vec<ApiScanHistoryEntry> = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json.len(), 1);
        let run = &json[0];
        assert_eq!(run.scope_label, "Anime");
        assert!(run.dry_run);
        assert!(run.search_missing);
        assert_eq!(run.matches_found, 9924);
        assert_eq!(run.links_created, 446);
        assert_eq!(run.links_updated, 164);
        assert_eq!(run.total_runtime_ms, 288_200);
        assert_eq!(run.dead_count, 17);
        assert_eq!(run.cache_hit_ratio, Some(0.94));
        assert_eq!(run.auto_acquire.requests, 10);
        assert_eq!(run.auto_acquire.successes, 4);
    }

    #[tokio::test]
    async fn api_get_scan_history_respects_mode_and_limit_filters() {
        let ctx = test_state().await;

        ctx.database
            .record_scan_run(&ScanRunRecord {
                dry_run: false,
                library_filter: Some("Movies".to_string()),
                search_missing: false,
                library_items_found: 1,
                source_items_found: 2,
                matches_found: 3,
                links_created: 4,
                links_updated: 5,
                ..Default::default()
            })
            .await
            .unwrap();

        let response = api_get_scan_history(
            State(ctx),
            Query(make_scan_history_query(
                None,
                Some("live"),
                Some("exclude"),
                Some(1),
            )),
        )
        .await;

        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let json: Vec<ApiScanHistoryEntry> = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json.len(), 1);
        assert_eq!(json[0].scope_label, "Movies");
        assert!(!json[0].dry_run);
        assert!(!json[0].search_missing);
    }

    #[tokio::test]
    async fn api_get_discover_returns_cached_gap_items() {
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
        let response = api_get_discover(
            State(state),
            Query(ApiDiscoverQuery {
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .expect("discover response");
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let json: ApiDiscoverResponse = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json.items.len(), 1);
        assert_eq!(json.items[0].rd_torrent_id, "rd-1");
        assert_eq!(json.items[0].parsed_title, "Missing Show");
        assert!(json
            .status_message
            .as_deref()
            .unwrap_or_default()
            .contains("cached results only"));
    }

    #[tokio::test]
    async fn api_get_discover_rejects_invalid_library_filter() {
        let ctx = test_state().await;
        let response = api_get_discover(
            State(ctx),
            Query(ApiDiscoverQuery {
                library: Some("Nope".to_string()),
            }),
        )
        .await;

        let (status, body) = response.expect_err("expected bad request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let body = body.into_response();
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Unknown library filter"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_post_repair_auto_runs_real_repair_flow() {
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
        let response = api_post_repair_auto(State(state.clone()))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiRepairResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.success);
        assert_eq!(json.repaired, 1);
        assert_eq!(json.failed, 0);
        assert!(json.message.contains("1 repaired, 0 unrepairable"));

        let repaired = state.database.get_active_links().await.unwrap();
        let repaired = repaired
            .into_iter()
            .find(|link| link.target_path == target_path)
            .expect("expected repaired active link");
        assert_eq!(repaired.source_path, replacement);
    }

    #[tokio::test]
    async fn api_get_links_respects_limit_without_loading_full_result_in_handler() {
        let ctx = test_state().await;
        ctx.database
            .insert_link(&LinkRecord {
                id: None,
                source_path: PathBuf::from("/mnt/rd/show/ep01.mkv"),
                target_path: PathBuf::from("/plex/show/S01E01.mkv"),
                media_id: "tvdb-1".to_string(),
                media_type: MediaType::Tv,
                status: LinkStatus::Active,
                created_at: None,
                updated_at: None,
            })
            .await
            .unwrap();
        ctx.database
            .insert_link(&LinkRecord {
                id: None,
                source_path: PathBuf::from("/mnt/rd/show/ep02.mkv"),
                target_path: PathBuf::from("/plex/show/S01E02.mkv"),
                media_id: "tvdb-1".to_string(),
                media_type: MediaType::Tv,
                status: LinkStatus::Active,
                created_at: None,
                updated_at: None,
            })
            .await
            .unwrap();

        let response = api_get_links(
            State(ctx),
            Query(std::collections::HashMap::from([(
                "limit".to_string(),
                "1".to_string(),
            )])),
        )
        .await;
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let links: Vec<ApiLink> = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, "/plex/show/S01E02.mkv");
    }

    #[tokio::test]
    async fn api_get_config_validate_uses_full_config_validation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::mem::forget(dir);
        let mut cfg = test_config(&root);
        cfg.web.enabled = true;
        cfg.web.bind_address = "0.0.0.0".to_string();
        let db = Database::new(&cfg.db_path).await.unwrap();
        let state = WebState::new(cfg, db);

        let Json(response) = api_get_config_validate(State(state)).await;

        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("web.bind_address=0.0.0.0 exposes the web UI")));
    }

    #[tokio::test]
    async fn api_post_cleanup_audit_returns_real_report_summary() {
        let ctx = test_state().await;

        let response = api_post_cleanup_audit(
            State(ctx),
            Json(ApiCleanupAuditRequest {
                scope: "anime".to_string(),
            }),
        )
        .await;
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let response: ApiCleanupAuditResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(response.success);
        assert_eq!(response.message, "Audit complete");
        assert!(!response.report_path.is_empty());

        let report_json = std::fs::read_to_string(&response.report_path).unwrap();
        let report: CleanupReport = serde_json::from_str(&report_json).unwrap();

        assert_eq!(response.total_findings, report.summary.total_findings);
        assert_eq!(response.critical, report.summary.critical);
        assert_eq!(response.high, report.summary.high);
        assert_eq!(response.warning, report.summary.warning);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_post_cleanup_prune_returns_real_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.cleanup.prune.quarantine_path = dir.path().join("quarantine");

        let library_root = cfg.libraries[0].path.clone();
        let source_root = cfg.sources[0].path.clone();
        let source_path = source_root.join("source.mkv");
        let symlink_path = library_root.join("Show - S01E01.mkv");
        std::fs::write(&source_path, "video").unwrap();
        std::os::unix::fs::symlink(&source_path, &symlink_path).unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source_path.clone(),
            target_path: symlink_path.clone(),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let report = CleanupReport {
            version: 1,
            created_at: chrono::Utc::now(),
            scope: CleanupScope::Anime,
            findings: vec![crate::cleanup_audit::CleanupFinding {
                symlink_path: symlink_path.clone(),
                source_path: source_path.clone(),
                media_id: "tvdb-1".to_string(),
                severity: crate::cleanup_audit::FindingSeverity::High,
                confidence: 1.0,
                reasons: vec![crate::cleanup_audit::FindingReason::BrokenSource],
                parsed: crate::cleanup_audit::ParsedContext {
                    library_title: "Show".to_string(),
                    parsed_title: "Show".to_string(),
                    season: Some(1),
                    episode: Some(1),
                },
                alternate_match: None,
                db_tracked: false,
                ownership: crate::cleanup_audit::CleanupOwnership::Foreign,
            }],
            summary: crate::cleanup_audit::CleanupSummary {
                total_findings: 1,
                critical: 0,
                high: 1,
                warning: 0,
            },
        };
        let report_path = dir.path().join("report.json");
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview = crate::cleanup_audit::run_prune(&cfg, &db, &report_path, false, None, None)
            .await
            .unwrap();

        let state = WebState::new(cfg, db);
        let response = api_post_cleanup_prune(
            State(state),
            Json(ApiCleanupPruneRequest {
                report_path: report_path.to_string_lossy().to_string(),
                token: preview.confirmation_token,
                max_delete: None,
            }),
        )
        .await;
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::OK);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let response: ApiCleanupPruneResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(response.success);
        assert_eq!(response.candidates, 1);
        assert_eq!(response.managed_candidates, 1);
        assert_eq!(response.foreign_candidates, 0);
        assert_eq!(response.removed, 1);
        assert_eq!(response.quarantined, 0);
        assert_eq!(response.skipped, 0);
        assert!(!symlink_path.exists());
    }

    #[tokio::test]
    async fn api_post_cleanup_audit_rejects_invalid_scope_with_bad_request() {
        let ctx = test_state().await;

        let response = api_post_cleanup_audit(
            State(ctx),
            Json(ApiCleanupAuditRequest {
                scope: "nope".to_string(),
            }),
        )
        .await;
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let response: ApiCleanupAuditResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!response.success);
        assert!(response.message.contains("Invalid scope"));
    }

    #[tokio::test]
    async fn api_post_cleanup_prune_returns_bad_request_for_invalid_token() {
        let ctx = test_state().await;
        let response = api_post_cleanup_prune(
            State(ctx),
            Json(ApiCleanupPruneRequest {
                report_path: "/tmp/does-not-exist.json".to_string(),
                token: "bad-token".to_string(),
                max_delete: None,
            }),
        )
        .await;
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let response: ApiCleanupPruneResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!response.success);
        assert!(response.message.contains("Prune failed"));
    }
}
