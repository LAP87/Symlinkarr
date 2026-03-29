//! Web UI module for Symlinkarr
//!
//! Provides a web-based interface for managing symlinks, viewing status,
//! triggering scans, and running cleanup operations.

pub mod api;
pub mod filters;
pub mod handlers;
pub mod templates;

use anyhow::Result;
use axum::{
    extract::State,
    http::{
        header::{HOST, ORIGIN, REFERER},
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
    },
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;

/// Shared application state passed to handlers
#[derive(Clone)]
pub struct WebState {
    pub config: Arc<Config>,
    pub database: Arc<Database>,
}

impl WebState {
    pub fn new(config: Config, database: Database) -> Self {
        Self {
            config: Arc::new(config),
            database: Arc::new(database),
        }
    }
}

/// Custom error type for web handlers
#[derive(Debug)]
pub struct WebError(pub String);

impl IntoResponse for WebError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0).into_response()
    }
}

/// Create the Axum router with all routes
fn create_router(state: WebState) -> Router {
    // Main app routes
    let app_routes = Router::new()
        // Dashboard
        .route("/", get(handlers::get_dashboard))
        // Status & Health
        .route("/status", get(handlers::get_status))
        .route("/health", get(handlers::get_health))
        // Scan
        .route("/scan", get(handlers::get_scan))
        .route("/scan/trigger", post(handlers::post_scan_trigger))
        .route("/scan/history", get(handlers::get_scan_history))
        .route("/scan/history/:id", get(handlers::get_scan_run_detail))
        // Cleanup
        .route("/cleanup", get(handlers::get_cleanup))
        .route("/cleanup/audit", post(handlers::post_cleanup_audit))
        .route("/cleanup/prune", get(handlers::get_cleanup_prune))
        .route("/cleanup/prune", post(handlers::post_cleanup_prune))
        // Links
        .route("/links", get(handlers::get_links))
        .route("/links/dead", get(handlers::get_dead_links))
        .route("/links/repair", post(handlers::post_repair))
        // Config
        .route("/config", get(handlers::get_config))
        .route("/config/validate", post(handlers::post_config_validate))
        // Doctor
        .route("/doctor", get(handlers::get_doctor))
        // Discover
        .route("/discover", get(handlers::get_discover))
        .route("/discover/add", post(handlers::post_discover_add))
        // Backup
        .route("/backup", get(handlers::get_backup))
        .route("/backup/create", post(handlers::post_backup_create))
        .route("/backup/restore", post(handlers::post_backup_restore));

    // API routes
    let api_routes = api::create_router(state.clone());

    // Combine all routes
    Router::new()
        .merge(app_routes)
        .nest("/api/v1", api_routes)
        .layer(middleware::from_fn(reject_cross_origin_mutations))
        .layer(TraceLayer::new_for_http())
        .nest_service("/static", ServeDir::new(static_dir()))
        .with_state(state)
}

fn method_requires_same_origin(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn header_value_str(value: &HeaderValue) -> Option<&str> {
    value
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn header_authority(value: &HeaderValue) -> Option<String> {
    header_value_str(value).map(|value| value.to_ascii_lowercase())
}

fn uri_authority(value: &HeaderValue) -> Option<String> {
    let uri: Uri = header_value_str(value)?.parse().ok()?;
    uri.authority()
        .map(|authority| authority.as_str().to_ascii_lowercase())
}

fn is_same_origin_browser_mutation(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(header_authority) else {
        return false;
    };

    if let Some(origin) = headers.get(ORIGIN).and_then(uri_authority) {
        return origin == host;
    }

    if let Some(referer) = headers.get(REFERER).and_then(uri_authority) {
        return referer == host;
    }

    false
}

fn forbidden_origin_response(path: &str) -> axum::response::Response {
    if path.starts_with("/api/") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "cross-origin mutation blocked; use the same origin as the web UI or a non-browser client without Origin/Referer headers"
            })),
        )
            .into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Cross-origin mutation blocked; submit the form from the same Symlinkarr origin.",
    )
        .into_response()
}

