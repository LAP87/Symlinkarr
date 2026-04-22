use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{header, Request},
};
use tower::ServiceExt;

use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig, MediaBrowserConfig,
    PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
    SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
};
use crate::db::{AcquisitionJobSeed, AcquisitionRelinkKind, Database, ScanRunRecord};
use crate::models::{LinkRecord, LinkStatus, MediaType};

fn basic_auth_header(username: &str, password: &str) -> String {
    let credentials = format!("{username}:{password}");
    format!("Basic {}", BASE64_STANDARD.encode(credentials))
}

fn test_basic_auth_credentials() -> (String, String) {
    (
        "operator".to_string(),
        generate_browser_session_token().expect("test browser session token"),
    )
}

fn test_api_key() -> String {
    generate_browser_session_token().expect("test api key")
}

#[test]
fn constant_time_str_eq_handles_equal_and_unequal_lengths() {
    assert!(constant_time_str_eq("abcd", "abcd"));
    assert!(!constant_time_str_eq("abcd", "abc"));
    assert!(!constant_time_str_eq("abcd", "abce"));
}

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
        run_token: Some("scan-run-web".to_string()),
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
        skip_reason_json: None,
        runtime_checks_ms: 11,
        library_scan_ms: 22,
        source_inventory_ms: 33,
        matching_ms: 44,
        title_enrichment_ms: 55,
        linking_ms: 66,
        plex_refresh_ms: 77,
        plex_refresh_requested_paths: 3,
        plex_refresh_unique_paths: 2,
        plex_refresh_planned_batches: 2,
        plex_refresh_coalesced_batches: 1,
        plex_refresh_coalesced_paths: 2,
        plex_refresh_refreshed_batches: 1,
        plex_refresh_refreshed_paths_covered: 2,
        plex_refresh_skipped_batches: 1,
        plex_refresh_unresolved_paths: 0,
        plex_refresh_capped_batches: 1,
        plex_refresh_aborted_due_to_cap: true,
        plex_refresh_failed_batches: 0,
        media_server_refresh_json: None,
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

async fn remote_guarded_router() -> (Router, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::mem::forget(dir);
    let mut cfg = test_config(&root);
    let (username, password) = test_basic_auth_credentials();
    cfg.web.bind_address = "0.0.0.0".to_string();
    cfg.web.allow_remote = true;
    cfg.web.username = username.clone();
    cfg.web.password = password.clone();
    let db = Database::new(&cfg.db_path).await.unwrap();
    (create_router(WebState::new(cfg, db)), username, password)
}

