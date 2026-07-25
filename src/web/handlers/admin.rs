use super::*;
use crate::commands::importer::{
    apply_import_plan, backfill_import_links, build_import_report, validate_options,
    write_import_report, ImportOptions,
};
use crate::{
    ImportContentType, ImportLookupMode, ImportMetadataMode, ImportMode, ImportProbeTool,
    OutputFormat,
};
use anyhow::Result;

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct DiscoverQuery {
    pub library: Option<String>,
    #[serde(default)]
    pub refresh_cache: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct ImportPreviewForm {
    pub source: String,
    pub destination: Option<String>,
    pub movie_destination: Option<String>,
    pub tv_destination: Option<String>,
    pub anime_destination: Option<String>,
    pub rules: Option<String>,
    pub content_type: String,
    pub mode: String,
    #[serde(default)]
    pub force: bool,
    pub lookup_mode: String,
    pub metadata_mode: String,
    pub probe_tool: String,
    pub confidence_filter: Option<String>,
    pub max_lookups: Option<usize>,
    #[serde(default)]
    pub offline: bool,
    #[serde(default)]
    pub refresh_metadata: bool,
    #[serde(default)]
    pub folders_only: bool,
    pub csrf_token: String,
}

impl ImportPreviewForm {
    fn draft(&self) -> ImportPreviewDraftView {
        ImportPreviewDraftView {
            source: self.source.clone(),
            destination: self.destination.clone().unwrap_or_default(),
            movie_destination: self.movie_destination.clone().unwrap_or_default(),
            tv_destination: self.tv_destination.clone().unwrap_or_default(),
            anime_destination: self.anime_destination.clone().unwrap_or_default(),
            rules: self.rules.clone().unwrap_or_default(),
            content_type: defaulted(&self.content_type, "auto"),
            force: self.force,
            lookup_mode: defaulted(&self.lookup_mode, "cache"),
            metadata_mode: defaulted(&self.metadata_mode, "fast"),
            probe_tool: defaulted(&self.probe_tool, "auto"),
            confidence_filter: self
                .confidence_filter
                .as_deref()
                .map(|value| defaulted(value, "all"))
                .unwrap_or_else(|| "all".to_string()),
            max_lookups: self.max_lookups.unwrap_or(50),
            offline: self.offline,
            refresh_metadata: self.refresh_metadata,
            folders_only: self.folders_only,
        }
    }

    fn options(&self) -> Result<ImportOptions> {
        Ok(ImportOptions {
            source: PathBuf::from(self.source.trim()),
            destination: optional_path(self.destination.as_deref()),
            movie_destination: optional_path(self.movie_destination.as_deref()),
            tv_destination: optional_path(self.tv_destination.as_deref()),
            anime_destination: optional_path(self.anime_destination.as_deref()),
            rules: optional_path(self.rules.as_deref()),
            content_type: parse_import_content_type(&self.content_type)?,
            mode: if self.force {
                ImportMode::Aggressive
            } else {
                parse_import_mode(&defaulted(&self.mode, "safe"))?
            },
            metadata_mode: parse_import_metadata_mode(&self.metadata_mode)?,
            probe_tool: parse_import_probe_tool(&self.probe_tool)?,
            lookup_mode: parse_import_lookup_mode(&self.lookup_mode)?,
            offline: self.offline,
            refresh_metadata: self.refresh_metadata,
            max_lookups: self.max_lookups.unwrap_or(50),
            report_path: None,
            yes: false,
            folders_only: self.folders_only,
            create_links: false,
            output: OutputFormat::Json,
        })
    }
}

fn defaulted(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn optional_path(value: Option<&str>) -> Option<PathBuf> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn parse_import_content_type(value: &str) -> Result<ImportContentType> {
    match defaulted(value, "auto").as_str() {
        "movie" => Ok(ImportContentType::Movie),
        "tv" => Ok(ImportContentType::Tv),
        "anime" => Ok(ImportContentType::Anime),
        "auto" => Ok(ImportContentType::Auto),
        other => anyhow::bail!("Unsupported import content type '{}'", other),
    }
}

fn parse_import_mode(value: &str) -> Result<ImportMode> {
    match defaulted(value, "preview").as_str() {
        "preview" => Ok(ImportMode::Preview),
        "safe" => Ok(ImportMode::Safe),
        "aggressive" => Ok(ImportMode::Aggressive),
        other => anyhow::bail!("Unsupported import mode '{}'", other),
    }
}

fn parse_import_lookup_mode(value: &str) -> Result<ImportLookupMode> {
    match defaulted(value, "cache").as_str() {
        "off" => Ok(ImportLookupMode::Off),
        "cache" => Ok(ImportLookupMode::Cache),
        "remote" => Ok(ImportLookupMode::Remote),
        other => anyhow::bail!("Unsupported import lookup mode '{}'", other),
    }
}

fn parse_import_metadata_mode(value: &str) -> Result<ImportMetadataMode> {
    match defaulted(value, "fast").as_str() {
        "fast" => Ok(ImportMetadataMode::Fast),
        "probe" => Ok(ImportMetadataMode::Probe),
        "strict" => Ok(ImportMetadataMode::Strict),
        other => anyhow::bail!("Unsupported import metadata mode '{}'", other),
    }
}

fn parse_import_probe_tool(value: &str) -> Result<ImportProbeTool> {
    match defaulted(value, "auto").as_str() {
        "auto" => Ok(ImportProbeTool::Auto),
        "ffprobe" => Ok(ImportProbeTool::Ffprobe),
        "mediainfo" => Ok(ImportProbeTool::Mediainfo),
        other => anyhow::bail!("Unsupported import probe tool '{}'", other),
    }
}

fn import_enum_label<T: std::fmt::Debug>(value: T) -> String {
    format!("{:?}", value).to_ascii_lowercase()
}

fn import_preview_result_view(
    report: crate::import_report::ImportReport,
    confidence_filter: &str,
    report_path: Option<PathBuf>,
    applied: bool,
    force: bool,
) -> ImportPreviewResultView {
    let total_candidates = report.candidates.len();
    let confidence_filter = defaulted(confidence_filter, "all");
    let filtered_candidates = report
        .candidates
        .into_iter()
        .filter(|candidate| {
            confidence_filter == "all"
                || import_enum_label(candidate.confidence) == confidence_filter
        })
        .take(200)
        .map(|candidate| {
            let confidence = import_enum_label(candidate.confidence);
            let decision = import_enum_label(candidate.decision);
            let needs_review = matches!(confidence.as_str(), "low" | "ambiguous")
                || matches!(decision.as_str(), "needslookup" | "needsreview");
            ImportCandidatePreviewView {
                source_path: candidate.source_path.display().to_string(),
                target_path: candidate
                    .target_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
                title_hint: candidate.title_hint,
                media_id: candidate
                    .explicit_media_id
                    .unwrap_or_else(|| "-".to_string()),
                confidence,
                decision,
                action: import_enum_label(candidate.action),
                reason: candidate.reason.unwrap_or_else(|| "-".to_string()),
                needs_review,
            }
        })
        .collect::<Vec<_>>();

    ImportPreviewResultView {
        summary: report.summary,
        source_shape: import_enum_label(report.source_shape),
        plan_label: if force {
            "force".to_string()
        } else {
            "safe".to_string()
        },
        warnings: report.warnings,
        handoff: report.handoff,
        report_path: report_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string()),
        applied,
        total_candidates,
        shown_candidates: filtered_candidates.len(),
        confidence_filter,
        candidates: filtered_candidates,
    }
}

fn default_import_draft() -> ImportPreviewDraftView {
    ImportPreviewDraftView {
        content_type: "auto".to_string(),
        force: false,
        lookup_mode: "cache".to_string(),
        metadata_mode: "fast".to_string(),
        probe_tool: "auto".to_string(),
        confidence_filter: "all".to_string(),
        max_lookups: 50,
        ..ImportPreviewDraftView::default()
    }
}

fn import_web_report_path(backup_root: &StdPath) -> PathBuf {
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let pid = std::process::id();
    backup_root
        .join("import-reports")
        .join(format!("symlinkarr-import-web-{ts}-{pid}.json"))
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
        .into_response()
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

/// GET /import - Import preview page
pub(crate) async fn get_import(State(state): State<WebState>) -> impl IntoResponse {
    let template = ImportTemplate {
        libraries: state.config.libraries.clone(),
        draft: default_import_draft(),
        feedback: None,
        result: None,
        csrf_token: browser_csrf_token(&state),
    };
    (
        StatusCode::OK,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
        .into_response()
}

/// POST /import/preview - Build a read-only provider import preview
pub(crate) async fn post_import_preview(
    State(state): State<WebState>,
    Form(form): Form<ImportPreviewForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/import/preview")
    {
        return response;
    }

    let draft = form.draft();
    let feedback;
    let mut result = None;

    match form.options().and_then(|options| {
        if options.source.as_os_str().is_empty() {
            anyhow::bail!("Source path is required");
        }
        validate_options(&options)?;
        Ok(options)
    }) {
        Ok(options) => {
            let tmdb = if options.lookup_mode == ImportLookupMode::Remote && state.config.has_tmdb()
            {
                Some(crate::api::tmdb::TmdbClient::new(
                    &state.config.api.tmdb_api_key,
                    Some(&state.config.api.tmdb_read_access_token),
                    state.config.api.cache_ttl_hours,
                ))
            } else {
                None
            };
            let mut tvdb =
                if options.lookup_mode == ImportLookupMode::Remote && state.config.has_tvdb() {
                    Some(crate::api::tvdb::TvdbClient::new(
                        &state.config.api.tvdb_api_key,
                        state.config.api.cache_ttl_hours,
                    ))
                } else {
                    None
                };
            match build_import_report(
                &options,
                Some(state.database.as_ref()),
                tmdb.as_ref(),
                tvdb.as_mut(),
            )
            .await
            {
                Ok(report) => {
                    feedback = Some(FormFeedbackView {
                        success: true,
                        message: "Import preview built without writing files.".to_string(),
                    });
                    result = Some(import_preview_result_view(
                        report,
                        &draft.confidence_filter,
                        None,
                        false,
                        draft.force,
                    ));
                }
                Err(err) => {
                    feedback = Some(FormFeedbackView {
                        success: false,
                        message: format!("Import preview failed: {}", err),
                    });
                }
            }
        }
        Err(err) => {
            feedback = Some(FormFeedbackView {
                success: false,
                message: format!("Import preview failed: {}", err),
            });
        }
    }

    let template = ImportTemplate {
        libraries: state.config.libraries.clone(),
        draft,
        feedback,
        result,
        csrf_token: browser_csrf_token(&state),
    };
    (
        StatusCode::OK,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
        .into_response()
}

/// POST /import/apply - Apply a provider import plan from the Web UI
pub(crate) async fn post_import_apply(
    State(state): State<WebState>,
    Form(form): Form<ImportPreviewForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/import/apply") {
        return response;
    }

    let draft = form.draft();
    let feedback;
    let mut result = None;

    match form.options().and_then(|mut options| {
        if options.source.as_os_str().is_empty() {
            anyhow::bail!("Source path is required");
        }
        if options.mode == ImportMode::Preview {
            options.mode = ImportMode::Safe;
        }
        options.yes = true;
        options.report_path = Some(import_web_report_path(&state.config.backup.path));
        validate_options(&options)?;
        Ok(options)
    }) {
        Ok(options) => {
            let tmdb = if options.lookup_mode == ImportLookupMode::Remote && state.config.has_tmdb()
            {
                Some(crate::api::tmdb::TmdbClient::new(
                    &state.config.api.tmdb_api_key,
                    Some(&state.config.api.tmdb_read_access_token),
                    state.config.api.cache_ttl_hours,
                ))
            } else {
                None
            };
            let mut tvdb =
                if options.lookup_mode == ImportLookupMode::Remote && state.config.has_tvdb() {
                    Some(crate::api::tvdb::TvdbClient::new(
                        &state.config.api.tvdb_api_key,
                        state.config.api.cache_ttl_hours,
                    ))
                } else {
                    None
                };
            match build_import_report(
                &options,
                Some(state.database.as_ref()),
                tmdb.as_ref(),
                tvdb.as_mut(),
            )
            .await
            {
                Ok(report) => {
                    let apply_options = options.clone();
                    let (mut report, write_result) = match tokio::task::spawn_blocking(move || {
                        let mut report = report;
                        let result = apply_import_plan(&mut report, &apply_options);
                        (report, result)
                    })
                    .await
                    {
                        Ok(result) => result,
                        Err(err) => {
                            feedback = Some(FormFeedbackView {
                                success: false,
                                message: format!(
                                    "Import apply worker failed before completion: {}",
                                    err
                                ),
                            });
                            let template = ImportTemplate {
                                libraries: state.config.libraries.clone(),
                                draft,
                                feedback,
                                result,
                                csrf_token: browser_csrf_token(&state),
                            };
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Html(template.render().unwrap_or_else(|e| e.to_string())),
                            )
                                .into_response();
                        }
                    };
                    if write_result.is_ok() && !options.folders_only {
                        backfill_import_links(
                            &mut report,
                            state.database.as_ref(),
                            options.content_type,
                        )
                        .await;
                    }
                    let report_for_write = report.clone();
                    let report_path = options.report_path.clone();
                    let report_path = match tokio::task::spawn_blocking(move || {
                        write_import_report(&report_for_write, report_path.as_deref())
                    })
                    .await
                    {
                        Ok(result) => result,
                        Err(err) => Err(anyhow::anyhow!(
                            "import report writer worker failed: {}",
                            err
                        )),
                    };
                    match (write_result, report_path) {
                        (Ok(()), Ok(path)) => {
                            feedback = Some(FormFeedbackView {
                                success: true,
                                message: format!(
                                    "Import applied. Report written to {}.",
                                    path.display()
                                ),
                            });
                            result = Some(import_preview_result_view(
                                report,
                                &draft.confidence_filter,
                                Some(path),
                                true,
                                draft.force,
                            ));
                        }
                        (Err(err), Ok(path)) => {
                            feedback = Some(FormFeedbackView {
                                success: false,
                                message: format!(
                                    "Import apply failed after report planning: {}. Report written to {}.",
                                    err,
                                    path.display()
                                ),
                            });
                            result = Some(import_preview_result_view(
                                report,
                                &draft.confidence_filter,
                                Some(path),
                                false,
                                draft.force,
                            ));
                        }
                        (Err(err), Err(report_err)) => {
                            feedback = Some(FormFeedbackView {
                                success: false,
                                message: format!(
                                    "Import apply failed: {}; report write also failed: {}",
                                    err, report_err
                                ),
                            });
                        }
                        (Ok(()), Err(report_err)) => {
                            feedback = Some(FormFeedbackView {
                                success: false,
                                message: format!(
                                    "Import applied but report write failed: {}",
                                    report_err
                                ),
                            });
                            result = Some(import_preview_result_view(
                                report,
                                &draft.confidence_filter,
                                None,
                                true,
                                draft.force,
                            ));
                        }
                    }
                }
                Err(err) => {
                    feedback = Some(FormFeedbackView {
                        success: false,
                        message: format!("Import apply failed: {}", err),
                    });
                }
            }
        }
        Err(err) => {
            feedback = Some(FormFeedbackView {
                success: false,
                message: format!("Import apply failed: {}", err),
            });
        }
    }

    let template = ImportTemplate {
        libraries: state.config.libraries.clone(),
        draft,
        feedback,
        result,
        csrf_token: browser_csrf_token(&state),
    };
    (
        StatusCode::OK,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
        .into_response()
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
