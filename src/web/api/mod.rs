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

use crate::cleanup_audit::CleanupScope;
#[cfg(test)]
use crate::cleanup_audit::{self, CleanupReport};
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
use crate::db::ScanHistoryRecord;
use crate::media_servers::{
    configured_refresh_backends, deferred_refresh_summary, LibraryInvalidationOutcome,
    LibraryInvalidationServerOutcome,
};

use super::{
    clamp_link_list_limit, latest_cleanup_report_created_at, resolve_cleanup_report_path,
    should_surface_cleanup_audit_outcome, should_surface_scan_outcome,
    templates::{skip_reason_group_label, skip_reason_help, skip_reason_label},
    WebState,
};
use cleanup::*;
use scan::*;

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
        .route("/scan/{id}", get(api_get_scan_run))
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
        .route("/cache/invalidate", post(api_post_cache_invalidate))
        .route("/cache", axum::routing::delete(api_delete_cache))
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
pub struct ApiDiscoverSummary {
    pub folders: usize,
    pub placements: usize,
    pub creates: usize,
    pub updates: usize,
    pub blocked: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDiscoverFolder {
    pub library_name: String,
    pub media_id: String,
    pub title: String,
    pub folder_path: String,
    pub existing_links: usize,
    pub planned_creates: usize,
    pub planned_updates: usize,
    pub blocked: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDiscoverItem {
    pub library_name: String,
    pub media_id: String,
    pub title: String,
    pub folder_path: String,
    pub source_path: String,
    pub source_name: String,
    pub target_path: String,
    pub action: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiDiscoverResponse {
    pub summary: ApiDiscoverSummary,
    pub folders: Vec<ApiDiscoverFolder>,
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
pub struct ApiErrorResponse {
    pub error: String,
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
            summary: ApiDiscoverSummary {
                folders: snapshot.summary.folders,
                placements: snapshot.summary.placements,
                creates: snapshot.summary.creates,
                updates: snapshot.summary.updates,
                blocked: snapshot.summary.blocked,
            },
            folders: snapshot
                .folders
                .into_iter()
                .map(|folder| ApiDiscoverFolder {
                    library_name: folder.library_name,
                    media_id: folder.media_id,
                    title: folder.title,
                    folder_path: folder.folder_path.display().to_string(),
                    existing_links: folder.existing_links,
                    planned_creates: folder.planned_creates,
                    planned_updates: folder.planned_updates,
                    blocked: folder.blocked,
                })
                .collect(),
            items: snapshot
                .items
                .into_iter()
                .map(|item| ApiDiscoverItem {
                    library_name: item.library_name,
                    media_id: item.media_id,
                    title: item.title,
                    folder_path: item.folder_path.display().to_string(),
                    source_path: item.source_path.display().to_string(),
                    source_name: item.source_name,
                    target_path: item.target_path.display().to_string(),
                    action: item.action.as_str().to_string(),
                    season: item.season,
                    episode: item.episode,
                })
                .collect(),
            status_message: snapshot.status_message.or_else(|| {
                (!query.refresh_cache).then(|| {
                    "Showing cached or on-disk discover results only. Set refresh_cache=true when you want a slower live cache sync first."
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

// ─── Cache management ──────────────────────────────────────────────

#[derive(Deserialize)]
struct CacheInvalidateRequest {
    /// Cache key prefix, exact key, or short-form media ID (e.g., "tmdb:tv:", "tmdb:12345", "tvdb:67890", "anime-lists")
    key: String,
}

#[derive(Serialize)]
struct CacheInvalidateResponse {
    invalidated: u64,
    key: String,
}

#[derive(Serialize)]
struct CacheClearResponse {
    cleared: u64,
}

async fn api_post_cache_invalidate(
    State(state): State<WebState>,
    Json(body): Json<CacheInvalidateRequest>,
) -> Response {
    match crate::commands::cache::invalidate_metadata_cache(&state.database, &body.key).await {
        Ok(deleted) => Json(CacheInvalidateResponse {
            invalidated: deleted,
            key: body.key,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_delete_cache(State(state): State<WebState>) -> Response {
    match crate::commands::cache::clear_metadata_cache(&state.database).await {
        Ok(deleted) => Json(CacheClearResponse { cleared: deleted }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

mod cleanup;
mod scan;

#[cfg(test)]
mod tests;
