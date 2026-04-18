use super::*;

#[derive(Debug, Default, Clone, Deserialize)]
pub(super) struct ApiAnimeRemediationQuery {
    pub plex_db: Option<String>,
    pub full: Option<bool>,
    pub state: Option<String>,
    pub reason: Option<String>,
    pub title: Option<String>,
    pub format: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiAnimeRemediationResponse {
    pub generated_at: String,
    pub plex_db_path: String,
    pub full: bool,
    pub filesystem_mixed_root_groups: usize,
    pub plex_duplicate_show_groups: usize,
    pub plex_hama_anidb_tvdb_groups: usize,
    pub correlated_hama_split_groups: usize,
    pub remediation_groups: usize,
    pub returned_groups: usize,
    pub visible_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub state_filter: String,
    pub reason_filter: Option<String>,
    pub title_filter: Option<String>,
    pub blocked_reason_summary: Vec<ApiAnimeRemediationBlockedReasonSummary>,
    pub available_blocked_reasons: Vec<ApiAnimeRemediationBlockedReasonSummary>,
    pub groups: Vec<AnimeRemediationPlanGroup>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ApiAnimeRemediationPreviewRequest {
    pub plex_db: Option<String>,
    pub title: Option<String>,
    pub library: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiAnimeRemediationBlockedReasonSummary {
    pub code: String,
    pub label: String,
    pub recommended_action: String,
    pub groups: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiAnimeRemediationPreviewResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub plex_db_path: String,
    pub title_filter: Option<String>,
    pub total_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub cleanup_candidates: usize,
    pub confirmation_token: String,
    pub blocked_reason_summary: Vec<ApiAnimeRemediationBlockedReasonSummary>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ApiAnimeRemediationApplyRequest {
    pub report_path: String,
    pub token: String,
    pub max_delete: Option<usize>,
    pub library: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiAnimeRemediationApplyResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub total_groups: usize,
    pub eligible_groups: usize,
    pub blocked_groups: usize,
    pub candidates: usize,
    pub quarantined: usize,
    pub removed: usize,
    pub skipped: usize,
    pub safety_snapshot: Option<String>,
    pub media_server_invalidation: Option<LibraryInvalidationOutcome>,
}

pub(super) fn api_blocked_reason_summary(
    summary: &[crate::commands::cleanup::AnimeRemediationBlockedReasonSummary],
) -> Vec<ApiAnimeRemediationBlockedReasonSummary> {
    summary
        .iter()
        .map(|entry| ApiAnimeRemediationBlockedReasonSummary {
            code: entry.code.as_str().to_string(),
            label: entry.label.clone(),
            recommended_action: entry.recommended_action.clone(),
            groups: entry.groups,
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiRepairResponse {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stale: usize,
    pub running: bool,
    pub started_at: Option<String>,
    pub scope_label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiRepairJob {
    pub status: String,
    pub started_at: String,
    pub scope_label: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiRepairOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stale: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiRepairStatusResponse {
    pub active_job: Option<ApiRepairJob>,
    pub last_outcome: Option<ApiRepairOutcome>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiCleanupAuditResponse {
    pub success: bool,
    pub message: String,
    pub report_path: String,
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
    pub running: bool,
    pub started_at: Option<String>,
    pub scope_label: Option<String>,
    pub libraries_label: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiCleanupAuditJob {
    pub status: String,
    pub started_at: String,
    pub scope_label: String,
    pub libraries_label: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiCleanupAuditOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub libraries_label: String,
    pub success: bool,
    pub message: String,
    pub report_path: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiCleanupAuditStatusResponse {
    pub active_job: Option<ApiCleanupAuditJob>,
    pub last_outcome: Option<ApiCleanupAuditOutcome>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiCleanupPruneResponse {
    pub success: bool,
    pub message: String,
    pub candidates: usize,
    pub blocked_candidates: usize,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub blocked_reason_summary: Vec<ApiPruneBlockedReasonSummary>,
    pub removed: usize,
    pub quarantined: usize,
    pub skipped: usize,
    pub media_server_invalidation: Option<LibraryInvalidationOutcome>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiPruneBlockedReasonSummary {
    pub code: String,
    pub label: String,
    pub candidates: usize,
    pub recommended_action: String,
}

pub(super) fn api_prune_blocked_reason_summary(
    summary: &[crate::cleanup_audit::PruneBlockedReasonSummary],
) -> Vec<ApiPruneBlockedReasonSummary> {
    summary
        .iter()
        .map(|entry| ApiPruneBlockedReasonSummary {
            code: entry.code.to_string(),
            label: entry.label.clone(),
            candidates: entry.candidates,
            recommended_action: entry.recommended_action.clone(),
        })
        .collect()
}

#[derive(Deserialize)]
pub(super) struct ApiCleanupAuditRequest {
    pub scope: String,
}

#[derive(Deserialize)]
pub(super) struct ApiCleanupPruneRequest {
    pub report_path: String,
    pub token: String,
    pub max_delete: Option<usize>,
}

pub(super) fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

pub(super) fn canonical_plex_db_path(path: std::path::PathBuf) -> Option<std::path::PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
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

pub(super) fn resolve_plex_db_path(query_path: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return canonical_plex_db_path(std::path::PathBuf::from(requested));
    }

    default_plex_db_candidates()
        .into_iter()
        .map(std::path::PathBuf::from)
        .find_map(canonical_plex_db_path)
}

/// GET /api/v1/report/anime-remediation
pub(super) async fn api_get_anime_remediation(
    State(state): State<WebState>,
    Query(query): Query<ApiAnimeRemediationQuery>,
) -> Result<Response, (StatusCode, Json<ApiErrorResponse>)> {
    let filters = AnimeRemediationGroupFilters::parse(
        query.state.as_deref(),
        query.reason.as_deref(),
        query.title.as_deref(),
    )
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: format!("Invalid anime remediation filters: {}", e),
            }),
        )
    })?;

    let Some(plex_db_path) = resolve_plex_db_path(query.plex_db.as_deref()) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "Plex DB path is required or must exist at a standard local path"
                    .to_string(),
            }),
        ));
    };

    let full = query.full.unwrap_or(false);
    let wants_tsv = matches!(query.format.as_deref(), Some("tsv"));
    if let Some(format) = query.format.as_deref() {
        if format != "json" && format != "tsv" {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiErrorResponse {
                    error: format!(
                        "Invalid anime remediation format '{}' (expected json or tsv)",
                        format
                    ),
                }),
            ));
        }
    }
    match build_anime_remediation_report(&state.config, &state.database, &plex_db_path, full).await
    {
        Ok(Some(report)) => {
            let assessed_groups = assess_anime_remediation_groups(&report.groups).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiErrorResponse {
                        error: format!("Failed to assess anime remediation backlog: {}", e),
                    }),
                )
            })?;
            let filtered_groups =
                filter_anime_remediation_groups(assessed_groups.clone(), &filters);
            if wants_tsv {
                let body = render_anime_remediation_groups_tsv(&filtered_groups);
                return Ok((
                    [(CONTENT_TYPE, "text/tab-separated-values; charset=utf-8")],
                    body,
                )
                    .into_response());
            }

            let eligible_groups = filtered_groups
                .iter()
                .filter(|group| group.eligible)
                .count();
            let blocked_groups = filtered_groups.len().saturating_sub(eligible_groups);
            Ok(Json(ApiAnimeRemediationResponse {
                generated_at: report.generated_at,
                plex_db_path: plex_db_path.to_string_lossy().to_string(),
                full,
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
                reason_filter: filters.block_code.map(|code| code.as_str().to_string()),
                title_filter: filters.title_contains.clone(),
                blocked_reason_summary: api_blocked_reason_summary(
                    &summarize_anime_remediation_blocked_reasons(&filtered_groups),
                ),
                available_blocked_reasons: api_blocked_reason_summary(
                    &anime_remediation_block_reason_catalog(),
                ),
                groups: filtered_groups,
            })
            .into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiErrorResponse {
                error: "No anime libraries are configured for remediation reporting".to_string(),
            }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to build anime remediation report: {}", e),
            }),
        )),
    }
}

