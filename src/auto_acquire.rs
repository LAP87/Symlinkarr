use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use regex::Regex;
use tokio::time::sleep;
use tracing::{debug, info, warn};

#[cfg(test)]
use dmm::rank_dmm_movie_results;
use dmm::{fetch_dmm_by_kind, search_dmm_candidates};
use queue::{load_persistent_queue, persist_terminal_outcome, submit_request};

use crate::api::decypharr::{DecypharrClient, DecypharrTorrent};
use crate::api::dmm::{
    DmmClient, DmmMediaKind, DmmTitleCandidate, DmmTorrentLookup, DmmTorrentResult,
};
use crate::api::prowlarr::{ProwlarrClient, ProwlarrResult};
use crate::config::Config;
use crate::db::{
    AcquisitionJobRecord, AcquisitionJobSeed, AcquisitionJobStatus, AcquisitionJobUpdate,
    AcquisitionRelinkKind, Database,
};
use crate::source_scanner::{ParserKind, SourceScanner};
use crate::utils::{user_println, ProgressLine};

const MAX_CANDIDATES: usize = 5;
const BLOCKED_RETRY_MINUTES: i64 = 10;
const NO_RESULT_RETRY_HOURS: i64 = 6;

/// Exponential backoff for Failed retries: 30 * 3^(attempts-1), capped at 180 minutes.
fn failed_retry_minutes(attempts: i64) -> i64 {
    let exp = (attempts - 1).max(0) as u32;
    let base: i64 = 30_i64.saturating_mul(3_i64.saturating_pow(exp));
    base.min(180)
}

/// Exponential backoff for CompletedUnlinked retries: 5 * 3^(attempts-1), capped at 120 minutes.
fn completed_unlinked_retry_minutes(attempts: i64) -> i64 {
    let exp = (attempts - 1).max(0) as u32;
    let base: i64 = 5_i64.saturating_mul(3_i64.saturating_pow(exp));
    base.min(120)
}

#[derive(Debug, Clone)]
pub enum RelinkCheck {
    MediaId(String),
    MediaEpisode {
        media_id: String,
        season: u32,
        episode: u32,
    },
    SymlinkPath(PathBuf),
}

#[derive(Debug, Clone)]
pub struct AutoAcquireRequest {
    pub label: String,
    pub query: String,
    pub query_hints: Vec<String>,
    pub imdb_id: Option<String>,
    pub categories: Vec<i32>,
    pub arr: String,
    pub library_filter: Option<String>,
    pub relink_check: RelinkCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoAcquireStatus {
    DryRun,
    NoResult,
    Blocked,
    CompletedLinked,
    CompletedUnlinked,
    Failed,
}

#[derive(Debug, Clone)]
pub struct AutoAcquireOutcome {
    pub status: AutoAcquireStatus,
    pub reason_code: &'static str,
    pub release_title: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct AutoAcquireBatchSummary {
    pub total: usize,
    pub submitted: usize,
    pub dry_run: usize,
    pub no_result: usize,
    pub blocked: usize,
    pub failed: usize,
    pub completed_linked: usize,
    pub completed_unlinked: usize,
    pub deferred_capacity: usize,
    pub reason_counts: BTreeMap<String, u64>,
}

fn increment_reason_count(reason_counts: &mut BTreeMap<String, u64>, reason_code: &str) {
    *reason_counts.entry(reason_code.to_string()).or_insert(0) += 1;
}

impl AutoAcquireBatchSummary {
    pub fn handled(&self) -> usize {
        self.dry_run
            + self.no_result
            + self.blocked
            + self.failed
            + self.completed_linked
            + self.completed_unlinked
    }
}

#[derive(Debug)]
enum SubmitAttempt {
    Immediate(AutoAcquireOutcome),
    Deferred { reason: String },
    Submitted(Box<SubmittedAcquire>),
}

#[derive(Debug, Clone, Copy)]
enum QueueGuard {
    Capacity,
    Failing,
}

#[derive(Debug, Clone)]
struct QueuedAcquire {
    job_id: i64,
    attempts: i64,
    request: AutoAcquireRequest,
}

#[derive(Debug, Clone)]
struct SubmittedAcquire {
    job_id: i64,
    attempts: i64,
    request: AutoAcquireRequest,
    arr: String,
    release_title: String,
    tracker: TorrentTracker,
    submitted_at: DateTime<Utc>,
    reused_existing: bool,
}

#[derive(Debug, Clone)]
struct RelinkPendingAcquire {
    submitted: SubmittedAcquire,
    completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct AnimeRequestContext {
    desired_season: u32,
    desired_episode: u32,
    query_season: Option<u32>,
    query_episode: Option<u32>,
    absolute_query_episode: Option<u32>,
    acceptable_episode_slots: Vec<(u32, u32)>,
    title_tokens: Vec<String>,
    upgrade: bool,
}

#[derive(Debug, Clone)]
struct DownloadCandidate {
    title: String,
    url: String,
    info_hash: Option<String>,
}

#[derive(Debug)]
enum CandidateLookup {
    Hits {
        query: String,
        source: &'static str,
        candidates: Vec<DownloadCandidate>,
    },
    Pending(String),
    Empty,
}

#[derive(Debug, Clone)]
struct DmmLookupPlan {
    kind: DmmMediaKind,
    season: Option<u32>,
    search_queries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DmmLookupCacheKey {
    kind: DmmMediaKind,
    imdb_id: String,
    season: Option<u32>,
}

#[derive(Debug, Default)]
struct DmmSearchSession {
    imdb_lookups: HashMap<DmmLookupCacheKey, DmmTorrentLookup>,
}

impl DmmSearchSession {
    fn cache_key(
        kind: DmmMediaKind,
        imdb_id: &str,
        season: Option<u32>,
    ) -> Option<DmmLookupCacheKey> {
        match kind {
            DmmMediaKind::Movie => Some(DmmLookupCacheKey {
                kind,
                imdb_id: imdb_id.to_string(),
                season: None,
            }),
            DmmMediaKind::Show => Some(DmmLookupCacheKey {
                kind,
                imdb_id: imdb_id.to_string(),
                season: Some(season?),
            }),
        }
    }

    fn get_cached_lookup(
        &self,
        kind: DmmMediaKind,
        imdb_id: &str,
        season: Option<u32>,
    ) -> Option<DmmTorrentLookup> {
        let key = Self::cache_key(kind, imdb_id, season)?;
        self.imdb_lookups.get(&key).cloned()
    }

