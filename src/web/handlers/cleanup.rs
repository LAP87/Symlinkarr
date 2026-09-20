use super::*;
use std::collections::HashSet;

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct AnimeRemediationQuery {
    #[serde(default)]
    pub full: bool,
    pub plex_db: Option<String>,
    pub state: Option<String>,
    pub reason: Option<String>,
    pub title: Option<String>,
}
fn cleanup_report_summary_from_path(path: &StdPath) -> Option<CleanupReportSummaryView> {
    let report = load_cleanup_report(path)?;
    Some(CleanupReportSummaryView::from_report(
        path.to_path_buf(),
        report,
    ))
}

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

/// Environment variable operators can set to allow a Plex DB outside the
/// standard local paths. Accepts a PATH-style list of database files or the
/// directories that contain them.
pub(super) const PLEX_DB_ENV_VAR: &str = "SYMLINKARR_PLEX_DB";

fn canonical_plex_db_path(path: PathBuf) -> Option<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    let canonical = path.canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }

    canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.eq_ignore_ascii_case("db"))?;

    Some(canonical)
}

fn configured_plex_db_roots() -> Vec<PathBuf> {
    let Ok(value) = std::env::var(PLEX_DB_ENV_VAR) else {
        return Vec::new();
    };

    std::env::split_paths(&value)
        .filter_map(|entry| {
            let canonical = entry.canonicalize().ok()?;
            if canonical.is_dir() {
                Some(canonical)
            } else {
                canonical.parent().map(StdPath::to_path_buf)
            }
        })
        .collect()
}

fn allowed_plex_db_roots() -> Vec<PathBuf> {
    let mut roots = configured_plex_db_roots();
    for candidate in default_plex_db_candidates() {
        if let Some(parent) = canonical_plex_db_path(PathBuf::from(candidate))
            .as_deref()
            .and_then(StdPath::parent)
        {
            let parent = parent.to_path_buf();
            if !roots.contains(&parent) {
                roots.push(parent);
            }
        }
    }
    roots
}

fn confine_plex_db_path(requested: &str, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    let candidate = canonical_plex_db_path(PathBuf::from(requested))
        .ok_or_else(|| format!("Plex DB not found or not a .db file: {}", requested))?;

    if allowed_roots.is_empty() {
        return Err(format!(
            "Custom Plex DB paths are disabled because no Plex DB was found at a standard local path; set {} to the Plex database file or directory to allow one",
            PLEX_DB_ENV_VAR
        ));
    }

    if allowed_roots.iter().any(|root| candidate.starts_with(root)) {
        Ok(candidate)
    } else {
        Err(format!(
            "Plex DB path {} is outside the allowed Plex database directories; set {} to allow a custom location",
            candidate.display(),
            PLEX_DB_ENV_VAR
        ))
    }
}

fn default_plex_db_path() -> Option<PathBuf> {
    if let Ok(value) = std::env::var(PLEX_DB_ENV_VAR) {
        if let Some(path) = std::env::split_paths(&value).find_map(canonical_plex_db_path) {
            return Some(path);
        }
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find_map(canonical_plex_db_path)
}

fn resolve_plex_db_path(query_path: Option<&str>) -> Result<PathBuf, String> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return confine_plex_db_path(requested, &allowed_plex_db_roots());
    }

    default_plex_db_path().ok_or_else(|| {
        "Plex DB path is required or must exist at a standard local path".to_string()
    })
}

pub(super) async fn visible_last_cleanup_audit_outcome(
    state: &WebState,
) -> Option<BackgroundCleanupAuditOutcomeView> {
    let latest_report_created_at = latest_cleanup_report_path(&state.config.backup.path)
        .as_deref()
        .and_then(cleanup_report_summary_from_path)
        .map(|summary| summary.created_at);

    state
        .last_cleanup_audit_outcome()
        .await
        .filter(|outcome| {
            should_surface_cleanup_audit_outcome(outcome, latest_report_created_at.as_deref())
        })
        .map(Into::into)
}

async fn visible_last_repair_outcome(state: &WebState) -> Option<BackgroundRepairOutcomeView> {
    state.last_repair_outcome().await.map(Into::into)
}

async fn candidate_streaming_guard_view(
    state: &WebState,
    candidate_paths: Vec<PathBuf>,
) -> Option<MutationStreamingGuardView> {
    if !state.config.has_tautulli() || candidate_paths.is_empty() {
        return None;
    }

    let tautulli = TautulliClient::new(&state.config.tautulli);
    let active_paths = match tautulli.get_active_file_paths().await {
        Ok(paths) => paths,
        Err(err) => {
            tracing::warn!(
                "cleanup web playback guard query failed (non-fatal): {}",
                err
            );
            return None;
        }
    };

    let active_path_set: HashSet<_> = active_paths.into_iter().collect();
    let protected_paths = candidate_paths
        .into_iter()
        .filter(|path| active_path_set.contains(path.to_string_lossy().as_ref()))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    if protected_paths.is_empty() {
        return None;
    }

    let protected_count = protected_paths.len();
    Some(MutationStreamingGuardView {
        protected_count,
        protected_paths: protected_paths.into_iter().take(6).collect(),
    })
}