/// POST /api/v1/cleanup/anime-remediation/preview
pub(super) async fn api_post_anime_remediation_preview(
    State(state): State<WebState>,
    Json(req): Json<ApiAnimeRemediationPreviewRequest>,
) -> impl IntoResponse {
    let Some(plex_db_path) = resolve_plex_db_path(req.plex_db.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiAnimeRemediationPreviewResponse {
                success: false,
                message: "Anime remediation preview failed: Plex DB path is required or must exist at a standard local path".to_string(),
                report_path: String::new(),
                plex_db_path: String::new(),
                title_filter: req.title.clone(),
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                cleanup_candidates: 0,
                confirmation_token: String::new(),
                blocked_reason_summary: Vec::new(),
            }),
        );
    };

    match preview_anime_remediation_plan(
        &state.config,
        &state.database,
        req.library.as_deref(),
        &plex_db_path,
        req.title.as_deref(),
        None,
    )
    .await
    {
        Ok((plan, report_path)) => (
            StatusCode::OK,
            Json(ApiAnimeRemediationPreviewResponse {
                success: true,
                message: format!(
                    "Anime remediation preview saved. Review {} before applying.",
                    report_path.display()
                ),
                report_path: report_path
                    .canonicalize()
                    .unwrap_or(report_path)
                    .to_string_lossy()
                    .to_string(),
                plex_db_path: plan.plex_db_path.to_string_lossy().to_string(),
                title_filter: plan.title_filter.clone(),
                total_groups: plan.total_groups,
                eligible_groups: plan.eligible_groups,
                blocked_groups: plan.blocked_groups,
                cleanup_candidates: plan.cleanup_candidates,
                confirmation_token: plan.confirmation_token.clone(),
                blocked_reason_summary: api_blocked_reason_summary(&plan.blocked_reason_summary),
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ApiAnimeRemediationPreviewResponse {
                success: false,
                message: format!("Anime remediation preview failed: {}", err),
                report_path: String::new(),
                plex_db_path: plex_db_path.to_string_lossy().to_string(),
                title_filter: req.title.clone(),
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                cleanup_candidates: 0,
                confirmation_token: String::new(),
                blocked_reason_summary: Vec::new(),
            }),
        ),
    }
}

