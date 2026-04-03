//! JSON API endpoints for automation

use axum::{
    extract::{Path, Query, State},
    http::{header::CONTENT_TYPE, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::backup::BackupManager;
use crate::cleanup_audit::{self, CleanupReport, CleanupScope};
use crate::commands::cleanup::{
    anime_remediation_block_reason_catalog, apply_anime_remediation_plan_with_refresh,
    apply_cleanup_prune_with_refresh, assess_anime_remediation_groups,
    filter_anime_remediation_groups, preview_anime_remediation_plan,
    render_anime_remediation_groups_tsv, summarize_anime_remediation_blocked_reasons,
    AnimeRemediationGroupFilters, AnimeRemediationPlanGroup, CleanupPruneApplyArgs,
};
use crate::commands::config::validate_config_report;
use crate::commands::discover::load_discovery_snapshot;
use crate::commands::doctor::{collect_doctor_checks, DoctorCheckMode};
use crate::commands::report::build_anime_remediation_report;
use crate::commands::selected_libraries;
use crate::config::Config;
use crate::db::{Database, ScanHistoryRecord};
use crate::media_servers::{
    configured_refresh_backends, deferred_refresh_summary, LibraryInvalidationOutcome,
    LibraryInvalidationServerOutcome,
};

use super::{
    clamp_link_list_limit, latest_cleanup_report_created_at, resolve_cleanup_report_path,
    should_surface_cleanup_audit_outcome, should_surface_scan_outcome, WebState,
};

/// Create the API router
pub fn create_router(state: WebState) -> Router<WebState> {
    Router::new()
        .route("/status", get(api_get_status))
        .route("/health", get(api_get_health))
        .route("/discover", get(api_get_discover))
        .route("/scan", post(api_post_scan))
        .route("/scan/status", get(api_get_scan_status))
        .route("/scan/jobs", get(api_get_scan_jobs))
        .route("/scan/history", get(api_get_scan_history))
        .route("/scan/:id", get(api_get_scan_run))
        .route("/report/anime-remediation", get(api_get_anime_remediation))
        .route(
            "/cleanup/anime-remediation/preview",
            post(api_post_anime_remediation_preview),
        )
        .route(
            "/cleanup/anime-remediation/apply",
            post(api_post_anime_remediation_apply),
        )
        .route("/repair/auto", post(api_post_repair_auto))
        .route("/repair/status", get(api_get_repair_status))
        .route("/cleanup/audit", post(api_post_cleanup_audit))
        .route("/cleanup/audit/status", get(api_get_cleanup_audit_status))
        .route("/cleanup/audit/jobs", get(api_get_cleanup_audit_jobs))
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
    #[serde(default)]
    pub refresh_cache: bool,
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
    pub plex: String,
    pub emby: String,
    pub jellyfin: String,
    pub refresh_backends: Vec<String>,
    pub deferred_refresh: ApiDeferredRefreshSummary,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ApiDeferredRefreshSummary {
    pub pending_targets: usize,
    pub servers: Vec<ApiDeferredRefreshServerSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDeferredRefreshServerSummary {
    pub server: String,
    pub queued_targets: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiScanResponse {
    pub success: bool,
    pub message: String,
    pub created: u64,
    pub updated: u64,
    pub skipped: u64,
    pub running: bool,
    pub started_at: Option<String>,
    pub scope_label: Option<String>,
    pub search_missing: bool,
    pub dry_run: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiScanJob {
    pub id: i64,
    pub status: String,
    pub started_at: String,
    pub scope_label: String,
    pub search_missing: bool,
    pub dry_run: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiScanOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiScanStatusResponse {
    pub active_job: Option<ApiScanJob>,
    pub last_outcome: Option<ApiScanOutcome>,
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
pub struct ApiPlexRefreshSummary {
    pub runtime_ms: i64,
    pub requested_paths: i64,
    pub unique_paths: i64,
    pub planned_batches: i64,
    pub coalesced_batches: i64,
    pub coalesced_paths: i64,
    pub refreshed_batches: i64,
    pub refreshed_paths_covered: i64,
    pub skipped_batches: i64,
    pub unresolved_paths: i64,
    pub capped_batches: i64,
    pub aborted_due_to_cap: bool,
    pub deferred_due_to_lock: bool,
    pub failed_batches: i64,
}

#[derive(Serialize, Deserialize)]
pub struct ApiMediaServerRefreshServer {
    pub server: String,
    pub requested_targets: i64,
    pub refresh: ApiPlexRefreshSummary,
}

#[derive(Serialize, Deserialize)]
pub struct ApiSkipReasonCount {
    pub reason: String,
    pub count: i64,
}

#[derive(Serialize, Deserialize)]
pub struct ApiSkipEventSample {
    pub event_at: String,
    pub action: String,
    pub reason: String,
    pub target_path: String,
    pub source_path: Option<String>,
    pub media_id: Option<String>,
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
    pub plex_refresh: ApiPlexRefreshSummary,
    pub media_server_refresh: Vec<ApiMediaServerRefreshServer>,
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
    pub skip_reasons: Vec<ApiSkipReasonCount>,
    pub skip_event_samples: Vec<ApiSkipEventSample>,
    pub runtime_checks_ms: i64,
    pub library_scan_ms: i64,
    pub source_inventory_ms: i64,
    pub matching_ms: i64,
    pub title_enrichment_ms: i64,
    pub linking_ms: i64,
    pub plex_refresh_ms: i64,
    pub plex_refresh: ApiPlexRefreshSummary,
    pub media_server_refresh: Vec<ApiMediaServerRefreshServer>,
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

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiAnimeRemediationQuery {
    pub plex_db: Option<String>,
    pub full: Option<bool>,
    pub state: Option<String>,
    pub reason: Option<String>,
    pub title: Option<String>,
    pub format: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiAnimeRemediationResponse {
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
    pub reason_filter: Option<String>,
    pub title_filter: Option<String>,
    pub blocked_reason_summary: Vec<ApiAnimeRemediationBlockedReasonSummary>,
    pub available_blocked_reasons: Vec<ApiAnimeRemediationBlockedReasonSummary>,
    pub groups: Vec<AnimeRemediationPlanGroup>,
}

#[derive(Debug, Deserialize)]
pub struct ApiAnimeRemediationPreviewRequest {
    pub plex_db: Option<String>,
    pub title: Option<String>,
    pub library: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiAnimeRemediationBlockedReasonSummary {
    pub code: String,
    pub label: String,
    pub recommended_action: String,
    pub groups: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiAnimeRemediationPreviewResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub plex_db_path: String,
    pub title_filter: Option<String>,
    pub total_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub cleanup_candidates: usize,
    pub confirmation_token: String,
    pub blocked_reason_summary: Vec<ApiAnimeRemediationBlockedReasonSummary>,
}

#[derive(Debug, Deserialize)]
pub struct ApiAnimeRemediationApplyRequest {
    pub report_path: String,
    pub token: String,
    pub max_delete: Option<usize>,
    pub library: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiAnimeRemediationApplyResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub total_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub candidates: usize,
    pub quarantined: usize,
    pub removed: usize,
    pub skipped: usize,
    pub safety_snapshot: Option<String>,
    pub media_server_invalidation: Option<LibraryInvalidationOutcome>,
}

fn api_blocked_reason_summary(
    summary: &[crate::commands::cleanup::AnimeRemediationBlockedReasonSummary],
) -> Vec<ApiAnimeRemediationBlockedReasonSummary> {
    summary
        .iter()
        .map(|entry| ApiAnimeRemediationBlockedReasonSummary {
            code: entry.code.as_str().to_string(),
            label: entry.label.clone(),
            recommended_action: entry.recommended_action.clone(),
            groups: entry.groups,
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
pub struct ApiRepairResponse {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stale: usize,
    pub running: bool,
    pub started_at: Option<String>,
    pub scope_label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiRepairJob {
    pub status: String,
    pub started_at: String,
    pub scope_label: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiRepairOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stale: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiRepairStatusResponse {
    pub active_job: Option<ApiRepairJob>,
    pub last_outcome: Option<ApiRepairOutcome>,
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
    pub running: bool,
    pub started_at: Option<String>,
    pub scope_label: Option<String>,
    pub libraries_label: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiCleanupAuditJob {
    pub status: String,
    pub started_at: String,
    pub scope_label: String,
    pub libraries_label: String,
}

#[derive(Serialize, Deserialize)]
pub struct ApiCleanupAuditOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub libraries_label: String,
    pub success: bool,
    pub message: String,
    pub report_path: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiCleanupAuditStatusResponse {
    pub active_job: Option<ApiCleanupAuditJob>,
    pub last_outcome: Option<ApiCleanupAuditOutcome>,
}

#[derive(Serialize, Deserialize)]
pub struct ApiCleanupPruneResponse {
    pub success: bool,
    pub message: String,
    pub candidates: usize,
    pub blocked_candidates: usize,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub blocked_reason_summary: Vec<ApiPruneBlockedReasonSummary>,
    pub removed: usize,
    pub quarantined: usize,
    pub skipped: usize,
    pub media_server_invalidation: Option<LibraryInvalidationOutcome>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiPruneBlockedReasonSummary {
    pub code: String,
    pub label: String,
    pub candidates: usize,
    pub recommended_action: String,
}

fn api_prune_blocked_reason_summary(
    summary: &[crate::cleanup_audit::PruneBlockedReasonSummary],
) -> Vec<ApiPruneBlockedReasonSummary> {
    summary
        .iter()
        .map(|entry| ApiPruneBlockedReasonSummary {
            code: entry.code.to_string(),
            label: entry.label.clone(),
            candidates: entry.candidates,
            recommended_action: entry.recommended_action.clone(),
        })
        .collect()
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDoctorCheck {
    pub check: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
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
    let refresh_backends = configured_refresh_backends(&state.config)
        .into_iter()
        .map(|server| server.service_key().to_string())
        .collect();
    let deferred_refresh = deferred_refresh_summary(&state.config)
        .map(|summary| ApiDeferredRefreshSummary {
            pending_targets: summary.pending_targets,
            servers: summary
                .servers
                .into_iter()
                .map(|entry| ApiDeferredRefreshServerSummary {
                    server: entry.server.service_key().to_string(),
                    queued_targets: entry.queued_targets,
                })
                .collect(),
        })
        .unwrap_or_default();
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

    let plex_status = if state.config.has_plex() {
        "configured"
    } else {
        "missing"
    };

    let emby_status = if state.config.has_emby() {
        "configured"
    } else {
        "missing"
    };

    let jellyfin_status = if state.config.has_jellyfin() {
        "configured"
    } else {
        "missing"
    };

    Json(ApiHealth {
        database: db_status.to_string(),
        tmdb: tmdb_status.to_string(),
        tvdb: tvdb_status.to_string(),
        realdebrid: rd_status.to_string(),
        plex: plex_status.to_string(),
        emby: emby_status.to_string(),
        jellyfin: jellyfin_status.to_string(),
        refresh_backends,
        deferred_refresh,
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
        query.refresh_cache,
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
            status_message: snapshot.status_message.or_else(|| {
                (!query.refresh_cache).then(|| {
                    "Showing cached RD results only. Set refresh_cache=true when you want a slower live cache sync first."
                        .to_string()
                })
            }),
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

    match state
        .start_scan(dry_run, search_missing, library_name.clone())
        .await
    {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(ApiScanResponse {
                success: true,
                message: format!(
                    "Scan started in background for {}. Poll /api/v1/scan/jobs or /api/v1/scan/history for completion.",
                    job.scope_label
                ),
                created: 0,
                updated: 0,
                skipped: 0,
                running: true,
                started_at: Some(job.started_at),
                scope_label: Some(job.scope_label),
                search_missing: job.search_missing,
                dry_run: job.dry_run,
            }),
        ),
        Err(e) => {
            let active_scan = state.active_scan().await;
            (
                StatusCode::CONFLICT,
                Json(ApiScanResponse {
                    success: false,
                    message: format!("Scan not started: {}", e),
                    created: 0,
                    updated: 0,
                    skipped: 0,
                    running: active_scan.is_some(),
                    started_at: active_scan.as_ref().map(|job| job.started_at.clone()),
                    scope_label: active_scan.as_ref().map(|job| job.scope_label.clone()),
                    search_missing: active_scan.as_ref().is_some_and(|job| job.search_missing),
                    dry_run: active_scan.as_ref().is_some_and(|job| job.dry_run),
                }),
            )
        }
    }
}

fn api_scan_job_from_active(job: crate::web::ActiveScanJob) -> ApiScanJob {
    ApiScanJob {
        id: 0,
        status: "running".to_string(),
        started_at: job.started_at,
        scope_label: job.scope_label,
        search_missing: job.search_missing,
        dry_run: job.dry_run,
        library_items_found: 0,
        source_items_found: 0,
        matches_found: 0,
        links_created: 0,
        links_updated: 0,
        dead_marked: 0,
    }
}

/// GET /api/v1/scan/status
pub async fn api_get_scan_status(State(state): State<WebState>) -> Json<ApiScanStatusResponse> {
    let latest_run_started_at = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.into_iter().next().map(|run| run.started_at));

    Json(ApiScanStatusResponse {
        active_job: state.active_scan().await.map(api_scan_job_from_active),
        last_outcome: state
            .last_scan_outcome()
            .await
            .filter(|outcome| {
                should_surface_scan_outcome(outcome, latest_run_started_at.as_deref())
            })
            .map(|outcome| ApiScanOutcome {
                finished_at: outcome.finished_at,
                scope_label: outcome.scope_label,
                dry_run: outcome.dry_run,
                search_missing: outcome.search_missing,
                success: outcome.success,
                message: outcome.message,
            }),
    })
}

/// GET /api/v1/scan/jobs
pub async fn api_get_scan_jobs(State(state): State<WebState>) -> Json<Vec<ApiScanJob>> {
    let history = state
        .database
        .get_scan_history(50)
        .await
        .unwrap_or_default();

    let mut jobs = Vec::new();
    if let Some(active_scan) = state.active_scan().await {
        jobs.push(api_scan_job_from_active(active_scan));
    }

    jobs.extend(history.into_iter().map(|h| {
        ApiScanJob {
            id: h.id,
            status: "completed".to_string(),
            started_at: h.started_at.to_string(),
            scope_label: h
                .library_filter
                .clone()
                .unwrap_or_else(|| "All Libraries".to_string()),
            search_missing: h.search_missing,
            dry_run: h.dry_run,
            library_items_found: h.library_items_found,
            source_items_found: h.source_items_found,
            matches_found: h.matches_found,
            links_created: h.links_created,
            links_updated: h.links_updated,
            dead_marked: h.dead_marked,
        }
    }));

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

fn media_server_refresh_entries(
    record: &ScanHistoryRecord,
) -> Vec<LibraryInvalidationServerOutcome> {
    record
        .media_server_refresh_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<LibraryInvalidationServerOutcome>>(json).ok())
        .unwrap_or_default()
}

fn plex_refresh_summary_from_record(record: &ScanHistoryRecord) -> ApiPlexRefreshSummary {
    let deferred_due_to_lock = media_server_refresh_entries(record)
        .iter()
        .any(|entry| entry.refresh.deferred_due_to_lock);
    ApiPlexRefreshSummary {
        runtime_ms: record.plex_refresh_ms,
        requested_paths: record.plex_refresh_requested_paths,
        unique_paths: record.plex_refresh_unique_paths,
        planned_batches: record.plex_refresh_planned_batches,
        coalesced_batches: record.plex_refresh_coalesced_batches,
        coalesced_paths: record.plex_refresh_coalesced_paths,
        refreshed_batches: record.plex_refresh_refreshed_batches,
        refreshed_paths_covered: record.plex_refresh_refreshed_paths_covered,
        skipped_batches: record.plex_refresh_skipped_batches,
        unresolved_paths: record.plex_refresh_unresolved_paths,
        capped_batches: record.plex_refresh_capped_batches,
        aborted_due_to_cap: record.plex_refresh_aborted_due_to_cap,
        deferred_due_to_lock,
        failed_batches: record.plex_refresh_failed_batches,
    }
}

fn api_refresh_summary_from_telemetry(
    telemetry: &crate::media_servers::LibraryRefreshTelemetry,
) -> ApiPlexRefreshSummary {
    ApiPlexRefreshSummary {
        runtime_ms: 0,
        requested_paths: telemetry.requested_paths as i64,
        unique_paths: telemetry.unique_paths as i64,
        planned_batches: telemetry.planned_batches as i64,
        coalesced_batches: telemetry.coalesced_batches as i64,
        coalesced_paths: telemetry.coalesced_paths as i64,
        refreshed_batches: telemetry.refreshed_batches as i64,
        refreshed_paths_covered: telemetry.refreshed_paths_covered as i64,
        skipped_batches: telemetry.skipped_batches as i64,
        unresolved_paths: telemetry.unresolved_paths as i64,
        capped_batches: telemetry.capped_batches as i64,
        aborted_due_to_cap: telemetry.aborted_due_to_cap,
        deferred_due_to_lock: telemetry.deferred_due_to_lock,
        failed_batches: telemetry.failed_batches as i64,
    }
}

fn media_server_refresh_from_record(
    record: &ScanHistoryRecord,
) -> Vec<ApiMediaServerRefreshServer> {
    media_server_refresh_entries(record)
        .into_iter()
        .map(|entry| ApiMediaServerRefreshServer {
            server: entry.server.to_string(),
            requested_targets: entry.requested_targets as i64,
            refresh: api_refresh_summary_from_telemetry(&entry.refresh),
        })
        .collect()
}

fn skip_reasons_from_record(record: &ScanHistoryRecord) -> Vec<ApiSkipReasonCount> {
    let mut entries = record
        .skip_reason_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<std::collections::BTreeMap<String, i64>>(json).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(reason, count)| ApiSkipReasonCount { reason, count })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.reason.cmp(&b.reason)));
    entries
}

fn api_skip_event_samples(
    events: Vec<crate::db::LinkEventHistoryRecord>,
) -> Vec<ApiSkipEventSample> {
    events
        .into_iter()
        .map(|event| ApiSkipEventSample {
            event_at: event.event_at,
            action: event.action,
            reason: event.note.unwrap_or_else(|| "unknown".to_string()),
            target_path: event.target_path.display().to_string(),
            source_path: event.source_path.map(|path| path.display().to_string()),
            media_id: event.media_id,
        })
        .collect()
}

fn scan_history_entry_from_record(record: ScanHistoryRecord) -> ApiScanHistoryEntry {
    let scope_label = scan_scope_label(&record);
    let total_runtime_ms = scan_total_runtime_ms(&record);
    let dead_count = scan_dead_count(&record);
    let auto_acquire_successes = scan_auto_acquire_successes(&record);
    let started_at = record.started_at.clone();
    let plex_refresh = plex_refresh_summary_from_record(&record);
    let media_server_refresh = media_server_refresh_from_record(&record);

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
        plex_refresh,
        media_server_refresh,
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

fn scan_run_detail_from_record(
    record: ScanHistoryRecord,
    skip_event_samples: Vec<ApiSkipEventSample>,
) -> ApiScanRunDetail {
    let scope_label = scan_scope_label(&record);
    let total_runtime_ms = scan_total_runtime_ms(&record);
    let auto_acquire_successes = scan_auto_acquire_successes(&record);
    let started_at = record.started_at.clone();
    let plex_refresh = plex_refresh_summary_from_record(&record);
    let media_server_refresh = media_server_refresh_from_record(&record);
    let skip_reasons = skip_reasons_from_record(&record);

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
        skip_reasons,
        skip_event_samples,
        runtime_checks_ms: record.runtime_checks_ms,
        library_scan_ms: record.library_scan_ms,
        source_inventory_ms: record.source_inventory_ms,
        matching_ms: record.matching_ms,
        title_enrichment_ms: record.title_enrichment_ms,
        linking_ms: record.linking_ms,
        plex_refresh_ms: record.plex_refresh_ms,
        plex_refresh,
        media_server_refresh,
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

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        let path = std::path::PathBuf::from(requested);
        return path.exists().then_some(path);
    }

    default_plex_db_candidates()
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.exists())
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
        Ok(Some(run)) => {
            let skip_event_samples = match run.run_token.as_deref() {
                Some(token) => state
                    .database
                    .get_skip_link_events_for_run_token(token, 25)
                    .await
                    .map(api_skip_event_samples)
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(ApiErrorResponse {
                                error: format!("Failed to load scan run {} skip events: {}", id, e),
                            }),
                        )
                    })?,
                None => Vec::new(),
            };

            Ok(Json(scan_run_detail_from_record(run, skip_event_samples)))
        }
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

/// GET /api/v1/report/anime-remediation
pub async fn api_get_anime_remediation(
    State(state): State<WebState>,
    Query(query): Query<ApiAnimeRemediationQuery>,
) -> Result<Response, (StatusCode, Json<ApiErrorResponse>)> {
    let filters = AnimeRemediationGroupFilters::parse(
        query.state.as_deref(),
        query.reason.as_deref(),
        query.title.as_deref(),
    )
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: format!("Invalid anime remediation filters: {}", e),
            }),
        )
    })?;

    let Some(plex_db_path) = resolve_plex_db_path(query.plex_db.as_deref()) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "Plex DB path is required or must exist at a standard local path"
                    .to_string(),
            }),
        ));
    };

    let full = query.full.unwrap_or(false);
    let wants_tsv = matches!(query.format.as_deref(), Some("tsv"));
    if let Some(format) = query.format.as_deref() {
        if format != "json" && format != "tsv" {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiErrorResponse {
                    error: format!(
                        "Invalid anime remediation format '{}' (expected json or tsv)",
                        format
                    ),
                }),
            ));
        }
    }
    match build_anime_remediation_report(&state.config, &state.database, &plex_db_path, full).await
    {
        Ok(Some(report)) => {
            let assessed_groups = assess_anime_remediation_groups(&report.groups).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorResponse {
                        error: format!("Failed to assess anime remediation backlog: {}", e),
                    }),
                )
            })?;
            let filtered_groups =
                filter_anime_remediation_groups(assessed_groups.clone(), &filters);
            if wants_tsv {
                let body = render_anime_remediation_groups_tsv(&filtered_groups);
                return Ok((
                    [(CONTENT_TYPE, "text/tab-separated-values; charset=utf-8")],
                    body,
                )
                    .into_response());
            }

            let eligible_groups = filtered_groups
                .iter()
                .filter(|group| group.eligible)
                .count();
            let blocked_groups = filtered_groups.len().saturating_sub(eligible_groups);
            Ok(Json(ApiAnimeRemediationResponse {
                generated_at: report.generated_at,
                plex_db_path: plex_db_path.to_string_lossy().to_string(),
                full,
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
                reason_filter: filters.block_code.map(|code| code.as_str().to_string()),
                title_filter: filters.title_contains.clone(),
                blocked_reason_summary: api_blocked_reason_summary(
                    &summarize_anime_remediation_blocked_reasons(&filtered_groups),
                ),
                available_blocked_reasons: api_blocked_reason_summary(
                    &anime_remediation_block_reason_catalog(),
                ),
                groups: filtered_groups,
            })
            .into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiErrorResponse {
                error: "No anime libraries are configured for remediation reporting".to_string(),
            }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to build anime remediation report: {}", e),
            }),
        )),
    }
}

