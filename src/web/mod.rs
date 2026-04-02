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
        header::{COOKIE, HOST, ORIGIN, REFERER, SET_COOKIE},
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
    },
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::FutureExt;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info, warn};

use crate::cleanup_audit::{CleanupAuditor, CleanupReport, CleanupScope};
use crate::config::{Config, ContentType, LibraryConfig};
use crate::db::Database;

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
    pub fn new(config: Config, database: Database) -> Self {
        Self {
            config: Arc::new(config),
            database: Arc::new(database),
            browser_session_token: Arc::new(generate_browser_session_token()),
            background_jobs: Arc::new(Mutex::new(BackgroundJobState::default())),
        }
    }

    fn browser_session_token(&self) -> &str {
        self.browser_session_token.as_str()
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
                            "Completed: {} added or updated, {} removed.",
                            added, removed
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
                    let (repaired, failed, skipped, stale) =
                        crate::commands::repair::summarize_repair_results(&results);
                    let message = if repaired == 0 && failed == 0 && skipped == 0 && stale == 0 {
                        "Repair completed. No dead links required action.".to_string()
                    } else {
                        format!(
                            "Repair completed: {} repaired, {} unrepairable, {} skipped, {} stale record(s).",
                            repaired, failed, skipped, stale
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

fn cleanup_scope_label(scope: CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Anime => "Anime",
        CleanupScope::Tv => "TV",
        CleanupScope::Movie => "Movies",
        CleanupScope::All => "All Libraries",
    }
}

fn cleanup_scope_slug(scope: CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Anime => "anime",
        CleanupScope::Tv => "tv",
        CleanupScope::Movie => "movie",
        CleanupScope::All => "all",
    }
}

fn cleanup_libraries_label(selected_libraries: &[String]) -> String {
    match selected_libraries {
        [] => "All Libraries".to_string(),
        [single] => single.clone(),
        [first, second] => format!("{}, {}", first, second),
        [first, second, third] => format!("{}, {}, {}", first, second, third),
        many => format!("{} libraries", many.len()),
    }
}

pub(crate) fn latest_cleanup_report_path(backup_dir: &Path) -> Option<PathBuf> {
    let mut reports: Vec<_> = std::fs::read_dir(backup_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            name.to_string_lossy().starts_with("cleanup-audit-")
                && name.to_string_lossy().ends_with(".json")
        })
        .collect();

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

pub(crate) fn load_cleanup_report(path: &Path) -> Option<CleanupReport> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

pub(crate) fn latest_cleanup_report_created_at(backup_dir: &Path) -> Option<String> {
    let path = latest_cleanup_report_path(backup_dir)?;
    let report = load_cleanup_report(&path)?;
    Some(
        report
            .created_at
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    )
}

fn effective_content_type(library: &LibraryConfig) -> ContentType {
    library
        .content_type
        .unwrap_or(ContentType::from_media_type(library.media_type))
}

fn library_matches_cleanup_scope(library: &LibraryConfig, scope: CleanupScope) -> bool {
    match scope {
        CleanupScope::Anime => effective_content_type(library) == ContentType::Anime,
        CleanupScope::Tv => effective_content_type(library) == ContentType::Tv,
        CleanupScope::Movie => effective_content_type(library) == ContentType::Movie,
        CleanupScope::All => true,
    }
}

fn resolve_cleanup_libraries(
    cfg: &Config,
    scope: CleanupScope,
    selected_libraries: &[String],
) -> std::result::Result<Vec<String>, String> {
    let selected_names = selected_libraries
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();

    let selected_names_set = selected_names.iter().copied().collect::<HashSet<_>>();

    let unknown = selected_names
        .iter()
        .copied()
        .filter(|want| {
            !cfg.libraries
                .iter()
                .any(|lib| lib.name.eq_ignore_ascii_case(want))
        })
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown library filter(s): {}. Available: {}",
            unknown.join(", "),
            cfg.libraries
                .iter()
                .map(|library| library.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let canonical = cfg
        .libraries
        .iter()
        .filter(|library| {
            selected_names_set.is_empty()
                || selected_names_set
                    .iter()
                    .any(|name| library.name.eq_ignore_ascii_case(name))
        })
        .filter(|library| library_matches_cleanup_scope(library, scope))
        .map(|library| library.name.clone())
        .collect::<Vec<_>>();

    if !selected_names_set.is_empty() && canonical.is_empty() {
        return Err(format!(
            "No libraries matched scope {:?} for selection: {}",
            scope,
            selected_libraries.join(", ")
        ));
    }

    Ok(canonical)
}

fn cleanup_audit_output_path(
    config: &Config,
    scope: CleanupScope,
    selected_libraries: &[String],
    timestamp: String,
) -> std::path::PathBuf {
    let scope_slug = cleanup_scope_slug(scope);
    if selected_libraries.len() == 1 {
        config.backup.path.join(format!(
            "cleanup-audit-{}-{}-{}.json",
            scope_slug, selected_libraries[0], timestamp
        ))
    } else if !selected_libraries.is_empty() {
        config.backup.path.join(format!(
            "cleanup-audit-{}-multi-{}-{}.json",
            scope_slug,
            selected_libraries.len(),
            timestamp
        ))
    } else {
        config
            .backup
            .path
            .join(format!("cleanup-audit-{}-{}.json", scope_slug, timestamp))
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            guard_browser_mutations,
        ))
        .layer(TraceLayer::new_for_http())
        .nest_service("/static", ServeDir::new(static_dir()))
        .with_state(state)
}

fn ensure_remote_bind_allowed(config: &Config) -> Result<()> {
    if config.web.requires_remote_ack() && !config.web.allow_remote {
        anyhow::bail!(
            "Refusing to start web UI on {} without web.allow_remote=true",
            config.web.normalized_bind_address()
        );
    }
    Ok(())
}

const BROWSER_SESSION_COOKIE: &str = "symlinkarr_browser_session";

fn method_requires_same_origin(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn method_receives_browser_session(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD)
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

fn request_has_browser_metadata(headers: &HeaderMap) -> bool {
    headers.contains_key(ORIGIN) || headers.contains_key(REFERER)
}

fn request_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(COOKIE).and_then(header_value_str)?;
    raw.split(';').find_map(|entry| {
        let (cookie_name, cookie_value) = entry.trim().split_once('=')?;
        if cookie_name.trim() == name {
            Some(cookie_value.trim().to_string())
        } else {
            None
        }
    })
}

fn browser_session_cookie_header(token: &str) -> String {
    format!("{BROWSER_SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict")
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

fn has_valid_browser_session(headers: &HeaderMap, state: &WebState) -> bool {
    request_cookie_value(headers, BROWSER_SESSION_COOKIE).as_deref()
        == Some(state.browser_session_token())
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

fn missing_browser_session_response(path: &str) -> axum::response::Response {
    if path.starts_with("/api/") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "browser mutation blocked; refresh the Symlinkarr UI from the same origin and retry with the issued browser session"
            })),
        )
            .into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Browser mutation blocked; refresh the Symlinkarr UI from the same origin and retry.",
    )
        .into_response()
}