/// POST /api/v1/cleanup/anime-remediation/apply
pub(super) async fn api_post_anime_remediation_apply(
    State(state): State<WebState>,
    Json(req): Json<ApiAnimeRemediationApplyRequest>,
) -> impl IntoResponse {
    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &req.report_path)
    {
        Ok(path) => path,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiAnimeRemediationApplyResponse {
                    success: false,
                    message: format!("Anime remediation apply failed: {}", err),
                    report_path: String::new(),
                    total_groups: 0,
                    eligible_groups: 0,
                    blocked_groups: 0,
                    candidates: 0,
                    quarantined: 0,
                    removed: 0,
                    skipped: 0,
                    safety_snapshot: None,
                    media_server_invalidation: None,
                }),
            );
        }
    };

    match apply_anime_remediation_plan_with_refresh(
        &state.config,
        &state.database,
        req.library.as_deref(),
        &report_path,
        Some(&req.token),
        req.max_delete,
        false,
    )
    .await
    {
        Ok((plan, outcome, safety_snapshot, invalidation)) => (
            StatusCode::OK,
            Json(ApiAnimeRemediationApplyResponse {
                success: true,
                message: "Anime remediation applied".to_string(),
                report_path: report_path.to_string_lossy().to_string(),
                total_groups: plan.total_groups,
                eligible_groups: plan.eligible_groups,
                blocked_groups: plan.blocked_groups,
                candidates: outcome.candidates,
                quarantined: outcome.quarantined,
                removed: outcome.removed,
                skipped: outcome.skipped,
                safety_snapshot: safety_snapshot
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string()),
                media_server_invalidation: Some(invalidation),
            }),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ApiAnimeRemediationApplyResponse {
                success: false,
                message: format!("Anime remediation apply failed: {}", err),
                report_path: report_path.to_string_lossy().to_string(),
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                candidates: 0,
                quarantined: 0,
                removed: 0,
                skipped: 0,
                safety_snapshot: None,
                media_server_invalidation: None,
            }),
        ),
    }
}