fn noconfig_router() -> Router {
    Router::new().route("/", axum::routing::get(handlers::get_noconfig))
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

async fn get_html_with_headers(
    router: &Router,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, axum::http::HeaderMap, String) {
    let mut request = Request::builder().uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    (status, headers, body)
}

fn browser_session_cookie(headers: &axum::http::HeaderMap) -> String {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .find_map(|value| {
            value
                .to_str()
                .ok()
                .filter(|cookie| cookie.starts_with("symlinkarr_browser_session="))
                .map(|cookie| cookie.split(';').next().unwrap_or(cookie).to_string())
        })
        .expect("expected symlinkarr browser session cookie")
}

fn browser_session_token(cookie: &str) -> String {
    cookie
        .split_once('=')
        .map(|(_, value)| value.to_string())
        .expect("expected cookie name=value format")
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
async fn dashboard_page_exposes_primary_operator_actions() {
    let router = test_router().await;
    let (status, dashboard) = get_html(&router, "/").await;

    assert_eq!(status, 200);
    assert!(dashboard.contains("Dashboard"));
    assert!(dashboard.contains("Needs Attention"));
    assert!(dashboard.contains("Live Activity"));
    assert!(dashboard.contains("href=\"/scan\""));
    assert!(dashboard.contains("href=\"/status\""));
    assert!(dashboard.contains("hx-get=\"/dashboard/needs-attention\""));
    assert!(dashboard.contains("hx-get=\"/dashboard/activity-feed\""));
    assert!(dashboard.contains("href=\"/scan/history/"));
    assert!(dashboard.contains("Queue 1"));
}

#[tokio::test]
async fn dashboard_activity_feed_route_renders_fragment() {
    let router = test_router().await;
    let (status, fragment) = get_html(&router, "/dashboard/activity-feed").await;

    assert_eq!(status, 200);
    assert!(fragment.contains("Live Activity"));
    assert!(fragment.contains("Running now"));
    assert!(fragment.contains("Latest outcomes"));
    assert!(fragment.contains("hx-get=\"/dashboard/activity-feed\""));
}

#[tokio::test]
async fn dashboard_needs_attention_route_renders_fragment() {
    let router = test_router().await;
    let (status, fragment) = get_html(&router, "/dashboard/needs-attention").await;

    assert_eq!(status, 200);
    assert!(fragment.contains("Needs Attention"));
    assert!(fragment.contains("Operator priorities"));
    assert!(fragment.contains("hx-get=\"/dashboard/needs-attention\""));
}

#[tokio::test]
async fn status_page_exposes_link_health_actions_and_seeded_rows() {
    let router = test_router().await;
    let (status, status_page) = get_html(&router, "/status").await;

    assert_eq!(status, 200);
    assert!(status_page.contains("href=\"/scan\""));
    assert!(status_page.contains("tvdb-1"));
    assert!(status_page.contains("S01E01.mkv"));
    assert!(status_page.contains("No persistent dead links are currently tracked."));
    assert!(status_page.contains("Recent auto-acquire jobs"));
    assert!(status_page.contains("Queued Anime"));
    assert!(status_page.contains("Needs Relink"));
}

#[tokio::test]
async fn scan_page_exposes_trigger_form_and_history_controls() {
    let router = test_router().await;
    let (status, scan_page) = get_html(&router, "/scan").await;

    assert_eq!(status, 200);
    assert!(scan_page.contains("action=\"/scan/trigger\""));
    assert!(scan_page.contains("name=\"dry_run\""));
    assert!(scan_page.contains("name=\"search_missing\""));
    assert!(scan_page.contains("action=\"/scan\""));
    assert!(scan_page.contains("href=\"/scan/history\""));
}

#[tokio::test]
async fn cleanup_page_exposes_audit_form_and_dead_link_entrypoint() {
    let router = test_router().await;
    let (status, cleanup_page) = get_html(&router, "/cleanup").await;

    assert_eq!(status, 200);
    assert!(cleanup_page.contains("action=\"/cleanup/audit\""));
    assert!(cleanup_page.contains("name=\"libraries\""));
    assert!(cleanup_page.contains("href=\"/links/dead\""));
}

#[tokio::test]
async fn noconfig_page_exposes_restore_and_bootstrap_paths() {
    let router = noconfig_router();
    let (status, page) = get_html(&router, "/").await;

    assert_eq!(status, 200);
    assert!(page.contains("Setup required"));
    assert!(page.contains("Restore from backup"));
    assert!(page.contains("Create new installation"));
    assert!(page.contains("symlinkarr restore &lt;path-to-backup.json&gt;"));
    assert!(page.contains("symlinkarr bootstrap"));
    assert!(page.contains("/wiki/Backup-and-Restore"));
    assert!(page.contains("/wiki/Configuration-and-Doctor"));
    assert!(page.contains("Auto-restore:"));
}

#[tokio::test]
async fn health_alias_redirects_to_status() {
    let router = test_router().await;
    let (status, headers, _body) = get_html_with_headers(&router, "/health", &[]).await;

    assert_eq!(status, 308);
    assert_eq!(
        headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/status")
    );
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

    assert_eq!(status, 202);
    assert_eq!(body["success"], true);
    assert_eq!(body["running"], true);
    assert_eq!(body["scope_label"], "Anime");
    assert_eq!(body["report_path"], "");
}

#[tokio::test]
async fn api_blocks_cross_origin_mutations() {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (status, body) = post_json_with_headers(
        &router,
        "/api/v1/cleanup/audit",
        serde_json::json!({ "scope": "anime" }),
        &[
            (header::AUTHORIZATION.as_str(), auth.as_str()),
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
async fn html_requests_require_basic_auth_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::mem::forget(dir);
    let mut cfg = test_config(&root);
    let (username, password) = test_basic_auth_credentials();
    cfg.web.username = username.clone();
    cfg.web.password = password.clone();
    let db = Database::new(&cfg.db_path).await.unwrap();
    let router = create_router(WebState::new(cfg, db));

    let (status, headers, _body) = get_html_with_headers(&router, "/", &[]).await;
    assert_eq!(status, 401);
    assert_eq!(
        headers
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Basic realm=\"Symlinkarr\"")
    );

    let auth = basic_auth_header(&username, &password);
    let (status, _headers, body) = get_html_with_headers(
        &router,
        "/",
        &[(header::AUTHORIZATION.as_str(), auth.as_str())],
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("href=\"/scan\""));
    assert!(body.contains("href=\"/status\""));
}

#[tokio::test]
async fn api_requests_accept_bearer_api_key_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::mem::forget(dir);
    let mut cfg = test_config(&root);
    let api_key = test_api_key();
    let bearer = format!("Bearer {api_key}");
    cfg.web.api_key = api_key.clone();
    let db = Database::new(&cfg.db_path).await.unwrap();
    let router = create_router(WebState::new(cfg, db));

    let (status, body) = post_json_with_headers(
        &router,
        "/api/v1/cleanup/audit",
        serde_json::json!({ "scope": "anime" }),
        &[(header::AUTHORIZATION.as_str(), bearer.as_str())],
    )
    .await;

    assert_eq!(status, 202);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["success"], true);
}

#[tokio::test]
async fn api_requests_require_auth_when_api_key_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::mem::forget(dir);
    let mut cfg = test_config(&root);
    cfg.web.api_key = test_api_key();
    let db = Database::new(&cfg.db_path).await.unwrap();
    let router = create_router(WebState::new(cfg, db));

    let (status, body) = post_json_with_headers(
        &router,
        "/api/v1/cleanup/audit",
        serde_json::json!({ "scope": "anime" }),
        &[],
    )
    .await;

    assert_eq!(status, 401);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["error"], "authentication required");
}