fn generate_browser_session_token() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::getrandom(&mut bytes).is_ok() {
        return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:016x}{:08x}", nanos, std::process::id())
}

async fn guard_browser_mutations(
    State(state): State<WebState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let origin = request.headers().get(ORIGIN).and_then(header_value_str);
    let referer = request.headers().get(REFERER).and_then(header_value_str);
    let host = request.headers().get(HOST).and_then(header_value_str);
    let has_browser_metadata = request_has_browser_metadata(request.headers());
    let has_valid_session = has_valid_browser_session(request.headers(), &state);
    let should_issue_session = method_receives_browser_session(&method) && !has_valid_session;

    if method_requires_same_origin(&method) && has_browser_metadata {
        if !is_same_origin_browser_mutation(request.headers()) {
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

        if !has_valid_session {
            warn!(
                method = %method,
                path,
                host = host.unwrap_or("<missing>"),
                origin = origin.unwrap_or("<missing>"),
                referer = referer.unwrap_or("<missing>"),
                "blocked browser mutation without issued session cookie"
            );
            return missing_browser_session_response(request.uri().path());
        }
    }

    let mut response = next.run(request).await;
    if should_issue_session {
        if let Ok(value) = HeaderValue::from_str(&browser_session_cookie_header(
            state.browser_session_token(),
        )) {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }
    response
}

/// Start the web server
///
/// Binds to the specified port and serves the web UI.
/// This function blocks until the server is shut down.
pub async fn serve(config: Config, db: Database, port: u16) -> Result<()> {
    ensure_remote_bind_allowed(&config)?;
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
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
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

        let (status, cleanup_page) = get_html(&router, "/cleanup").await;
        assert_eq!(status, 200);
        assert!(cleanup_page.contains("Anime Remediation Backlog"));
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
        let (_, headers, _) = get_html_with_headers(&router, "/", &[]).await;
        let cookie = browser_session_cookie(&headers);
        let (status, body) = post_json_with_headers(
            &router,
            "/api/v1/cleanup/audit",
            serde_json::json!({ "scope": "anime" }),
            &[
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

    #[tokio::test]
    async fn browser_same_origin_mutations_require_issued_session_cookie() {
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

        assert_eq!(status, 403);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["error"],
            "browser mutation blocked; refresh the Symlinkarr UI from the same origin and retry with the issued browser session"
        );
    }

    #[tokio::test]
    async fn dashboard_get_sets_browser_session_cookie() {
        let router = test_router().await;
        let (status, headers, body) = get_html_with_headers(&router, "/", &[]).await;

        assert_eq!(status, 200);
        assert!(body.contains("Dashboard"));
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
    fn remote_bind_is_allowed_when_acknowledged() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.web.enabled = true;
        cfg.web.bind_address = "0.0.0.0".to_string();
        cfg.web.allow_remote = true;

        assert!(ensure_remote_bind_allowed(&cfg).is_ok());
    }

    #[test]
    fn panic_message_extracts_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(super::panic_message(payload), "boom");
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
}