/// POST /api/v1/repair/auto
pub(super) async fn api_post_repair_auto(State(state): State<WebState>) -> impl IntoResponse {
    info!("API: Starting background auto repair");

    match state.start_repair().await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(ApiRepairResponse {
                success: true,
                message: format!(
                    "Repair started in background for {}. Poll /api/v1/repair/status for the finished outcome.",
                    job.scope_label
                ),
                repaired: 0,
                failed: 0,
                skipped: 0,
                stale: 0,
                running: true,
                started_at: Some(job.started_at),
                scope_label: Some(job.scope_label),
            }),
        ),
        Err(err) => {
            let active_repair = state.active_repair().await;
            (
                StatusCode::CONFLICT,
                Json(ApiRepairResponse {
                    success: false,
                    message: format!("Repair not started: {}", err),
                    repaired: 0,
                    failed: 0,
                    skipped: 0,
                    stale: 0,
                    running: active_repair.is_some(),
                    started_at: active_repair.as_ref().map(|job| job.started_at.clone()),
                    scope_label: active_repair.map(|job| job.scope_label),
                }),
            )
        }
    }
}

/// GET /api/v1/repair/status
pub(super) async fn api_get_repair_status(
    State(state): State<WebState>,
) -> Json<ApiRepairStatusResponse> {
    Json(ApiRepairStatusResponse {
        active_job: state.active_repair().await.map(|job| ApiRepairJob {
            status: "running".to_string(),
            started_at: job.started_at,
            scope_label: job.scope_label,
        }),
        last_outcome: state
            .last_repair_outcome()
            .await
            .map(|outcome| ApiRepairOutcome {
                finished_at: outcome.finished_at,
                scope_label: outcome.scope_label,
                success: outcome.success,
                message: outcome.message,
                repaired: outcome.repaired,
                failed: outcome.failed,
                skipped: outcome.skipped,
                stale: outcome.stale,
            }),
    })
}

/// POST /api/v1/cleanup/audit
pub(super) async fn api_post_cleanup_audit(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupAuditRequest>,
) -> impl IntoResponse {
    info!("API: Starting cleanup audit");

    let scope = match CleanupScope::parse(&req.scope) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Invalid scope: {}", e),
                    report_path: String::new(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    running: false,
                    started_at: None,
                    scope_label: None,
                    libraries_label: None,
                }),
            );
        }
    };

    match state.start_cleanup_audit(scope, Vec::new()).await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(ApiCleanupAuditResponse {
                success: true,
                message: format!(
                    "Cleanup audit started in background for {}. Poll /api/v1/cleanup/audit/jobs or inspect /cleanup for the finished report.",
                    job.scope_label
                ),
                report_path: String::new(),
                total_findings: 0,
                critical: 0,
                high: 0,
                warning: 0,
                running: true,
                started_at: Some(job.started_at),
                scope_label: Some(job.scope_label),
                libraries_label: Some(job.libraries_label),
            }),
        ),
        Err(e) => {
            let active_audit = state.active_cleanup_audit().await;
            (
                StatusCode::CONFLICT,
                Json(ApiCleanupAuditResponse {
                    success: false,
                    message: format!("Cleanup audit not started: {}", e),
                    report_path: String::new(),
                    total_findings: 0,
                    critical: 0,
                    high: 0,
                    warning: 0,
                    running: active_audit.is_some(),
                    started_at: active_audit.as_ref().map(|job| job.started_at.clone()),
                    scope_label: active_audit.as_ref().map(|job| job.scope_label.clone()),
                    libraries_label: active_audit.as_ref().map(|job| job.libraries_label.clone()),
                }),
            )
        }
    }
}