/// POST /api/v1/cleanup/anime-remediation/preview
pub async fn api_post_anime_remediation_preview(
    State(state): State<WebState>,
    Json(req): Json<ApiAnimeRemediationPreviewRequest>,
) -> impl IntoResponse {
    let Some(plex_db_path) = resolve_plex_db_path(req.plex_db.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiAnimeRemediationPreviewResponse {
                success: false,
                message: "Anime remediation preview failed: Plex DB path is required or must exist at a standard local path".to_string(),
                report_path: String::new(),
                plex_db_path: String::new(),
                title_filter: req.title.clone(),
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                cleanup_candidates: 0,
                confirmation_token: String::new(),
                blocked_reason_summary: Vec::new(),
            }),
        );
    };

    match preview_anime_remediation_plan(
        &state.config,
        &state.database,
        req.library.as_deref(),
        &plex_db_path,
        req.title.as_deref(),
        None,
    )
    .await
    {
        Ok((plan, report_path)) => (
            StatusCode::OK,
            Json(ApiAnimeRemediationPreviewResponse {
                success: true,
                message: format!(
                    "Anime remediation preview saved. Review {} before applying.",
                    report_path.display()
                ),
                report_path: report_path
                    .canonicalize()
                    .unwrap_or(report_path)
                    .to_string_lossy()
                    .to_string(),
                plex_db_path: plan.plex_db_path.to_string_lossy().to_string(),
                title_filter: plan.title_filter.clone(),
                total_groups: plan.total_groups,
                eligible_groups: plan.eligible_groups,
                blocked_groups: plan.blocked_groups,
                cleanup_candidates: plan.cleanup_candidates,
                confirmation_token: plan.confirmation_token.clone(),
                blocked_reason_summary: api_blocked_reason_summary(&plan.blocked_reason_summary),
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ApiAnimeRemediationPreviewResponse {
                success: false,
                message: format!("Anime remediation preview failed: {}", err),
                report_path: String::new(),
                plex_db_path: plex_db_path.to_string_lossy().to_string(),
                title_filter: req.title.clone(),
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                cleanup_candidates: 0,
                confirmation_token: String::new(),
                blocked_reason_summary: Vec::new(),
            }),
        ),
    }
}