async fn reject_cross_origin_mutations(
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let origin = request.headers().get(ORIGIN).and_then(header_value_str);
    let referer = request.headers().get(REFERER).and_then(header_value_str);
    let host = request.headers().get(HOST).and_then(header_value_str);

    if method_requires_same_origin(&method)
        && (origin.is_some() || referer.is_some())
        && !is_same_origin_browser_mutation(request.headers())
    {
        warn!(
            method = %method,
            path,
            host = host.unwrap_or("<missing>"),
            origin = origin.unwrap_or("<missing>"),
            referer = referer.unwrap_or("<missing>"),
            "blocked cross-origin mutation request"
        );
        return forbidden_origin_response(request.uri().path());
    }

    next.run(request).await
}

/// Start the web server
///
/// Binds to the specified port and serves the web UI.
/// This function blocks until the server is shut down.
pub async fn serve(config: Config, db: Database, port: u16) -> Result<()> {
    let bind_address = config.web.normalized_bind_address();
    let state = WebState::new(config, db);
    let addr = format!("{}:{}", bind_address, port);

    let router = create_router(state);

    info!("Starting Symlinkarr web UI on {}", addr);
    if matches!(bind_address.as_str(), "0.0.0.0" | "::") {
        info!(
            "Dashboard (same host hint): http://127.0.0.1:{} (set web.bind_address for a concrete URL)",
            port
        );
    } else {
        info!("Dashboard: http://{}", addr);
    }

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}

