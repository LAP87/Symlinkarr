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

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

fn canonical_plex_db_path(path: std::path::PathBuf) -> Option<std::path::PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
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

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return canonical_plex_db_path(std::path::PathBuf::from(requested));
    }

    default_plex_db_candidates()
        .into_iter()
        .map(std::path::PathBuf::from)
        .find_map(canonical_plex_db_path)
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

mod scan;

#[cfg(test)]
mod tests;