    fn cache_lookup(
        &mut self,
        kind: DmmMediaKind,
        imdb_id: &str,
        season: Option<u32>,
        lookup: DmmTorrentLookup,
    ) {
        let Some(key) = Self::cache_key(kind, imdb_id, season) else {
            return;
        };
        self.imdb_lookups.insert(key, lookup);
    }

    async fn fetch_lookup(
        &mut self,
        dmm: &DmmClient,
        kind: DmmMediaKind,
        imdb_id: &str,
        season: Option<u32>,
    ) -> Result<Option<DmmTorrentLookup>> {
        if let Some(cached) = self.get_cached_lookup(kind, imdb_id, season) {
            debug!(
                "DMM: reusing cached lookup for imdb={} kind={:?} season={:?}",
                imdb_id, kind, season
            );
            return Ok(Some(cached));
        }

        let lookup = fetch_dmm_by_kind(dmm, kind, imdb_id, season).await?;
        if let Some(ref cachedable) = lookup {
            self.cache_lookup(kind, imdb_id, season, cachedable.clone());
        }
        Ok(lookup)
    }
}

pub async fn process_auto_acquire_queue(
    cfg: &Config,
    db: &Database,
    requests: Vec<AutoAcquireRequest>,
    dry_run: bool,
) -> Result<AutoAcquireBatchSummary> {
    let decypharr = DecypharrClient::from_config(&cfg.decypharr);
    let dmm = cfg.has_dmm().then(|| DmmClient::from_config(&cfg.dmm));
    let mut dmm_session = DmmSearchSession::default();
    let poll_interval = Duration::from_secs(cfg.decypharr.poll_interval_seconds.max(1));
    let (mut pending, mut downloading, mut relinking) = if dry_run {
        (
            requests
                .into_iter()
                .map(|request| QueuedAcquire {
                    job_id: 0,
                    attempts: 0,
                    request,
                })
                .collect::<VecDeque<_>>(),
            Vec::new(),
            Vec::new(),
        )
    } else {
        load_persistent_queue(db, requests).await?
    };
    let mut summary = AutoAcquireBatchSummary {
        total: pending.len() + downloading.len() + relinking.len(),
        ..AutoAcquireBatchSummary::default()
    };

    if summary.total == 0 {
        return Ok(summary);
    }

    user_println(format!(
        "   🧾 Auto-acquire queue initialized: {} active job(s), max_in_flight={}",
        summary.total, cfg.decypharr.max_in_flight
    ));
    let mut progress = ProgressLine::new("Queue status:");

    while !pending.is_empty() || !downloading.is_empty() || !relinking.is_empty() {
        let mut deferred_capacity = false;

        while downloading.len() < cfg.decypharr.max_in_flight && !pending.is_empty() {
            let queued = pending.pop_front().unwrap();
            let submit_attempt = match submit_request(
                cfg,
                &decypharr,
                dmm.as_ref(),
                &mut dmm_session,
                &queued.request,
                dry_run,
            )
            .await
            {
                Ok(attempt) => attempt,
                Err(err) => {
                    let outcome = request_error_outcome(err);
                    if !dry_run {
                        persist_terminal_outcome(db, queued.job_id, queued.attempts, &outcome)
                            .await?;
                    }
                    print_terminal_outcome(&queued.request, &outcome);
                    record_terminal_outcome(&mut summary, &outcome);
                    continue;
                }
            };
            match submit_attempt {
                SubmitAttempt::Immediate(outcome) => {
                    if !dry_run {
                        persist_terminal_outcome(db, queued.job_id, queued.attempts, &outcome)
                            .await?;
                    }
                    print_terminal_outcome(&queued.request, &outcome);
                    record_terminal_outcome(&mut summary, &outcome);
                }
                SubmitAttempt::Deferred { reason } => {
                    if !dry_run {
                        db.update_acquisition_job_state(
                            queued.job_id,
                            &AcquisitionJobUpdate {
                                status: AcquisitionJobStatus::Queued,
                                release_title: None,
                                info_hash: None,
                                error: Some(reason.clone()),
                                next_retry_at: Some(
                                    Utc::now()
                                        + ChronoDuration::seconds(poll_interval.as_secs() as i64),
                                ),
                                submitted_at: None,
                                completed_at: None,
                                increment_attempts: false,
                            },
                        )
                        .await?;
                    }
                    pending.push_front(queued);
                    summary.deferred_capacity += 1;
                    increment_reason_count(
                        &mut summary.reason_counts,
                        "auto_acquire_queue_capacity_deferred",
                    );
                    deferred_capacity = true;
                    user_println(format!("      ⏳ Queue paused: {}", reason));
                    break;
                }
                SubmitAttempt::Submitted(mut submitted) => {
                    if !dry_run {
                        submitted.job_id = queued.job_id;
                        submitted.attempts = queued.attempts;
                        db.update_acquisition_job_state(
                            submitted.job_id,
                            &AcquisitionJobUpdate {
                                status: AcquisitionJobStatus::Downloading,
                                release_title: Some(submitted.release_title.clone()),
                                info_hash: submitted.tracker.info_hash.clone(),
                                error: None,
                                next_retry_at: None,
                                submitted_at: Some(submitted.submitted_at),
                                completed_at: None,
                                increment_attempts: !submitted.reused_existing,
                            },
                        )
                        .await?;
                    }
                    if submitted.reused_existing {
                        user_println(format!(
                            "      ♻ '{}' → already present in Decypharr ({})",
                            submitted.request.query, submitted.release_title
                        ));
                    } else {
                        user_println(format!(
                            "      📥 '{}' → {} queued via Decypharr ({})",
                            submitted.request.query, submitted.release_title, submitted.arr
                        ));
                        summary.submitted += 1;
                    }
                    downloading.push(*submitted);
                }
            }
        }

        if dry_run {
            while let Some(queued) = pending.pop_front() {
                let submit_attempt = match submit_request(
                    cfg,
                    &decypharr,
                    dmm.as_ref(),
                    &mut dmm_session,
                    &queued.request,
                    true,
                )
                .await
                {
                    Ok(attempt) => attempt,
                    Err(err) => {
                        let outcome = request_error_outcome(err);
                        print_terminal_outcome(&queued.request, &outcome);
                        record_terminal_outcome(&mut summary, &outcome);
                        continue;
                    }
                };
                match submit_attempt {
                    SubmitAttempt::Immediate(outcome) => {
                        print_terminal_outcome(&queued.request, &outcome);
                        record_terminal_outcome(&mut summary, &outcome);
                    }
                    SubmitAttempt::Deferred { reason } => {
                        user_println(format!("      ⏳ '{}' → {}", queued.request.query, reason));
                        summary.blocked += 1;
                        increment_reason_count(
                            &mut summary.reason_counts,
                            "auto_acquire_queue_capacity_deferred",
                        );
                    }
                    SubmitAttempt::Submitted(_) => unreachable!("dry-run should not submit"),
                }
            }
            break;
        }

        if !downloading.is_empty() {
            let queue_snapshots = fetch_queue_snapshots(&decypharr, &downloading).await?;
            let mut still_downloading = Vec::new();

            for submitted in downloading.drain(..) {
                match inspect_submitted(cfg, db, &submitted, queue_snapshots.get(&submitted.arr))
                    .await?
                {
                    SubmittedState::Downloading => still_downloading.push(submitted),
                    SubmittedState::Failed(message) => {
                        db.update_acquisition_job_state(
                            submitted.job_id,
                            &AcquisitionJobUpdate {
                                status: AcquisitionJobStatus::Failed,
                                release_title: Some(submitted.release_title.clone()),
                                info_hash: submitted.tracker.info_hash.clone(),
                                error: Some(message.clone()),
                                next_retry_at: Some(
                                    Utc::now()
                                        + ChronoDuration::minutes(failed_retry_minutes(
                                            submitted.attempts,
                                        )),
                                ),
                                submitted_at: Some(submitted.submitted_at),
                                completed_at: None,
                                increment_attempts: false,
                            },
                        )
                        .await?;
                        let outcome = AutoAcquireOutcome {
                            status: AutoAcquireStatus::Failed,
                            reason_code: "auto_acquire_download_failed",
                            release_title: Some(submitted.release_title.clone()),
                            message,
                        };
                        print_terminal_outcome(&submitted.request, &outcome);
                        record_terminal_outcome(&mut summary, &outcome);
                    }
                    SubmittedState::Completed => {
                        let completed_at = Utc::now();
                        db.update_acquisition_job_state(
                            submitted.job_id,
                            &AcquisitionJobUpdate {
                                status: AcquisitionJobStatus::Relinking,
                                release_title: Some(submitted.release_title.clone()),
                                info_hash: submitted.tracker.info_hash.clone(),
                                error: None,
                                next_retry_at: None,
                                submitted_at: Some(submitted.submitted_at),
                                completed_at: Some(completed_at),
                                increment_attempts: false,
                            },
                        )
                        .await?;
                        user_println(format!(
                            "      ✅ '{}' download complete, waiting for relink",
                            submitted.request.query
                        ));
                        relinking.push(RelinkPendingAcquire {
                            submitted,
                            completed_at,
                        });
                    }
                }
            }

            downloading = still_downloading;
        }

        if !relinking.is_empty() {
            run_relink_scans(cfg, db, &relinking).await?;
            let mut still_relinking = Vec::new();

            for pending_link in relinking.drain(..) {
                if relink_satisfied(db, &pending_link.submitted.request.relink_check).await? {
                    db.update_acquisition_job_state(
                        pending_link.submitted.job_id,
                        &AcquisitionJobUpdate {
                            status: AcquisitionJobStatus::CompletedLinked,
                            release_title: Some(pending_link.submitted.release_title.clone()),
                            info_hash: pending_link.submitted.tracker.info_hash.clone(),
                            error: None,
                            next_retry_at: None,
                            submitted_at: Some(pending_link.submitted.submitted_at),
                            completed_at: Some(Utc::now()),
                            increment_attempts: false,
                        },
                    )
                    .await?;
                    let outcome = AutoAcquireOutcome {
                        status: AutoAcquireStatus::CompletedLinked,
                        reason_code: "auto_acquire_completed_linked",
                        release_title: Some(pending_link.submitted.release_title.clone()),
                        message: "download complete and linked".to_string(),
                    };
                    print_terminal_outcome(&pending_link.submitted.request, &outcome);
                    record_terminal_outcome(&mut summary, &outcome);
                    continue;
                }

                if Utc::now() - pending_link.completed_at
                    >= ChronoDuration::minutes(cfg.decypharr.relink_timeout_minutes as i64)
                {
                    let message = "download complete but relink timed out".to_string();
                    db.update_acquisition_job_state(
                        pending_link.submitted.job_id,
                        &AcquisitionJobUpdate {
                            status: AcquisitionJobStatus::CompletedUnlinked,
                            release_title: Some(pending_link.submitted.release_title.clone()),
                            info_hash: pending_link.submitted.tracker.info_hash.clone(),
                            error: Some(message.clone()),
                            next_retry_at: Some(
                                Utc::now()
                                    + ChronoDuration::minutes(completed_unlinked_retry_minutes(
                                        pending_link.submitted.attempts,
                                    )),
                            ),
                            submitted_at: Some(pending_link.submitted.submitted_at),
                            completed_at: Some(pending_link.completed_at),
                            increment_attempts: false,
                        },
                    )
                    .await?;
                    let outcome = AutoAcquireOutcome {
                        status: AutoAcquireStatus::CompletedUnlinked,
                        reason_code: "auto_acquire_relink_timeout",
                        release_title: Some(pending_link.submitted.release_title.clone()),
                        message,
                    };
                    print_terminal_outcome(&pending_link.submitted.request, &outcome);
                    record_terminal_outcome(&mut summary, &outcome);
                    continue;
                }

                still_relinking.push(pending_link);
            }

            relinking = still_relinking;
        }

        print_progress(
            &mut progress,
            &summary,
            pending.len(),
            downloading.len(),
            relinking.len(),
        );

        if pending.is_empty() && downloading.is_empty() && relinking.is_empty() {
            break;
        }

        if deferred_capacity || !downloading.is_empty() || !relinking.is_empty() {
            sleep(poll_interval).await;
        } else if !pending.is_empty() {
            // Avoid a tight loop if requests keep failing immediately.
            sleep(Duration::from_secs(1)).await;
        }
    }

    progress.finish(format!(
        "pending=0, downloading=0, relinking=0, done={}/{}",
        summary.handled(),
        summary.total
    ));

    Ok(summary)
}

async fn search_download_candidates(
    cfg: &Config,
    prowlarr: Option<&ProwlarrClient>,
    dmm: Option<&DmmClient>,
    dmm_session: &mut DmmSearchSession,
    request: &AutoAcquireRequest,
    candidate_queries: &[String],
) -> Result<CandidateLookup> {
    if let Some(prowlarr) = prowlarr {
        for query in candidate_queries {
            let hits = prowlarr.search_ranked(query, &request.categories).await?;
            let hits = rank_candidate_hits(request, query, hits);
            let candidates = hits
                .into_iter()
                .filter_map(|hit| {
                    let url = hit.best_url()?.to_string();
                    let info_hash = hit
                        .magnet_url
                        .as_deref()
                        .and_then(extract_btih)
                        .map(|hash| hash.to_ascii_lowercase());
                    Some(DownloadCandidate {
                        title: hit.title,
                        url,
                        info_hash,
                    })
                })
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return Ok(CandidateLookup::Hits {
                    query: query.clone(),
                    source: "Prowlarr",
                    candidates,
                });
            }
        }
    }