/// Resolve the static file directory. Checks (in order):
/// 1. `src/web/static` (development)
/// 2. Next to the executable at `<exe_dir>/static` (Docker / installed)
fn static_dir() -> std::path::PathBuf {
    let dev_path = std::path::PathBuf::from("src/web/static");
    if dev_path.is_dir() {
        return dev_path;
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe_sibling = exe.parent().unwrap_or(exe.as_path()).join("static");
        if exe_sibling.is_dir() {
            return exe_sibling;
        }
    }
    // Fallback — will 404 but won't panic
    dev_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request},
    };
    use tower::ServiceExt;

    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
        SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::{AcquisitionJobSeed, AcquisitionRelinkKind, Database, ScanRunRecord};
    use crate::models::{LinkRecord, LinkStatus, MediaType};

    fn test_config(root: &std::path::Path) -> Config {
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
                media_type: MediaType::Tv,
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
            db_path: root.join("test.sqlite").display().to_string(),
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

    async fn test_router() -> Router {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::mem::forget(dir);
        let cfg = test_config(&root);
        let db = Database::new(&cfg.db_path).await.unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: root.join("source").join("show.mkv"),
            target_path: root
                .join("library")
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
            source_items_found: 5,
            matches_found: 1,
            links_created: 1,
            links_updated: 0,
            dead_marked: 0,
            links_removed: 0,
            links_skipped: 0,
            ambiguous_skipped: 0,
            runtime_checks_ms: 11,
            library_scan_ms: 22,
            source_inventory_ms: 33,
            matching_ms: 44,
            title_enrichment_ms: 55,
            linking_ms: 66,
            plex_refresh_ms: 77,
            dead_link_sweep_ms: 88,
            cache_hit_ratio: Some(0.75),
            candidate_slots: 12,
            scored_candidates: 3,
            exact_id_hits: 1,
            auto_acquire_requests: 2,
            auto_acquire_missing_requests: 1,
            auto_acquire_cutoff_requests: 1,
            auto_acquire_dry_run_hits: 1,
            auto_acquire_submitted: 0,
            auto_acquire_no_result: 0,
            auto_acquire_blocked: 0,
            auto_acquire_failed: 0,
            auto_acquire_completed_linked: 0,
            auto_acquire_completed_unlinked: 0,
        })
        .await
        .unwrap();

        db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
            request_key: "test-queued-job".to_string(),
            label: "Queued Anime".to_string(),
            query: "Queued Anime".to_string(),
            query_hints: vec!["Queued Anime S01E01".to_string()],
            imdb_id: Some("tt1234567".to_string()),
            categories: vec![5070],
            arr: "sonarr".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_kind: AcquisitionRelinkKind::MediaId,
            relink_value: "tvdb-1".to_string(),
        }])
        .await
        .unwrap();

        create_router(WebState::new(cfg, db))
    }

    async fn get_html(router: &Router, path: &str) -> (u16, String) {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        (status, body)
    }

    async fn post_json(
        router: &Router,
        path: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap();
        (status, body)
    }

    async fn post_json_with_headers(
        router: &Router,
        path: &str,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) -> (u16, String) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");

        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = router
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        (status, body)
    }

    async fn post_form_with_headers(
        router: &Router,
        path: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> (u16, String) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/x-www-form-urlencoded");

        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = router
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn dashboard_status_and_scan_render_successfully() {
        let router = test_router().await;
        let (status, dashboard) = get_html(&router, "/").await;
        assert_eq!(status, 200);
        assert!(dashboard.contains("Dashboard"));
        assert!(dashboard.contains("Current baseline"));
        assert!(dashboard.contains("Queue 1"));

        let (status, status_page) = get_html(&router, "/status").await;
        assert_eq!(status, 200);
        assert!(status_page.contains("Queue pressure"));
        assert!(status_page.contains("Recent Links"));
        assert!(status_page.contains("Active Links"));

        let (status, scan_page) = get_html(&router, "/scan").await;
        assert_eq!(status, 200);
        assert!(scan_page.contains("Start Real Scan"));
        assert!(scan_page.contains("Search Missing"));
        assert!(scan_page.contains("Latest Run"));
    }

    #[tokio::test]
    async fn cleanup_audit_api_returns_report_summary() {
        let router = test_router().await;
        let (status, body) = post_json(
            &router,
            "/api/v1/cleanup/audit",
            serde_json::json!({ "scope": "anime" }),
        )
        .await;

        assert_eq!(status, 200);
        assert_eq!(body["success"], true);

        let report_path = body["report_path"].as_str().unwrap();
        let report_json = std::fs::read_to_string(report_path).unwrap();
        let report: crate::cleanup_audit::CleanupReport =
            serde_json::from_str(&report_json).unwrap();

        assert_eq!(body["total_findings"], report.summary.total_findings);
        assert_eq!(body["critical"], report.summary.critical);
        assert_eq!(body["high"], report.summary.high);
        assert_eq!(body["warning"], report.summary.warning);
    }

    #[tokio::test]
    async fn api_blocks_cross_origin_mutations() {
        let router = test_router().await;
        let (status, body) = post_json_with_headers(
            &router,
            "/api/v1/cleanup/audit",
            serde_json::json!({ "scope": "anime" }),
            &[
                (header::HOST.as_str(), "127.0.0.1:8726"),
                (header::ORIGIN.as_str(), "http://evil.example"),
            ],
        )
        .await;

        assert_eq!(status, 403);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["error"],
            "cross-origin mutation blocked; use the same origin as the web UI or a non-browser client without Origin/Referer headers"
        );
    }

    #[tokio::test]
    async fn api_allows_same_origin_mutations() {
        let router = test_router().await;
        let (status, body) = post_json_with_headers(
            &router,
            "/api/v1/cleanup/audit",
            serde_json::json!({ "scope": "anime" }),
            &[
                (header::HOST.as_str(), "127.0.0.1:8726"),
                (header::ORIGIN.as_str(), "http://127.0.0.1:8726"),
            ],
        )
        .await;

        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["success"], true);
    }

    #[tokio::test]
    async fn ui_blocks_cross_origin_form_posts() {
        let router = test_router().await;
        let (status, body) = post_form_with_headers(
            &router,
            "/config/validate",
            "",
            &[
                (header::HOST.as_str(), "127.0.0.1:8726"),
                (header::REFERER.as_str(), "http://evil.example/form"),
            ],
        )
        .await;

        assert_eq!(status, 403);
        assert!(body.contains("Cross-origin mutation blocked"));
    }
}
