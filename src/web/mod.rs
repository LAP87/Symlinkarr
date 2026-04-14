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
        header::{AUTHORIZATION, COOKIE, HOST, ORIGIN, REFERER, SET_COOKIE, WWW_AUTHENTICATE},
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
    },
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::FutureExt;
use serde_json::json;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info, warn};

use crate::cleanup_audit::{CleanupAuditor, CleanupReport, CleanupScope};
use crate::config::{Config, ContentType, LibraryConfig};
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

pub(crate) fn resolve_cleanup_report_path(
    backup_dir: &Path,
    report: &str,
) -> anyhow::Result<PathBuf> {
    let trimmed = report.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Cleanup report path is required");
    }

    let requested = Path::new(trimmed);
    let backup_root = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());

    if requested.is_absolute() {
        let canonical = requested
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Cleanup report not found: {}", requested.display()))?;
        if !canonical.starts_with(&backup_root) {
            anyhow::bail!("Cleanup report must be inside the configured backup directory");
        }
        return Ok(canonical);
    }

    if requested.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("Cleanup report path must stay inside the configured backup directory");
    }

    let joined = backup_dir.join(requested);
    let joined_parent = joined.parent().unwrap_or(backup_dir);
    let canonical_parent = joined_parent.canonicalize().map_err(|_| {
        anyhow::anyhow!(
            "Cleanup report parent not found: {}",
            joined_parent.display()
        )
    })?;
    if !canonical_parent.starts_with(&backup_root) {
        anyhow::bail!("Cleanup report must be inside the configured backup directory");
    }

    let canonical = if joined.exists() {
        joined
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Cleanup report not found: {}", joined.display()))?
    } else {
        canonical_parent.join(
            joined
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Cleanup report filename is missing"))?,
        )
    };

    if !canonical.starts_with(&backup_root) {
        anyhow::bail!("Cleanup report must be inside the configured backup directory");
    }

    Ok(canonical)
}

pub(crate) fn clamp_link_list_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(100).clamp(1, 10_000)
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

/// Infer the cleanup scope from the content types of the selected libraries.
/// If all selected libraries share a single content type, use that as the scope.
/// Otherwise (mixed types or no selection) use `All`.
pub(crate) fn infer_cleanup_scope(cfg: &Config, selected_libraries: &[String]) -> CleanupScope {
    use std::collections::HashSet;
    let types: HashSet<ContentType> = cfg
        .libraries
        .iter()
        .filter(|lib| {
            selected_libraries.is_empty()
                || selected_libraries
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&lib.name))
        })
        .map(effective_content_type)
        .collect();

    if types.len() == 1 {
        match types.into_iter().next().unwrap() {
            ContentType::Anime => CleanupScope::Anime,
            ContentType::Tv => CleanupScope::Tv,
            ContentType::Movie => CleanupScope::Movie,
        }
    } else {
        CleanupScope::All
    }
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

fn ensure_remote_bind_allowed(config: &Config) -> Result<()> {
    if config.web.requires_remote_ack() && !config.web.allow_remote {
        anyhow::bail!(
            "Refusing to start web UI on {} without web.allow_remote=true",
            config.web.normalized_bind_address()
        );
    }
    if config.web.requires_remote_ack() && config.web.allow_remote && !config.web.has_basic_auth() {
        anyhow::bail!(
            "Refusing to start web UI on {} with web.allow_remote=true unless web.username/web.password are configured",
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
    request_cookie_value(headers, BROWSER_SESSION_COOKIE)
        .as_deref()
        .map(|token| constant_time_str_eq(token, state.browser_session_token()))
        .unwrap_or(false)
}

fn has_valid_browser_csrf_token(token: &str, state: &WebState) -> bool {
    constant_time_str_eq(token.trim(), state.browser_session_token())
}

fn constant_time_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();

    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());

    for idx in 0..max_len {
        let left_byte = left.get(idx).copied().unwrap_or_default();
        let right_byte = right.get(idx).copied().unwrap_or_default();
        diff |= usize::from(left_byte ^ right_byte);
    }

    diff == 0
}

fn request_basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let authorization = headers.get(AUTHORIZATION).and_then(header_value_str)?;
    let encoded = authorization
        .strip_prefix("Basic ")
        .or_else(|| authorization.strip_prefix("basic "))?;
    let decoded = BASE64_STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

fn request_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(api_key) = headers.get("x-api-key").and_then(header_value_str) {
        return Some(api_key.to_string());
    }

    let authorization = headers.get(AUTHORIZATION).and_then(header_value_str)?;
    authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
        .map(|value| value.to_string())
}