    let Some(dmm) = dmm else {
        if !cfg.has_dmm() {
            return Ok(CandidateLookup::Empty);
        }
        return Ok(CandidateLookup::Empty);
    };

    search_dmm_candidates(cfg, dmm, dmm_session, request).await
}

fn size_score(file_size: i64) -> i64 {
    match file_size {
        size if size >= 50 * 1024 * 1024 * 1024 => 160,
        size if size >= 20 * 1024 * 1024 * 1024 => 90,
        size if size >= 8 * 1024 * 1024 * 1024 => 45,
        size if size >= 2 * 1024 * 1024 * 1024 => 20,
        _ => 0,
    }
}

fn build_candidate_queries(request: &AutoAcquireRequest) -> Vec<String> {
    let mut queries = Vec::new();
    push_candidate_query(&mut queries, request.query.trim());
    for hint in &request.query_hints {
        push_candidate_query(&mut queries, hint);
    }

    let cleaned_label = clean_request_label(&request.label);
    push_candidate_query(&mut queries, &cleaned_label);

    let label_without_year = strip_year_tokens(&cleaned_label);
    push_candidate_query(&mut queries, &label_without_year);

    let query_without_year = strip_year_tokens(&request.query);
    push_candidate_query(&mut queries, &query_without_year);
    for hint in &request.query_hints {
        let hint_without_year = strip_year_tokens(hint);
        push_candidate_query(&mut queries, &hint_without_year);
    }

    if normalize_arr_name(&request.arr) == "sonarranime" {
        for fallback in anime_batch_fallbacks(&cleaned_label) {
            push_candidate_query(&mut queries, &fallback);
        }
        for fallback in anime_batch_fallbacks(&label_without_year) {
            push_candidate_query(&mut queries, &fallback);
        }
        for fallback in anime_batch_fallbacks(&request.query) {
            push_candidate_query(&mut queries, &fallback);
        }
        for fallback in anime_batch_fallbacks(&query_without_year) {
            push_candidate_query(&mut queries, &fallback);
        }
        for hint in &request.query_hints {
            for fallback in anime_batch_fallbacks(hint) {
                push_candidate_query(&mut queries, &fallback);
            }
        }
    }

    queries
}

fn push_candidate_query(queries: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return;
    }

