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
    middleware,
    routing::{get, post},
    Router,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::FutureExt;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info};

#[cfg(test)]
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};

#[cfg(test)]
use self::auth::constant_time_str_eq;
use self::auth::{
    add_security_headers, ensure_remote_bind_allowed, generate_browser_session_token,
    guard_browser_mutations, guard_web_auth, has_valid_browser_csrf_token,
    invalid_browser_csrf_response,
};
pub(crate) use self::cleanup::{
    clamp_link_list_limit, infer_cleanup_scope, latest_cleanup_report_created_at,
    latest_cleanup_report_path, load_cleanup_report, resolve_cleanup_report_path,
};
use self::cleanup::{
    cleanup_audit_output_path, cleanup_libraries_label, cleanup_scope_label,
    resolve_cleanup_libraries,
};
use crate::cleanup_audit::{CleanupAuditor, CleanupScope};
use crate::config::Config;
use crate::db::Database;

const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'";

#[derive(Clone, Debug)]
pub(crate) struct ActiveScanJob {
    pub started_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveCleanupAuditJob {
    pub started_at: String,
    pub scope_label: String,
    pub libraries_label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveRepairJob {
    pub started_at: String,
    pub scope_label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LastScanOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LastCleanupAuditOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub libraries_label: String,
    pub success: bool,
    pub message: String,
    pub report_path: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct LastRepairOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stale: usize,
}

#[derive(Clone, Debug, Default)]
struct BackgroundJobState {
    active_scan: Option<ActiveScanJob>,
    active_cleanup_audit: Option<ActiveCleanupAuditJob>,
    active_repair: Option<ActiveRepairJob>,
    last_scan_outcome: Option<LastScanOutcome>,
    last_cleanup_audit_outcome: Option<LastCleanupAuditOutcome>,
    last_repair_outcome: Option<LastRepairOutcome>,
}

/// Shared application state passed to handlers
#[derive(Clone)]
pub struct WebState {
    pub config: Arc<Config>,
    pub database: Arc<Database>,
    browser_session_token: Arc<String>,
    background_jobs: Arc<Mutex<BackgroundJobState>>,
}

impl WebState {
    #[cfg(test)]
    pub fn new(config: Config, database: Database) -> Self {
        Self::try_new(config, database).expect("failed to generate secure browser session token")
    }

    pub fn try_new(config: Config, database: Database) -> Result<Self> {
        Ok(Self {
            config: Arc::new(config),
            database: Arc::new(database),
            browser_session_token: Arc::new(generate_browser_session_token()?),
            background_jobs: Arc::new(Mutex::new(BackgroundJobState::default())),
        })
    }

    fn browser_session_token(&self) -> &str {
        self.browser_session_token.as_str()
    }

    fn browser_mutation_guard_enabled(&self) -> bool {
        self.config.web.requires_remote_ack()
    }

    pub(crate) async fn active_scan(&self) -> Option<ActiveScanJob> {
        self.background_jobs.lock().await.active_scan.clone()
    }

    pub(crate) async fn active_cleanup_audit(&self) -> Option<ActiveCleanupAuditJob> {
        self.background_jobs
            .lock()
            .await
            .active_cleanup_audit
            .clone()
    }

    pub(crate) async fn active_repair(&self) -> Option<ActiveRepairJob> {
        self.background_jobs.lock().await.active_repair.clone()
    }

    pub(crate) async fn last_scan_outcome(&self) -> Option<LastScanOutcome> {
        self.background_jobs.lock().await.last_scan_outcome.clone()
    }

    pub(crate) async fn last_cleanup_audit_outcome(&self) -> Option<LastCleanupAuditOutcome> {
        self.background_jobs
            .lock()
            .await
            .last_cleanup_audit_outcome
            .clone()
    }

    pub(crate) async fn last_repair_outcome(&self) -> Option<LastRepairOutcome> {
        self.background_jobs
            .lock()
            .await
            .last_repair_outcome
            .clone()
    }

    pub(crate) async fn start_scan(
        &self,
        dry_run: bool,
        search_missing: bool,
        library_filter: Option<String>,
    ) -> std::result::Result<ActiveScanJob, String> {
        let library_filter = library_filter
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        crate::commands::selected_libraries(self.config.as_ref(), library_filter.as_deref())
            .map_err(|err| err.to_string())?;

        let mut background_jobs = self.background_jobs.lock().await;
        if let Some(job) = background_jobs.active_scan.clone() {
            return Err(format!(
                "A scan is already running for {} (started {}).",
                job.scope_label, job.started_at
            ));
        }
        if let Some(job) = background_jobs.active_cleanup_audit.clone() {
            return Err(format!(
                "A cleanup audit is already running for {} ({}) started {}.",
                job.scope_label, job.libraries_label, job.started_at
            ));
        }
        if let Some(job) = background_jobs.active_repair.clone() {
            return Err(format!(
                "A repair run is already running for {} (started {}).",
                job.scope_label, job.started_at
            ));
        }

        let job = ActiveScanJob {
            started_at: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            scope_label: library_filter
                .clone()
                .unwrap_or_else(|| "All Libraries".to_string()),
            dry_run,
            search_missing,
        };
        background_jobs.active_scan = Some(job.clone());
        drop(background_jobs);

        let config = self.config.clone();
        let database = self.database.clone();
        let background_jobs = self.background_jobs.clone();
        let background_job = job.clone();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                crate::commands::scan::run_scan(
                    config.as_ref(),
                    database.as_ref(),
                    dry_run,
                    search_missing,
                    crate::OutputFormat::Json,
                    library_filter.as_deref(),
                )
                .await
            })
            .catch_unwind()
            .await;

            let finished_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
            let outcome = match result {
                Ok(Ok((added, removed))) => {
                    let dead_link_suffix = tracked_dead_link_suffix(database.as_ref()).await;
                    info!(
                        "Background scan completed (scope={}, dry_run={}, search_missing={}): added_or_updated={}, removed={}",
                        background_job.scope_label,
                        background_job.dry_run,
                        background_job.search_missing,
                        added,
                        removed
                    );
                    LastScanOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        dry_run: background_job.dry_run,
                        search_missing: background_job.search_missing,
                        success: true,
                        message: format!(
                            "Completed: {} added or updated, {} removed.{}",
                            added, removed, dead_link_suffix
                        ),
                    }
                }
                Ok(Err(err)) => {
                    error!(
                        "Background scan failed (scope={}, dry_run={}, search_missing={}): {}",
                        background_job.scope_label,
                        background_job.dry_run,
                        background_job.search_missing,
                        err
                    );
                    LastScanOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        dry_run: background_job.dry_run,
                        search_missing: background_job.search_missing,
                        success: false,
                        message: err.to_string(),
                    }
                }
                Err(panic) => {
                    let message = format!(
                        "internal panic while running background scan: {}",
                        panic_message(panic)
                    );
                    error!(
                        "Background scan panicked (scope={}, dry_run={}, search_missing={}): {}",
                        background_job.scope_label,
                        background_job.dry_run,
                        background_job.search_missing,
                        message
                    );
                    LastScanOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        dry_run: background_job.dry_run,
                        search_missing: background_job.search_missing,
                        success: false,
                        message,
                    }
                }
            };

            let mut background_jobs = background_jobs.lock().await;
            background_jobs.last_scan_outcome = Some(outcome);
            background_jobs.active_scan = None;
        });

        Ok(job)
    }

    pub(crate) async fn start_cleanup_audit(
        &self,
        scope: CleanupScope,
        selected_libraries: Vec<String>,
    ) -> std::result::Result<ActiveCleanupAuditJob, String> {
        let canonical_libraries =
            resolve_cleanup_libraries(self.config.as_ref(), scope, &selected_libraries)?;

        let mut background_jobs = self.background_jobs.lock().await;
        if let Some(job) = background_jobs.active_scan.clone() {
            return Err(format!(
                "A scan is already running for {} (started {}).",
                job.scope_label, job.started_at
            ));
        }
        if let Some(job) = background_jobs.active_cleanup_audit.clone() {
            return Err(format!(
                "A cleanup audit is already running for {} ({}) started {}.",
                job.scope_label, job.libraries_label, job.started_at
            ));
        }
        if let Some(job) = background_jobs.active_repair.clone() {
            return Err(format!(
                "A repair run is already running for {} (started {}).",
                job.scope_label, job.started_at
            ));
        }

        let job = ActiveCleanupAuditJob {
            started_at: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            scope_label: cleanup_scope_label(scope).to_string(),
            libraries_label: cleanup_libraries_label(&canonical_libraries),
        };
        background_jobs.active_cleanup_audit = Some(job.clone());
        drop(background_jobs);

        let config = self.config.clone();
        let database = self.database.clone();
        let background_jobs = self.background_jobs.clone();
        let background_job = job.clone();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                let auditor =
                    CleanupAuditor::new_with_progress(config.as_ref(), database.as_ref(), false);
                let output_path = cleanup_audit_output_path(
                    config.as_ref(),
                    scope,
                    canonical_libraries.as_slice(),
                    Utc::now().format("%Y%m%d-%H%M%S").to_string(),
                );
                auditor
                    .run_audit_filtered(
                        scope,
                        (!canonical_libraries.is_empty()).then_some(canonical_libraries.as_slice()),
                        Some(&output_path),
                    )
                    .await
            })
            .catch_unwind()
            .await;

            let finished_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
            let outcome = match result {
                Ok(Ok(report_path)) => {
                    info!(
                        "Background cleanup audit completed (scope={}, libraries={}): {}",
                        background_job.scope_label,
                        background_job.libraries_label,
                        report_path.display()
                    );
                    LastCleanupAuditOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        libraries_label: background_job.libraries_label.clone(),
                        success: true,
                        message: format!("Report written to {}", report_path.display()),
                        report_path: Some(report_path.to_string_lossy().to_string()),
                    }
                }
                Ok(Err(err)) => {
                    error!(
                        "Background cleanup audit failed (scope={}, libraries={}): {}",
                        background_job.scope_label, background_job.libraries_label, err
                    );
                    LastCleanupAuditOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        libraries_label: background_job.libraries_label.clone(),
                        success: false,
                        message: err.to_string(),
                        report_path: None,
                    }
                }
                Err(panic) => {
                    let message = format!(
                        "internal panic while running background cleanup audit: {}",
                        panic_message(panic)
                    );
                    error!(
                        "Background cleanup audit panicked (scope={}, libraries={}): {}",
                        background_job.scope_label, background_job.libraries_label, message
                    );
                    LastCleanupAuditOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        libraries_label: background_job.libraries_label.clone(),
                        success: false,
                        message,
                        report_path: None,
                    }
                }
            };

            let mut background_jobs = background_jobs.lock().await;
            background_jobs.last_cleanup_audit_outcome = Some(outcome);
            background_jobs.active_cleanup_audit = None;
        });

        Ok(job)
    }

    pub(crate) async fn start_repair(&self) -> std::result::Result<ActiveRepairJob, String> {
        let mut background_jobs = self.background_jobs.lock().await;
        if let Some(job) = background_jobs.active_scan.clone() {
            return Err(format!(
                "A scan is already running for {} (started {}).",
                job.scope_label, job.started_at
            ));
        }
        if let Some(job) = background_jobs.active_cleanup_audit.clone() {
            return Err(format!(
                "A cleanup audit is already running for {} ({}) started {}.",
                job.scope_label, job.libraries_label, job.started_at
            ));
        }
        if let Some(job) = background_jobs.active_repair.clone() {
            return Err(format!(
                "A repair run is already running for {} (started {}).",
                job.scope_label, job.started_at
            ));
        }

        let job = ActiveRepairJob {
            started_at: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            scope_label: "All Libraries".to_string(),
        };
        background_jobs.active_repair = Some(job.clone());
        drop(background_jobs);

        let config = self.config.clone();
        let database = self.database.clone();
        let background_jobs = self.background_jobs.clone();
        let background_job = job.clone();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                crate::commands::repair::execute_repair_auto(
                    config.as_ref(),
                    database.as_ref(),
                    None,
                    false,
                    false,
                )
                .await
            })
            .catch_unwind()
            .await;

            let finished_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
            let outcome = match result {
                Ok(Ok(results)) => {
                    let dead_link_suffix = tracked_dead_link_suffix(database.as_ref()).await;
                    let (repaired, failed, skipped, stale) =
                        crate::commands::repair::summarize_repair_results(&results);
                    let message = if repaired == 0 && failed == 0 && skipped == 0 && stale == 0 {
                        format!(
                            "Repair completed. No dead links required action.{}",
                            dead_link_suffix
                        )
                    } else {
                        format!(
                            "Repair completed: {} repaired, {} unrepairable, {} skipped, {} stale record(s).{}",
                            repaired, failed, skipped, stale, dead_link_suffix
                        )
                    };

                    info!(
                        "Background repair completed (scope={}): repaired={}, failed={}, skipped={}, stale={}",
                        background_job.scope_label, repaired, failed, skipped, stale
                    );

                    LastRepairOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        success: true,
                        message,
                        repaired,
                        failed,
                        skipped,
                        stale,
                    }
                }
                Ok(Err(err)) => {
                    error!(
                        "Background repair failed (scope={}): {}",
                        background_job.scope_label, err
                    );
                    LastRepairOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        success: false,
                        message: err.to_string(),
                        repaired: 0,
                        failed: 0,
                        skipped: 0,
                        stale: 0,
                    }
                }
                Err(panic) => {
                    let message = format!(
                        "internal panic while running background repair: {}",
                        panic_message(panic)
                    );
                    error!(
                        "Background repair panicked (scope={}): {}",
                        background_job.scope_label, message
                    );
                    LastRepairOutcome {
                        finished_at,
                        scope_label: background_job.scope_label.clone(),
                        success: false,
                        message,
                        repaired: 0,
                        failed: 0,
                        skipped: 0,
                        stale: 0,
                    }
                }
            };

            let mut background_jobs = background_jobs.lock().await;
            background_jobs.last_repair_outcome = Some(outcome);
            background_jobs.active_repair = None;
        });

        Ok(job)
    }

    #[cfg(test)]
    pub(crate) async fn set_active_scan_for_test(&self, job: Option<ActiveScanJob>) {
        self.background_jobs.lock().await.active_scan = job;
    }

    #[cfg(test)]
    pub(crate) async fn set_active_cleanup_audit_for_test(
        &self,
        job: Option<ActiveCleanupAuditJob>,
    ) {
        self.background_jobs.lock().await.active_cleanup_audit = job;
    }

    #[cfg(test)]
    pub(crate) async fn set_last_scan_outcome_for_test(&self, outcome: Option<LastScanOutcome>) {
        self.background_jobs.lock().await.last_scan_outcome = outcome;
    }

    #[cfg(test)]
    pub(crate) async fn set_last_cleanup_audit_outcome_for_test(
        &self,
        outcome: Option<LastCleanupAuditOutcome>,
    ) {
        self.background_jobs.lock().await.last_cleanup_audit_outcome = outcome;
    }

    #[cfg(test)]
    pub(crate) async fn set_active_repair_for_test(&self, job: Option<ActiveRepairJob>) {
        self.background_jobs.lock().await.active_repair = job;
    }

    #[cfg(test)]
    pub(crate) async fn set_last_repair_outcome_for_test(
        &self,
        outcome: Option<LastRepairOutcome>,
    ) {
        self.background_jobs.lock().await.last_repair_outcome = outcome;
    }
}