async fn cleanup_report_streaming_guard_view(
    state: &WebState,
    report: &cleanup_audit::CleanupReport,
    include_legacy_anime_roots: bool,
) -> Option<MutationStreamingGuardView> {
    let candidate_paths = match crate::commands::cleanup::cleanup_report_candidate_paths(
        &state.config,
        &state.database,
        report,
        include_legacy_anime_roots,
    )
    .await
    {
        Ok(paths) => paths,
        Err(err) => {
            tracing::warn!(
                "cleanup web playback guard could not derive candidate paths: {}",
                err
            );
            return None;
        }
    };

    candidate_streaming_guard_view(state, candidate_paths).await
}

async fn cleanup_report_path_streaming_guard_view(
    state: &WebState,
    report_path: &StdPath,
    include_legacy_anime_roots: bool,
) -> Option<MutationStreamingGuardView> {
    let candidate_paths = match crate::commands::cleanup::cleanup_report_candidate_paths_from_path(
        &state.config,
        &state.database,
        report_path,
        include_legacy_anime_roots,
    )
    .await
    {
        Ok(paths) => paths,
        Err(err) => {
            tracing::warn!(
                "cleanup web playback guard could not load report candidate paths: {}",
                err
            );
            return None;
        }
    };

    candidate_streaming_guard_view(state, candidate_paths).await
}