    let normalized = crate::utils::normalize(trimmed);
    if normalized.is_empty()
        || queries
            .iter()
            .any(|existing| crate::utils::normalize(existing) == normalized)
    {
        return;
    }

    queries.push(trimmed.to_string());
}

fn strip_year_tokens(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !is_year_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_request_label(label: &str) -> String {
    let mut cleaned = label.trim().to_string();
    let suffixes = [" upgrade (unlinked)", " (unlinked)", " upgrade", " (new)"];

    loop {
        let mut changed = false;
        for suffix in suffixes {
            if cleaned.ends_with(suffix) {
                cleaned.truncate(cleaned.len() - suffix.len());
                cleaned = cleaned.trim().to_string();
                changed = true;
            }
        }

        if !changed {
            return cleaned;
        }
    }
}

fn is_year_token(token: &str) -> bool {
    let trimmed = token
        .trim_matches(|ch: char| matches!(ch, '(' | ')' | '[' | ']' | '{' | '}'))
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
    trimmed.len() == 4
        && trimmed.chars().all(|ch| ch.is_ascii_digit())
        && trimmed
            .parse::<u32>()
            .map(|year| (1900..=2035).contains(&year))
            .unwrap_or(false)
}

fn anime_batch_fallbacks(query: &str) -> Vec<String> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.len() < 2 {
        return Vec::new();
    }

    let mut fallbacks = Vec::new();
    let last = tokens.last().copied().unwrap_or_default();
    if let Some(season) = parse_season_token(last) {
        let title = tokens[..tokens.len() - 1].join(" ");
        if !title.is_empty() {
            fallbacks.push(format!("{} S{:02}", title, season));
            fallbacks.push(title);
        }
    } else if is_episode_number_token(last) {
        let title = tokens[..tokens.len() - 1].join(" ");
        if !title.is_empty() {
            fallbacks.push(title);
        }
    }

    fallbacks
}

fn rank_candidate_hits(
    request: &AutoAcquireRequest,
    search_query: &str,
    hits: Vec<ProwlarrResult>,
) -> Vec<ProwlarrResult> {
    if normalize_arr_name(&request.arr) != "sonarranime" {
        return hits;
    }

    let Some(context) = build_anime_request_context(request) else {
        return hits;
    };
    let scanner = SourceScanner::new();
    let query_is_specific = query_has_specific_numbering(search_query);
    let mut scored_hits = hits
        .into_iter()
        .filter_map(|hit| {
            anime_hit_score(&context, &scanner, search_query, &hit).map(|score| (score, hit))
        })
        .collect::<Vec<_>>();

    if scored_hits.is_empty() {
        debug!(
            "Auto-acquire: anime ranking rejected all Prowlarr hits for '{}'",
            search_query
        );
        return Vec::new();
    }

    scored_hits.sort_by(|(score_a, hit_a), (score_b, hit_b)| {
        score_b
            .cmp(score_a)
            .then_with(|| hit_b.seeders.unwrap_or(0).cmp(&hit_a.seeders.unwrap_or(0)))
            .then_with(|| hit_b.size.cmp(&hit_a.size))
    });

    if query_is_specific {
        return scored_hits.into_iter().map(|(_, hit)| hit).collect();
    }

    scored_hits
        .into_iter()
        .filter(|(score, _)| *score >= 1_000)
        .map(|(_, hit)| hit)
        .collect()
}

fn build_anime_request_context(request: &AutoAcquireRequest) -> Option<AnimeRequestContext> {
    let RelinkCheck::MediaEpisode {
        season, episode, ..
    } = &request.relink_check
    else {
        return None;
    };

    let scanner = SourceScanner::new();
    let query_variants = scanner.parse_release_title_variants(&request.query);
    let mut query_season = None;
    let mut query_episode = None;
    let mut absolute_query_episode = None;
    let mut acceptable_episode_slots = Vec::new();
    push_episode_slot(&mut acceptable_episode_slots, (*season, *episode));

    for (kind, parsed) in query_variants {
        match (parsed.season, parsed.episode, kind) {
            (Some(parsed_season), Some(parsed_episode), _) => {
                query_season = Some(parsed_season);
                query_episode = Some(parsed_episode);
                push_episode_slot(
                    &mut acceptable_episode_slots,
                    (parsed_season, parsed_episode),
                );
            }
            (None, Some(parsed_episode), ParserKind::Anime) => {
                // Episode-only hint (absolute numbering, no season detected).
                // If we already have a query_episode from another source, keep it;
                // otherwise record this as the episode number for the current season.
                if query_episode.is_none() {
                    query_episode = Some(parsed_episode);
                }
                absolute_query_episode = Some(parsed_episode);
            }
            _ => {}
        }
    }

    for hint in &request.query_hints {
        for (kind, parsed) in scanner.parse_release_title_variants(hint) {
            match (parsed.season, parsed.episode, kind) {
                (Some(parsed_season), Some(parsed_episode), _) => {
                    push_episode_slot(
                        &mut acceptable_episode_slots,
                        (parsed_season, parsed_episode),
                    );
                }
                (None, Some(parsed_episode), ParserKind::Anime) => {
                    // Episode-only hint from a hint string.
                    // Record the episode number so acceptable slots include it.
                    if query_episode.is_none() {
                        query_episode = Some(parsed_episode);
                    }
                    if absolute_query_episode.is_none() {
                        absolute_query_episode = Some(parsed_episode);
                    }
                }
                _ => {}
            }
        }
    }

    if absolute_query_episode.is_none() {
        for candidate in std::iter::once(request.query.as_str())
            .chain(request.query_hints.iter().map(String::as_str))
        {
            if let Some(last) = candidate.split_whitespace().last() {
                if is_episode_number_token(last) {
                    absolute_query_episode = last.parse().ok();
                    if absolute_query_episode.is_some() {
                        break;
                    }
                }
            }
        }
    }

    Some(AnimeRequestContext {
        desired_season: *season,
        desired_episode: *episode,
        query_season,
        query_episode,
        absolute_query_episode,
        acceptable_episode_slots,
        title_tokens: request_title_tokens(&scanner, request),
        upgrade: request.label.contains("upgrade"),
    })
}

fn push_episode_slot(slots: &mut Vec<(u32, u32)>, slot: (u32, u32)) {
    if slot.0 == 0 && slot.1 == 0 {
        return;
    }

    if !slots.contains(&slot) {
        slots.push(slot);
    }
}

fn request_title_tokens(scanner: &SourceScanner, request: &AutoAcquireRequest) -> Vec<String> {
    let mut best_tokens = Vec::new();

    for candidate in std::iter::once(clean_request_label(&request.label))
        .chain(std::iter::once(request.query.clone()))
        .chain(request.query_hints.iter().cloned())
    {
        for (_, parsed) in scanner.parse_release_title_variants(&candidate) {
            let tokens = normalized_tokens(&parsed.parsed_title);
            if tokens.len() > best_tokens.len() {
                best_tokens = tokens;
            }
        }
    }

    if !best_tokens.is_empty() {
        return best_tokens;
    }

    let cleaned_label = clean_request_label(&request.label);
    strip_year_tokens(&cleaned_label)
        .split_whitespace()
        .filter(|token| !is_numbering_token(token))
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn is_numbering_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    parse_season_token(&lower).is_some()
        || is_episode_number_token(&lower)
        || is_year_token(&lower)
        || matches!(lower.as_str(), "upgrade" | "new" | "unlinked")
        || matches!(lower.as_str(), "bd" | "bdrip" | "brrip" | "hdrip" | "dvdr")
}

fn anime_hit_score(
    context: &AnimeRequestContext,
    scanner: &SourceScanner,
    search_query: &str,
    hit: &ProwlarrResult,
) -> Option<i64> {
    let hit_tokens_vec = normalized_tokens(&hit.title);
    let hit_tokens: HashSet<_> = hit_tokens_vec.iter().map(String::as_str).collect();
    let title_matches = context
        .title_tokens
        .iter()
        .filter(|token| hit_tokens.contains(token.as_str()))
        .count() as i64;

    let mut best_score = None::<i64>;
    for (_, parsed) in scanner.parse_release_title_variants(&hit.title) {
        if let Some(score) = anime_parsed_variant_score(context, &parsed) {
            best_score = Some(best_score.map_or(score, |best| best.max(score)));
        }
    }

    if let Some(score) = anime_pack_score(context, &hit.title, title_matches) {
        best_score = Some(best_score.map_or(score, |best| best.max(score)));
    }

    if best_score.is_none() {
        let exact_episode_token = format!(
            "s{:02}e{:02}",
            context.desired_season, context.desired_episode
        );
        if hit_tokens.contains(exact_episode_token.as_str()) {
            best_score = Some(2_350);
        }
    }

    if best_score.is_none() {
        if let Some(absolute_episode) = context.absolute_query_episode {
            let absolute_token = absolute_episode.to_string();
            if hit_tokens.contains(absolute_token.as_str()) && title_matches > 0 {
                best_score = Some(2_150);
            }
        }
    }

    let Some(mut score) = best_score else {
        if query_has_specific_numbering(search_query) {
            return None;
        }
        return None;
    };

    if title_matches > 0 {
        score += title_matches * 40;
    }

    if let Some(seeders) = hit.seeders {
        score += i64::from(seeders.clamp(0, 200));
    }

    score += match hit.size {
        size if size >= 20 * 1024 * 1024 * 1024 => 60,
        size if size >= 8 * 1024 * 1024 * 1024 => 35,
        size if size >= 2 * 1024 * 1024 * 1024 => 15,
        _ => 0,
    };

    Some(score)
}

fn anime_parsed_variant_score(
    context: &AnimeRequestContext,
    parsed: &crate::models::SourceItem,
) -> Option<i64> {
    let quality_bonus = anime_quality_bonus(parsed.quality.as_deref(), context.upgrade);

    if let (Some(season), Some(episode)) = (parsed.season, parsed.episode) {
        if season == context.desired_season && episode == context.desired_episode {
            return Some(2_500 + quality_bonus);
        }
        if context
            .acceptable_episode_slots
            .contains(&(season, episode))
        {
            return Some(2_460 + quality_bonus);
        }
        return None;
    }

    let absolute_episode = parsed.episode?;
    if Some(absolute_episode) == context.absolute_query_episode {
        return Some(2_420 + quality_bonus);
    }

    if context.desired_season == 1
        && context.query_season == Some(1)
        && context.query_episode == Some(context.desired_episode)
        && absolute_episode == context.desired_episode
    {
        return Some(2_200 + quality_bonus);
    }

    None
}

fn anime_quality_bonus(quality: Option<&str>, upgrade: bool) -> i64 {
    let Some(quality) = quality.map(|value| value.to_ascii_lowercase()) else {
        return 0;
    };

    let bonus = match quality.as_str() {
        "2160p" | "4k" => 140,
        "1080p" => 90,
        "720p" => 40,
        _ => 0,
    };

    if upgrade {
        bonus
    } else {
        bonus / 2
    }
}

fn anime_pack_score(context: &AnimeRequestContext, title: &str, title_matches: i64) -> Option<i64> {
    let normalized = crate::utils::normalize(title);
    let tokens: HashSet<_> = normalized.split_whitespace().collect();
    let desired_number = context
        .absolute_query_episode
        .unwrap_or(context.desired_episode);

    let matches_desired_season = season_token_matches(&tokens, &normalized, context.desired_season);
    if has_conflicting_explicit_season(&tokens, &normalized, context.desired_season) {
        return None;
    }

    let contains_desired_range = episode_ranges(title)
        .into_iter()
        .any(|(start, end)| (start..=end).contains(&desired_number));
    let complete = contains_complete_marker(&tokens);

    if matches_desired_season && (contains_desired_range || complete) {
        return Some(1_520 + if context.upgrade { 80 } else { 0 });
    }

    if !matches_desired_season
        && context.desired_season == 1
        && (contains_desired_range || complete)
    {
        return Some(1_240 + if context.upgrade { 60 } else { 0 });
    }

    if !matches_desired_season
        && context.absolute_query_episode.is_some()
        && contains_desired_range
        && title_matches >= minimum_pack_title_matches(context)
    {
        return Some(1_320 + if context.upgrade { 70 } else { 0 });
    }

    None
}

fn has_conflicting_explicit_season(
    tokens: &HashSet<&str>,
    normalized_title: &str,
    desired_season: u32,
) -> bool {
    (1..=100).any(|season| {
        season != desired_season && season_token_matches(tokens, normalized_title, season)
    })
}

fn minimum_pack_title_matches(context: &AnimeRequestContext) -> i64 {
    let _ = context;
    1
}

fn season_token_matches(tokens: &HashSet<&str>, normalized_title: &str, season: u32) -> bool {
    let compact = format!("s{}", season);
    let padded = format!("s{:02}", season);
    let title_tokens: Vec<&str> = normalized_title.split_whitespace().collect();
    tokens.contains(compact.as_str())
        || tokens.contains(padded.as_str())
        || has_token_phrase(&title_tokens, "season", &season.to_string())
        || has_token_phrase(&title_tokens, &ordinal_number(season), "season")
        || ordinal_word(season)
            .map(|word| has_token_phrase(&title_tokens, word, "season"))
            .unwrap_or(false)
}

fn has_token_phrase(tokens: &[&str], first: &str, second: &str) -> bool {
    tokens
        .windows(2)
        .any(|window| window[0] == first && window[1] == second)
}

fn ordinal_number(value: u32) -> String {
    let suffix = match value % 100 {
        11..=13 => "th",
        _ => match value % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{}{}", value, suffix)
}

fn ordinal_word(value: u32) -> Option<&'static str> {
    match value {
        1 => Some("first"),
        2 => Some("second"),
        3 => Some("third"),
        4 => Some("fourth"),
        5 => Some("fifth"),
        6 => Some("sixth"),
        7 => Some("seventh"),
        8 => Some("eighth"),
        9 => Some("ninth"),
        10 => Some("tenth"),
        _ => None,
    }
}

fn contains_complete_marker(tokens: &HashSet<&str>) -> bool {
    tokens.contains("complete") || tokens.contains("batch") || tokens.contains("end")
}

fn episode_ranges(title: &str) -> Vec<(u32, u32)> {
    static RANGE_REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = RANGE_REGEX.get_or_init(|| {
        Regex::new(r"(?i)(\d{1,3})\s*[-~]\s*(\d{1,3})(?:\s*(?:v\d+|end))?")
            .expect("valid episode range regex")
    });

    regex
        .captures_iter(title)
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            if title[..whole.start()]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_ascii_digit())
            {
                return None;
            }
            if title[whole.end()..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit())
            {
                return None;
            }