/// GET /api/v1/cleanup/audit/status
pub(super) async fn api_get_cleanup_audit_status(
    State(state): State<WebState>,
) -> Json<ApiCleanupAuditStatusResponse> {
    let latest_report_created_at = latest_cleanup_report_created_at(&state.config.backup.path);

    Json(ApiCleanupAuditStatusResponse {
        active_job: state
            .active_cleanup_audit()
            .await
            .map(|active_audit| ApiCleanupAuditJob {
                status: "running".to_string(),
                started_at: active_audit.started_at,
                scope_label: active_audit.scope_label,
                libraries_label: active_audit.libraries_label,
            }),
        last_outcome: state
            .last_cleanup_audit_outcome()
            .await
            .filter(|outcome| {
                should_surface_cleanup_audit_outcome(outcome, latest_report_created_at.as_deref())
            })
            .map(|outcome| ApiCleanupAuditOutcome {
                finished_at: outcome.finished_at,
                scope_label: outcome.scope_label,
                libraries_label: outcome.libraries_label,
                success: outcome.success,
                message: outcome.message,
                report_path: outcome.report_path,
            }),
    })
}

/// GET /api/v1/cleanup/audit/jobs
pub(super) async fn api_get_cleanup_audit_jobs(
    State(state): State<WebState>,
) -> Json<Vec<ApiCleanupAuditJob>> {
    let mut jobs = Vec::new();
    if let Some(active_audit) = state.active_cleanup_audit().await {
        jobs.push(ApiCleanupAuditJob {
            status: "running".to_string(),
            started_at: active_audit.started_at,
            scope_label: active_audit.scope_label,
            libraries_label: active_audit.libraries_label,
        });
    }

    Json(jobs)
}

/// POST /api/v1/cleanup/prune
pub(super) async fn api_post_cleanup_prune(
    State(state): State<WebState>,
    Json(req): Json<ApiCleanupPruneRequest>,
) -> impl IntoResponse {
    info!("API: Applying prune");

    let report_path = match resolve_cleanup_report_path(&state.config.backup.path, &req.report_path)
    {
        Ok(path) => path,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiCleanupPruneResponse {
                    success: false,
                    message: format!("Prune failed: {}", err),
                    candidates: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    blocked_reason_summary: Vec::new(),
                    removed: 0,
                    quarantined: 0,
                    skipped: 0,
                    media_server_invalidation: None,
                }),
            );
        }
    };

    let selected = match selected_libraries(state.config.as_ref(), None) {
        Ok(selected) => selected,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiCleanupPruneResponse {
                    success: false,
                    message: format!("Prune failed: {}", e),
                    candidates: 0,
                    blocked_candidates: 0,
                    managed_candidates: 0,
                    foreign_candidates: 0,
                    blocked_reason_summary: Vec::new(),
                    removed: 0,
                    quarantined: 0,
                    skipped: 0,
                    media_server_invalidation: None,
                }),
            );
        }
    };

    match apply_cleanup_prune_with_refresh(
        &state.config,
        &state.database,
        CleanupPruneApplyArgs {
            libraries: &selected,
            report_path: &report_path,
            include_legacy_anime_roots: false,
            max_delete: req.max_delete,
            confirm_token: Some(&req.token),
            emit_text: false,
        },
    )
    .await
    {
        Ok((outcome, invalidation)) => (
            StatusCode::OK,
            Json(ApiCleanupPruneResponse {
                success: true,
                message: "Prune applied".to_string(),
                candidates: outcome.candidates,
                blocked_candidates: outcome.blocked_candidates,
                managed_candidates: outcome.managed_candidates,
                foreign_candidates: outcome.foreign_candidates,
                blocked_reason_summary: api_prune_blocked_reason_summary(
                    &outcome.blocked_reason_summary,
                ),
                removed: outcome.removed,
                quarantined: outcome.quarantined,
                skipped: outcome.skipped,
                media_server_invalidation: Some(invalidation),
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiCleanupPruneResponse {
                success: false,
                message: format!("Prune failed: {}", e),
                candidates: 0,
                blocked_candidates: 0,
                managed_candidates: 0,
                foreign_candidates: 0,
                blocked_reason_summary: Vec::new(),
                removed: 0,
                quarantined: 0,
                skipped: 0,
                media_server_invalidation: None,
            }),
        ),
    }
}
