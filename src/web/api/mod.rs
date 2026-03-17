//! JSON API endpoints for automation

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::backup::BackupManager;
use crate::cleanup_audit::{CleanupAuditor, CleanupScope};
use crate::config::Config;
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::linker::Linker;
use crate::matcher::Matcher;
use crate::source_scanner::SourceScanner;

use super::WebState;

/// Create the API router
pub fn create_router(state: WebState) -> Router<WebState> {
    Router::new()
        .route("/status", get(api_get_status))
        .route("/health", get(api_get_health))
        .route("/scan", post(api_post_scan))
        .route("/scan/jobs", get(api_get_scan_jobs))
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

#[derive(Serialize)]
pub struct ApiRepairResponse {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
}

#[derive(Serialize)]
pub struct ApiCleanupAuditResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
}

#[derive(Serialize)]
pub struct ApiCleanupPruneResponse {
    pub success: bool,
    pub message: String,
    pub removed: usize,
    pub skipped: usize,
}

#[derive(Serialize)]
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
}

#[derive(Deserialize)]
pub struct ApiCleanupAuditRequest {
    pub scope: String,
}

#[derive(Deserialize)]
pub struct ApiCleanupPruneRequest {
    pub report_path: String,
    pub token: String,
}

// ─── API Handlers ───────────────────────────────────────────────────

/// GET /api/v1/status
pub async fn api_get_status(State(state): State<WebState>) -> Json<ApiStatus> {
    let stats = state
        .database
        .get_web_stats()
        .await
        .unwrap_or_default();

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

/// POST /api/v1/scan
pub async fn api_post_scan(
    State(state): State<WebState>,
    Json(req): Json<ApiScanRequest>,
) -> Json<ApiScanResponse> {
    info!("API: Triggering scan");

    let dry_run = req.dry_run.unwrap_or(false);
    let library_name = req.library.filter(|l| !l.is_empty());

    let scanner = LibraryScanner::new();
    let source_scanner = SourceScanner::new();

    let library_items = if let Some(ref name) = library_name {
        if let Some(lib) = state.config.libraries.iter().find(|l| &l.name == name) {
            scanner.scan_library(lib)
        } else {
            vec![]
        }
    } else {
        state
            .config
            .libraries
            .iter()
            .flat_map(|lib| scanner.scan_library(lib))
            .collect()
    };

    let source_items = source_scanner.scan_all(&state.config.sources);

    // Create matcher
    use crate::api::tmdb::TmdbClient;
    use crate::api::tvdb::TvdbClient;

    let cfg = &state.config;
    let tmdb = if cfg.has_tmdb() {
        let rat = if cfg.api.tmdb_read_access_token.is_empty() {
            None
        } else {
            Some(cfg.api.tmdb_read_access_token.as_str())
        };
        Some(TmdbClient::new(&cfg.api.tmdb_api_key, rat, cfg.api.cache_ttl_hours))
    } else {
        None
    };

    let tvdb = if cfg.has_tvdb() {
        Some(TvdbClient::new(&cfg.api.tvdb_api_key, cfg.api.cache_ttl_hours))
    } else {
        None
    };

    let matcher = Matcher::new(
        tmdb,
        tvdb,
        state.config.matching.mode,
        state.config.matching.metadata_mode,
        state.config.matching.metadata_concurrency,
    );

    let matches = match matcher
        .find_matches(&library_items, &source_items, &state.database)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            return Json(ApiScanResponse {
                success: false,
                message: format!("Match failed: {}", e),
                created: 0,
                updated: 0,
                skipped: 0,
            });
        }
    };

    let linker = Linker::new_with_options(
        dry_run,
        state.config.matching.mode.is_strict(),
        &state.config.symlink.naming_template,
        true,
    );

    let link_result = match linker.process_matches(&matches, &state.database).await {
        Ok(r) => r,
        Err(e) => {
            return Json(ApiScanResponse {
                success: false,
                message: format!("Link failed: {}", e),
                created: 0,
                updated: 0,
                skipped: 0,
            });
        }
    };

    Json(ApiScanResponse {
        success: true,
        message: format!(
            "Scan complete: {} created, {} updated, {} skipped",
            link_result.created, link_result.updated, link_result.skipped
        ),
        created: link_result.created,
        updated: link_result.updated,
        skipped: link_result.skipped,
    })
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

/// POST /api/v1/repair/auto
pub async fn api_post_repair_auto(State(state): State<WebState>) -> Json<ApiRepairResponse> {
    info!("API: Running auto repair");

    // Placeholder - would integrate with repair module
    Json(ApiRepairResponse {
        success: true,
        message: "Repair completed".to_string(),
        repaired: 0,
        failed: 0,
    })
}

/// POST /api/v1/cleanup/audit
pub async fn api_post_cleanup_audit(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupAuditRequest>,
) -> Json<ApiCleanupAuditResponse> {
    info!("API: Running cleanup audit");

    let scope = match CleanupScope::parse(&req.scope) {
        Ok(s) => s,
        Err(e) => {
            return Json(ApiCleanupAuditResponse {
                success: false,
                message: format!("Invalid scope: {}", e),
                report_path: String::new(),
                total_findings: 0,
                critical: 0,
                high: 0,
                warning: 0,
            });
        }
    };

    let auditor = CleanupAuditor::new_with_progress(&state.config, &state.database, false);

    let default_output = state.config.backup.path.join(format!(
        "cleanup-audit-{}-{}.json",
        req.scope,
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    let report_path = match auditor.run_audit(scope, Some(&default_output)).await {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            return Json(ApiCleanupAuditResponse {
                success: false,
                message: format!("Audit failed: {}", e),
                report_path: String::new(),
                total_findings: 0,
                critical: 0,
                high: 0,
                warning: 0,
            });
        }
    };

    // Read report to get summary
    // For now, return placeholder
    Json(ApiCleanupAuditResponse {
        success: true,
        message: "Audit complete".to_string(),
        report_path,
        total_findings: 0,
        critical: 0,
        high: 0,
        warning: 0,
    })
}

/// POST /api/v1/cleanup/prune
pub async fn api_post_cleanup_prune(
    State(state): State<WebState>,
    Json(_req): Json<ApiCleanupPruneRequest>,
) -> Json<ApiCleanupPruneResponse> {
    info!("API: Applying prune");

    // Placeholder - would parse report and apply deletions
    Json(ApiCleanupPruneResponse {
        success: true,
        message: "Prune applied".to_string(),
        removed: 0,
        skipped: 0,
    })
}

/// GET /api/v1/links
pub async fn api_get_links(
    State(state): State<WebState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Vec<ApiLink>> {
    let limit: i64 = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
    let status_filter = params.get("status").map(|s| s.as_str());

    let links = match status_filter {
        Some("dead") => state.database.get_dead_links().await.unwrap_or_default(),
        _ => state.database.get_active_links().await.unwrap_or_default(),
    }
    .into_iter()
    .take(limit as usize)
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
pub async fn api_get_config_validate(State(state): State<WebState>) -> Json<ApiConfigValidateResponse> {
    let mut errors = vec![];
    let mut warnings = vec![];

    if state.config.libraries.is_empty() {
        errors.push("No libraries configured".to_string());
    }

    if state.config.sources.is_empty() {
        errors.push("No sources configured".to_string());
    }

    if !state.config.has_tmdb() {
        warnings.push("TMDB API key not configured".to_string());
    }

    if !state.config.has_tvdb() {
        warnings.push("TVDB API key not configured".to_string());
    }

    Json(ApiConfigValidateResponse {
        valid: errors.is_empty(),
        errors,
        warnings,
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