            let start = caps.get(1)?.as_str().parse::<u32>().ok()?;
            let end = caps.get(2)?.as_str().parse::<u32>().ok()?;
            if start == 0 || end == 0 || start > end || end > 400 {
                return None;
            }
            Some((start, end))
        })
        .collect()
}

fn query_has_specific_numbering(query: &str) -> bool {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if let Some(last) = tokens.last().copied() {
        if parse_season_token(last).is_some() || is_episode_number_token(last) {
            return true;
        }
    }

    let scanner = SourceScanner::new();
    scanner
        .parse_release_title_variants(query)
        .into_iter()
        .any(|(_, parsed)| parsed.episode.is_some())
}

fn parse_season_token(token: &str) -> Option<u32> {
    let lower = token.to_ascii_lowercase();
    let (season, episode) = lower.split_once('e')?;
    let season = season.strip_prefix('s')?;
    if season.is_empty()
        || episode.is_empty()
        || !season.chars().all(|ch| ch.is_ascii_digit())
        || !episode.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    season.parse().ok()
}

fn is_episode_number_token(token: &str) -> bool {
    !is_year_token(token)
        && token.len() <= 4
        && token.chars().all(|ch| ch.is_ascii_digit())
        && token.parse::<u32>().map(|value| value > 0).unwrap_or(false)
}

