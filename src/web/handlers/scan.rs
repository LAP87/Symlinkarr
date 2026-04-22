use super::*;

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct ScanHistoryQuery {
    pub library: Option<String>,
    pub mode: Option<String>,
    pub search_missing: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnimeSearchOverrideForm {
    pub media_id: String,
    pub preferred_title: Option<String>,
    pub extra_hints: Option<String>,
    pub note: Option<String>,
    #[serde(default)]
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteAnimeSearchOverrideForm {
    pub media_id: String,
    #[serde(default)]
    pub csrf_token: String,
}

pub(super) fn scan_run_views(history: Vec<ScanHistoryRecord>) -> Vec<ScanRunView> {
    history.into_iter().map(ScanRunView::from_record).collect()
}

fn scan_history_filters_from_query(query: &ScanHistoryQuery) -> ScanHistoryFilters {
    ScanHistoryFilters {
        library: query.library.clone().unwrap_or_default(),
        mode: query.mode.clone().unwrap_or_else(|| "any".to_string()),
        search_missing: query
            .search_missing
            .clone()
            .unwrap_or_else(|| "any".to_string()),
        limit: query.limit.unwrap_or(25).clamp(1, 200),
    }
}
pub(super) async fn visible_last_scan_outcome(
    state: &WebState,
) -> Option<BackgroundScanOutcomeView> {
    let latest_run_started_at = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.into_iter().next().map(|run| run.started_at));

    state
        .last_scan_outcome()
        .await
        .filter(|outcome| should_surface_scan_outcome(outcome, latest_run_started_at.as_deref()))
        .map(Into::into)
}
async fn filtered_scan_history(
    state: &WebState,
    query: &ScanHistoryQuery,
) -> (ScanHistoryFilters, Vec<ScanRunView>) {
    let filters = scan_history_filters_from_query(query);
    let fetch_limit = (filters.limit * 5).clamp(50, 500);
    let history = match state.database.get_scan_history(fetch_limit).await {
        Ok(history) => history,
        Err(e) => {
            error!("Failed to get scan history: {}", e);
            Vec::new()
        }
    };

    let filtered = history
        .into_iter()
        .filter(|run| {
            if !filters.library.trim().is_empty()
                && run.library_filter.as_deref().unwrap_or_default() != filters.library
            {
                return false;
            }

            match filters.mode.as_str() {
                "dry" if !run.dry_run => return false,
                "live" if run.dry_run => return false,
                _ => {}
            }

            match filters.search_missing.as_str() {
                "only" if !run.search_missing => return false,
                "exclude" if run.search_missing => return false,
                _ => {}
            }

            true
        })
        .take(filters.limit as usize)
        .map(ScanRunView::from_record)
        .collect::<Vec<_>>();

    (filters, filtered)
}

fn skip_event_views(events: Vec<LinkEventHistoryRecord>) -> Vec<SkipEventView> {
    events
        .into_iter()
        .map(|event| {
            let reason = event.note.unwrap_or_else(|| "unknown".to_string());
            SkipEventView {
                event_at: event.event_at,
                action: event.action,
                reason_label: skip_reason_label(&reason),
                reason_group: skip_reason_group_label(&reason),
                reason,
                target_path: event.target_path.display().to_string(),
                source_path: event.source_path.map(|path| path.display().to_string()),
                media_id: event.media_id,
            }
        })
        .collect()
}

fn has_anime_library(config: &crate::config::Config) -> bool {
    config
        .libraries
        .iter()
        .any(|library| library.content_type == Some(crate::config::ContentType::Anime))
}

fn normalize_optional_form_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn valid_anime_override_media_id(media_id: &str) -> bool {
    let Some((prefix, raw_id)) = media_id.trim().split_once('-') else {
        return false;
    };

    matches!(prefix, "tvdb" | "tmdb")
        && !raw_id.is_empty()
        && raw_id.chars().all(|ch| ch.is_ascii_digit())
}

fn parse_anime_override_hints(raw: Option<&str>) -> Vec<String> {
    let mut hints = Vec::<String>::new();

    for line in raw.unwrap_or_default().lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let normalized = crate::utils::normalize(trimmed);
        if normalized.is_empty()
            || hints
                .iter()
                .any(|existing| crate::utils::normalize(existing) == normalized)
        {
            continue;
        }

        hints.push(trimmed.to_string());
    }

    hints
}

fn find_local_anime_override_target(
    config: &crate::config::Config,
    media_id: &str,
) -> Option<String> {
    let scanner = crate::library_scanner::LibraryScanner::new();

    config
        .libraries
        .iter()
        .filter(|library| library.content_type == Some(crate::config::ContentType::Anime))
        .find_map(|library| {
            scanner
                .scan_library(library)
                .into_iter()
                .find(|item| item.id.to_string() == media_id)
                .map(|item| format!("{} in {}", item.title, item.library_name))
        })
}