#[tokio::test]
async fn api_allows_same_origin_mutations() {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (_, headers, _) = get_html_with_headers(
        &router,
        "/",
        &[(header::AUTHORIZATION.as_str(), auth.as_str())],
    )
    .await;
    let cookie = browser_session_cookie(&headers);
    let (status, body) = post_json_with_headers(
        &router,
        "/api/v1/cleanup/audit",
        serde_json::json!({ "scope": "anime" }),
        &[
            (header::AUTHORIZATION.as_str(), auth.as_str()),
            (header::HOST.as_str(), "127.0.0.1:8726"),
            (header::ORIGIN.as_str(), "http://127.0.0.1:8726"),
            (header::COOKIE.as_str(), &cookie),
        ],
    )
    .await;

    assert_eq!(status, 202);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["running"], true);
}

#[tokio::test]
async fn local_only_ui_mutations_are_open_without_session_or_csrf() {
    let router = test_router().await;
    let (status, body) = post_form_with_headers(&router, "/config/validate", "", &[]).await;

    assert_eq!(status, 200);
    assert!(body.contains("action=\"/config/validate\""));
    assert!(body.contains("Validate Config"));
}

#[tokio::test]
async fn ui_blocks_cross_origin_form_posts_when_remote_exposed() {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (status, body) = post_form_with_headers(
        &router,
        "/config/validate",
        "",
        &[
            (header::AUTHORIZATION.as_str(), auth.as_str()),
            (header::HOST.as_str(), "127.0.0.1:8726"),
            (header::REFERER.as_str(), "http://evil.example/form"),
        ],
    )
    .await;

    assert_eq!(status, 403);
    assert!(body.contains("Cross-origin mutation blocked"));
}