async fn resolve_arr_name(decypharr: &DecypharrClient, requested: &str) -> Result<String> {
    let arrs = decypharr.get_arrs().await?;
    if arrs.is_empty() {
        anyhow::bail!("Decypharr reports no Arr instances; cannot route auto-acquire");
    }

    if let Some(arr) = arrs
        .iter()
        .find(|arr| normalize_arr_name(&arr.name) == normalize_arr_name(requested))
    {
        return Ok(arr.name.clone());
    }

    let available = arrs
        .iter()
        .map(|arr| arr.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "Decypharr Arr '{}' not found. Available: {}",
        requested,
        available
    );
}

fn queue_block_reason(
    torrents: &[DecypharrTorrent],
    max_in_flight: usize,
) -> Option<(QueueGuard, String)> {
    let failing: Vec<String> = torrents
        .iter()
        .filter(|torrent| torrent.is_failed() && !torrent.is_complete)
        .take(3)
        .map(|torrent| {
            let reason = torrent.failure_reason().unwrap_or("unknown error");
            format!("{} ({})", torrent.name, reason)
        })
        .collect();
    if !failing.is_empty() {
        return Some((
            QueueGuard::Failing,
            format!(
                "Decypharr has failing torrents in this category: {}. Clean them up in DMM/Decypharr before auto-acquire continues.",
                failing.join(", ")
            ),
        ));
    }

    let in_flight = torrents
        .iter()
        .filter(|torrent| !torrent.is_complete && !torrent.is_failed())
        .count();
    if in_flight >= max_in_flight {
        return Some((
            QueueGuard::Capacity,
            format!(
                "Decypharr already has {} in-flight torrent(s) in this category (limit {}). Waiting avoids filling the RD queue.",
                in_flight, max_in_flight
            ),
        ));
    }

    None
}

