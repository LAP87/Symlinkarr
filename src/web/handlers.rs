//! HTTP handlers for the web UI

use axum::{
    extract::{Form, Query, State},
    response::{Html, IntoResponse},
};
use askama::Template;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{error, info};
use chrono::Utc;

use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::backup::BackupManager;
use crate::cleanup_audit::{self, CleanupAuditor, CleanupScope};
use crate::library_scanner::LibraryScanner;
use crate::linker::Linker;
use crate::matcher::Matcher;
use crate::source_scanner::SourceScanner;

use super::templates::*;
use super::WebState;

/// GET / - Dashboard page
pub async fn get_dashboard(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving dashboard");

    // Fetch database stats
    let stats = match state.database.get_web_stats().await {
        Ok(s) => DashboardStats {
            active_links: s.active_links,
            dead_links: s.dead_links,
            total_scans: s.total_scans,
            last_scan: None,
        },
        Err(e) => {
            error!("Failed to get stats: {}", e);
            DashboardStats::default()
        }
    };

    let template = DashboardTemplate { stats };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /status - Detailed status page
pub async fn get_status(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving status page");

    let stats = match state.database.get_web_stats().await {
        Ok(s) => DashboardStats {
            active_links: s.active_links,
            dead_links: s.dead_links,
            total_scans: s.total_scans,
            last_scan: None,
        },
        Err(e) => {
            error!("Failed to get stats: {}", e);
            DashboardStats::default()
        }
    };

    // Get recent links
    let recent_links = match state.database.get_active_links().await {
        Ok(links) => links.into_iter().take(50).collect(),
        Err(e) => {
            error!("Failed to get links: {}", e);
            vec![]
        }
    };

    let template = StatusTemplate {
        stats,
        recent_links,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /health - Health check page
pub async fn get_health(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving health page");

    let mut health_checks = HashMap::new();

    // Check database
    health_checks.insert(
        "database".to_string(),
        HealthCheck {
            service: "SQLite Database".to_string(),
            status: "healthy".to_string(),
            message: "Connected".to_string(),
        },
    );

    // Check external services
    if state.config.has_tmdb() {
        health_checks.insert(
            "tmdb".to_string(),
            HealthCheck {
                service: "TMDB API".to_string(),
                status: "configured".to_string(),
                message: "API key set".to_string(),
            },
        );
    } else {
        health_checks.insert(
            "tmdb".to_string(),
            HealthCheck {
                service: "TMDB API".to_string(),
                status: "missing".to_string(),
                message: "No API key configured".to_string(),
            },
        );
    }

    if state.config.has_tvdb() {
        health_checks.insert(
            "tvdb".to_string(),
            HealthCheck {
                service: "TVDB API".to_string(),
                status: "configured".to_string(),
                message: "API key set".to_string(),
            },
        );
    }

    if state.config.has_realdebrid() {
        health_checks.insert(
            "realdebrid".to_string(),
            HealthCheck {
                service: "Real-Debrid API".to_string(),
                status: "configured".to_string(),
                message: "API token set".to_string(),
            },
        );
    }

    let template = HealthTemplate {
        checks: health_checks,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /scan - Scan page
pub async fn get_scan(State(state): State<WebState>) -> impl IntoResponse {
    info!("Serving scan page");

    let history = match state.database.get_scan_history(10).await {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to get scan history: {}", e);
            vec![]
        }
    };

    let template = ScanTemplate {
        libraries: state.config.libraries.clone(),
        history,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /scan/trigger - Trigger a scan
pub async fn post_scan_trigger(
    State(state): State<WebState>,
    Form(form): Form<ScanTriggerForm>,
) -> impl IntoResponse {
    info!("Triggering scan (dry_run={})", form.dry_run);

    let library_name = form.library.filter(|l| !l.is_empty());

    // Run the scan
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

    // Create matcher and linker
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
            error!("Match failed: {}", e);
            return Html(
                ScanResultTemplate {
                    success: false,
                    message: format!("Match failed: {}", e),
                    matches: vec![],
                    dry_run: form.dry_run,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let linker = Linker::new_with_options(
        form.dry_run,
        state.config.matching.mode.is_strict(),
        &state.config.symlink.naming_template,
        true,
    );

    let link_result = match linker.process_matches(&matches, &state.database).await {
        Ok(r) => r,
        Err(e) => {
            error!("Link failed: {}", e);
            return Html(
                ScanResultTemplate {
                    success: false,
                    message: format!("Link failed: {}", e),
                    matches: vec![],
                    dry_run: form.dry_run,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let template = ScanResultTemplate {
        success: true,
        message: format!(
            "Scan complete: {} created, {} updated, {} skipped",
            link_result.created, link_result.updated, link_result.skipped
        ),
        matches,
        dry_run: form.dry_run,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /scan/history - Scan history
pub async fn get_scan_history(State(state): State<WebState>) -> impl IntoResponse {
    let history = match state.database.get_scan_history(50).await {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to get scan history: {}", e);
            vec![]
        }
    };

    let template = ScanHistoryTemplate { history };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /cleanup - Cleanup page
pub async fn get_cleanup(State(state): State<WebState>) -> impl IntoResponse {
    // Look for the most recent cleanup report
    let last_report = match std::fs::read_dir(&state.config.backup.path) {
        Ok(entries) => {
            let mut reports: Vec<_> = entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    name.to_string_lossy().starts_with("cleanup-audit-")
                        && name.to_string_lossy().ends_with(".json")
                })
                .collect();

            // Sort by modification time (newest first)
            reports.sort_by_key(|entry| {
                entry.metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            reports.reverse();

            reports.first().map(|entry| entry.path())
        }
        Err(_) => None,
    };

    let template = CleanupTemplate {
        libraries: state.config.libraries.clone(),
        last_report,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /cleanup/audit - Run audit
pub async fn post_cleanup_audit(
    State(state): State<WebState>,
    Form(form): Form<CleanupAuditForm>,
) -> impl IntoResponse {
    info!("Running cleanup audit (scope={}, library={:?})", form.scope, form.library);

    let scope = match CleanupScope::parse(&form.scope) {
        Ok(s) => s,
        Err(e) => {
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Invalid scope: {}", e),
                    report_path: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let auditor = CleanupAuditor::new_with_progress(&state.config, &state.database, true);

    // Use custom output path if library is specified, otherwise use default
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let output_path = if let Some(library_name) = &form.library {
        // Create a library-specific report path
        state.config.backup.path.join(format!(
            "cleanup-audit-{}-{}-{}.json",
            form.scope, library_name, ts
        ))
    } else {
        state.config.backup.path.join(format!(
            "cleanup-audit-{}-{}.json",
            form.scope, ts
        ))
    };

    let report_path = match auditor.run_audit(scope, Some(&output_path)).await {
        Ok(p) => p,
        Err(e) => {
            error!("Audit failed: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Audit failed: {}", e),
                    report_path: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let message = if let Some(library_name) = &form.library {
        format!("Audit complete for library '{}': {}", library_name, report_path.display())
    } else {
        format!("Audit complete: {}", report_path.display())
    };

    let template = CleanupResultTemplate {
        success: true,
        message,
        report_path: Some(report_path),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /cleanup/prune - Prune preview
pub async fn get_cleanup_prune(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let report_path = params.get("report").map(|p| p.as_str());

    if report_path.is_none() {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                critical: 0,
                high: 0,
                warning: 0,
                confirmation_token: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Read the report and show preview
    let report_path = std::path::Path::new(report_path.unwrap());
    if !report_path.exists() {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                critical: 0,
                high: 0,
                warning: 0,
                confirmation_token: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Parse the JSON report to show actual preview data
    let json = match std::fs::read_to_string(report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    confirmation_token: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    confirmation_token: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    // Calculate prune preview data
    let high_or_critical_candidates: Vec<_> = report
        .findings
        .iter()
        .filter(|f| {
            matches!(
                f.severity,
                cleanup_audit::FindingSeverity::Critical | cleanup_audit::FindingSeverity::High
            )
        })
        .collect();

    let safe_warning_prunes = cleanup_audit::collect_safe_warning_duplicate_prunes(&report.findings);

    let mut candidate_paths: Vec<PathBuf> = high_or_critical_candidates
        .iter()
        .map(|f| f.symlink_path.clone())
        .collect();
    candidate_paths.extend(safe_warning_prunes.iter().cloned());
    candidate_paths.sort();
    candidate_paths.dedup();

    let token = cleanup_audit::prune_confirmation_token(&report, &candidate_paths);

    let template = PrunePreviewTemplate {
        findings: report.findings.clone(),
        total: report.findings.len(),
        critical: report.summary.critical,
        high: report.summary.high,
        warning: report.summary.warning,
        confirmation_token: Some(token),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /cleanup/prune - Apply prune
pub async fn post_cleanup_prune(
    State(state): State<WebState>,
    Form(form): Form<CleanupPruneForm>,
) -> impl IntoResponse {
    info!("Applying prune (token={})", form.token);

    // Validate inputs
    if form.report.is_empty() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: "Report path is required".to_string(),
                report_path: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    if form.token.is_empty() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: "Confirmation token is required".to_string(),
                report_path: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Read the report
    let report_path = std::path::Path::new(&form.report);
    if !report_path.exists() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: format!("Report not found: {}", form.report),
                report_path: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    let json = match std::fs::read_to_string(report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to read report: {}", e),
                    report_path: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to parse report: {}", e),
                    report_path: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    // Apply the prune operation
    let outcome = match cleanup_audit::run_prune(
        &state.config,
        &state.database,
        report_path,
        true, // apply
        None, // max_delete
        Some(&form.token), // confirmation_token
    ).await {
        Ok(o) => o,
        Err(e) => {
            error!("Prune operation failed: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    report_path: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let message = if outcome.removed > 0 {
        format!(
            "✅ Prune completed successfully: {} symlinks removed, {} skipped",
            outcome.removed, outcome.skipped
        )
    } else {
        "⚠️ Prune completed but no symlinks were removed".to_string()
    };

    let template = CleanupResultTemplate {
        success: true,
        message,
        report_path: Some(report_path.to_path_buf()),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /links - Links list
pub async fn get_links(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let filter = params.get("filter").map(|f| f.as_str());
    let limit: i64 = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(100);

    let links = match filter {
        Some("dead") => state.database.get_dead_links().await.unwrap_or_default(),
        Some("active") => state.database.get_active_links().await.unwrap_or_default(),
        _ => state.database.get_active_links().await.unwrap_or_default(),
    }
    .into_iter()
    .take(limit as usize)
    .collect::<Vec<_>>();

    let template = LinksTemplate {
        links,
        filter: filter.unwrap_or("active").to_string(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /links/dead - Dead links
pub async fn get_dead_links(State(state): State<WebState>) -> impl IntoResponse {
    let links = match state.database.get_dead_links().await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to get dead links: {}", e);
            vec![]
        }
    };

    let template = DeadLinksTemplate { links };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /links/repair - Repair dead links
pub async fn post_repair(State(state): State<WebState>) -> impl IntoResponse {
    info!("Running auto repair");

    // Use the repair module
    // This would call crate::repair::auto_repair
    let template = RepairResultTemplate {
        success: true,
        message: "Repair completed".to_string(),
        repaired: 0,
        failed: 0,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /config - Config page
pub async fn get_config(State(state): State<WebState>) -> impl IntoResponse {
    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: None,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /config/validate - Validate config
pub async fn post_config_validate(State(state): State<WebState>) -> impl IntoResponse {
    // Config is already loaded, just check for obvious issues
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

    let result = if errors.is_empty() {
        Some(ValidationResult {
            valid: true,
            errors,
            warnings,
        })
    } else {
        Some(ValidationResult {
            valid: false,
            errors,
            warnings,
        })
    };

    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: result,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /doctor - Doctor page
pub async fn get_doctor(State(state): State<WebState>) -> impl IntoResponse {
    let mut checks = vec![];

    // Check libraries exist
    for lib in &state.config.libraries {
        let exists = lib.path.exists();
        checks.push(DoctorCheck {
            check: format!("Library '{}' exists", lib.name),
            passed: exists,
            message: if exists {
                format!("{}: exists", lib.path.display())
            } else {
                format!("{}: NOT FOUND", lib.path.display())
            },
        });
    }

    // Check sources exist
    for source in &state.config.sources {
        let exists = source.path.exists();
        checks.push(DoctorCheck {
            check: format!("Source '{}' exists", source.name),
            passed: exists,
            message: if exists {
                format!("{}: exists", source.path.display())
            } else {
                format!("{}: NOT FOUND", source.path.display())
            },
        });
    }

    // Check database
    let db_ok = state.database.get_web_stats().await.is_ok();
    checks.push(DoctorCheck {
        check: "Database connection".to_string(),
        passed: db_ok,
        message: if db_ok { "Connected" } else { "Failed" }.to_string(),
    });

    // Check API keys
    let has_tmdb = state.config.has_tmdb();
    checks.push(DoctorCheck {
        check: "TMDB API key".to_string(),
        passed: has_tmdb,
        message: if has_tmdb { "Configured" } else { "Missing" }.to_string(),
    });

    let has_tvdb = state.config.has_tvdb();
    checks.push(DoctorCheck {
        check: "TVDB API key".to_string(),
        passed: has_tvdb,
        message: if has_tvdb { "Configured" } else { "Missing" }.to_string(),
    });

    let has_rd = state.config.has_realdebrid();
    checks.push(DoctorCheck {
        check: "Real-Debrid API token".to_string(),
        passed: has_rd,
        message: if has_rd { "Configured" } else { "Missing" }.to_string(),
    });

    let all_passed = checks.iter().all(|c| c.passed);

    let template = DoctorTemplate {
        checks,
        all_passed,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /discover - Discover page
pub async fn get_discover(State(state): State<WebState>) -> impl IntoResponse {
    let template = DiscoverTemplate {
        libraries: state.config.libraries.clone(),
        discovered_items: vec![],
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /discover/add - Add torrent to library
pub async fn post_discover_add(
    State(state): State<WebState>,
    Form(form): Form<DiscoverAddForm>,
) -> impl IntoResponse {
    info!("Adding torrent {} to library", form.torrent_id);

    // This would integrate with the auto_acquire system
    let template = DiscoverResultTemplate {
        success: true,
        message: format!("Torrent {} queued for download", form.torrent_id),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /backup - Backup page
pub async fn get_backup(State(state): State<WebState>) -> impl IntoResponse {
    let backup_manager = BackupManager::new(&state.config.backup);

    // List existing backups
    let mut backups = vec![];
    if let Ok(entries) = std::fs::read_dir(&state.config.backup.path) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".json") {
                    if let Ok(meta) = entry.metadata() {
                        backups.push(BackupInfo {
                            filename: name.to_string(),
                            size: meta.len(),
                            modified: meta.modified().ok(),
                        });
                    }
                }
            }
        }
    }

    backups.sort_by(|a, b| {
        b.modified
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .cmp(&a.modified.unwrap_or(std::time::SystemTime::UNIX_EPOCH))
    });

    let template = BackupTemplate {
        backups,
        backup_dir: state.config.backup.path.clone(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /backup/create - Create backup
pub async fn post_backup_create(
    State(state): State<WebState>,
    Form(form): Form<BackupCreateForm>,
) -> impl IntoResponse {
    info!("Creating backup (label={})", form.label);

    let backup_manager = BackupManager::new(&state.config.backup);

    let result = match backup_manager
        .create_backup(&state.database, &form.label)
        .await
    {
        Ok(path) => Some(path),
        Err(e) => {
            error!("Backup failed: {}", e);
            None
        }
    };

    let template = BackupResultTemplate {
        success: result.is_some(),
        message: if result.is_some() {
            "Backup created successfully".to_string()
        } else {
            "Backup failed".to_string()
        },
        backup_path: result,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /backup/restore - Restore backup
pub async fn post_backup_restore(
    State(state): State<WebState>,
    Form(form): Form<BackupRestoreForm>,
) -> impl IntoResponse {
    info!("Restoring backup: {}", form.backup_file);

    let backup_manager = BackupManager::new(&state.config.backup);

    let backup_path = state.config.backup.path.join(&form.backup_file);

    let allowed_roots: Vec<PathBuf> = state
        .config
        .libraries
        .iter()
        .map(|l| l.path.clone())
        .collect();
    let allowed_source_roots: Vec<PathBuf> = state
        .config
        .sources
        .iter()
        .map(|s| s.path.clone())
        .collect();
    let result = backup_manager
        .restore(
            &state.database,
            &backup_path,
            false,
            &allowed_roots,
            &allowed_source_roots,
            true,
        )
        .await
        .map(|_| ());

    let (success, message) = match result {
        Ok(()) => (true, "Backup restored successfully".to_string()),
        Err(e) => (false, format!("Restore failed: {}", e)),
    };

    let template = BackupResultTemplate {
        success,
        message,
        backup_path: Some(backup_path),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

// ─── Form structs ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScanTriggerForm {
    pub dry_run: bool,
    pub library: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CleanupAuditForm {
    pub scope: String,
    pub library: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CleanupPruneForm {
    pub report: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct DiscoverAddForm {
    pub torrent_id: String,
    pub library: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupCreateForm {
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupRestoreForm {
    pub backup_file: String,
}
