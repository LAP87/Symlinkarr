use super::*;

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

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return canonical_plex_db_path(PathBuf::from(requested));
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find_map(canonical_plex_db_path)
}

async fn visible_last_cleanup_audit_outcome(
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

    let Some(plex_db_path) = resolve_plex_db_path(query.plex_db.as_deref()) else {
        return Html(
            AnimeRemediationTemplate {
                summary: None,
                groups: vec![],
                error_message: Some(
                    "Plex DB path is required or must exist at a standard local path".to_string(),
                ),
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        );
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

    let Some(plex_db_path) = resolve_plex_db_path(form.plex_db.as_deref()) else {
        return Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: "Anime remediation preview failed: Plex DB path is required or must exist at a standard local path".to_string(),
                preview: None,
                apply: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response();
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
                    csrf_token: browser_csrf_token(&state),
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            )
            .into_response();
        }
    };

    match apply_anime_remediation_plan_with_refresh(
        &state.config,
        &state.database,
        form.library.as_deref(),
        &report_path,
        Some(form.token.trim()),
        form.max_delete,
        true,
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
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
        )
        .into_response(),
        Err(err) => Html(
            AnimeRemediationResultTemplate {
                success: false,
                message: format!("Anime remediation apply failed: {}", err),
                preview: None,
                apply: None,
                csrf_token: browser_csrf_token(&state),
            }
            .render()
            .unwrap_or_else(|e| e.to_string()),
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
        confirmation_token: prune_plan.map(|plan| plan.confirmation_token),
        already_applied: report.applied_at.is_some(),
        error_message: None,
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

    let (outcome, invalidation) = match apply_cleanup_prune_with_refresh(
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
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("Prune operation failed: {}", e);
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

/// GET /links/dead - Dead links
pub(crate) async fn get_dead_links(State(state): State<WebState>) -> impl IntoResponse {
    let links = match state.database.get_dead_links().await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to get dead links: {}", e);
            vec![]
        }
    };

    let active_repair = state.active_repair().await.map(Into::into);
    let last_repair_outcome = if active_repair.is_none() {
        visible_last_repair_outcome(&state).await
    } else {
        None
    };

    let template = DeadLinksTemplate {
        links,
        active_repair,
        last_repair_outcome,
        csrf_token: browser_csrf_token(&state),
    };
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /links/repair - Repair dead links
pub(crate) async fn post_repair(
    State(state): State<WebState>,
    Form(form): Form<BrowserMutationForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/links/repair") {
        return response;
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