async fn fetch_queue_snapshots(
    decypharr: &DecypharrClient,
    downloading: &[SubmittedAcquire],
) -> Result<HashMap<String, Vec<DecypharrTorrent>>> {
    let mut snapshots = HashMap::new();
    let unique_arrs: HashSet<_> = downloading.iter().map(|item| item.arr.as_str()).collect();
    for arr in unique_arrs {
        snapshots.insert(
            arr.to_string(),
            decypharr.list_torrents(Some(arr), None).await?,
        );
    }
    Ok(snapshots)
}

enum SubmittedState {
    Downloading,
    Completed,
    Failed(String),
}

async fn inspect_submitted(
    cfg: &Config,
    db: &Database,
    submitted: &SubmittedAcquire,
    queue: Option<&Vec<DecypharrTorrent>>,
) -> Result<SubmittedState> {
    let queue = queue.map(Vec::as_slice).unwrap_or(&[]);

    if let Some(torrent) = find_matching_torrent(queue, &submitted.tracker) {
        let progress = format!(
            "{} / state={} / progress={:.0}%",
            torrent.name, torrent.state, torrent.progress
        );
        debug!("Decypharr progress: {}", progress);

        if torrent.is_complete {
            return Ok(SubmittedState::Completed);
        }
        if let Some(reason) = torrent.failure_reason() {
            return Ok(SubmittedState::Failed(format!(
                "Decypharr torrent '{}' failed: {}. Check DMM/Decypharr queue cleanup.",
                torrent.name, reason
            )));
        }

        return Ok(SubmittedState::Downloading);
    }

    // Torrent no longer in Decypharr queue — it was accepted and confirmed
    // downloading, so Decypharr likely finished and cleaned it up.
    // Verify completion: check RD cache (by info hash) and mount (by name).
    if release_completed(cfg, db, submitted).await {
        info!(
            "Torrent '{}' gone from Decypharr queue but confirmed on RD/mount — completed",
            submitted.release_title
        );
        return Ok(SubmittedState::Completed);
    }

    // Not confirmed anywhere — could be a mount-sync delay or a silent failure.
    if Utc::now() - submitted.submitted_at
        >= ChronoDuration::minutes(cfg.decypharr.completion_timeout_minutes as i64)
    {
        return Ok(SubmittedState::Failed(format!(
            "Timed out waiting {} minute(s) — torrent not in Decypharr queue, RD cache, or mount",
            cfg.decypharr.completion_timeout_minutes
        )));
    }

    debug!(
        "Torrent '{}' not in Decypharr queue and not yet confirmed, waiting...",
        submitted.release_title
    );
    Ok(SubmittedState::Downloading)
}

/// Check whether a release that vanished from Decypharr actually completed.
/// Tries (in order): RD cache hash lookup, then mount directory/file check.
async fn release_completed(cfg: &Config, db: &Database, submitted: &SubmittedAcquire) -> bool {
    // 1. Check RD cache by info hash (most reliable)
    if let Some(hash) = &submitted.tracker.info_hash {
        match db.rd_torrent_downloaded_by_hash(hash).await {
            Ok(true) => {
                debug!("Hash {} found as downloaded in RD cache", hash);
                return true;
            }
            Ok(false) => {}
            Err(e) => warn!("Failed to check RD cache by hash: {}", e),
        }
    }

    // 2. Check mount by release title (directory or file)
    for source in &cfg.sources {
        let candidate = source.path.join(&submitted.release_title);
        if candidate.exists() {
            debug!("Found release on mount: {:?}", candidate);
            return true;
        }
    }

    false
}

async fn run_relink_scans(
    cfg: &Config,
    db: &Database,
    relinking: &[RelinkPendingAcquire],
) -> Result<()> {
    let mut library_filters = Vec::<Option<String>>::new();

    for pending in relinking {
        let filter = pending.submitted.request.library_filter.clone();
        if !library_filters.contains(&filter) {
            library_filters.push(filter);
        }
    }

    for filter in library_filters {
        Box::pin(crate::commands::scan::run_scan(
            cfg,
            db,
            false,
            false,
            crate::OutputFormat::Text,
            filter.as_deref(),
        ))
        .await?;
    }

    Ok(())
}

async fn relink_satisfied(db: &Database, check: &RelinkCheck) -> Result<bool> {
    match check {
        RelinkCheck::MediaId(media_id) => db.has_active_link_for_media(media_id).await,
        RelinkCheck::MediaEpisode {
            media_id,
            season,
            episode,
        } => {
            db.has_active_link_for_episode(media_id, *season, *episode)
                .await
        }
        RelinkCheck::SymlinkPath(path) => Ok(symlink_restored(path)),
    }
}

fn symlink_restored(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    std::fs::read_link(path)
        .map(|target| target.exists())
        .unwrap_or(false)
}