async fn load_anime_override_views(state: &WebState) -> Vec<AnimeSearchOverrideView> {
    match state.database.list_anime_search_overrides().await {
        Ok(entries) => entries.into_iter().map(Into::into).collect(),
        Err(err) => {
            error!("Failed to load anime search overrides: {}", err);
            Vec::new()
        }
    }
}

async fn build_scan_template(
    state: &WebState,
    query: &ScanHistoryQuery,
    anime_override_feedback: Option<FormFeedbackView>,
) -> ScanTemplate {
    let mut scan_query = query.clone();
    if scan_query.limit.is_none() {
        scan_query.limit = Some(10);
    }
    let (filters, history) = filtered_scan_history(state, &scan_query).await;
    let latest_run = history.first().cloned();
    let active_scan = state.active_scan().await.map(Into::into);
    let last_scan_outcome = if active_scan.is_none() {
        visible_last_scan_outcome(state).await
    } else {
        None
    };
    let queue = match state.database.get_acquisition_job_counts().await {
        Ok(counts) => queue_overview_from_counts(counts),
        Err(e) => {
            error!("Failed to get acquisition queue counts: {}", e);
            QueueOverview::default()
        }
    };
    let anime_search_overrides = load_anime_override_views(state).await;
    let anime_override_panel_open =
        anime_override_feedback.is_some() || !anime_search_overrides.is_empty();

    ScanTemplate {
        libraries: state.config.libraries.clone(),
        active_scan,
        last_scan_outcome,
        latest_run,
        history,
        queue,
        anime_search_overrides,
        anime_override_feedback,
        anime_override_panel_open,
        filters,
        default_dry_run: state.config.symlink.dry_run,
        csrf_token: browser_csrf_token(state),
    }
}
/// GET /scan - Scan page
pub(crate) async fn get_scan(
    State(state): State<WebState>,
    Query(query): Query<ScanHistoryQuery>,
) -> impl IntoResponse {
    info!("Serving scan page");

    let template = build_scan_template(&state, &query, None).await;
    Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
}

/// POST /scan/trigger - Trigger a scan
pub(crate) async fn post_scan_trigger(
    State(state): State<WebState>,
    Form(form): Form<ScanTriggerForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/scan/trigger") {
        return response;
    }

    info!(
        "Triggering scan (dry_run={}, search_missing={})",
        form.dry_run, form.search_missing
    );

    let library_name = form.library.as_deref().filter(|l| !l.is_empty());

    match state
        .start_scan(
            form.dry_run,
            form.search_missing,
            library_name.map(|value| value.to_string()),
        )
        .await
    {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Html(
                ScanResultTemplate {
                    success: true,
                    message: format!(
                        "Scan started in background for {}. Refresh /scan or /scan/history for the finished run.",
                        job.scope_label
                    ),
                    active_scan: Some(job.into()),
                    last_scan_outcome: None,
                    latest_run: None,
                    dry_run: form.dry_run,
                }
                .render()
                .unwrap_or_else(|e| e.to_string()),
            ),
        )
            .into_response(),
        Err(e) => {
            error!("Scan rejected: {}", e);
            (
                StatusCode::CONFLICT,
                Html(
                    ScanResultTemplate {
                        success: false,
                        message: format!("Scan not started: {}", e),
                        active_scan: state.active_scan().await.map(Into::into),
                        last_scan_outcome: visible_last_scan_outcome(&state).await,
                        latest_run: None,
                        dry_run: form.dry_run,
                    }
                    .render()
                    .unwrap_or_else(|e| e.to_string()),
                ),
            )
                .into_response()
        }
    }
}