/// POST /api/v1/cleanup/anime-remediation/apply
pub async fn api_post_anime_remediation_apply(
    State(state): State<WebState>,
    Json(req): Json<ApiAnimeRemediationApplyRequest>,
) -> impl IntoResponse {
    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &req.report_path)
    {
        Ok(path) => path,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiAnimeRemediationApplyResponse {
                    success: false,
                    message: format!("Anime remediation apply failed: {}", err),
                    report_path: String::new(),
                    total_groups: 0,
                    eligible_groups: 0,
                    blocked_groups: 0,
                    candidates: 0,
                    quarantined: 0,
                    removed: 0,
                    skipped: 0,
                    safety_snapshot: None,
                    media_server_invalidation: None,
                }),
            );
        }
    };

    match apply_anime_remediation_plan_with_refresh(
        &state.config,
        &state.database,
        req.library.as_deref(),
        &report_path,
        Some(&req.token),
        req.max_delete,
        false,
    )
    .await
    {
        Ok((plan, outcome, safety_snapshot, invalidation)) => (
            StatusCode::OK,
            Json(ApiAnimeRemediationApplyResponse {
                success: true,
                message: "Anime remediation applied".to_string(),
                report_path: report_path.to_string_lossy().to_string(),
                total_groups: plan.total_groups,
                eligible_groups: plan.eligible_groups,
                blocked_groups: plan.blocked_groups,
                candidates: outcome.candidates,
                quarantined: outcome.quarantined,
                removed: outcome.removed,
                skipped: outcome.skipped,
                safety_snapshot: safety_snapshot
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string()),
                media_server_invalidation: Some(invalidation),
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ApiAnimeRemediationApplyResponse {
                success: false,
                message: format!("Anime remediation apply failed: {}", err),
                report_path: report_path.to_string_lossy().to_string(),
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                candidates: 0,
                quarantined: 0,
                removed: 0,
                skipped: 0,
                safety_snapshot: None,
                media_server_invalidation: None,
            }),
        ),
    }
}

