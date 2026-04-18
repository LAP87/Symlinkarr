use super::*;

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct DiscoverQuery {
    pub library: Option<String>,
    #[serde(default)]
    pub refresh_cache: bool,
}
/// GET /config - Config page
pub(crate) async fn get_config(State(state): State<WebState>) -> impl IntoResponse {
    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: None,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /config/validate - Validate config
pub(crate) async fn post_config_validate(
    State(state): State<WebState>,
    Form(form): Form<BrowserMutationForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/config/validate")
    {
        return response;
    }

    let report = validate_config_report(&state.config).await;
    let result = Some(ValidationResult {
        valid: report.errors.is_empty(),
        errors: report.errors,
        warnings: report.warnings,
    });

    let template = ConfigTemplate {
        config: (*state.config).clone(),
        validation_result: result,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /doctor - Doctor page
pub(crate) async fn get_doctor(State(state): State<WebState>) -> impl IntoResponse {
    let checks = collect_doctor_checks(&state.config, &state.database, DoctorCheckMode::ReadOnly)
        .await
        .into_iter()
        .map(|check| DoctorCheck {
            check: check.name,
            passed: check.ok,
            message: check.detail,
        })
        .collect::<Vec<_>>();

    let all_passed = checks.iter().all(|c| c.passed);

    let template = DoctorTemplate { checks, all_passed };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /discover - Discover page
pub(crate) async fn get_discover(
    State(state): State<WebState>,
    Query(query): Query<DiscoverQuery>,
) -> impl IntoResponse {
    let template = DiscoverTemplate {
        libraries: state.config.libraries.clone(),
        selected_library: query.library.unwrap_or_default(),
        refresh_cache: query.refresh_cache,
    };
    (
        StatusCode::OK,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
}

/// GET /discover/content - Discover content fragment
pub(crate) async fn get_discover_content(
    State(state): State<WebState>,
    Query(query): Query<DiscoverQuery>,
) -> impl IntoResponse {
    match load_discovery_snapshot(
        &state.config,
        &state.database,
        query.library.as_deref(),
        query.refresh_cache,
    )
    .await
    {
        Ok(snapshot) => {
            let template = DiscoverContentTemplate {
                discover_summary: snapshot.summary,
                folder_plans: snapshot.folders,
                discovered_items: snapshot.items,
                status_message: snapshot.status_message.or_else(|| {
                    (!query.refresh_cache).then(|| {
                        "Showing cached or on-disk discover results only. Enable refresh when you want a slower live cache sync first."
                            .to_string()
                    })
                }),
            };
            (
                StatusCode::OK,
                Html(template.render().unwrap_or_else(|e| e.to_string())),
            )
        }
        Err(err) => {
            let message = err.to_string();
            let template = DiscoverContentTemplate {
                discover_summary: DiscoverSummary::default(),
                folder_plans: vec![],
                discovered_items: vec![],
                status_message: Some(if message.contains("Unknown library filter") {
                    format!("Invalid library filter: {}", message)
                } else {
                    format!("Discover failed: {}", message)
                }),
            };
            (
                if message.contains("Unknown library filter") {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                Html(template.render().unwrap_or_else(|e| e.to_string())),
            )
        }
    }
}

/// GET /backup - Backup page
pub(crate) async fn get_backup(State(state): State<WebState>) -> impl IntoResponse {
    let backup_manager = BackupManager::new(&state.config.backup);
    let current_active_links = state
        .database
        .get_web_stats()
        .await
        .map(|stats| stats.active_links.max(0) as usize)
        .unwrap_or(0);
    let backups = backup_manager
        .list()
        .unwrap_or_default()
        .into_iter()
        .map(|backup| {
            let (kind_label, kind_badge_class) = match &backup.backup_type {
                crate::backup::BackupType::Scheduled => {
                    ("Symlinkarr Backup".to_string(), "badge-info")
                }
                crate::backup::BackupType::Safety { .. } => {
                    ("Restore Point".to_string(), "badge-warning")
                }
            };
            let link_delta_label = if backup.symlink_count == current_active_links {
                "Matches current tracked links".to_string()
            } else if backup.symlink_count > current_active_links {
                format!(
                    "{} more than current",
                    backup.symlink_count - current_active_links
                )
            } else {
                format!(
                    "{} fewer than current",
                    current_active_links - backup.symlink_count
                )
            };

            BackupInfo {
                filename: backup.filename,
                label: backup.label,
                kind_label,
                kind_badge_class,
                created_at: format_backup_timestamp(backup.timestamp),
                age_label: format_backup_age(backup.timestamp),
                recorded_links: backup.symlink_count,
                link_delta_label,
                manifest_size_bytes: backup.file_size,
                database_snapshot_size_bytes: backup
                    .database_snapshot
                    .map(|snapshot| snapshot.size_bytes),
                config_snapshot_present: backup
                    .app_state
                    .as_ref()
                    .and_then(|state| state.config_snapshot.as_ref())
                    .is_some(),
                secret_snapshot_count: backup
                    .app_state
                    .as_ref()
                    .map(|state| state.secret_snapshots.len())
                    .unwrap_or(0),
            }
        })
        .collect();

    let template = BackupTemplate {
        backups,
        backup_dir: state.config.backup.path.clone(),
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /backup/create - Create backup
pub(crate) async fn post_backup_create(
    State(state): State<WebState>,
    Form(form): Form<BackupCreateForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/backup/create") {
        return response;
    }

    info!("Creating backup (label={})", form.label);

    let backup_manager = BackupManager::new(&state.config.backup);

    let result = match backup_manager
        .create_backup(&state.config, &state.database, &form.label)
        .await
    {
        Ok(path) => Some(path),
        Err(e) => {
            error!("Backup failed: {}", e);
            None
        }
    };

    let created_summary = result.as_ref().and_then(|path| {
        backup_manager
            .list()
            .ok()
            .and_then(|items| items.into_iter().find(|backup| &backup.path == path))
    });
    let database_snapshot_path = result.as_ref().map(|path| path.with_extension("sqlite3"));
    let template = BackupResultTemplate {
        success: result.is_some(),
        message: if result.is_some() {
            "Backup created successfully".to_string()
        } else {
            "Backup failed".to_string()
        },
        backup_path: result,
        database_snapshot_path,
        config_snapshot_path: created_summary
            .as_ref()
            .and_then(|backup| backup.app_state.as_ref())
            .and_then(|state| state.config_snapshot.as_ref())
            .map(|file| state.config.backup.path.join(&file.filename)),
        secret_snapshot_count: created_summary
            .as_ref()
            .and_then(|backup| backup.app_state.as_ref())
            .map(|state| state.secret_snapshots.len())
            .unwrap_or(0),
        app_state_restore_summary: None,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /backup/restore - Restore backup
pub(crate) async fn post_backup_restore(
    State(state): State<WebState>,
    Form(form): Form<BackupRestoreForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/backup/restore")
    {
        return response;
    }

    info!("Restoring backup: {}", form.backup_file);

    let backup_manager = BackupManager::new(&state.config.backup);
    let backup_path = match backup_manager.resolve_restore_path(StdPath::new(&form.backup_file)) {
        Ok(path) => path,
        Err(e) => {
            let template = BackupResultTemplate {
                success: false,
                message: format!("Restore failed: {}", e),
                backup_path: None,
                database_snapshot_path: None,
                config_snapshot_path: None,
                secret_snapshot_count: 0,
                app_state_restore_summary: None,
            };
            return Html(
                template
                    .render()
                    .unwrap_or_else(|render_err| render_err.to_string()),
            )
            .into_response();
        }
    };

    if let Err(e) = ensure_backup_restore_runtime_healthy(&state.config, "backup restore").await {
        let template = BackupResultTemplate {
            success: false,
            message: format!("Restore failed: {}", e),
            backup_path: Some(backup_path),
            database_snapshot_path: None,
            config_snapshot_path: None,
            secret_snapshot_count: 0,
            app_state_restore_summary: None,
        };
        return Html(
            template
                .render()
                .unwrap_or_else(|render_err| render_err.to_string()),
        )
        .into_response();
    }

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
        .await;
    let app_state_restore_result = match &result {
        Ok(_) => Some(backup_manager.restore_app_state(&state.config, &backup_path, false)),
        Err(_) => None,
    };

    let (success, message, app_state_restore_summary) = match result {
        Ok((restored, skipped, errors)) => {
            let summary = match app_state_restore_result {
                Some(Ok(summary)) => Some(summary),
                Some(Err(err)) => {
                    return Html(
                        BackupResultTemplate {
                            success: false,
                            message: format!(
                                "Links were restored, but app state restore failed: {}",
                                err
                            ),
                            backup_path: Some(backup_path),
                            database_snapshot_path: None,
                            config_snapshot_path: None,
                            secret_snapshot_count: 0,
                            app_state_restore_summary: None,
                        }
                        .render()
                        .unwrap_or_else(|render_err| render_err.to_string()),
                    )
                    .into_response();
                }
                None => None,
            };
            let app_state_message = summary
                .as_ref()
                .filter(|summary| summary.present)
                .map(|summary| {
                    format!(
                        " Links restored: {restored}, skipped: {skipped}, errors: {errors}. App state: config {}, secrets restored {}, secrets skipped {}.",
                        if summary.config_restored {
                            "restored"
                        } else if summary.config_included {
                            "skipped"
                        } else {
                            "not included"
                        },
                        summary.secrets_restored,
                        summary.secrets_skipped
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        " Links restored: {restored}, skipped: {skipped}, errors: {errors}."
                    )
                });
            (
                true,
                format!("Backup restored successfully.{app_state_message}"),
                summary,
            )
        }
        Err(e) => (false, format!("Restore failed: {}", e), None),
    };

    let template = BackupResultTemplate {
        success,
        message,
        backup_path: Some(backup_path),
        database_snapshot_path: None,
        config_snapshot_path: None,
        secret_snapshot_count: 0,
        app_state_restore_summary,
    };

    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}
