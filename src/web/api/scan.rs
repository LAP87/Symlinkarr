use super::*;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiScanResponse {
    pub success: bool,
    pub message: String,
    pub created: u64,
    pub updated: u64,
    pub skipped: u64,
    pub running: bool,
    pub started_at: Option<String>,
    pub scope_label: Option<String>,
    pub search_missing: bool,
    pub dry_run: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiScanJob {
    pub id: i64,
    pub status: String,
    pub started_at: String,
    pub scope_label: String,
    pub search_missing: bool,
    pub dry_run: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiScanOutcome {
    pub finished_at: String,
    pub scope_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiScanStatusResponse {
    pub active_job: Option<ApiScanJob>,
    pub last_outcome: Option<ApiScanOutcome>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub(super) struct ApiScanHistoryQuery {
    pub library: Option<String>,
    pub mode: Option<String>,
    pub search_missing: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiScanAutoAcquireSummary {
    pub requests: i64,
    pub missing_requests: i64,
    pub cutoff_requests: i64,
    pub dry_run_hits: i64,
    pub submitted: i64,
    pub no_result: i64,
    pub blocked: i64,
    pub failed: i64,
    pub completed_linked: i64,
    pub completed_unlinked: i64,
    pub successes: i64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiPlexRefreshSummary {
    pub runtime_ms: i64,
    pub requested_paths: i64,
    pub unique_paths: i64,
    pub planned_batches: i64,
    pub coalesced_batches: i64,
    pub coalesced_paths: i64,
    pub refreshed_batches: i64,
    pub refreshed_paths_covered: i64,
    pub skipped_batches: i64,
    pub unresolved_paths: i64,
    pub capped_batches: i64,
    pub aborted_due_to_cap: bool,
    pub deferred_due_to_lock: bool,
    pub failed_batches: i64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiMediaServerRefreshServer {
    pub server: String,
    pub requested_targets: i64,
    pub refresh: ApiPlexRefreshSummary,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiSkipReasonCount {
    pub reason: String,
    pub label: String,
    pub group: String,
    pub help: String,
    pub count: i64,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiSkipEventSample {
    pub event_at: String,
    pub action: String,
    pub reason: String,
    pub reason_label: String,
    pub reason_group: String,
    pub reason_help: String,
    pub target_path: String,
    pub source_path: Option<String>,
    pub media_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiScanHistoryEntry {
    pub id: i64,
    pub started_at: String,
    pub scope_label: String,
    pub origin: String,
    pub origin_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub total_runtime_ms: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub cache_hit_ratio: Option<f64>,
    pub dead_count: i64,
    pub skip_reasons: Vec<ApiSkipReasonCount>,
    pub plex_refresh: ApiPlexRefreshSummary,
    pub media_server_refresh: Vec<ApiMediaServerRefreshServer>,
    pub auto_acquire: ApiScanAutoAcquireSummary,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ApiScanRunDetail {
    pub id: i64,
    pub started_at: String,
    pub library_filter: Option<String>,
    pub scope_label: String,
    pub origin: String,
    pub origin_label: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
    pub links_removed: i64,
    pub links_skipped: i64,
    pub ambiguous_skipped: i64,
    pub skip_reasons: Vec<ApiSkipReasonCount>,
    pub skip_event_samples: Vec<ApiSkipEventSample>,
    pub runtime_checks_ms: i64,
    pub library_scan_ms: i64,
    pub source_inventory_ms: i64,
    pub matching_ms: i64,
    pub title_enrichment_ms: i64,
    pub linking_ms: i64,
    pub plex_refresh_ms: i64,
    pub plex_refresh: ApiPlexRefreshSummary,
    pub media_server_refresh: Vec<ApiMediaServerRefreshServer>,
    pub dead_link_sweep_ms: i64,
    pub total_runtime_ms: i64,
    pub cache_hit_ratio: Option<f64>,
    pub candidate_slots: i64,
    pub scored_candidates: i64,
    pub exact_id_hits: i64,
    pub auto_acquire_requests: i64,
    pub auto_acquire_missing_requests: i64,
    pub auto_acquire_cutoff_requests: i64,
    pub auto_acquire_dry_run_hits: i64,
    pub auto_acquire_submitted: i64,
    pub auto_acquire_no_result: i64,
    pub auto_acquire_blocked: i64,
    pub auto_acquire_failed: i64,
    pub auto_acquire_completed_linked: i64,
    pub auto_acquire_completed_unlinked: i64,
    pub auto_acquire_successes: i64,
}

/// POST /api/v1/scan
pub(super) async fn api_post_scan(
    State(state): State<WebState>,
    Json(req): Json<ApiScanRequest>,
) -> impl IntoResponse {
    info!("API: Triggering scan");

    let dry_run = req.dry_run.unwrap_or(false);
    let library_name = req.library.filter(|l| !l.is_empty());
    let search_missing = req.search_missing.unwrap_or(false);

    match state
        .start_scan(dry_run, search_missing, library_name.clone())
        .await
    {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(ApiScanResponse {
                success: true,
                message: format!(
                    "Scan started in background for {}. Poll /api/v1/scan/jobs or /api/v1/scan/history for completion.",
                    job.scope_label
                ),
                created: 0,
                updated: 0,
                skipped: 0,
                running: true,
                started_at: Some(job.started_at),
                scope_label: Some(job.scope_label),
                search_missing: job.search_missing,
                dry_run: job.dry_run,
            }),
        ),
        Err(e) => {
            let active_scan = state.active_scan().await;
            (
                StatusCode::CONFLICT,
                Json(ApiScanResponse {
                    success: false,
                    message: format!("Scan not started: {}", e),
                    created: 0,
                    updated: 0,
                    skipped: 0,
                    running: active_scan.is_some(),
                    started_at: active_scan.as_ref().map(|job| job.started_at.clone()),
                    scope_label: active_scan.as_ref().map(|job| job.scope_label.clone()),
                    search_missing: active_scan.as_ref().is_some_and(|job| job.search_missing),
                    dry_run: active_scan.as_ref().is_some_and(|job| job.dry_run),
                }),
            )
        }
    }
}

pub(super) fn api_scan_job_from_active(job: crate::web::ActiveScanJob) -> ApiScanJob {
    ApiScanJob {
        id: 0,
        status: "running".to_string(),
        started_at: job.started_at,
        scope_label: job.scope_label,
        search_missing: job.search_missing,
        dry_run: job.dry_run,
        library_items_found: 0,
        source_items_found: 0,
        matches_found: 0,
        links_created: 0,
        links_updated: 0,
        dead_marked: 0,
    }
}

/// GET /api/v1/scan/status
pub(super) async fn api_get_scan_status(
    State(state): State<WebState>,
) -> Json<ApiScanStatusResponse> {
    let latest_run_started_at = state
        .database
        .get_scan_history(1)
        .await
        .ok()
        .and_then(|history| history.into_iter().next().map(|run| run.started_at));

    Json(ApiScanStatusResponse {
        active_job: state.active_scan().await.map(api_scan_job_from_active),
        last_outcome: state
            .last_scan_outcome()
            .await
            .filter(|outcome| {
                should_surface_scan_outcome(outcome, latest_run_started_at.as_deref())
            })
            .map(|outcome| ApiScanOutcome {
                finished_at: outcome.finished_at,
                scope_label: outcome.scope_label,
                dry_run: outcome.dry_run,
                search_missing: outcome.search_missing,
                success: outcome.success,
                message: outcome.message,
            }),
    })
}

/// GET /api/v1/scan/jobs
pub(super) async fn api_get_scan_jobs(State(state): State<WebState>) -> Json<Vec<ApiScanJob>> {
    let history = state
        .database
        .get_scan_history(50)
        .await
        .unwrap_or_default();

    let mut jobs = Vec::new();
    if let Some(active_scan) = state.active_scan().await {
        jobs.push(api_scan_job_from_active(active_scan));
    }

    jobs.extend(history.into_iter().map(|h| {
        ApiScanJob {
            id: h.id,
            status: "completed".to_string(),
            started_at: h.started_at.to_string(),
            scope_label: h
                .library_filter
                .clone()
                .unwrap_or_else(|| "All Libraries".to_string()),
            search_missing: h.search_missing,
            dry_run: h.dry_run,
            library_items_found: h.library_items_found,
            source_items_found: h.source_items_found,
            matches_found: h.matches_found,
            links_created: h.links_created,
            links_updated: h.links_updated,
            dead_marked: h.dead_marked,
        }
    }));

    Json(jobs)
}

pub(super) fn scan_scope_label(record: &ScanHistoryRecord) -> String {
    record
        .library_filter
        .clone()
        .unwrap_or_else(|| "All Libraries".to_string())
}

pub(super) fn scan_total_runtime_ms(record: &ScanHistoryRecord) -> i64 {
    record.runtime_checks_ms
        + record.library_scan_ms
        + record.source_inventory_ms
        + record.matching_ms
        + record.title_enrichment_ms
        + record.linking_ms
        + record.plex_refresh_ms
        + record.dead_link_sweep_ms
}

pub(super) fn scan_auto_acquire_successes(record: &ScanHistoryRecord) -> i64 {
    record.auto_acquire_dry_run_hits
        + record.auto_acquire_submitted
        + record.auto_acquire_completed_linked
        + record.auto_acquire_completed_unlinked
}

pub(super) fn scan_dead_count(record: &ScanHistoryRecord) -> i64 {
    record.dead_marked + record.links_removed
}

pub(super) fn scan_history_matches_query(
    record: &ScanHistoryRecord,
    query: &ApiScanHistoryQuery,
) -> bool {
    if query
        .library
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|library| record.library_filter.as_deref().unwrap_or_default() != library)
    {
        return false;
    }

    match query.mode.as_deref() {
        Some("dry") if !record.dry_run => return false,
        Some("live") if record.dry_run => return false,
        _ => {}
    }

    match query.search_missing.as_deref() {
        Some("only") if !record.search_missing => return false,
        Some("exclude") if record.search_missing => return false,
        _ => {}
    }

    true
}

pub(super) fn scan_history_query_limit(query: &ApiScanHistoryQuery) -> i64 {
    query.limit.unwrap_or(25).clamp(1, 200)
}

pub(super) fn scan_history_fetch_limit(query: &ApiScanHistoryQuery) -> i64 {
    (scan_history_query_limit(query) * 10).clamp(50, 1_000)
}

pub(super) fn media_server_refresh_entries(
    record: &ScanHistoryRecord,
) -> Vec<LibraryInvalidationServerOutcome> {
    record
        .media_server_refresh_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<LibraryInvalidationServerOutcome>>(json).ok())
        .unwrap_or_default()
}

pub(super) fn plex_refresh_summary_from_record(
    record: &ScanHistoryRecord,
) -> ApiPlexRefreshSummary {
    let deferred_due_to_lock = media_server_refresh_entries(record)
        .iter()
        .any(|entry| entry.refresh.deferred_due_to_lock);
    ApiPlexRefreshSummary {
        runtime_ms: record.plex_refresh_ms,
        requested_paths: record.plex_refresh_requested_paths,
        unique_paths: record.plex_refresh_unique_paths,
        planned_batches: record.plex_refresh_planned_batches,
        coalesced_batches: record.plex_refresh_coalesced_batches,
        coalesced_paths: record.plex_refresh_coalesced_paths,
        refreshed_batches: record.plex_refresh_refreshed_batches,
        refreshed_paths_covered: record.plex_refresh_refreshed_paths_covered,
        skipped_batches: record.plex_refresh_skipped_batches,
        unresolved_paths: record.plex_refresh_unresolved_paths,
        capped_batches: record.plex_refresh_capped_batches,
        aborted_due_to_cap: record.plex_refresh_aborted_due_to_cap,
        deferred_due_to_lock,
        failed_batches: record.plex_refresh_failed_batches,
    }
}

pub(super) fn api_refresh_summary_from_telemetry(
    telemetry: &crate::media_servers::LibraryRefreshTelemetry,
) -> ApiPlexRefreshSummary {
    ApiPlexRefreshSummary {
        runtime_ms: 0,
        requested_paths: telemetry.requested_paths as i64,
        unique_paths: telemetry.unique_paths as i64,
        planned_batches: telemetry.planned_batches as i64,
        coalesced_batches: telemetry.coalesced_batches as i64,
        coalesced_paths: telemetry.coalesced_paths as i64,
        refreshed_batches: telemetry.refreshed_batches as i64,
        refreshed_paths_covered: telemetry.refreshed_paths_covered as i64,
        skipped_batches: telemetry.skipped_batches as i64,
        unresolved_paths: telemetry.unresolved_paths as i64,
        capped_batches: telemetry.capped_batches as i64,
        aborted_due_to_cap: telemetry.aborted_due_to_cap,
        deferred_due_to_lock: telemetry.deferred_due_to_lock,
        failed_batches: telemetry.failed_batches as i64,
    }
}

pub(super) fn media_server_refresh_from_record(
    record: &ScanHistoryRecord,
) -> Vec<ApiMediaServerRefreshServer> {
    media_server_refresh_entries(record)
        .into_iter()
        .map(|entry| ApiMediaServerRefreshServer {
            server: entry.server.to_string(),
            requested_targets: entry.requested_targets as i64,
            refresh: api_refresh_summary_from_telemetry(&entry.refresh),
        })
        .collect()
}

pub(super) fn skip_reasons_from_record(record: &ScanHistoryRecord) -> Vec<ApiSkipReasonCount> {
    let mut entries = record
        .skip_reason_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<std::collections::BTreeMap<String, i64>>(json).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(reason, count)| ApiSkipReasonCount {
            label: skip_reason_label(&reason),
            group: skip_reason_group_label(&reason),
            help: skip_reason_help(&reason),
            reason,
            count,
        })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    entries
}

pub(super) fn api_skip_event_samples(
    events: Vec<crate::db::LinkEventHistoryRecord>,
) -> Vec<ApiSkipEventSample> {
    events
        .into_iter()
        .map(|event| {
            let reason = event.note.unwrap_or_else(|| "unknown".to_string());
            ApiSkipEventSample {
                event_at: event.event_at,
                action: event.action,
                reason_label: skip_reason_label(&reason),
                reason_group: skip_reason_group_label(&reason),
                reason_help: skip_reason_help(&reason),
                reason,
                target_path: event.target_path.display().to_string(),
                source_path: event.source_path.map(|path| path.display().to_string()),
                media_id: event.media_id,
            }
        })
        .collect()
}

pub(super) fn scan_history_entry_from_record(record: ScanHistoryRecord) -> ApiScanHistoryEntry {
    let scope_label = scan_scope_label(&record);
    let total_runtime_ms = scan_total_runtime_ms(&record);
    let dead_count = scan_dead_count(&record);
    let auto_acquire_successes = scan_auto_acquire_successes(&record);
    let started_at = record.started_at.clone();
    let plex_refresh = plex_refresh_summary_from_record(&record);
    let media_server_refresh = media_server_refresh_from_record(&record);
    let skip_reasons = skip_reasons_from_record(&record);

    ApiScanHistoryEntry {
        id: record.id,
        started_at,
        scope_label,
        origin: record.origin.as_str().to_string(),
        origin_label: crate::web::templates::scan_run_origin_label(record.origin).to_string(),
        dry_run: record.dry_run,
        search_missing: record.search_missing,
        total_runtime_ms,
        matches_found: record.matches_found,
        links_created: record.links_created,
        links_updated: record.links_updated,
        cache_hit_ratio: record.cache_hit_ratio,
        dead_count,
        skip_reasons,
        plex_refresh,
        media_server_refresh,
        auto_acquire: ApiScanAutoAcquireSummary {
            requests: record.auto_acquire_requests,
            missing_requests: record.auto_acquire_missing_requests,
            cutoff_requests: record.auto_acquire_cutoff_requests,
            dry_run_hits: record.auto_acquire_dry_run_hits,
            submitted: record.auto_acquire_submitted,
            no_result: record.auto_acquire_no_result,
            blocked: record.auto_acquire_blocked,
            failed: record.auto_acquire_failed,
            completed_linked: record.auto_acquire_completed_linked,
            completed_unlinked: record.auto_acquire_completed_unlinked,
            successes: auto_acquire_successes,
        },
    }
}

pub(super) fn scan_run_detail_from_record(
    record: ScanHistoryRecord,
    skip_event_samples: Vec<ApiSkipEventSample>,
) -> ApiScanRunDetail {
    let scope_label = scan_scope_label(&record);
    let total_runtime_ms = scan_total_runtime_ms(&record);
    let auto_acquire_successes = scan_auto_acquire_successes(&record);
    let started_at = record.started_at.clone();
    let plex_refresh = plex_refresh_summary_from_record(&record);
    let media_server_refresh = media_server_refresh_from_record(&record);
    let skip_reasons = skip_reasons_from_record(&record);

    ApiScanRunDetail {
        id: record.id,
        started_at,
        library_filter: record.library_filter.clone(),
        scope_label,
        origin: record.origin.as_str().to_string(),
        origin_label: crate::web::templates::scan_run_origin_label(record.origin).to_string(),
        dry_run: record.dry_run,
        search_missing: record.search_missing,
        library_items_found: record.library_items_found,
        source_items_found: record.source_items_found,
        matches_found: record.matches_found,
        links_created: record.links_created,
        links_updated: record.links_updated,
        dead_marked: record.dead_marked,
        links_removed: record.links_removed,
        links_skipped: record.links_skipped,
        ambiguous_skipped: record.ambiguous_skipped,
        skip_reasons,
        skip_event_samples,
        runtime_checks_ms: record.runtime_checks_ms,
        library_scan_ms: record.library_scan_ms,
        source_inventory_ms: record.source_inventory_ms,
        matching_ms: record.matching_ms,
        title_enrichment_ms: record.title_enrichment_ms,
        linking_ms: record.linking_ms,
        plex_refresh_ms: record.plex_refresh_ms,
        plex_refresh,
        media_server_refresh,
        dead_link_sweep_ms: record.dead_link_sweep_ms,
        total_runtime_ms,
        cache_hit_ratio: record.cache_hit_ratio,
        candidate_slots: record.candidate_slots,
        scored_candidates: record.scored_candidates,
        exact_id_hits: record.exact_id_hits,
        auto_acquire_requests: record.auto_acquire_requests,
        auto_acquire_missing_requests: record.auto_acquire_missing_requests,
        auto_acquire_cutoff_requests: record.auto_acquire_cutoff_requests,
        auto_acquire_dry_run_hits: record.auto_acquire_dry_run_hits,
        auto_acquire_submitted: record.auto_acquire_submitted,
        auto_acquire_no_result: record.auto_acquire_no_result,
        auto_acquire_blocked: record.auto_acquire_blocked,
        auto_acquire_failed: record.auto_acquire_failed,
        auto_acquire_completed_linked: record.auto_acquire_completed_linked,
        auto_acquire_completed_unlinked: record.auto_acquire_completed_unlinked,
        auto_acquire_successes,
    }
}

/// GET /api/v1/scan/history
pub(super) async fn api_get_scan_history(
    State(state): State<WebState>,
    Query(query): Query<ApiScanHistoryQuery>,
) -> Json<Vec<ApiScanHistoryEntry>> {
    let limit = scan_history_query_limit(&query);
    let fetch_limit = scan_history_fetch_limit(&query);

    let history = state
        .database
        .get_scan_history(fetch_limit)
        .await
        .unwrap_or_default();

    let items = history
        .into_iter()
        .filter(|record| scan_history_matches_query(record, &query))
        .take(limit as usize)
        .map(scan_history_entry_from_record)
        .collect();

    Json(items)
}

/// GET /api/v1/scan/:id
pub(super) async fn api_get_scan_run(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> Result<Json<ApiScanRunDetail>, (StatusCode, Json<ApiErrorResponse>)> {
    match state.database.get_scan_run(id).await {
        Ok(Some(run)) => {
            let skip_event_samples = match run.run_token.as_deref() {
                Some(token) => state
                    .database
                    .get_skip_link_events_for_run_token(token, 25)
                    .await
                    .map(api_skip_event_samples)
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(ApiErrorResponse {
                                error: format!("Failed to load scan run {} skip events: {}", id, e),
                            }),
                        )
                    })?,
                None => Vec::new(),
            };

            Ok(Json(scan_run_detail_from_record(run, skip_event_samples)))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiErrorResponse {
                error: format!("Scan run {} not found", id),
            }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to load scan run {}: {}", id, e),
            }),
        )),
    }
}
