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
use misc::*;
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: String,
}

#[derive(Deserialize)]
pub struct ApiScanRequest {
    pub dry_run: Option<bool>,
    pub library: Option<String>,
    pub search_missing: Option<bool>,
}

// ─── API Handlers ───────────────────────────────────────────────────

mod cleanup;
mod misc;
mod scan;

#[cfg(test)]
mod tests;