/// POST /api/v1/repair/auto
pub async fn api_post_repair_auto(State(state): State<WebState>) -> impl IntoResponse {
    info!("API: Starting background auto repair");

    match state.start_repair().await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(ApiRepairResponse {
                success: true,
                message: format!(
                    "Repair started in background for {}. Poll /api/v1/repair/status for the finished outcome.",
                    job.scope_label
                ),
                repaired: 0,
                failed: 0,
                skipped: 0,
                stale: 0,
                running: true,
                started_at: Some(job.started_at),
                scope_label: Some(job.scope_label),
            }),
        ),
        Err(err) => {
            let active_repair = state.active_repair().await;
            (
                StatusCode::CONFLICT,
                Json(ApiRepairResponse {
                    success: false,
                    message: format!("Repair not started: {}", err),
                    repaired: 0,
                    failed: 0,
                    skipped: 0,
                    stale: 0,
                    running: active_repair.is_some(),
                    started_at: active_repair.as_ref().map(|job| job.started_at.clone()),
                    scope_label: active_repair.map(|job| job.scope_label),
                }),
            )
        }
    }
}

/// GET /api/v1/repair/status
pub async fn api_get_repair_status(State(state): State<WebState>) -> Json<ApiRepairStatusResponse> {
    Json(ApiRepairStatusResponse {
        active_job: state.active_repair().await.map(|job| ApiRepairJob {
            status: "running".to_string(),
            started_at: job.started_at,
            scope_label: job.scope_label,
        }),
        last_outcome: state
            .last_repair_outcome()
            .await
            .map(|outcome| ApiRepairOutcome {
                finished_at: outcome.finished_at,
                scope_label: outcome.scope_label,
                success: outcome.success,
                message: outcome.message,
                repaired: outcome.repaired,
                failed: outcome.failed,
                skipped: outcome.skipped,
                stale: outcome.stale,
            }),
    })
}

/// POST /api/v1/cleanup/audit
pub async fn api_post_cleanup_audit(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupAuditRequest>,
) -> impl IntoResponse {
    info!("API: Starting cleanup audit");

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
                    running: false,
                    started_at: None,
                    scope_label: None,
                    libraries_label: None,
                }),
            );
        }
    };

    match state.start_cleanup_audit(scope, Vec::new()).await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(ApiCleanupAuditResponse {
                success: true,
                message: format!(
                    "Cleanup audit started in background for {}. Poll /api/v1/cleanup/audit/jobs or inspect /cleanup for the finished report.",
                    job.scope_label
                ),
                report_path: String::new(),
                total_findings: 0,
                critical: 0,
                high: 0,
                warning: 0,
                running: true,
                started_at: Some(job.started_at),
                scope_label: Some(job.scope_label),
                libraries_label: Some(job.libraries_label),
            }),
        ),
        Err(e) => {
            let active_audit = state.active_cleanup_audit().await;
            (
                StatusCode::CONFLICT,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Cleanup audit not started: {}", e),
                    report_path: String::new(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    running: active_audit.is_some(),
                    started_at: active_audit.as_ref().map(|job| job.started_at.clone()),
                    scope_label: active_audit.as_ref().map(|job| job.scope_label.clone()),
                    libraries_label: active_audit.as_ref().map(|job| job.libraries_label.clone()),
                }),
            )
        }
    }
}