/// GET /cleanup - Cleanup page
pub(crate) async fn get_cleanup(State(state): State<WebState>) -> impl IntoResponse {
    let last_report = latest_cleanup_report_path(&state.config.backup.path);

    let last_report_summary = last_report
        .as_deref()
        .and_then(cleanup_report_summary_from_path);
    let active_cleanup_audit = state.active_cleanup_audit().await.map(Into::into);
    let last_cleanup_audit_outcome = if active_cleanup_audit.is_none() {
        visible_last_cleanup_audit_outcome(&state).await
    } else {
        None
    };

    let template = CleanupTemplate {
        libraries: state.config.libraries.clone(),
        active_cleanup_audit,
        last_cleanup_audit_outcome,
        last_report: last_report_summary,
        last_report_path: last_report,
        csrf_token: browser_csrf_token(&state),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /cleanup/anime-remediation - Read-only anime remediation backlog
pub(crate) async fn get_cleanup_anime_remediation(
    State(state): State<WebState>,
    Query(query): Query<AnimeRemediationQuery>,
) -> impl IntoResponse {
    let filters = match AnimeRemediationGroupFilters::parse(
        query.state.as_deref(),
        query.reason.as_deref(),
        query.title.as_deref(),
    ) {
        Ok(filters) => filters,
        Err(err) => {
            return Html(
                AnimeRemediationTemplate {
                    summary: None,
                    groups: vec![],
                    error_message: Some(format!("Invalid anime remediation filters: {}", err)),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
        }
    };

    let plex_db_path = match resolve_plex_db_path(query.plex_db.as_deref()) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                AnimeRemediationTemplate {
                    summary: None,
                    groups: vec![],
                    error_message: Some(err),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    match build_anime_remediation_report(&state.config, &state.database, &plex_db_path, query.full)
        .await
    {
        Ok(Some(report)) => {
            let assessed_groups = assess_anime_remediation_groups(&report.groups);

            match assessed_groups {
                Ok(assessed_groups) => {
                    let filtered_groups =
                        filter_anime_remediation_groups(assessed_groups.clone(), &filters);
                    let eligible_groups = filtered_groups
                        .iter()
                        .filter(|group| group.eligible)
                        .count();
                    let blocked_groups = filtered_groups.len().saturating_sub(eligible_groups);
                    let blocked_reason_summary =
                        summarize_anime_remediation_blocked_reasons(&filtered_groups)
                            .into_iter()
                            .map(Into::into)
                            .collect();
                    let available_blocked_reasons = anime_remediation_block_reason_catalog()
                        .into_iter()
                        .map(Into::into)
                        .collect();

                    Html(
                        AnimeRemediationTemplate {
                            summary: Some(AnimeRemediationSummaryView {
                                generated_at: report.generated_at,
                                plex_db_path: plex_db_path.display().to_string(),
                                full: query.full,
                                filesystem_mixed_root_groups: report.filesystem_mixed_root_groups,
                                plex_duplicate_show_groups: report.plex_duplicate_show_groups,
                                plex_hama_anidb_tvdb_groups: report.plex_hama_anidb_tvdb_groups,
                                correlated_hama_split_groups: report.correlated_hama_split_groups,
                                remediation_groups: report.remediation_groups,
                                returned_groups: report.returned_groups,
                                visible_groups: filtered_groups.len(),
                                eligible_groups,
                                blocked_groups,
                                state_filter: filters.visibility.as_str().to_string(),
                                reason_filter: filters
                                    .block_code
                                    .map(|code| code.as_str().to_string())
                                    .unwrap_or_default(),
                                title_filter: filters.title_contains.clone().unwrap_or_default(),
                                blocked_reason_summary,
                                available_blocked_reasons,
                            }),
                            groups: filtered_groups
                                .into_iter()
                                .map(AnimeRemediationGroupView::from_plan_group)
                                .collect(),
                            error_message: None,
                            csrf_token: browser_csrf_token(&state),
                        }
                        .render()
                        .unwrap_or_else(|e| e.to_string()),
                    )
                }
                Err(err) => {
                    error!("Failed to assess anime remediation backlog: {}", err);
                    Html(
                        AnimeRemediationTemplate {
                            summary: None,
                            groups: vec![],
                            error_message: Some(format!(
                                "Failed to assess anime remediation backlog: {}",
                                err
                            )),
                            csrf_token: browser_csrf_token(&state),
                        }
                        .render()
                        .unwrap_or_else(|e| e.to_string()),
                    )
                }
            }
        }
        Ok(None) => Html(
            AnimeRemediationTemplate {
                summary: None,
                groups: vec![],
                error_message: Some(
                    "No anime libraries are configured for remediation reporting".to_string(),
                ),
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        ),
        Err(err) => {
            error!("Failed to build anime remediation report: {}", err);
            Html(
                AnimeRemediationTemplate {
                    summary: None,
                    groups: vec![],
                    error_message: Some(format!(
                        "Failed to build anime remediation report: {}",
                        err
                    )),
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
        }
    }
}

/// POST /cleanup/anime-remediation/preview - Build a guarded remediation plan
pub(crate) async fn post_cleanup_anime_remediation_preview(
    State(state): State<WebState>,
    Form(form): Form<AnimeRemediationPreviewForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(
        &state,
        &form.csrf_token,
        "/cleanup/anime-remediation/preview",
    ) {
        return response;
    }

    let plex_db_path = match resolve_plex_db_path(form.plex_db.as_deref()) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                AnimeRemediationResultTemplate {
                    success: false,
                    message: format!("Anime remediation preview failed: {}", err),
                    preview: None,
                    apply: None,
                    playback_guard: None,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    match preview_anime_remediation_plan(
        &state.config,
        &state.database,
        form.library.as_deref(),
        &plex_db_path,
        form.title.as_deref(),
        None,
    )
    .await
    {
        Ok((plan, report_path)) => Html(
            AnimeRemediationResultTemplate {
                success: true,
                message: format!(
                    "Anime remediation preview saved. Review {} before applying.",
                    report_path.display()
                ),
                preview: Some(AnimeRemediationPreviewResultView {
                    report_path,
                    plex_db_path: plan.plex_db_path.display().to_string(),
                    title_filter: plan.title_filter.unwrap_or_default(),
                    total_groups: plan.total_groups,
                    eligible_groups: plan.eligible_groups,
                    blocked_groups: plan.blocked_groups,
                    cleanup_candidates: plan.cleanup_candidates,
                    confirmation_token: plan.confirmation_token,
                    blocked_reason_summary: plan
                        .blocked_reason_summary
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    groups: plan
                        .groups
                        .into_iter()
                        .map(AnimeRemediationGroupView::from_plan_group)
                        .collect(),
                }),
                apply: None,
                playback_guard: cleanup_report_streaming_guard_view(
                    &state,
                    &plan.cleanup_report,
                    true,
                )
                .await,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
        Err(err) => Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: format!("Anime remediation preview failed: {}", err),
                preview: None,
                apply: None,
                playback_guard: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
    }
}

/// POST /cleanup/anime-remediation/apply - Apply a saved guarded remediation plan
pub(crate) async fn post_cleanup_anime_remediation_apply(
    State(state): State<WebState>,
    Form(form): Form<AnimeRemediationApplyForm>,
) -> impl IntoResponse {
    if let Some(response) =
        require_browser_csrf_token(&state, &form.csrf_token, "/cleanup/anime-remediation/apply")
    {
        return response;
    }

    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &form.report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                AnimeRemediationResultTemplate {
                    success: false,
                    message: format!("Anime remediation apply failed: {}", err),
                    preview: None,
                    apply: None,
                    playback_guard: None,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let playback_guard = cleanup_report_path_streaming_guard_view(&state, &report_path, true).await;

    match crate::operations::OperationCoordinator::new(state.database.as_ref().clone())
        .run(
            crate::operations::OperationRequest::new(
                "anime_remediation_apply",
                "web",
                form.library.clone(),
            ),
            apply_anime_remediation_plan_with_refresh(
                &state.config,
                &state.database,
                form.library.as_deref(),
                &report_path,
                Some(form.token.trim()),
                form.max_delete,
                true,
            ),
        )
        .await
    {
        Ok((plan, outcome, safety_snapshot, invalidation)) => Html(
            AnimeRemediationResultTemplate {
                success: true,
                message: "Anime remediation applied.".to_string(),
                preview: None,
                apply: Some(AnimeRemediationApplyResultView {
                    report_path,
                    total_groups: plan.total_groups,
                    eligible_groups: plan.eligible_groups,
                    blocked_groups: plan.blocked_groups,
                    candidates: outcome.candidates,
                    quarantined: outcome.quarantined,
                    removed: outcome.removed,
                    skipped: outcome.skipped,
                    safety_snapshot,
                    media_server_invalidation_summary: invalidation.summary_suffix(),
                }),
                csrf_token: browser_csrf_token(&state),
                playback_guard,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
        Err(err) => (
            if err.downcast_ref::<crate::db::OperationConflict>().is_some() {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            },
            Html(
                AnimeRemediationResultTemplate {
                    success: false,
                    message: format!("Anime remediation apply failed: {}", err),
                    preview: None,
                    apply: None,
                    playback_guard,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
    }
}

/// POST /cleanup/audit - Run audit
pub(crate) async fn post_cleanup_audit(
    State(state): State<WebState>,
    body: Bytes,
) -> impl IntoResponse {
    let form = CleanupAuditForm::from_form_bytes(&body);
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/cleanup/audit") {
        return response;
    }

    let selected_libraries = form.selected_libraries();
    let scope = infer_cleanup_scope(&state.config, &selected_libraries);
    info!(
        "Running cleanup audit (scope={:?}, libraries={:?})",
        scope, selected_libraries
    );

    match state.start_cleanup_audit(scope, selected_libraries.clone()).await
    {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Html(
                CleanupResultTemplate {
                    success: true,
                    message: format!(
                        "Cleanup audit started in background for {} across {}. Refresh /cleanup for the finished report.",
                        job.scope_label, job.libraries_label
                    ),
                    active_cleanup_audit: Some(job.into()),
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
        Err(e) => {
            error!("Cleanup audit rejected: {}", e);
            (
                StatusCode::CONFLICT,
                Html(
                    CleanupResultTemplate {
                        success: false,
                        message: format!("Cleanup audit not started: {}", e),
                        active_cleanup_audit: state.active_cleanup_audit().await.map(Into::into),
                        last_cleanup_audit_outcome: visible_last_cleanup_audit_outcome(&state)
                            .await,
                        report_path: None,
                        report_summary: None,
                    }
                    .render()
                    .unwrap_or_else(|e| e.to_string()),
                ),
            )
                .into_response()
        }
    }
}

/// GET /cleanup/prune - Prune preview
pub(crate) async fn get_cleanup_prune(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(raw_report) = params.get("report").map(|p| p.as_str()) else {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                actionable_candidates: 0,
                critical: 0,
                high: 0,
                warning: 0,
                blocked_candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                reason_counts: vec![],
                blocked_reason_summary: vec![],
                legacy_anime_root_groups: vec![],
                report_path: None,
                confirmation_token: None,
                already_applied: false,
                error_message: None,
                playback_guard: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    };

    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, raw_report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    actionable_candidates: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    reason_counts: vec![],
                    blocked_reason_summary: vec![],
                    legacy_anime_root_groups: vec![],
                    report_path: None,
                    confirmation_token: None,
                    already_applied: false,
                    error_message: Some(err.to_string()),
                    playback_guard: None,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    if !report_path.exists() {
        return Html(
            PrunePreviewTemplate {
                findings: vec![],
                total: 0,
                actionable_candidates: 0,
                critical: 0,
                high: 0,
                warning: 0,
                blocked_candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                reason_counts: vec![],
                blocked_reason_summary: vec![],
                legacy_anime_root_groups: vec![],
                report_path: None,
                confirmation_token: None,
                already_applied: false,
                error_message: Some(format!(
                    "Cleanup report not found: {}",
                    report_path.display()
                )),
                playback_guard: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
    }

    // Parse the JSON report to show actual preview data
    let json = match std::fs::read_to_string(&report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    actionable_candidates: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    reason_counts: vec![],
                    blocked_reason_summary: vec![],
                    legacy_anime_root_groups: vec![],
                    report_path: None,
                    confirmation_token: None,
                    already_applied: false,
                    error_message: Some(format!("Failed to read report: {}", e)),
                    playback_guard: None,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let mut report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse cleanup report: {}", e);
            return Html(
                PrunePreviewTemplate {
                    findings: vec![],
                    total: 0,
                    actionable_candidates: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    reason_counts: vec![],
                    blocked_reason_summary: vec![],
                    legacy_anime_root_groups: vec![],
                    report_path: None,
                    confirmation_token: None,
                    already_applied: false,
                    error_message: Some(format!("Failed to parse report: {}", e)),
                    playback_guard: None,
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            );
        }
    };

    let prune_plan =
        match cleanup_audit::hydrate_report_db_tracked_flags(&state.database, &mut report).await {
            Ok(()) => Some(cleanup_audit::build_prune_plan(
                &report,
                state.config.cleanup.prune.quarantine_foreign,
                false,
            )),
            Err(e) => {
                error!("Failed to hydrate cleanup report DB state: {}", e);
                None
            }
        };

    let template = PrunePreviewTemplate {
        findings: report
            .findings
            .clone()
            .into_iter()
            .map(|finding| {
                let action = prune_plan
                    .as_ref()
                    .map(|plan| plan.action_for_path(&finding.symlink_path))
                    .unwrap_or(crate::cleanup_audit::PrunePathAction::ObserveOnly);
                PruneFindingView::from_finding(finding, action)
            })
            .collect(),
        total: report.findings.len(),
        actionable_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.candidate_paths.len())
            .unwrap_or(0),
        critical: report.summary.critical,
        high: report.summary.high,
        warning: report.summary.warning,
        blocked_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.blocked_candidates)
            .unwrap_or(0),
        managed_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.managed_candidates)
            .unwrap_or(0),
        foreign_candidates: prune_plan
            .as_ref()
            .map(|plan| plan.foreign_candidates)
            .unwrap_or(0),
        reason_counts: prune_plan
            .as_ref()
            .map(|plan| plan.reason_counts.clone())
            .unwrap_or_default(),
        blocked_reason_summary: prune_plan
            .as_ref()
            .map(|plan| plan.blocked_reason_summary.clone())
            .unwrap_or_default(),
        legacy_anime_root_groups: prune_plan
            .as_ref()
            .map(|plan| plan.legacy_anime_root_groups.clone())
            .unwrap_or_default(),
        report_path: Some(report_path.to_path_buf()),
        confirmation_token: prune_plan
            .as_ref()
            .map(|plan| plan.confirmation_token.clone()),
        already_applied: report.applied_at.is_some(),
        error_message: None,
        playback_guard: match prune_plan.as_ref() {
            Some(plan) => {
                candidate_streaming_guard_view(&state, plan.candidate_paths.clone()).await
            }
            None => None,
        },
        csrf_token: browser_csrf_token(&state),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// POST /cleanup/prune - Apply prune
pub(crate) async fn post_cleanup_prune(
    State(state): State<WebState>,
    Form(form): Form<CleanupPruneForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/cleanup/prune") {
        return response;
    }

    info!("Applying prune from web UI");

    // Validate inputs
    if form.report.is_empty() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: "Report path is required".to_string(),
                active_cleanup_audit: None,
                last_cleanup_audit_outcome: None,
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response();
    }

    // Read the report
    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &form.report) {
        Ok(path) => path,
        Err(err) => {
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: err.to_string(),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };
    if !report_path.exists() {
        return Html(
            CleanupResultTemplate {
                success: false,
                message: format!("Report not found: {}", report_path.display()),
                active_cleanup_audit: None,
                last_cleanup_audit_outcome: None,
                report_path: None,
                report_summary: None,
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response();
    }

    let json = match std::fs::read_to_string(&report_path) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to read cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to read report: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let _report: cleanup_audit::CleanupReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to parse cleanup report: {}", e);
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Failed to parse report: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let selected = match selected_libraries(state.config.as_ref(), None) {
        Ok(selected) => selected,
        Err(e) => {
            return Html(
                CleanupResultTemplate {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    active_cleanup_audit: None,
                    last_cleanup_audit_outcome: None,
                    report_path: None,
                    report_summary: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    let (outcome, invalidation) =
        match crate::operations::OperationCoordinator::new(state.database.as_ref().clone())
            .run(
                crate::operations::OperationRequest::new("cleanup_prune_apply", "web", None),
                apply_cleanup_prune_with_refresh(
                    &state.config,
                    &state.database,
                    CleanupPruneApplyArgs {
                        libraries: &selected,
                        report_path: &report_path,
                        include_legacy_anime_roots: false,
                        max_delete: None,
                        confirm_token: None,
                        emit_text: true,
                    },
                ),
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!("Prune operation failed: {}", e);
                let status = if e.downcast_ref::<crate::db::OperationConflict>().is_some() {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::BAD_REQUEST
                };
                return (
                    status,
                    Html(
                        CleanupResultTemplate {
                            success: false,
                            message: format!("Prune failed: {}", e),
                            active_cleanup_audit: None,
                            last_cleanup_audit_outcome: None,
                            report_path: None,
                            report_summary: None,
                        }
                        .render()
                        .unwrap_or_else(|e| e.to_string()),
                    ),
                )
                    .into_response();
            }
        };

    let mut message = if outcome.removed > 0 {
        format!(
            "✅ Prune completed successfully: {} symlinks removed, {} skipped",
            outcome.removed, outcome.skipped
        )
    } else {
        "⚠️ Prune completed but no symlinks were removed".to_string()
    };
    if let Some(suffix) = invalidation.summary_suffix() {
        message.push_str(&format!(" ({})", suffix));
    }

    let template = CleanupResultTemplate {
        success: true,
        message,
        active_cleanup_audit: None,
        last_cleanup_audit_outcome: None,
        report_summary: cleanup_report_summary_from_path(&report_path),
        report_path: Some(report_path.to_path_buf()),
    };

    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// GET /links - Links list
pub(crate) async fn get_links(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let filter = params.get("filter").map(|f| f.as_str());
    let limit = clamp_link_list_limit(params.get("limit").and_then(|l| l.parse().ok()));

    let links = match filter {
        Some("dead") => state
            .database
            .get_dead_links_limited(limit)
            .await
            .unwrap_or_default(),
        Some("active") => state
            .database
            .get_active_links_limited(limit)
            .await
            .unwrap_or_default(),
        _ => state
            .database
            .get_active_links_limited(limit)
            .await
            .unwrap_or_default(),
    };

    let template = LinksTemplate {
        links,
        filter: filter.unwrap_or("active").to_string(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct DeadLinksQuery {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
    pub library: Option<String>,
    pub search: Option<String>,
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeadLinksPruneForm {
    #[serde(default)]
    pub csrf_token: String,
    pub library: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeadLinksExportForm {
    #[serde(default)]
    pub csrf_token: String,
    pub library: Option<String>,
}

/// GET /links/dead - Dead links
pub(crate) async fn get_dead_links(
    State(state): State<WebState>,
    Query(query): Query<DeadLinksQuery>,
) -> impl IntoResponse {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(50).clamp(10, 200);
    let offset = ((page - 1) * page_size) as i64;
    let limit = page_size as i64;

    let selected_library = query
        .library
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "All");

    let library_root = if let Some(ref lib_name) = selected_library {
        state
            .config
            .libraries
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(lib_name))
            .map(|l| l.path.to_string_lossy().to_string())
    } else {
        None
    };

    let search_term = query
        .search
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (links, total_count) = match state
        .database
        .get_dead_links_paginated(
            limit,
            offset,
            library_root.as_deref(),
            search_term.as_deref(),
        )
        .await
    {
        Ok(res) => res,
        Err(e) => {
            error!("Failed to get paginated dead links: {}", e);
            (vec![], 0)
        }
    };

    let total_pages = if total_count == 0 {
        1
    } else {
        total_count.div_ceil(page_size)
    };

    let library_counts_raw = state
        .database
        .count_dead_links_by_library(&state.config.libraries)
        .await
        .unwrap_or_default();

    let library_counts = library_counts_raw
        .into_iter()
        .map(|(name, count)| DeadLinkLibraryCountView { name, count })
        .collect();

    let active_repair = state.active_repair().await.map(Into::into);
    let last_repair_outcome = if active_repair.is_none() {
        visible_last_repair_outcome(&state).await
    } else {
        None
    };

    let active_dead_prune = state.active_dead_prune().await.map(Into::into);
    let last_dead_prune_outcome = if active_dead_prune.is_none() {
        state.last_dead_prune_outcome().await.map(Into::into)
    } else {
        None
    };

    let backfill_handoff_configured = state.config.handoff.markers_dir.is_some();

    let template = DeadLinksTemplate {
        links,
        total_count,
        page,
        page_size,
        total_pages,
        selected_library,
        search_query: search_term,
        library_counts,
        active_repair,
        last_repair_outcome,
        active_dead_prune,
        last_dead_prune_outcome,
        backfill_handoff_configured,
        flash_message: query.message,
        error_message: query.error,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /links/dead/prune - Prune dead symlinks
pub(crate) async fn post_dead_links_prune(
    State(state): State<WebState>,
    Form(form): Form<DeadLinksPruneForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/links/dead") {
        return response;
    }

    match state
        .start_dead_prune(form.library.clone(), form.dry_run)
        .await
    {
        Ok(job) => {
            let msg = format!(
                "Dead-link prune started in background for {}{}.",
                job.scope_label,
                if form.dry_run { " (dry-run)" } else { "" }
            );
            Redirect::to(&format!(
                "/links/dead?message={}",
                url_encode_component(&msg)
            ))
            .into_response()
        }
        Err(err) => Redirect::to(&format!(
            "/links/dead?error={}",
            url_encode_component(&format!("Prune not started: {}", err))
        ))
        .into_response(),
    }
}

/// POST /links/dead/export-wanted - Export dead links grouped by media ID for Backfill-Buddy
pub(crate) async fn post_dead_links_export_wanted(
    State(state): State<WebState>,
    Form(form): Form<DeadLinksExportForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/links/dead") {
        return response;
    }

    let library_filter = form
        .library
        .as_deref()
        .filter(|s| !s.trim().is_empty() && *s != "All");
    match crate::commands::cleanup::group_dead_links_wanted(
        &state.config,
        &state.database,
        library_filter,
    )
    .await
    {
        Ok(items) => {
            let total_dead: usize = items.iter().map(|i| i.dead_count).sum();
            let target_dir = state
                .config
                .handoff
                .markers_dir
                .clone()
                .unwrap_or_else(|| state.config.backup.path.clone());

            let filename = format!(
                "symlinkarr-dead-wanted-{}.json",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            );
            let target_path = target_dir.join(&filename);

            if let Some(parent) = target_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            match serde_json::to_string_pretty(&items) {
                Ok(json_content) => {
                    if let Err(e) = std::fs::write(&target_path, json_content) {
                        return Redirect::to(&format!(
                            "/links/dead?error={}",
                            url_encode_component(&format!("Failed to write export file: {}", e))
                        ))
                        .into_response();
                    }
                    let msg = format!(
                        "Exported {} wanted media items ({} dead links) to {}",
                        items.len(),
                        total_dead,
                        target_path.display()
                    );
                    Redirect::to(&format!(
                        "/links/dead?message={}",
                        url_encode_component(&msg)
                    ))
                    .into_response()
                }
                Err(e) => Redirect::to(&format!(
                    "/links/dead?error={}",
                    url_encode_component(&format!("Failed to serialize export: {}", e))
                ))
                .into_response(),
            }
        }
        Err(err) => Redirect::to(&format!(
            "/links/dead?error={}",
            url_encode_component(&format!("Failed to group dead links: {}", err))
        ))
        .into_response(),
    }
}

/// GET /links/dead/wanted.json - Download dead links grouped by media ID as JSON
pub(crate) async fn get_dead_links_wanted_json(
    State(state): State<WebState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let library_filter = query
        .get("library")
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty() && *s != "All");
    match crate::commands::cleanup::group_dead_links_wanted(
        &state.config,
        &state.database,
        library_filter,
    )
    .await
    {
        Ok(items) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "application/json"),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"symlinkarr-dead-wanted.json\"",
                ),
            ],
            serde_json::to_string_pretty(&items).unwrap_or_default(),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// POST /links/sweep - Trigger an on-demand dead-link sweep
pub(crate) async fn post_dead_link_sweep(
    State(state): State<WebState>,
    Form(form): Form<BrowserMutationForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/links/sweep") {
        return response;
    }
    match crate::commands::cleanup::sweep_dead_links(
        &state.config,
        &state.database,
        None,
        None,
        false,
    )
    .await
    {
        Ok(outcome) => {
            let msg = format!(
                "Dead-link sweep completed: {} marked, {} removed, {} skipped",
                outcome.dead.dead_marked, outcome.dead.removed, outcome.dead.skipped
            );
            Redirect::to(&format!(
                "/links/dead?message={}",
                url_encode_component(&msg)
            ))
            .into_response()
        }
        Err(err) => Redirect::to(&format!(
            "/links/dead?error={}",
            url_encode_component(&err.to_string())
        ))
        .into_response(),
    }
}

/// POST /links/repair - Repair dead links
pub(crate) async fn post_repair(
    State(state): State<WebState>,
    Form(form): Form<BrowserMutationForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/links/repair") {
        return response;
    }

    if let Some(ref return_to) = form.return_to {
        if return_to.starts_with('/') {
            match state.start_repair().await {
                Ok(job) => {
                    let msg = format!(
                        "Repair started in background for {}. Showing live progress below.",
                        job.scope_label
                    );
                    return Redirect::to(&format!(
                        "{}?message={}",
                        return_to,
                        url_encode_component(&msg)
                    ))
                    .into_response();
                }
                Err(err) => {
                    return Redirect::to(&format!(
                        "{}?error={}",
                        return_to,
                        url_encode_component(&format!("Repair not started: {}", err))
                    ))
                    .into_response();
                }
            }
        }
    }

    info!("Starting background auto repair");

    match state.start_repair().await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Html(
                RepairResultTemplate {
                    success: true,
                    message: format!(
                        "Repair started in background for {}. Refresh /links/dead for the finished outcome.",
                        job.scope_label
                    ),
                    repaired: 0,
                    failed: 0,
                    active_repair: Some(job.into()),
                    last_repair_outcome: None,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
        Err(err) => {
            let message = err.to_string();
            let active_repair = state.active_repair().await.map(Into::into);
            (
                StatusCode::CONFLICT,
                Html(
                    RepairResultTemplate {
                        success: false,
                        message: format!("Repair not started: {}", message),
                        repaired: 0,
                        failed: 0,
                        active_repair,
                        last_repair_outcome: visible_last_repair_outcome(&state).await,
                    }
                    .render()
                    .unwrap_or_else(|e| e.to_string()),
                ),
            )
                .into_response()
        }
    }
}

fn url_encode_component(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PinsQuery {
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddPinForm {
    pub folder: String,
    pub media_id: String,
    pub note: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeletePinForm {
    pub folder: String,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImportPinsForm {
    #[serde(default)]
    pub csrf_token: String,
    #[serde(default)]
    pub link_now: Option<String>,
}

/// GET /pins - List source pins
pub(crate) async fn get_pins(
    State(state): State<WebState>,
    Query(query): Query<PinsQuery>,
) -> impl IntoResponse {
    let pins = state.database.list_source_pins().await.unwrap_or_default();
    let (markers_dir, pending_markers_count) = match &state.config.handoff.markers_dir {
        Some(dir) => {
            let count = std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| {
                            let p = e.path();
                            p.is_file()
                                && p.extension().is_some_and(|ext| ext == "json")
                                && !p
                                    .file_name()
                                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                        })
                        .count()
                })
                .unwrap_or(0);
            (Some(dir.display().to_string()), count)
        }
        None => (None, 0),
    };
    let template = PinsTemplate {
        pins,
        markers_dir,
        pending_markers_count,
        flash_message: query.message,
        error_message: query.error,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /pins - Add or update a source pin
pub(crate) async fn post_pin(
    State(state): State<WebState>,
    Form(form): Form<AddPinForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/pins") {
        return response;
    }
    let folder = form.folder.trim().trim_matches('/').to_string();
    if folder.is_empty() || folder.contains('/') {
        return Redirect::to("/pins?error=Folder+must+be+a+single+directory+name+without+slashes")
            .into_response();
    }
    let Some(id) = crate::models::MediaId::parse(&form.media_id) else {
        return Redirect::to("/pins?error=Media+ID+must+look+like+tvdb-123+or+tmdb-123")
            .into_response();
    };
    let note = form
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    match state
        .database
        .upsert_source_pins(&[(folder.clone(), id.to_string(), "manual".to_string(), note)])
        .await
    {
        Ok(_) => Redirect::to(&format!(
            "/pins?message=Pinned+{}+successfully",
            url_encode_component(&folder)
        ))
        .into_response(),
        Err(e) => Redirect::to(&format!(
            "/pins?error={}",
            url_encode_component(&e.to_string())
        ))
        .into_response(),
    }
}

/// POST /pins/delete - Remove a source pin
pub(crate) async fn post_pin_delete(
    State(state): State<WebState>,
    Form(form): Form<DeletePinForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/pins/delete") {
        return response;
    }
    let folder = form.folder.trim();
    match state.database.delete_source_pin(folder).await {
        Ok(true) => Redirect::to(&format!(
            "/pins?message=Removed+pin+for+{}",
            url_encode_component(folder)
        ))
        .into_response(),
        Ok(false) => Redirect::to("/pins?error=No+pin+found+for+that+folder").into_response(),
        Err(e) => Redirect::to(&format!(
            "/pins?error={}",
            url_encode_component(&e.to_string())
        ))
        .into_response(),
    }
}

/// POST /pins/import - Import handoff markers
pub(crate) async fn post_pins_import(
    State(state): State<WebState>,
    Form(form): Form<ImportPinsForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/pins/import") {
        return response;
    }
    let Some(dir) = state.config.handoff.markers_dir.clone() else {
        return Redirect::to(
            "/pins?error=Handoff+markers+directory+is+not+configured+in+config.toml",
        )
        .into_response();
    };
    let consume = state.config.handoff.consume_markers;
    let import = crate::handoff::import_markers(&state.database, &dir, consume).await;

    let link_now = matches!(
        form.link_now.as_deref(),
        Some("true") | Some("1") | Some("yes")
    );
    if link_now {
        match state.start_scan(false, false, None).await {
            Ok(job) => {
                let msg = format!(
                    "{} — background scan started to link pins (scope: {})",
                    import.summary_line(),
                    job.scope_label
                );
                return Redirect::to(&format!("/pins?message={}", url_encode_component(&msg)))
                    .into_response();
            }
            Err(e) => {
                let msg = format!(
                    "{} — pins imported, but scan could not start: {}",
                    import.summary_line(),
                    e
                );
                return Redirect::to(&format!("/pins?error={}", url_encode_component(&msg)))
                    .into_response();
            }
        }
    }

    Redirect::to(&format!(
        "/pins?message={}",
        url_encode_component(&import.summary_line())
    ))
    .into_response()
}

/// GET /links/quarantine - List Decypharr quarantined folders
pub(crate) async fn get_quarantine(State(state): State<WebState>) -> impl IntoResponse {
    let items = crate::quarantine::QuarantinedFolders::list_details(&state.config.sources);
    let template = QuarantineTemplate {
        items,
        sources_count: state.config.sources.len(),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}