fn print_terminal_outcome(request: &AutoAcquireRequest, outcome: &AutoAcquireOutcome) {
    match outcome.status {
        AutoAcquireStatus::DryRun => user_println(format!(
            "      ✓ '{}' → {}",
            request.query,
            outcome.release_title.as_deref().unwrap_or("preview")
        )),
        AutoAcquireStatus::NoResult => {
            user_println(format!("      ✗ '{}' → {}", request.query, outcome.message));
        }
        AutoAcquireStatus::Blocked | AutoAcquireStatus::Failed => {
            user_println(format!(
                "      ⚠️  '{}' → {}",
                request.query, outcome.message
            ));
        }
        AutoAcquireStatus::CompletedLinked => user_println(format!(
            "      ✅ '{}' → {} ({})",
            request.query,
            outcome
                .release_title
                .as_deref()
                .unwrap_or("Decypharr release"),
            outcome.message
        )),
        AutoAcquireStatus::CompletedUnlinked => user_println(format!(
            "      ⚠️  '{}' → {} ({})",
            request.query,
            outcome
                .release_title
                .as_deref()
                .unwrap_or("Decypharr release"),
            outcome.message
        )),
    }
}

fn record_terminal_outcome(summary: &mut AutoAcquireBatchSummary, outcome: &AutoAcquireOutcome) {
    match outcome.status {
        AutoAcquireStatus::DryRun => summary.dry_run += 1,
        AutoAcquireStatus::NoResult => {
            summary.no_result += 1;
            increment_reason_count(&mut summary.reason_counts, outcome.reason_code);
        }
        AutoAcquireStatus::Blocked => {
            summary.blocked += 1;
            increment_reason_count(&mut summary.reason_counts, outcome.reason_code);
        }
        AutoAcquireStatus::CompletedLinked => summary.completed_linked += 1,
        AutoAcquireStatus::CompletedUnlinked => {
            summary.completed_unlinked += 1;
            increment_reason_count(&mut summary.reason_counts, outcome.reason_code);
        }
        AutoAcquireStatus::Failed => {
            summary.failed += 1;
            increment_reason_count(&mut summary.reason_counts, outcome.reason_code);
        }
    }
}

fn request_error_outcome(err: anyhow::Error) -> AutoAcquireOutcome {
    AutoAcquireOutcome {
        status: AutoAcquireStatus::Failed,
        reason_code: "auto_acquire_internal_error",
        release_title: None,
        message: err.to_string(),
    }
}

fn print_progress(
    progress: &mut ProgressLine,
    summary: &AutoAcquireBatchSummary,
    pending: usize,
    downloading: usize,
    relinking: usize,
) {
    progress.update(format!(
        "pending={}, downloading={}, relinking={}, done={}/{}",
        pending,
        downloading,
        relinking,
        summary.handled(),
        summary.total
    ));
}

#[derive(Debug, Clone)]
struct TorrentTracker {
    category: String,
    info_hash: Option<String>,
    query_tokens: Vec<String>,
    added_after: chrono::DateTime<Utc>,
}

impl TorrentTracker {
    fn from_release(
        category: &str,
        query: &str,
        release_title: &str,
        info_hash: Option<&str>,
    ) -> Self {
        let query_tokens = {
            let tokens = normalized_tokens(release_title);
            if tokens.is_empty() {
                normalized_tokens(query)
            } else {
                tokens
            }
        };

        Self {
            category: category.to_string(),
            info_hash: info_hash.map(|hash| hash.to_ascii_lowercase()),
            query_tokens,
            added_after: Utc::now() - ChronoDuration::seconds(5),
        }
    }

    fn from_record(
        category: &str,
        query: &str,
        release_title: Option<&str>,
        info_hash: Option<&str>,
        submitted_at: DateTime<Utc>,
    ) -> Self {
        let query_tokens = release_title
            .map(normalized_tokens)
            .filter(|tokens| !tokens.is_empty())
            .unwrap_or_else(|| normalized_tokens(query));

        Self {
            category: category.to_string(),
            info_hash: info_hash.map(|hash| hash.to_ascii_lowercase()),
            query_tokens,
            added_after: submitted_at - ChronoDuration::seconds(5),
        }
    }
}

fn find_matching_torrent<'a>(
    torrents: &'a [DecypharrTorrent],
    tracker: &TorrentTracker,
) -> Option<&'a DecypharrTorrent> {
    if let Some(info_hash) = &tracker.info_hash {
        if let Some(hit) = torrents.iter().find(|torrent| {
            torrent.info_hash.eq_ignore_ascii_case(info_hash)
                || torrent
                    .info_hash
                    .to_ascii_lowercase()
                    .contains(info_hash.as_str())
        }) {
            return Some(hit);
        }
    }

    let mut matches: Vec<&DecypharrTorrent> = torrents
        .iter()
        .filter(|torrent| {
            torrent.category.eq_ignore_ascii_case(&tracker.category)
                && torrent
                    .added_on
                    .map(|added_on| added_on >= tracker.added_after)
                    .unwrap_or(true)
                && title_matches_tokens(&torrent.name, &tracker.query_tokens)
        })
        .collect();

    matches.sort_by_key(|torrent| torrent.added_on);
    matches.into_iter().last()
}

fn title_matches_tokens(title: &str, query_tokens: &[String]) -> bool {
    if query_tokens.is_empty() {
        return false;
    }

    let title_tokens: HashSet<_> = normalized_tokens(title).into_iter().collect();
    query_tokens
        .iter()
        .all(|token| title_tokens.contains(token.as_str()))
}

fn normalized_tokens(value: &str) -> Vec<String> {
    crate::utils::normalize(value)
        .split_whitespace()
        .map(|token| token.to_string())
        .collect()
}

fn extract_btih(uri: &str) -> Option<String> {
    let lower = uri.to_ascii_lowercase();
    let marker = "xt=urn:btih:";
    let idx = lower.find(marker)?;
    let hash = &uri[idx + marker.len()..];
    let end = hash.find('&').unwrap_or(hash.len());
    let hash = hash[..end].trim();
    (!hash.is_empty()).then(|| hash.to_string())
}

fn normalize_arr_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn parse_media_episode_value(value: &str) -> Result<(String, u32, u32)> {
    let mut parts = value.split('|');
    let media_id = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing media id in relink value '{}'", value))?
        .to_string();
    let season = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing season in relink value '{}'", value))?
        .parse::<u32>()?;
    let episode = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing episode in relink value '{}'", value))?
        .parse::<u32>()?;
    if parts.next().is_some() {
        anyhow::bail!("unexpected extra data in relink value '{}'", value);
    }
    Ok((media_id, season, episode))
}

mod dmm;
mod queue;

#[cfg(test)]
mod tests;