fn has_valid_basic_auth(headers: &HeaderMap, state: &WebState) -> bool {
    if !state.config.web.has_basic_auth() {
        return false;
    }

    let Some((username, password)) = request_basic_credentials(headers) else {
        return false;
    };

    constant_time_str_eq(&username, state.config.web.username.trim())
        && constant_time_str_eq(&password, &state.config.web.password)
}

fn has_valid_api_key(headers: &HeaderMap, state: &WebState) -> bool {
    if !state.config.web.has_api_key_auth() {
        return false;
    }

    let Some(api_key) = request_api_key(headers) else {
        return false;
    };

    constant_time_str_eq(&api_key, &state.config.web.api_key)
}

fn unauthorized_auth_response(path: &str, offer_basic: bool) -> axum::response::Response {
    let mut response = if path.starts_with("/api/") {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "authentication required"
            })),
        )
            .into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "Authentication required.").into_response()
    };

    if offer_basic {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Symlinkarr\""),
        );
    }

    response
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

fn invalid_browser_csrf_response(path: &str) -> axum::response::Response {
    if path.starts_with("/api/") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "browser mutation blocked; reload the Symlinkarr UI and retry with the issued CSRF token"
            })),
        )
            .into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Browser mutation blocked; reload the Symlinkarr UI and retry with the issued CSRF token.",
    )
        .into_response()
}

async fn add_security_headers(
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(CONTENT_SECURITY_POLICY_VALUE),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("same-origin"));
    response.headers_mut().insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

fn generate_browser_session_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_ok() {
        return Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }

    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        return Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }

    anyhow::bail!("OS entropy unavailable for browser session token generation")
}

async fn guard_web_auth(
    State(state): State<WebState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let path = request.uri().path().to_string();
    let is_api = path.starts_with("/api/");
    let require_basic = state.config.web.has_basic_auth();
    let require_api_auth =
        is_api && (state.config.web.has_basic_auth() || state.config.web.has_api_key_auth());

    if !require_basic && !require_api_auth {
        return next.run(request).await;
    }

    let basic_ok = has_valid_basic_auth(request.headers(), &state);
    let api_key_ok = is_api && has_valid_api_key(request.headers(), &state);

    let authorized = if is_api {
        (!require_api_auth) || basic_ok || api_key_ok
    } else {
        !require_basic || basic_ok
    };

    if !authorized {
        let offer_basic = state.config.web.has_basic_auth();
        if is_api {
            warn!(path, "blocked API request without configured auth");
        } else {
            warn!(path, "blocked web request without configured basic auth");
        }
        return unauthorized_auth_response(&path, offer_basic);
    }

    next.run(request).await
}

async fn guard_browser_mutations(
    State(state): State<WebState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let is_api = path.starts_with("/api/");
    let origin = request.headers().get(ORIGIN).and_then(header_value_str);
    let referer = request.headers().get(REFERER).and_then(header_value_str);
    let host = request.headers().get(HOST).and_then(header_value_str);
    let has_browser_metadata = request_has_browser_metadata(request.headers());
    let has_valid_session = has_valid_browser_session(request.headers(), &state);
    let should_issue_session = method_receives_browser_session(&method) && !has_valid_session;
    let enforce_browser_guard = state.browser_mutation_guard_enabled();

    if enforce_browser_guard && method_requires_same_origin(&method) {
        let require_browser_session = !is_api || has_browser_metadata;

        if has_browser_metadata && !is_same_origin_browser_mutation(request.headers()) {
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

        if require_browser_session && !has_valid_session {
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

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("Shutdown signal received; stopping web UI");
}

/// Start the web server
///
/// Binds to the specified port and serves the web UI.
/// This function blocks until the server is shut down.
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
        assert!(status_page.contains("Service connectivity"));
        assert!(status_page.contains("Recent Links"));
        assert!(status_page.contains("Active Links"));

        let (status, scan_page) = get_html(&router, "/scan").await;
        assert_eq!(status, 200);
        assert!(scan_page.contains("Start Scan"));
        assert!(scan_page.contains("Search Missing"));
        assert!(scan_page.contains("Latest Run"));

        let (status, cleanup_page) = get_html(&router, "/cleanup").await;
        assert_eq!(status, 200);
        assert!(cleanup_page.contains("How Cleanup Works"));
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
        assert!(body.contains("Dashboard"));
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
        assert!(body.contains("Configuration"));
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
        assert!(body.contains("Configuration"));
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
}