async fn tracked_dead_link_suffix(database: &Database) -> String {
    match database.get_stats().await {
        Ok((_, dead_links, _)) if dead_links > 0 => format!(
            " {} dead link(s) remain tracked and will continue to surface until repaired or pruned.",
            dead_links
        ),
        _ => String::new(),
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

pub(crate) fn parse_display_utc_timestamp(value: &str) -> Option<DateTime<Utc>> {
    ["%Y-%m-%d %H:%M:%S UTC", "%Y-%m-%d %H:%M:%S"]
        .into_iter()
        .find_map(|format| {
            NaiveDateTime::parse_from_str(value, format)
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
}

pub(crate) fn should_surface_scan_outcome(
    outcome: &LastScanOutcome,
    latest_run_started_at: Option<&str>,
) -> bool {
    if outcome.success {
        return latest_run_started_at.is_none();
    }

    let Some(latest_run_started_at) = latest_run_started_at else {
        return true;
    };

    match (
        parse_display_utc_timestamp(&outcome.finished_at),
        parse_display_utc_timestamp(latest_run_started_at),
    ) {
        (Some(outcome_finished_at), Some(latest_run_started_at)) => {
            outcome_finished_at >= latest_run_started_at
        }
        _ => true,
    }
}

pub(crate) fn should_surface_cleanup_audit_outcome(
    outcome: &LastCleanupAuditOutcome,
    latest_report_created_at: Option<&str>,
) -> bool {
    if outcome.success {
        return latest_report_created_at.is_none();
    }

    let Some(latest_report_created_at) = latest_report_created_at else {
        return true;
    };

    match (
        parse_display_utc_timestamp(&outcome.finished_at),
        parse_display_utc_timestamp(latest_report_created_at),
    ) {
        (Some(outcome_finished_at), Some(latest_report_created_at)) => {
            outcome_finished_at >= latest_report_created_at
        }
        _ => true,
    }
}

/// Create the Axum router with all routes
fn create_router(state: WebState) -> Router {
    // Main app routes
    let app_routes = Router::new()
        // Dashboard
        .route("/", get(handlers::get_dashboard))
        .route(
            "/dashboard/activity-feed",
            get(handlers::get_dashboard_activity_feed),
        )
        .route(
            "/dashboard/needs-attention",
            get(handlers::get_dashboard_needs_attention),
        )
        // Status & Health
        .route("/status", get(handlers::get_status))
        .route("/health", get(handlers::get_health))
        // Scan
        .route("/scan", get(handlers::get_scan))
        .route("/scan/trigger", post(handlers::post_scan_trigger))
        .route(
            "/scan/anime-overrides",
            post(handlers::post_scan_anime_override),
        )
        .route(
            "/scan/anime-overrides/delete",
            post(handlers::post_scan_anime_override_delete),
        )
        .route("/scan/history", get(handlers::get_scan_history))
        .route("/scan/history/{id}", get(handlers::get_scan_run_detail))
        // Cleanup
        .route("/cleanup", get(handlers::get_cleanup))
        .route(
            "/cleanup/anime-remediation",
            get(handlers::get_cleanup_anime_remediation),
        )
        .route(
            "/cleanup/anime-remediation/preview",
            post(handlers::post_cleanup_anime_remediation_preview),
        )
        .route(
            "/cleanup/anime-remediation/apply",
            post(handlers::post_cleanup_anime_remediation_apply),
        )
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
        .route("/discover/content", get(handlers::get_discover_content))
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
        .layer(middleware::from_fn(add_security_headers))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            guard_web_auth,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            guard_browser_mutations,
        ))
        .layer(TraceLayer::new_for_http())
        .nest_service("/static", ServeDir::new(static_dir()))
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("Shutdown signal received; stopping web UI");
}

/// Serve a minimal no-config page when config.yaml is missing.
/// This starts a lightweight HTTP server that only renders the setup page.
pub async fn serve_noconfig(port: u16) -> Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let router = Router::new()
        .route("/", get(handlers::get_noconfig))
        .nest_service("/static", ServeDir::new(static_dir()));

    crate::startup::emit_noconfig_banner(port);
    info!("Symlinkarr web UI (no-config mode) on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

pub async fn serve(config: Config, db: Database, port: u16) -> Result<()> {
    ensure_remote_bind_allowed(&config)?;
    let bind_address = config.web.normalized_bind_address();
    let state = WebState::try_new(config, db)?;
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
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Resolve the static file directory. Checks (in order):
/// 1. `src/web/static` (development)
/// 2. Next to the executable at `<exe_dir>/static` (legacy Docker / installed)
/// 3. System data dir at `/usr/local/share/symlinkarr/static` (current Docker / installed)
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
    let shared_path = std::path::PathBuf::from("/usr/local/share/symlinkarr/static");
    if shared_path.is_dir() {
        return shared_path;
    }
    // Fallback — will 404 but won't panic
    dev_path
}

mod auth;
mod cleanup;
#[cfg(test)]
mod tests;