/// GET /api/v1/cleanup/audit/status
pub async fn api_get_cleanup_audit_status(
    State(state): State<WebState>,
) -> Json<ApiCleanupAuditStatusResponse> {
    let latest_report_created_at = latest_cleanup_report_created_at(&state.config.backup.path);

    Json(ApiCleanupAuditStatusResponse {
        active_job: state
            .active_cleanup_audit()
            .await
            .map(|active_audit| ApiCleanupAuditJob {
                status: "running".to_string(),
                started_at: active_audit.started_at,
                scope_label: active_audit.scope_label,
                libraries_label: active_audit.libraries_label,
            }),
        last_outcome: state
            .last_cleanup_audit_outcome()
            .await
            .filter(|outcome| {
                should_surface_cleanup_audit_outcome(outcome, latest_report_created_at.as_deref())
            })
            .map(|outcome| ApiCleanupAuditOutcome {
                finished_at: outcome.finished_at,
                scope_label: outcome.scope_label,
                libraries_label: outcome.libraries_label,
                success: outcome.success,
                message: outcome.message,
                report_path: outcome.report_path,
            }),
    })
}

/// GET /api/v1/cleanup/audit/jobs
pub async fn api_get_cleanup_audit_jobs(
    State(state): State<WebState>,
) -> Json<Vec<ApiCleanupAuditJob>> {
    let mut jobs = Vec::new();
    if let Some(active_audit) = state.active_cleanup_audit().await {
        jobs.push(ApiCleanupAuditJob {
            status: "running".to_string(),
            started_at: active_audit.started_at,
            scope_label: active_audit.scope_label,
            libraries_label: active_audit.libraries_label,
        });
    }

    Json(jobs)
}

/// POST /api/v1/cleanup/prune
pub async fn api_post_cleanup_prune(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupPruneRequest>,
) -> impl IntoResponse {
    info!("API: Applying prune");

    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &req.report_path)
    {
        Ok(path) => path,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiCleanupPruneResponse {
                    success: false,
                    message: format!("Prune failed: {}", err),
                    candidates: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    blocked_reason_summary: Vec::new(),
                    removed: 0,
                    quarantined: 0,
                    skipped: 0,
                    media_server_invalidation: None,
                }),
            );
        }
    };

    let selected = match selected_libraries(state.config.as_ref(), None) {
        Ok(selected) => selected,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiCleanupPruneResponse {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    candidates: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    blocked_reason_summary: Vec::new(),
                    removed: 0,
                    quarantined: 0,
                    skipped: 0,
                    media_server_invalidation: None,
                }),
            );
        }
    };

    match apply_cleanup_prune_with_refresh(
        &state.config,
        &state.database,
        CleanupPruneApplyArgs {
            libraries: &selected,
            report_path: &report_path,
            include_legacy_anime_roots: false,
            max_delete: req.max_delete,
            confirm_token: Some(&req.token),
            emit_text: false,
        },
    )
    .await
    {
        Ok((outcome, invalidation)) => (
            StatusCode::OK,
            Json(ApiCleanupPruneResponse {
                success: true,
                message: "Prune applied".to_string(),
                candidates: outcome.candidates,
                blocked_candidates: outcome.blocked_candidates,
                managed_candidates: outcome.managed_candidates,
                foreign_candidates: outcome.foreign_candidates,
                blocked_reason_summary: api_prune_blocked_reason_summary(
                    &outcome.blocked_reason_summary,
                ),
                removed: outcome.removed,
                quarantined: outcome.quarantined,
                skipped: outcome.skipped,
                media_server_invalidation: Some(invalidation),
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiCleanupPruneResponse {
                success: false,
                message: format!("Prune failed: {}", e),
                candidates: 0,
                blocked_candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                blocked_reason_summary: Vec::new(),
                removed: 0,
                quarantined: 0,
                skipped: 0,
                media_server_invalidation: None,
            }),
        ),
    }
}