#[tokio::test]
async fn browser_same_origin_mutations_require_issued_session_cookie_when_remote_exposed() {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (status, body) = post_json_with_headers(
        &router,
        "/api/v1/cleanup/audit",
        serde_json::json!({ "scope": "anime" }),
        &[
            (header::AUTHORIZATION.as_str(), auth.as_str()),
            (header::HOST.as_str(), "127.0.0.1:8726"),
            (header::ORIGIN.as_str(), "http://127.0.0.1:8726"),
        ],
    )
    .await;

    assert_eq!(status, 403);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json["error"],
        "browser mutation blocked; refresh the Symlinkarr UI from the same origin and retry with the issued browser session"
    );
}

#[tokio::test]
async fn ui_mutations_require_issued_session_cookie_even_without_browser_metadata_when_remote_exposed(
) {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (status, body) = post_form_with_headers(
        &router,
        "/config/validate",
        "",
        &[(header::AUTHORIZATION.as_str(), auth.as_str())],
    )
    .await;

    assert_eq!(status, 403);
    assert!(body.contains("Browser mutation blocked"));
}

#[tokio::test]
async fn ui_mutations_require_valid_csrf_token_after_session_is_issued_when_remote_exposed() {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (_, headers, _) = get_html_with_headers(
        &router,
        "/config",
        &[(header::AUTHORIZATION.as_str(), auth.as_str())],
    )
    .await;
    let cookie = browser_session_cookie(&headers);

    let (status, body) = post_form_with_headers(
        &router,
        "/config/validate",
        "",
        &[
            (header::AUTHORIZATION.as_str(), auth.as_str()),
            (header::COOKIE.as_str(), &cookie),
        ],
    )
    .await;

    assert_eq!(status, 403);
    assert!(body.contains("CSRF token"));
}

#[tokio::test]
async fn ui_mutations_accept_valid_csrf_token_with_issued_session_when_remote_exposed() {
    let (router, username, password) = remote_guarded_router().await;
    let auth = basic_auth_header(&username, &password);
    let (_, headers, _) = get_html_with_headers(
        &router,
        "/config",
        &[(header::AUTHORIZATION.as_str(), auth.as_str())],
    )
    .await;
    let cookie = browser_session_cookie(&headers);
    let csrf_token = browser_session_token(&cookie);
    let form = format!("csrf_token={csrf_token}");

    let (status, body) = post_form_with_headers(
        &router,
        "/config/validate",
        &form,
        &[
            (header::AUTHORIZATION.as_str(), auth.as_str()),
            (header::COOKIE.as_str(), &cookie),
        ],
    )
    .await;

    assert_eq!(status, 200);
    assert!(body.contains("action=\"/config/validate\""));
    assert!(body.contains("Validate Config"));
}

#[tokio::test]
async fn dashboard_get_sets_browser_session_cookie() {
    let router = test_router().await;
    let (status, headers, body) = get_html_with_headers(&router, "/", &[]).await;

    assert_eq!(status, 200);
    assert!(body.contains("href=\"/scan\""));
    let cookie = browser_session_cookie(&headers);
    assert!(cookie.starts_with("symlinkarr_browser_session="));
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/"));
}

#[tokio::test]
async fn dashboard_get_emits_security_headers() {
    let router = test_router().await;
    let (status, headers, _) = get_html_with_headers(&router, "/", &[]).await;

    assert_eq!(status, 200);
    assert_eq!(
        headers
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok()),
        Some(CONTENT_SECURITY_POLICY_VALUE)
    );
    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|value| value.to_str().ok()),
        Some("nosniff")
    );
}

#[test]
fn remote_bind_requires_explicit_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.web.enabled = true;
    cfg.web.bind_address = "0.0.0.0".to_string();

    let err = ensure_remote_bind_allowed(&cfg).unwrap_err();
    assert!(err.to_string().contains("web.allow_remote=true"));
}