/// POST /scan/anime-overrides - Save or update a manual anime search override
pub(crate) async fn post_scan_anime_override(
    State(state): State<WebState>,
    Form(form): Form<AnimeSearchOverrideForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(&state, &form.csrf_token, "/scan/anime-overrides") {
        return response;
    }

    let media_id = form.media_id.trim().to_string();
    let preferred_title = normalize_optional_form_text(form.preferred_title.as_deref());
    let extra_hints = parse_anime_override_hints(form.extra_hints.as_deref());
    let note = normalize_optional_form_text(form.note.as_deref());
    let local_anime_target = find_local_anime_override_target(&state.config, &media_id);

    let (status, feedback) = if !has_anime_library(&state.config) {
        (
            StatusCode::BAD_REQUEST,
            FormFeedbackView {
                success: false,
                message: "Anime override requires at least one configured anime library.".to_string(),
            },
        )
    } else if !valid_anime_override_media_id(&media_id) {
        (
            StatusCode::BAD_REQUEST,
            FormFeedbackView {
                success: false,
                message: "Media ID must use a tagged anime folder id like tvdb-12345 or tmdb-67890.".to_string(),
            },
        )
    } else if preferred_title.is_none() && extra_hints.is_empty() {
        (
            StatusCode::BAD_REQUEST,
            FormFeedbackView {
                success: false,
                message: "Add either a preferred title or at least one extra hint before saving an anime override.".to_string(),
            },
        )
    } else if local_anime_target.is_none() {
        (
            StatusCode::BAD_REQUEST,
            FormFeedbackView {
                success: false,
                message: format!(
                    "Media ID {} does not match any tagged folder in your configured anime libraries. Save overrides against an existing anime folder like `Series Name {{tvdb-12345}}` first.",
                    media_id
                ),
            },
        )
    } else {
        match state
            .database
            .upsert_anime_search_override(&crate::db::AnimeSearchOverrideSeed {
                media_id: media_id.clone(),
                preferred_title,
                extra_hints,
                note,
            })
            .await
        {
            Ok(()) => (
                StatusCode::OK,
                FormFeedbackView {
                    success: true,
                    message: format!(
                        "Saved anime search override for {} ({}). Future anime auto-acquire requests will prefer it before anime-lists hints.",
                        media_id,
                        local_anime_target.unwrap_or_else(|| "unknown anime folder".to_string())
                    ),
                },
            ),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                FormFeedbackView {
                    success: false,
                    message: format!("Failed to save anime override: {}", err),
                },
            ),
        }
    };

    let template = build_scan_template(
        &state,
        &ScanHistoryQuery {
            limit: Some(10),
            ..ScanHistoryQuery::default()
        },
        Some(feedback),
    )
    .await;
    (
        status,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
        .into_response()
}

/// POST /scan/anime-overrides/delete - Remove a manual anime search override
pub(crate) async fn post_scan_anime_override_delete(
    State(state): State<WebState>,
    Form(form): Form<DeleteAnimeSearchOverrideForm>,
) -> impl IntoResponse {
    if let Some(response) = require_browser_csrf_token(
        &state,
        &form.csrf_token,
        "/scan/anime-overrides/delete",
    ) {
        return response;
    }

    let media_id = form.media_id.trim().to_string();
    let (status, feedback) = if media_id.is_empty() {
        (
            StatusCode::BAD_REQUEST,
            FormFeedbackView {
                success: false,
                message: "Anime override delete requires a media id.".to_string(),
            },
        )
    } else {
        match state.database.delete_anime_search_override(&media_id).await {
            Ok(true) => (
                StatusCode::OK,
                FormFeedbackView {
                    success: true,
                    message: format!("Removed anime search override for {}.", media_id),
                },
            ),
            Ok(false) => (
                StatusCode::NOT_FOUND,
                FormFeedbackView {
                    success: false,
                    message: format!("No anime search override exists for {}.", media_id),
                },
            ),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                FormFeedbackView {
                    success: false,
                    message: format!("Failed to delete anime override: {}", err),
                },
            ),
        }
    };

    let template = build_scan_template(
        &state,
        &ScanHistoryQuery {
            limit: Some(10),
            ..ScanHistoryQuery::default()
        },
        Some(feedback),
    )
    .await;
    (
        status,
        Html(template.render().unwrap_or_else(|e| e.to_string())),
    )
        .into_response()
}

/// GET /scan/history - Scan history
pub(crate) async fn get_scan_history(
    State(state): State<WebState>,
    Query(query): Query<ScanHistoryQuery>,
) -> impl IntoResponse {
    let (filters, history) = filtered_scan_history(&state, &query).await;

    let template = ScanHistoryTemplate {
        libraries: state.config.libraries.clone(),
        history,
        filters,
    };
    Html(template.render().unwrap_or_else(|e| e.to_string()))
}

/// GET /scan/history/:id - Scan run detail
pub(crate) async fn get_scan_run_detail(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match state.database.get_scan_run(id).await {
        Ok(Some(run)) => {
            let skip_events = match run.run_token.as_deref() {
                Some(token) => match state
                    .database
                    .get_skip_link_events_for_run_token(token, 25)
                    .await
                {
                    Ok(events) => skip_event_views(events),
                    Err(e) => {
                        error!("Failed to load skip events for scan run {}: {}", id, e);
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            let template = ScanRunDetailTemplate {
                run: ScanRunView::from_record(run),
                skip_events,
            };
            Html(template.render().unwrap_or_else(|e| e.to_string())).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, format!("Scan run {} not found", id)).into_response(),
        Err(e) => {
            error!("Failed to load scan run {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load scan run {}: {}", id, e),
            )
                .into_response()
        }
    }
}