/// GET /api/v1/links
pub async fn api_get_links(
    State(state): State<WebState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<ApiLink>> {
    let limit = clamp_link_list_limit(params.get("limit").and_then(|l| l.parse().ok()));
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
    let report = validate_config_report(&state.config).await;

    Json(ApiConfigValidateResponse {
        valid: report.errors.is_empty(),
        errors: report.errors,
        warnings: report.warnings,
    })
}

/// GET /api/v1/doctor
pub async fn api_get_doctor(State(state): State<WebState>) -> Json<ApiDoctorResponse> {
    let checks = collect_doctor_checks(&state.config, &state.database, DoctorCheckMode::ReadOnly)
        .await
        .into_iter()
        .map(|check| ApiDoctorCheck {
            check: check.name,
            passed: check.ok,
            message: check.detail,
        })
        .collect::<Vec<_>>();

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
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Executor;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use tempfile::TempDir;

    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::{Database, ScanRunRecord};
    use crate::models::{LinkRecord, LinkStatus, MediaType};
    use crate::web::{
        ActiveCleanupAuditJob, ActiveScanJob, LastCleanupAuditOutcome, LastScanOutcome,
    };

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

    async fn test_state() -> WebState {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::mem::forget(dir);
        let cfg = test_config(&root);
        let db = Database::new(&cfg.db_path).await.unwrap();

        db.record_scan_run(&ScanRunRecord {
            dry_run: true,
            library_filter: Some("Anime".to_string()),
            run_token: Some("scan-run-test".to_string()),
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
            skip_reason_json: Some(
                r#"{"already_correct":6200,"source_missing_before_link":3044,"ambiguous_match":70}"#
                    .to_string(),
            ),
            runtime_checks_ms: 200,
            library_scan_ms: 12_400,
            source_inventory_ms: 148_200,
            matching_ms: 86_700,
            title_enrichment_ms: 16_400,
            linking_ms: 20_500,
            plex_refresh_ms: 3_100,
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
            media_server_refresh_json: Some(
                r#"[{"server":"plex","requested_targets":5,"refresh":{"requested_paths":12,"unique_paths":10,"planned_batches":5,"coalesced_batches":2,"coalesced_paths":7,"refreshed_batches":4,"refreshed_paths_covered":12,"skipped_batches":1,"unresolved_paths":0,"capped_batches":1,"aborted_due_to_cap":true,"deferred_due_to_lock":false,"failed_batches":0}},{"server":"emby","requested_targets":12,"refresh":{"requested_paths":12,"unique_paths":12,"planned_batches":1,"coalesced_batches":0,"coalesced_paths":0,"refreshed_batches":1,"refreshed_paths_covered":12,"skipped_batches":0,"unresolved_paths":0,"capped_batches":0,"aborted_due_to_cap":false,"deferred_due_to_lock":true,"failed_batches":0}}]"#.to_string(),
            ),
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
        db.record_link_event_fields_with_run_token(
            Some("scan-run-test"),
            "skipped",
            &root.join("anime/Show A (2024) {tvdb-1}/Season 01/Show A - S01E01.mkv"),
            Some(&root.join("rd/Show.A.S01E01.mkv")),
            Some("tvdb-1"),
            Some("source_missing_before_link"),
        )
        .await
        .unwrap();

        WebState::new(cfg, db)
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
    async fn api_get_health_reports_media_servers_as_optional() {
        let ctx = test_state().await;
        let Json(health) = api_get_health(State(ctx.clone())).await;

        assert_eq!(health.database, "healthy");
        assert_eq!(health.tmdb, "missing");
        assert_eq!(health.tvdb, "missing");
        assert_eq!(health.realdebrid, "missing");
        assert_eq!(health.plex, "missing");
        assert_eq!(health.emby, "missing");
        assert_eq!(health.jellyfin, "missing");
        assert!(health.refresh_backends.is_empty());
        assert_eq!(health.deferred_refresh.pending_targets, 0);
        assert!(health.deferred_refresh.servers.is_empty());
    }

    #[tokio::test]
    async fn api_get_health_reports_active_refresh_backends() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::mem::forget(dir);
        let mut cfg = test_config(&root);
        cfg.plex.url = "http://plex.local".to_string();
        cfg.plex.token = "token".to_string();
        cfg.plex.refresh_enabled = true;
        cfg.emby.url = "http://emby.local".to_string();
        cfg.emby.api_key = "key".to_string();
        cfg.emby.refresh_enabled = true;
        cfg.jellyfin.url = "http://jellyfin.local".to_string();
        cfg.jellyfin.api_key = "key".to_string();
        cfg.jellyfin.refresh_enabled = false;
        std::fs::write(
            cfg.backup.path.join(".media-server-refresh.queue.json"),
            r#"{
              "servers": [
                { "server": "plex", "paths": ["/library/anime", "/library/anime-2"] },
                { "server": "emby", "paths": ["/library/anime"] }
              ]
            }"#,
        )
        .unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        let ctx = WebState::new(cfg, db);
        let Json(health) = api_get_health(State(ctx)).await;

        assert_eq!(health.plex, "configured");
        assert_eq!(health.emby, "configured");
        assert_eq!(health.jellyfin, "configured");
        assert_eq!(health.refresh_backends, vec!["plex", "emby"]);
        assert_eq!(health.deferred_refresh.pending_targets, 3);
        assert_eq!(health.deferred_refresh.servers.len(), 2);
        assert!(health
            .deferred_refresh
            .servers
            .iter()
            .any(|entry| entry.server == "plex" && entry.queued_targets == 2));
        assert!(health
            .deferred_refresh
            .servers
            .iter()
            .any(|entry| entry.server == "emby" && entry.queued_targets == 1));
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
        assert_eq!(json.plex_refresh.runtime_ms, 3_100);
        assert_eq!(json.plex_refresh.planned_batches, 5);
        assert_eq!(json.plex_refresh.refreshed_batches, 4);
        assert_eq!(json.plex_refresh.capped_batches, 1);
        assert!(json.plex_refresh.aborted_due_to_cap);
        assert!(json.plex_refresh.deferred_due_to_lock);
        assert_eq!(json.skip_reasons.len(), 3);
        assert_eq!(json.skip_reasons[0].reason, "already_correct");
        assert_eq!(json.skip_reasons[0].count, 6200);
        assert_eq!(json.skip_reasons[1].reason, "source_missing_before_link");
        assert_eq!(json.skip_reasons[1].count, 3044);
        assert_eq!(json.skip_reasons[2].reason, "ambiguous_match");
        assert_eq!(json.skip_reasons[2].count, 70);
        assert_eq!(json.skip_event_samples.len(), 1);
        assert_eq!(
            json.skip_event_samples[0].reason,
            "source_missing_before_link"
        );
        assert_eq!(json.skip_event_samples[0].action, "skipped");
        assert!(json.skip_event_samples[0]
            .target_path
            .contains("Show A - S01E01.mkv"));
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
                skip_reason_json: None,
                runtime_checks_ms: 10,
                library_scan_ms: 20,
                source_inventory_ms: 30,
                matching_ms: 40,
                title_enrichment_ms: 50,
                linking_ms: 60,
                plex_refresh_ms: 70,
                media_server_refresh_json: None,
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
        assert_eq!(run.plex_refresh.planned_batches, 5);
        assert_eq!(run.plex_refresh.refreshed_batches, 4);
        assert_eq!(run.plex_refresh.capped_batches, 1);
        assert!(run.plex_refresh.aborted_due_to_cap);
        assert!(run.plex_refresh.deferred_due_to_lock);
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
                skip_reason_json: None,
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
    async fn api_get_scan_jobs_includes_active_background_scan() {
        let ctx = test_state().await;
        ctx.set_active_scan_for_test(Some(ActiveScanJob {
            started_at: "2026-03-29 23:59:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            dry_run: true,
            search_missing: true,
        }))
        .await;

        let response = api_get_scan_jobs(State(ctx)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        let jobs = json.as_array().unwrap();

        assert_eq!(jobs[0]["status"], "running");
        assert_eq!(jobs[0]["scope_label"], "Anime");
        assert_eq!(jobs[0]["search_missing"], true);
        assert_eq!(jobs[0]["dry_run"], true);
    }

    #[tokio::test]
    async fn api_get_scan_status_includes_last_outcome() {
        let ctx = test_state().await;
        ctx.set_last_scan_outcome_for_test(Some(LastScanOutcome {
            finished_at: "2099-03-29 23:58:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            dry_run: false,
            search_missing: true,
            success: false,
            message: "RD cache sync failed".to_string(),
        }))
        .await;

        let response = api_get_scan_status(State(ctx)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiScanStatusResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.active_job.is_none());
        let last_outcome = json.last_outcome.expect("expected last scan outcome");
        assert!(!last_outcome.success);
        assert_eq!(last_outcome.scope_label, "Anime");
        assert!(last_outcome.search_missing);
        assert_eq!(last_outcome.message, "RD cache sync failed");
    }

    #[tokio::test]
    async fn api_get_scan_status_hides_stale_failed_outcome_when_newer_run_exists() {
        let ctx = test_state().await;
        ctx.set_last_scan_outcome_for_test(Some(LastScanOutcome {
            finished_at: "2026-03-29 09:58:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            dry_run: false,
            search_missing: true,
            success: false,
            message: "stale failure".to_string(),
        }))
        .await;

        let response = api_get_scan_status(State(ctx)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiScanStatusResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.last_outcome.is_none());
    }

    #[tokio::test]
    async fn api_post_scan_rejects_when_background_scan_is_already_running() {
        let ctx = test_state().await;
        ctx.set_active_scan_for_test(Some(ActiveScanJob {
            started_at: "2026-03-29 23:59:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            dry_run: true,
            search_missing: false,
        }))
        .await;

        let response = api_post_scan(
            State(ctx),
            Json(ApiScanRequest {
                dry_run: Some(false),
                library: Some("Anime".to_string()),
                search_missing: Some(true),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiScanResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(!json.success);
        assert!(json.running);
        assert_eq!(json.scope_label.as_deref(), Some("Anime"));
        assert!(json.message.contains("already running"));
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
                refresh_cache: false,
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
            .contains("Real-Debrid API key not configured"));
        assert!(json
            .status_message
            .as_deref()
            .unwrap_or_default()
            .contains("live refresh is unavailable"));
    }

    #[tokio::test]
    async fn api_get_discover_rejects_invalid_library_filter() {
        let ctx = test_state().await;
        let response = api_get_discover(
            State(ctx),
            Query(ApiDiscoverQuery {
                library: Some("Nope".to_string()),
                refresh_cache: false,
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

    #[tokio::test]
    async fn api_get_doctor_uses_full_doctor_checks() {
        let ctx = test_state().await;
        let response = api_get_doctor(State(ctx)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiDoctorResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json
            .checks
            .iter()
            .any(|check| check.check == "db_schema_version"));
        assert!(json
            .checks
            .iter()
            .any(|check| check.check == "config_validation"));
        assert!(json
            .checks
            .iter()
            .any(|check| check.check == "cleanup.prune.enforce_policy"));
    }

    #[tokio::test]
    async fn api_get_doctor_does_not_create_missing_backup_dir_in_read_only_mode() {
        let ctx = test_state().await;
        let backup_dir = ctx.config.backup.path.clone();
        std::fs::remove_dir(&backup_dir).unwrap();
        assert!(!backup_dir.exists());

        let response = api_get_doctor(State(ctx)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiDoctorResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.checks.iter().any(|check| {
            check.check == "backup_dir"
                && check
                    .message
                    .contains("write probe skipped in read-only mode")
        }));
        assert!(!backup_dir.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_get_doctor_flags_existing_non_writable_backup_dir() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let backup_dir = cfg.backup.path.clone();
        std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let db = Database::new(&cfg.db_path).await.unwrap();
        let state = WebState::new(cfg, db);

        let response = api_get_doctor(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiDoctorResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.checks.iter().any(|check| {
            check.check == "backup_dir"
                && !check.passed
                && check.message.contains("denies write or traverse")
                && check.message.contains("mode=555")
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_post_repair_auto_starts_background_repair_flow() {
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
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiRepairResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.success);
        assert!(json.running);
        assert_eq!(json.repaired, 0);
        assert_eq!(json.failed, 0);
        assert!(json.message.contains("Repair started in background"));

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

        let status = api_get_repair_status(State(state.clone())).await;
        let status_json = serde_json::to_value(status.0).unwrap();
        assert!(status_json["active_job"].is_null());
        assert_eq!(status_json["last_outcome"]["repaired"], 1);
        assert_eq!(status_json["last_outcome"]["failed"], 0);

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
        cfg.web.allow_remote = true;
        let db = Database::new(&cfg.db_path).await.unwrap();
        let state = WebState::new(cfg, db);

        let Json(response) = api_get_config_validate(State(state)).await;

        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("web.allow_remote=true")));
    }

    #[tokio::test]
    async fn api_post_cleanup_audit_accepts_background_job() {
        let ctx = test_state().await;

        let response = api_post_cleanup_audit(
            State(ctx),
            Json(ApiCleanupAuditRequest {
                scope: "anime".to_string(),
            }),
        )
        .await;
        let body = response.into_response();
        assert_eq!(body.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let response: ApiCleanupAuditResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(response.success);
        assert!(response
            .message
            .contains("Cleanup audit started in background"));
        assert!(response.running);
        assert!(response.report_path.is_empty());
        assert_eq!(response.scope_label.as_deref(), Some("Anime"));
        assert_eq!(response.libraries_label.as_deref(), Some("Anime"));
        assert!(response.started_at.is_some());
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
                legacy_anime_root: None,
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
        let report_path = cfg.backup.path.join("report.json");
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview =
            crate::cleanup_audit::run_prune(&cfg, &db, &report_path, false, false, None, None)
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
        assert_eq!(response.blocked_candidates, 0);
        assert_eq!(response.managed_candidates, 1);
        assert_eq!(response.foreign_candidates, 0);
        assert!(response.blocked_reason_summary.is_empty());
        assert_eq!(response.removed, 1);
        assert_eq!(response.quarantined, 0);
        assert_eq!(response.skipped, 0);
        assert!(!symlink_path.exists());
    }

    #[tokio::test]
    async fn api_post_cleanup_prune_reports_blocked_policy_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.cleanup.prune.quarantine_foreign = false;

        let library_root = cfg.libraries[0].path.clone();
        let source_root = cfg.sources[0].path.clone();
        let source_path = source_root.join("source.mkv");
        let symlink_path = library_root.join("Show - S01E01.mkv");
        std::fs::write(&source_path, "video").unwrap();
        std::os::unix::fs::symlink(&source_path, &symlink_path).unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
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
                legacy_anime_root: None,
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
        let report_path = cfg.backup.path.join("report.json");
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview =
            crate::cleanup_audit::run_prune(&cfg, &db, &report_path, false, false, None, None)
                .await
                .unwrap();
        assert_eq!(preview.candidates, 0);
        assert_eq!(preview.blocked_candidates, 1);

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
        assert_eq!(body.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(body.into_body(), usize::MAX).await.unwrap();
        let response: ApiCleanupPruneResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(!response.success);
        assert!(response.message.contains("no actionable candidates remain"));
        assert_eq!(response.candidates, 0);
        assert_eq!(response.blocked_candidates, 0);
        assert!(response.blocked_reason_summary.is_empty());
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
    async fn api_get_cleanup_audit_jobs_includes_active_background_audit() {
        let ctx = test_state().await;
        ctx.set_active_cleanup_audit_for_test(Some(ActiveCleanupAuditJob {
            started_at: "2026-03-29 23:59:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            libraries_label: "Anime".to_string(),
        }))
        .await;

        let response = api_get_cleanup_audit_jobs(State(ctx)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        let jobs = json.as_array().unwrap();

        assert_eq!(jobs[0]["status"], "running");
        assert_eq!(jobs[0]["scope_label"], "Anime");
        assert_eq!(jobs[0]["libraries_label"], "Anime");
    }

    #[tokio::test]
    async fn api_get_cleanup_audit_status_includes_last_outcome() {
        let ctx = test_state().await;
        ctx.set_last_cleanup_audit_outcome_for_test(Some(LastCleanupAuditOutcome {
            finished_at: "2026-03-29 23:58:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            libraries_label: "Anime".to_string(),
            success: false,
            message: "source root unhealthy".to_string(),
            report_path: None,
        }))
        .await;

        let response = api_get_cleanup_audit_status(State(ctx))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiCleanupAuditStatusResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.active_job.is_none());
        let last_outcome = json
            .last_outcome
            .expect("expected last cleanup audit outcome");
        assert!(!last_outcome.success);
        assert_eq!(last_outcome.scope_label, "Anime");
        assert_eq!(last_outcome.message, "source root unhealthy");
    }

    #[tokio::test]
    async fn api_get_cleanup_audit_status_hides_stale_failed_outcome_when_newer_report_exists() {
        let ctx = test_state().await;
        let report_path = ctx
            .config
            .backup
            .path
            .join("cleanup-audit-anime-20260329.json");
        let report = CleanupReport {
            version: 1,
            created_at: chrono::Utc::now(),
            scope: CleanupScope::Anime,
            findings: vec![],
            summary: cleanup_audit::CleanupSummary {
                total_findings: 1,
                critical: 0,
                high: 1,
                warning: 0,
            },
        };
        std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();

        ctx.set_last_cleanup_audit_outcome_for_test(Some(LastCleanupAuditOutcome {
            finished_at: "2026-03-29 09:58:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            libraries_label: "Anime".to_string(),
            success: false,
            message: "stale cleanup failure".to_string(),
            report_path: None,
        }))
        .await;

        let response = api_get_cleanup_audit_status(State(ctx))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiCleanupAuditStatusResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.last_outcome.is_none());
    }

    #[tokio::test]
    async fn api_post_cleanup_audit_rejects_when_background_audit_is_already_running() {
        let ctx = test_state().await;
        ctx.set_active_cleanup_audit_for_test(Some(ActiveCleanupAuditJob {
            started_at: "2026-03-29 23:59:00 UTC".to_string(),
            scope_label: "Anime".to_string(),
            libraries_label: "Anime".to_string(),
        }))
        .await;

        let response = api_post_cleanup_audit(
            State(ctx),
            Json(ApiCleanupAuditRequest {
                scope: "anime".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiCleanupAuditResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(!json.success);
        assert!(json.message.contains("Cleanup audit not started"));
        assert!(json.running);
        assert_eq!(json.scope_label.as_deref(), Some("Anime"));
        assert_eq!(json.libraries_label.as_deref(), Some("Anime"));
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

    #[tokio::test]
    async fn api_post_cleanup_prune_rejects_report_outside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();
        let outside_report = dir.path().join("outside-report.json");
        std::fs::write(&outside_report, "{}").unwrap();

        let state = WebState::new(cfg, db);
        let response = api_post_cleanup_prune(
            State(state),
            Json(ApiCleanupPruneRequest {
                report_path: outside_report.to_string_lossy().to_string(),
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
        assert!(response
            .message
            .contains("Cleanup report must be inside the configured backup directory"));
    }

    #[tokio::test]
    async fn api_get_anime_remediation_returns_ranked_groups() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut cfg = test_config(&root);
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

        cfg.libraries[0].path = anime_root;
        let state = WebState::new(cfg, db);
        let response = api_get_anime_remediation(
            State(state),
            Query(ApiAnimeRemediationQuery {
                plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                full: Some(true),
                state: None,
                reason: None,
                title: None,
                format: None,
            }),
        )
        .await
        .unwrap()
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationResponse = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json.filesystem_mixed_root_groups, 1);
        assert_eq!(json.plex_hama_anidb_tvdb_groups, 1);
        assert_eq!(json.correlated_hama_split_groups, 1);
        assert_eq!(json.remediation_groups, 1);
        assert_eq!(json.returned_groups, 1);
        assert_eq!(json.visible_groups, 1);
        assert_eq!(json.eligible_groups, 1);
        assert_eq!(json.blocked_groups, 0);
        assert!(json.blocked_reason_summary.is_empty());
        assert_eq!(json.groups[0].recommended_tagged_root.path, tagged_root);
        assert_eq!(json.groups[0].legacy_roots[0].path, legacy_root);
    }

    #[tokio::test]
    async fn api_get_anime_remediation_can_export_filtered_tsv() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut cfg = test_config(&root);
        let anime_root = cfg.libraries[0].path.clone();
        let tagged_root = anime_root.join("Show A (2024) {tvdb-1}");
        let legacy_root = anime_root.join("Show A");

        std::fs::create_dir_all(tagged_root.join("Season 01")).unwrap();
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
        std::fs::write(legacy_root.join("Season 01/Show A - S01E01.mkv"), b"video").unwrap();

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
        let response = api_get_anime_remediation(
            State(state),
            Query(ApiAnimeRemediationQuery {
                plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                full: Some(true),
                state: Some("blocked".to_string()),
                reason: Some("legacy_roots_contain_real_media".to_string()),
                title: Some("Show A".to_string()),
                format: Some("tsv".to_string()),
            }),
        )
        .await
        .unwrap()
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/tab-separated-values; charset=utf-8"
        );
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("normalized_title"));
        assert!(body.contains("legacy_roots_contain_real_media"));
        assert!(body.contains("Show A"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_post_anime_remediation_preview_saves_plan_in_backup_dir() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let root = dir.path().to_path_buf();
        let mut cfg = test_config(&root);
        cfg.backup.path = std::path::PathBuf::from(root.file_name().unwrap()).join("backups");
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
        std::os::unix::fs::symlink("/tmp/source-a.mkv", &legacy_target).unwrap();

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
        let response = api_post_anime_remediation_preview(
            State(state.clone()),
            Json(ApiAnimeRemediationPreviewRequest {
                plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                title: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationPreviewResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.success);
        assert_eq!(json.total_groups, 1);
        assert_eq!(json.eligible_groups, 1);
        assert_eq!(json.cleanup_candidates, 1);
        assert!(json.blocked_reason_summary.is_empty());
        let report_path = std::path::PathBuf::from(&json.report_path);
        assert!(report_path.is_absolute());
        assert!(report_path.starts_with(root.join("backups")));
        assert!(report_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_post_anime_remediation_apply_uses_saved_plan_and_quarantines_legacy_symlink() {
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

        let preview = api_post_anime_remediation_preview(
            State(state.clone()),
            Json(ApiAnimeRemediationPreviewRequest {
                plex_db: Some(plex_db_path.to_string_lossy().to_string()),
                title: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();
        let preview_bytes = to_bytes(preview.into_body(), usize::MAX).await.unwrap();
        let preview_json: ApiAnimeRemediationPreviewResponse =
            serde_json::from_slice(&preview_bytes).unwrap();
        assert!(std::path::Path::new(&preview_json.report_path).is_absolute());

        let response = api_post_anime_remediation_apply(
            State(state.clone()),
            Json(ApiAnimeRemediationApplyRequest {
                report_path: preview_json.report_path.clone(),
                token: preview_json.confirmation_token.clone(),
                max_delete: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationApplyResponse = serde_json::from_slice(&bytes).unwrap();

        assert!(json.success);
        assert_eq!(json.candidates, 1);
        assert_eq!(json.quarantined, 1);
        assert_eq!(json.removed, 0);
        assert!(!legacy_target.exists());
        let quarantined_entries =
            walkdir::WalkDir::new(&state.config.cleanup.prune.quarantine_path)
                .into_iter()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_symlink())
                .count();
        assert!(quarantined_entries >= 1);
    }

    #[tokio::test]
    async fn api_post_anime_remediation_preview_rejects_missing_plex_db() {
        let ctx = test_state().await;
        let response = api_post_anime_remediation_preview(
            State(ctx),
            Json(ApiAnimeRemediationPreviewRequest {
                plex_db: Some("/tmp/definitely-missing-plex.db".to_string()),
                title: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationPreviewResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!json.success);
        assert!(json.message.contains("Plex DB path is required"));
    }

    #[tokio::test]
    async fn api_post_anime_remediation_apply_rejects_report_outside_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();
        let outside_report = dir.path().join("outside-report.json");
        std::fs::write(&outside_report, "{}").unwrap();

        let state = WebState::new(cfg, db);
        let response = api_post_anime_remediation_apply(
            State(state),
            Json(ApiAnimeRemediationApplyRequest {
                report_path: outside_report.to_string_lossy().to_string(),
                token: "bad-token".to_string(),
                max_delete: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationApplyResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!json.success);
        assert!(json
            .message
            .contains("Cleanup report must be inside the configured backup directory"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn api_post_anime_remediation_apply_rejects_relative_symlink_escape_in_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();
        let outside_dir = dir.path().join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("report.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside_dir, cfg.backup.path.join("linked")).unwrap();

        let state = WebState::new(cfg, db);
        let response = api_post_anime_remediation_apply(
            State(state),
            Json(ApiAnimeRemediationApplyRequest {
                report_path: "linked/report.json".to_string(),
                token: "bad-token".to_string(),
                max_delete: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationApplyResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!json.success);
        assert!(json
            .message
            .contains("Cleanup report must be inside the configured backup directory"));
    }

    #[tokio::test]
    async fn api_post_anime_remediation_apply_rejects_when_foreign_quarantine_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut cfg = test_config(&root);
        cfg.cleanup.prune.quarantine_foreign = false;
        let report_path = cfg.backup.path.join("anime-remediation.json");
        std::fs::write(&report_path, "{}").unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        let state = WebState::new(cfg, db);
        let response = api_post_anime_remediation_apply(
            State(state),
            Json(ApiAnimeRemediationApplyRequest {
                report_path: report_path.to_string_lossy().to_string(),
                token: "token".to_string(),
                max_delete: None,
                library: Some("Anime".to_string()),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: ApiAnimeRemediationApplyResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!json.success);
        assert!(json
            .message
            .contains("cleanup.prune.quarantine_foreign=true"));
    }
}