#[test]
fn remote_bind_is_allowed_when_basic_auth_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.web.enabled = true;
    cfg.web.bind_address = "0.0.0.0".to_string();
    cfg.web.allow_remote = true;
    let (username, password) = test_basic_auth_credentials();
    cfg.web.username = username;
    cfg.web.password = password;

    assert!(ensure_remote_bind_allowed(&cfg).is_ok());
}

#[test]
fn remote_bind_requires_basic_auth_when_exposed() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.web.enabled = true;
    cfg.web.bind_address = "0.0.0.0".to_string();
    cfg.web.allow_remote = true;
    cfg.web.api_key = test_api_key();

    let err = ensure_remote_bind_allowed(&cfg).unwrap_err();
    assert!(err.to_string().contains("web.username/web.password"));
}

#[test]
fn panic_message_extracts_str_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
    assert_eq!(super::panic_message(payload), "boom");
}

#[test]
fn clamp_link_list_limit_stays_within_guardrails() {
    assert_eq!(super::clamp_link_list_limit(None), 100);
    assert_eq!(super::clamp_link_list_limit(Some(0)), 1);
    assert_eq!(super::clamp_link_list_limit(Some(50_000)), 10_000);
}

#[test]
fn panic_message_extracts_string_payload() {
    let payload: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());
    assert_eq!(super::panic_message(payload), "boom");
}

#[test]
fn failed_scan_outcome_is_hidden_when_newer_scan_run_exists() {
    let outcome = LastScanOutcome {
        finished_at: "2026-03-29 10:00:00 UTC".to_string(),
        scope_label: "Anime".to_string(),
        dry_run: false,
        search_missing: true,
        success: false,
        message: "boom".to_string(),
    };

    assert!(!super::should_surface_scan_outcome(
        &outcome,
        Some("2026-03-29 10:05:00 UTC")
    ));
}

#[test]
fn failed_cleanup_outcome_is_hidden_when_newer_report_exists() {
    let outcome = LastCleanupAuditOutcome {
        finished_at: "2026-03-29 10:00:00 UTC".to_string(),
        scope_label: "Anime".to_string(),
        libraries_label: "Anime".to_string(),
        success: false,
        message: "boom".to_string(),
        report_path: None,
    };

    assert!(!super::should_surface_cleanup_audit_outcome(
        &outcome,
        Some("2026-03-29 10:05:00 UTC")
    ));
}

#[test]
fn resolve_cleanup_libraries_preserves_commas_and_filters_scope() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.libraries.push(LibraryConfig {
        name: "Movies, Archive".to_string(),
        path: dir.path().join("movies"),
        media_type: MediaType::Movie,
        content_type: Some(ContentType::Movie),
        depth: 1,
    });

    let selected = super::resolve_cleanup_libraries(
        &cfg,
        CleanupScope::Movie,
        &["Movies, Archive".to_string(), "Anime".to_string()],
    )
    .unwrap();

    assert_eq!(selected, vec!["Movies, Archive".to_string()]);
}

#[test]
fn infer_cleanup_scope_single_content_type() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = test_config(dir.path());
    // test_config has one Anime library
    assert_eq!(
        super::infer_cleanup_scope(&cfg, &["Anime".to_string()]),
        CleanupScope::Anime
    );
}

#[test]
fn infer_cleanup_scope_mixed_returns_all() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.libraries.push(LibraryConfig {
        name: "Movies".to_string(),
        path: dir.path().join("movies"),
        media_type: MediaType::Movie,
        content_type: Some(ContentType::Movie),
        depth: 1,
    });
    assert_eq!(
        super::infer_cleanup_scope(&cfg, &["Anime".to_string(), "Movies".to_string()]),
        CleanupScope::All
    );
}

#[test]
fn infer_cleanup_scope_empty_selection_uses_all_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path());
    cfg.libraries.push(LibraryConfig {
        name: "Movies".to_string(),
        path: dir.path().join("movies"),
        media_type: MediaType::Movie,
        content_type: Some(ContentType::Movie),
        depth: 1,
    });
    // Empty selection → looks at all configured libraries → mixed → All
    assert_eq!(super::infer_cleanup_scope(&cfg, &[]), CleanupScope::All);
}
