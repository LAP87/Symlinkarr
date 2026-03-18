use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use regex::Regex;
use tokio::time::sleep;
use tracing::{debug, info, warn};

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

pub async fn process_auto_acquire_queue(
    cfg: &Config,
    db: &Database,
    requests: Vec<AutoAcquireRequest>,
    dry_run: bool,
) -> Result<AutoAcquireBatchSummary> {
    let decypharr = DecypharrClient::from_config(&cfg.decypharr);
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
            match submit_request(cfg, &decypharr, &queued.request, dry_run).await? {
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
                match submit_request(cfg, &decypharr, &queued.request, true).await? {
                    SubmitAttempt::Immediate(outcome) => {
                        print_terminal_outcome(&queued.request, &outcome);
                        record_terminal_outcome(&mut summary, &outcome);
                    }
                    SubmitAttempt::Deferred { reason } => {
                        user_println(format!("      ⏳ '{}' → {}", queued.request.query, reason));
                        summary.blocked += 1;
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

async fn load_persistent_queue(
    db: &Database,
    requests: Vec<AutoAcquireRequest>,
) -> Result<(
    VecDeque<QueuedAcquire>,
    Vec<SubmittedAcquire>,
    Vec<RelinkPendingAcquire>,
)> {
    let seeds = requests
        .iter()
        .map(request_to_seed)
        .collect::<Result<Vec<_>>>()?;
    if !seeds.is_empty() {
        db.enqueue_acquisition_jobs(&seeds).await?;
    }

    let jobs = db.get_manageable_acquisition_jobs().await?;
    let mut pending = VecDeque::new();
    let mut downloading = Vec::new();
    let mut relinking = Vec::new();

    for job in jobs {
        match job.status {
            AcquisitionJobStatus::Queued
            | AcquisitionJobStatus::Blocked
            | AcquisitionJobStatus::NoResult
            | AcquisitionJobStatus::CompletedUnlinked
            | AcquisitionJobStatus::Failed => pending.push_back(QueuedAcquire {
                job_id: job.id,
                attempts: job.attempts,
                request: job_to_request(&job),
            }),
            AcquisitionJobStatus::Downloading => downloading.push(job_to_submitted(&job)),
            AcquisitionJobStatus::Relinking => relinking.push(job_to_relinking(&job)),
            AcquisitionJobStatus::CompletedLinked => {}
        }
    }

    Ok((pending, downloading, relinking))
}

fn request_to_seed(request: &AutoAcquireRequest) -> Result<AcquisitionJobSeed> {
    let (relink_kind, relink_value) = match &request.relink_check {
        RelinkCheck::MediaId(media_id) => (AcquisitionRelinkKind::MediaId, media_id.clone()),
        RelinkCheck::MediaEpisode {
            media_id,
            season,
            episode,
        } => (
            AcquisitionRelinkKind::MediaEpisode,
            format!("{}|{}|{}", media_id, season, episode),
        ),
        RelinkCheck::SymlinkPath(path) => (
            AcquisitionRelinkKind::SymlinkPath,
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8: {:?}", path))?
                .to_string(),
        ),
    };

    Ok(AcquisitionJobSeed {
        request_key: request_key(&request.relink_check)?,
        label: request.label.clone(),
        query: request.query.clone(),
        imdb_id: request.imdb_id.clone(),
        categories: request.categories.clone(),
        arr: request.arr.clone(),
        library_filter: request.library_filter.clone(),
        relink_kind,
        relink_value,
    })
}

fn request_key(check: &RelinkCheck) -> Result<String> {
    Ok(match check {
        RelinkCheck::MediaId(media_id) => format!("media:{}", media_id),
        RelinkCheck::MediaEpisode {
            media_id,
            season,
            episode,
        } => format!("episode:{}:{}:{}", media_id, season, episode),
        RelinkCheck::SymlinkPath(path) => format!(
            "symlink:{}",
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8: {:?}", path))?
        ),
    })
}

fn job_to_request(job: &AcquisitionJobRecord) -> AutoAcquireRequest {
    let relink_check = match job.relink_kind {
        AcquisitionRelinkKind::MediaId => RelinkCheck::MediaId(job.relink_value.clone()),
        AcquisitionRelinkKind::MediaEpisode => {
            let (media_id, season, episode) = parse_media_episode_value(&job.relink_value)
                .expect("invalid stored media-episode relink value");
            RelinkCheck::MediaEpisode {
                media_id,
                season,
                episode,
            }
        }
        AcquisitionRelinkKind::SymlinkPath => {
            RelinkCheck::SymlinkPath(PathBuf::from(&job.relink_value))
        }
    };

    AutoAcquireRequest {
        label: job.label.clone(),
        query: job.query.clone(),
        imdb_id: job.imdb_id.clone(),
        categories: job.categories.clone(),
        arr: job.arr.clone(),
        library_filter: job.library_filter.clone(),
        relink_check,
    }
}

fn job_to_submitted(job: &AcquisitionJobRecord) -> SubmittedAcquire {
    let submitted_at = job.submitted_at.unwrap_or_else(Utc::now);
    SubmittedAcquire {
        job_id: job.id,
        attempts: job.attempts,
        request: job_to_request(job),
        arr: job.arr.clone(),
        release_title: job
            .release_title
            .clone()
            .unwrap_or_else(|| job.label.clone()),
        tracker: TorrentTracker::from_record(
            &job.arr,
            &job.query,
            job.release_title.as_deref(),
            job.info_hash.as_deref(),
            submitted_at,
        ),
        submitted_at,
        reused_existing: false,
    }
}

fn job_to_relinking(job: &AcquisitionJobRecord) -> RelinkPendingAcquire {
    let submitted = job_to_submitted(job);
    RelinkPendingAcquire {
        submitted,
        completed_at: job.completed_at.unwrap_or_else(Utc::now),
    }
}

async fn persist_terminal_outcome(
    db: &Database,
    job_id: i64,
    attempts: i64,
    outcome: &AutoAcquireOutcome,
) -> Result<()> {
    let now = Utc::now();
    match outcome.status {
        AutoAcquireStatus::DryRun => Ok(()),
        AutoAcquireStatus::NoResult => {
            db.update_acquisition_job_state(
                job_id,
                &AcquisitionJobUpdate {
                    status: AcquisitionJobStatus::NoResult,
                    release_title: outcome.release_title.clone(),
                    info_hash: None,
                    error: Some(outcome.message.clone()),
                    next_retry_at: Some(now + ChronoDuration::hours(NO_RESULT_RETRY_HOURS)),
                    submitted_at: None,
                    completed_at: None,
                    increment_attempts: true,
                },
            )
            .await
        }
        AutoAcquireStatus::Blocked => {
            db.update_acquisition_job_state(
                job_id,
                &AcquisitionJobUpdate {
                    status: AcquisitionJobStatus::Blocked,
                    release_title: outcome.release_title.clone(),
                    info_hash: None,
                    error: Some(outcome.message.clone()),
                    next_retry_at: Some(now + ChronoDuration::minutes(BLOCKED_RETRY_MINUTES)),
                    submitted_at: None,
                    completed_at: None,
                    increment_attempts: false,
                },
            )
            .await
        }
        AutoAcquireStatus::CompletedLinked => {
            db.update_acquisition_job_state(
                job_id,
                &AcquisitionJobUpdate {
                    status: AcquisitionJobStatus::CompletedLinked,
                    release_title: outcome.release_title.clone(),
                    info_hash: None,
                    error: None,
                    next_retry_at: None,
                    submitted_at: None,
                    completed_at: Some(now),
                    increment_attempts: false,
                },
            )
            .await
        }
        AutoAcquireStatus::CompletedUnlinked => {
            db.update_acquisition_job_state(
                job_id,
                &AcquisitionJobUpdate {
                    status: AcquisitionJobStatus::CompletedUnlinked,
                    release_title: outcome.release_title.clone(),
                    info_hash: None,
                    error: Some(outcome.message.clone()),
                    next_retry_at: Some(
                        now + ChronoDuration::minutes(completed_unlinked_retry_minutes(attempts)),
                    ),
                    submitted_at: None,
                    completed_at: Some(now),
                    increment_attempts: false,
                },
            )
            .await
        }
        AutoAcquireStatus::Failed => {
            db.update_acquisition_job_state(
                job_id,
                &AcquisitionJobUpdate {
                    status: AcquisitionJobStatus::Failed,
                    release_title: outcome.release_title.clone(),
                    info_hash: None,
                    error: Some(outcome.message.clone()),
                    next_retry_at: Some(
                        now + ChronoDuration::minutes(failed_retry_minutes(attempts)),
                    ),
                    submitted_at: None,
                    completed_at: None,
                    increment_attempts: true,
                },
            )
            .await
        }
    }
}

async fn submit_request(
    cfg: &Config,
    decypharr: &DecypharrClient,
    request: &AutoAcquireRequest,
    dry_run: bool,
) -> Result<SubmitAttempt> {
    let prowlarr = cfg
        .has_prowlarr()
        .then(|| ProwlarrClient::new(&cfg.prowlarr));
    let arr = resolve_arr_name(decypharr, &request.arr).await?;

    let queue = decypharr.list_torrents(Some(&arr), None).await?;
    if let Some((guard, reason)) = queue_block_reason(&queue, cfg.decypharr.max_in_flight) {
        return Ok(match guard {
            QueueGuard::Capacity => SubmitAttempt::Deferred { reason },
            QueueGuard::Failing => SubmitAttempt::Immediate(AutoAcquireOutcome {
                status: AutoAcquireStatus::Blocked,
                release_title: None,
                message: reason,
            }),
        });
    }

    let candidate_queries = build_candidate_queries(request);
    let lookup =
        search_download_candidates(cfg, prowlarr.as_ref(), request, &candidate_queries).await?;

    let (search_query, source, candidates) = match lookup {
        CandidateLookup::Hits {
            query,
            source,
            candidates,
        } => (query, source, candidates),
        CandidateLookup::Pending(message) => {
            return Ok(SubmitAttempt::Immediate(AutoAcquireOutcome {
                status: AutoAcquireStatus::Blocked,
                release_title: None,
                message,
            }))
        }
        CandidateLookup::Empty => {
            let message = match (cfg.has_prowlarr(), cfg.has_dmm()) {
                (true, true) => format!(
                    "no Prowlarr result for '{}' ({} query variant(s) tried); DMM cache fallback also returned no usable result",
                    request.label,
                    candidate_queries.len()
                ),
                (true, false) => format!(
                    "no Prowlarr result for '{}' ({} query variant(s) tried)",
                    request.label,
                    candidate_queries.len()
                ),
                (false, true) => format!(
                    "DMM cache fallback found no usable result for '{}' ({} title variant(s) tried)",
                    request.label,
                    candidate_queries.len()
                ),
                (false, false) => "no external acquisition provider configured".to_string(),
            };
            return Ok(SubmitAttempt::Immediate(AutoAcquireOutcome {
                status: AutoAcquireStatus::NoResult,
                release_title: None,
                message,
            }));
        }
    };

    let mut last_add_error = None::<String>;
    for candidate in candidates.into_iter().take(MAX_CANDIDATES) {
        if dry_run {
            return Ok(SubmitAttempt::Immediate(AutoAcquireOutcome {
                status: AutoAcquireStatus::DryRun,
                release_title: Some(candidate.title),
                message: format!("would queue via Decypharr ({} via {})", arr, source),
            }));
        }

        let tracker = TorrentTracker::from_release(
            &arr,
            &search_query,
            &candidate.title,
            candidate.info_hash.as_deref(),
        );
        if let Some(existing) = find_matching_torrent(&queue, &tracker) {
            let submitted_at = existing.added_on.unwrap_or_else(Utc::now);
            return Ok(SubmitAttempt::Submitted(Box::new(SubmittedAcquire {
                job_id: 0,
                attempts: 0,
                request: request.clone(),
                arr,
                release_title: if existing.name.is_empty() {
                    candidate.title
                } else {
                    existing.name.clone()
                },
                tracker,
                submitted_at,
                reused_existing: true,
            })));
        }

        match decypharr.add_content(&[candidate.url], &arr, "none").await {
            Ok(_) => {
                return Ok(SubmitAttempt::Submitted(Box::new(SubmittedAcquire {
                    job_id: 0,
                    attempts: 0,
                    request: request.clone(),
                    arr,
                    release_title: candidate.title,
                    tracker,
                    submitted_at: Utc::now(),
                    reused_existing: false,
                })));
            }
            Err(err) => {
                last_add_error = Some(err.to_string());
                continue;
            }
        }
    }

    Ok(SubmitAttempt::Immediate(AutoAcquireOutcome {
        status: AutoAcquireStatus::Failed,
        release_title: None,
        message: last_add_error
            .unwrap_or_else(|| format!("no usable downloadable result from {}", source)),
    }))
}

async fn search_download_candidates(
    cfg: &Config,
    prowlarr: Option<&ProwlarrClient>,
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

    if !cfg.has_dmm() {
        return Ok(CandidateLookup::Empty);
    }

    search_dmm_candidates(cfg, request).await
}

async fn search_dmm_candidates(
    cfg: &Config,
    request: &AutoAcquireRequest,
) -> Result<CandidateLookup> {
    let Some(plan) = build_dmm_lookup_plan(request) else {
        return Ok(CandidateLookup::Empty);
    };

    let dmm = DmmClient::from_config(&cfg.dmm);
    let mut pending_reason = None::<String>;

    if let Some(imdb_id) = request.imdb_id.as_deref() {
        info!(
            "DMM: trying direct IMDb lookup {} for '{}'",
            imdb_id, request.label
        );
        match fetch_dmm_candidates_for_imdb(cfg, request, &plan, &dmm, imdb_id).await? {
            DmmImdbLookup::Hits(candidates) => {
                return Ok(CandidateLookup::Hits {
                    query: format!("imdb:{}", imdb_id),
                    source: "DMM",
                    candidates,
                });
            }
            DmmImdbLookup::Pending(reason) => pending_reason = Some(reason),
            DmmImdbLookup::Empty => {}
        }
    }

    for query in &plan.search_queries {
        let title_hits = dmm.search_title(query, plan.kind).await?;
        for title_hit in title_hits.into_iter().take(cfg.dmm.max_search_results) {
            let Some(lookup) =
                fetch_dmm_by_kind(&dmm, plan.kind, &title_hit.imdb_id, plan.season).await?
            else {
                continue;
            };

            match lookup {
                DmmTorrentLookup::Results(results) => {
                    let candidates = rank_dmm_candidates(
                        request,
                        query,
                        &title_hit,
                        results,
                        cfg.dmm.max_torrent_results,
                    );
                    if !candidates.is_empty() {
                        return Ok(CandidateLookup::Hits {
                            query: query.clone(),
                            source: "DMM",
                            candidates,
                        });
                    }
                }
                DmmTorrentLookup::Pending(status) => {
                    pending_reason = Some(format!(
                        "DMM cache scrape is {} for '{}' (query '{}')",
                        status, title_hit.title, query
                    ));
                }
                DmmTorrentLookup::Empty => {}
            }
        }
    }

    if let Some(reason) = pending_reason {
        Ok(CandidateLookup::Pending(reason))
    } else {
        Ok(CandidateLookup::Empty)
    }
}

enum DmmImdbLookup {
    Hits(Vec<DownloadCandidate>),
    Pending(String),
    Empty,
}

async fn fetch_dmm_by_kind(
    dmm: &DmmClient,
    kind: DmmMediaKind,
    imdb_id: &str,
    season: Option<u32>,
) -> Result<Option<DmmTorrentLookup>> {
    match kind {
        DmmMediaKind::Movie => Ok(Some(dmm.fetch_movie_results(imdb_id).await?)),
        DmmMediaKind::Show => {
            let Some(s) = season else { return Ok(None) };
            Ok(Some(dmm.fetch_tv_results(imdb_id, s).await?))
        }
    }
}

async fn fetch_dmm_candidates_for_imdb(
    cfg: &Config,
    request: &AutoAcquireRequest,
    plan: &DmmLookupPlan,
    dmm: &DmmClient,
    imdb_id: &str,
) -> Result<DmmImdbLookup> {
    let Some(lookup) = fetch_dmm_by_kind(dmm, plan.kind, imdb_id, plan.season).await? else {
        return Ok(DmmImdbLookup::Empty);
    };

    match lookup {
        DmmTorrentLookup::Results(results) => {
            let synthetic_title_hit = DmmTitleCandidate {
                title: request.label.clone(),
                imdb_id: imdb_id.to_string(),
                year: dmm_requested_year(request),
            };
            let candidates = rank_dmm_candidates(
                request,
                &format!("imdb:{}", imdb_id),
                &synthetic_title_hit,
                results,
                cfg.dmm.max_torrent_results,
            );
            if candidates.is_empty() {
                Ok(DmmImdbLookup::Empty)
            } else {
                Ok(DmmImdbLookup::Hits(candidates))
            }
        }
        DmmTorrentLookup::Pending(status) => Ok(DmmImdbLookup::Pending(format!(
            "DMM cache scrape is {} for IMDb {}",
            status, imdb_id
        ))),
        DmmTorrentLookup::Empty => Ok(DmmImdbLookup::Empty),
    }
}

fn build_dmm_lookup_plan(request: &AutoAcquireRequest) -> Option<DmmLookupPlan> {
    let kind = if normalize_arr_name(&request.arr) == "radarr" {
        DmmMediaKind::Movie
    } else if matches!(
        request.relink_check,
        RelinkCheck::MediaEpisode { .. } | RelinkCheck::MediaId(_)
    ) {
        DmmMediaKind::Show
    } else {
        return None;
    };

    let season = match &request.relink_check {
        RelinkCheck::MediaEpisode { season, .. } => Some(*season),
        _ => None,
    };

    let search_queries = build_dmm_search_queries(request, kind);
    (!search_queries.is_empty()).then_some(DmmLookupPlan {
        kind,
        season,
        search_queries,
    })
}

fn build_dmm_search_queries(request: &AutoAcquireRequest, kind: DmmMediaKind) -> Vec<String> {
    let scanner = SourceScanner::new();
    let mut queries = Vec::new();
    let mut titles = Vec::new();
    let mut years = Vec::new();
    let cleaned_label = clean_request_label(&request.label);

    for candidate in [request.query.as_str(), cleaned_label.as_str()] {
        for (_, parsed) in scanner.parse_release_title_variants(candidate) {
            let title = strip_year_tokens(&parsed.parsed_title);
            push_candidate_query(&mut titles, &title);
            if let Some(year) = parsed.year {
                if !years.contains(&year) {
                    years.push(year);
                }
            }
        }
    }

    if titles.is_empty() {
        let title = strip_numbering_tokens(&strip_year_tokens(&cleaned_label));
        push_candidate_query(&mut titles, &title);
        let title = strip_numbering_tokens(&strip_year_tokens(&request.query));
        push_candidate_query(&mut titles, &title);
    }

    for title in &titles {
        if kind == DmmMediaKind::Movie {
            for year in &years {
                push_candidate_query(&mut queries, &format!("{} {}", title, year));
            }
        }
        push_candidate_query(&mut queries, title);
    }

    queries
}

fn strip_numbering_tokens(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !is_numbering_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

fn rank_dmm_candidates(
    request: &AutoAcquireRequest,
    search_query: &str,
    title_hit: &DmmTitleCandidate,
    results: Vec<DmmTorrentResult>,
    max_results: usize,
) -> Vec<DownloadCandidate> {
    let ranked = if normalize_arr_name(&request.arr) == "sonarranime" {
        rank_dmm_anime_results(request, search_query, results)
    } else if matches!(request.relink_check, RelinkCheck::MediaEpisode { .. }) {
        rank_dmm_tv_results(request, results)
    } else {
        rank_dmm_movie_results(request, title_hit, results)
    };

    let mut deduped = Vec::new();
    let mut seen_hashes = HashSet::new();
    for result in ranked {
        let hash = result.hash.to_ascii_lowercase();
        if !seen_hashes.insert(hash.clone()) {
            continue;
        }
        deduped.push(DownloadCandidate {
            title: result.title.clone(),
            url: magnet_uri_from_hash(&result.hash),
            info_hash: Some(hash),
        });
        if deduped.len() >= max_results {
            break;
        }
    }

    deduped
}

fn magnet_uri_from_hash(hash: &str) -> String {
    format!("magnet:?xt=urn:btih:{}", hash)
}

/// Sort `items` by descending score, breaking ties by descending file_size.
/// Items for which `scorer` returns `None` are dropped.
/// If `min_score` is `Some(threshold)`, items scoring below the threshold are also dropped.
fn rank_by_score<T, F>(
    items: Vec<T>,
    scorer: F,
    size_of: impl Fn(&T) -> i64,
    min_score: Option<i64>,
) -> Vec<T>
where
    F: Fn(&T) -> Option<i64>,
{
    let mut scored: Vec<(i64, T)> = items
        .into_iter()
        .filter_map(|item| {
            let score = scorer(&item)? + size_score(size_of(&item));
            if let Some(min) = min_score {
                if score < min {
                    return None;
                }
            }
            Some((score, item))
        })
        .collect();
    scored.sort_by(|(a, item_a), (b, item_b)| {
        b.cmp(a).then_with(|| size_of(item_b).cmp(&size_of(item_a)))
    });
    scored.into_iter().map(|(_, item)| item).collect()
}

fn rank_dmm_movie_results(
    request: &AutoAcquireRequest,
    title_hit: &DmmTitleCandidate,
    results: Vec<DmmTorrentResult>,
) -> Vec<DmmTorrentResult> {
    let query_tokens = dmm_query_title_tokens(request);
    let requested_year = dmm_requested_year(request).or(title_hit.year);
    let exact_imdb_hit = request.imdb_id.as_deref() == Some(title_hit.imdb_id.as_str());
    rank_by_score(
        results,
        |result| {
            let title_tokens = normalized_tokens(&result.title);
            let title_token_set: HashSet<_> = title_tokens.iter().map(String::as_str).collect();
            let matched = query_tokens
                .iter()
                .filter(|token| title_token_set.contains(token.as_str()))
                .count() as i64;
            if matched == 0 && !exact_imdb_hit {
                return None;
            }

            let mut score = matched * 200;
            if exact_imdb_hit {
                score += 360;
            }
            if matched as usize == query_tokens.len() {
                score += 220;
            }
            if let Some(year) = requested_year {
                if title_tokens.iter().any(|token| token == &year.to_string()) {
                    score += 120;
                }
            }
            Some(score)
        },
        |r| r.file_size,
        None,
    )
}

fn rank_dmm_tv_results(
    request: &AutoAcquireRequest,
    results: Vec<DmmTorrentResult>,
) -> Vec<DmmTorrentResult> {
    let RelinkCheck::MediaEpisode {
        season, episode, ..
    } = &request.relink_check
    else {
        return Vec::new();
    };

    let scanner = SourceScanner::new();
    let upgrade = request.label.contains("upgrade");
    rank_by_score(
        results,
        |result| tv_result_score(&scanner, &result.title, *season, *episode, upgrade),
        |r| r.file_size,
        None,
    )
}

fn rank_dmm_anime_results(
    request: &AutoAcquireRequest,
    search_query: &str,
    results: Vec<DmmTorrentResult>,
) -> Vec<DmmTorrentResult> {
    let Some(context) = build_anime_request_context(request) else {
        return Vec::new();
    };
    let scanner = SourceScanner::new();
    let query_is_specific = query_has_specific_numbering(search_query);
    let min_score = if query_is_specific { None } else { Some(1_000) };
    rank_by_score(
        results,
        |result| {
            let hit_tokens_vec = normalized_tokens(&result.title);
            let hit_tokens: HashSet<_> = hit_tokens_vec.iter().map(String::as_str).collect();
            let title_matches = context
                .title_tokens
                .iter()
                .filter(|token| hit_tokens.contains(token.as_str()))
                .count() as i64;

            let mut best_score = None::<i64>;
            for (_, parsed) in scanner.parse_release_title_variants(&result.title) {
                if let Some(score) = anime_parsed_variant_score(&context, &parsed) {
                    best_score = Some(best_score.map_or(score, |best| best.max(score)));
                }
            }

            if let Some(score) = anime_pack_score(&context, &result.title) {
                best_score = Some(best_score.map_or(score, |best| best.max(score)));
            }

            let mut score = best_score?;
            if title_matches > 0 {
                score += title_matches * 40;
            }
            Some(score)
        },
        |r| r.file_size,
        min_score,
    )
}

fn tv_result_score(
    scanner: &SourceScanner,
    title: &str,
    desired_season: u32,
    desired_episode: u32,
    upgrade: bool,
) -> Option<i64> {
    let quality_bonus = if upgrade { 60 } else { 30 };
    let mut best_score = None::<i64>;
    for (_, parsed) in scanner.parse_release_title_variants(title) {
        if let (Some(season), Some(episode)) = (parsed.season, parsed.episode) {
            if season == desired_season && episode == desired_episode {
                best_score = Some(best_score.map_or(2_400 + quality_bonus, |best| {
                    best.max(2_400 + quality_bonus)
                }));
            }
        }
    }

    let normalized = crate::utils::normalize(title);
    let token_vec = normalized_tokens(title);
    let token_set = token_vec.iter().map(String::as_str).collect::<HashSet<_>>();
    if season_token_matches(&token_set, &normalized, desired_season)
        && (episode_ranges(title)
            .into_iter()
            .any(|(start, end)| (start..=end).contains(&desired_episode))
            || contains_complete_marker(&token_set))
    {
        best_score = Some(best_score.map_or(1_450, |best| best.max(1_450)));
    }

    best_score
}

fn dmm_query_title_tokens(request: &AutoAcquireRequest) -> Vec<String> {
    let title = strip_numbering_tokens(&strip_year_tokens(&clean_request_label(&request.label)));
    normalized_tokens(&title)
}

fn dmm_requested_year(request: &AutoAcquireRequest) -> Option<u32> {
    let scanner = SourceScanner::new();
    let cleaned_label = clean_request_label(&request.label);
    for candidate in [request.query.as_str(), cleaned_label.as_str()] {
        for (_, parsed) in scanner.parse_release_title_variants(candidate) {
            if parsed.year.is_some() {
                return parsed.year;
            }
        }
    }
    None
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

    let cleaned_label = clean_request_label(&request.label);
    push_candidate_query(&mut queries, &cleaned_label);

    let label_without_year = strip_year_tokens(&cleaned_label);
    push_candidate_query(&mut queries, &label_without_year);

    let query_without_year = strip_year_tokens(&request.query);
    push_candidate_query(&mut queries, &query_without_year);

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

    for (kind, parsed) in query_variants {
        match (parsed.season, parsed.episode, kind) {
            (Some(parsed_season), Some(parsed_episode), _) => {
                query_season = Some(parsed_season);
                query_episode = Some(parsed_episode);
            }
            (None, Some(parsed_episode), ParserKind::Anime) => {
                absolute_query_episode = Some(parsed_episode);
            }
            _ => {}
        }
    }

    if absolute_query_episode.is_none() && query_season.is_none() {
        if let Some(last) = request.query.split_whitespace().last() {
            if is_episode_number_token(last) {
                absolute_query_episode = last.parse().ok();
            }
        }
    }

    Some(AnimeRequestContext {
        desired_season: *season,
        desired_episode: *episode,
        query_season,
        query_episode,
        absolute_query_episode,
        title_tokens: request_title_tokens(&scanner, request),
        upgrade: request.label.contains("upgrade"),
    })
}

fn request_title_tokens(scanner: &SourceScanner, request: &AutoAcquireRequest) -> Vec<String> {
    let cleaned_label = clean_request_label(&request.label);
    let mut best_tokens = Vec::new();

    for (_, parsed) in scanner.parse_release_title_variants(&cleaned_label) {
        let tokens = normalized_tokens(&parsed.parsed_title);
        if tokens.len() > best_tokens.len() {
            best_tokens = tokens;
        }
    }

    if !best_tokens.is_empty() {
        return best_tokens;
    }

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

    if let Some(score) = anime_pack_score(context, &hit.title) {
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

fn anime_pack_score(context: &AnimeRequestContext, title: &str) -> Option<i64> {
    let normalized = crate::utils::normalize(title);
    let tokens: HashSet<_> = normalized.split_whitespace().collect();
    let desired_number = context
        .absolute_query_episode
        .unwrap_or(context.desired_episode);

    let matches_desired_season = season_token_matches(&tokens, &normalized, context.desired_season);
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

    None
}

fn season_token_matches(tokens: &HashSet<&str>, normalized_title: &str, season: u32) -> bool {
    let compact = format!("s{}", season);
    let padded = format!("s{:02}", season);
    let ordinal_numeric = format!("{} {}", ordinal_number(season), "season");
    tokens.contains(compact.as_str())
        || tokens.contains(padded.as_str())
        || normalized_title.contains(&format!("season {}", season))
        || normalized_title.contains(&ordinal_numeric)
        || ordinal_word(season)
            .map(|word| normalized_title.contains(&format!("{} season", word)))
            .unwrap_or(false)
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
    !is_year_token(token) && token.len() <= 4 && token.chars().all(|ch| ch.is_ascii_digit())
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
        AutoAcquireStatus::NoResult => summary.no_result += 1,
        AutoAcquireStatus::Blocked => summary.blocked += 1,
        AutoAcquireStatus::CompletedLinked => summary.completed_linked += 1,
        AutoAcquireStatus::CompletedUnlinked => summary.completed_unlinked += 1,
        AutoAcquireStatus::Failed => summary.failed += 1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::HashSet;

    #[test]
    fn failed_retry_backoff_values() {
        assert_eq!(failed_retry_minutes(1), 30);
        assert_eq!(failed_retry_minutes(2), 90);
        assert_eq!(failed_retry_minutes(3), 180);
        assert_eq!(failed_retry_minutes(4), 180); // capped
        assert_eq!(failed_retry_minutes(5), 180); // capped
                                                  // Edge: 0 or negative treated as attempt 1
        assert_eq!(failed_retry_minutes(0), 30);
    }

    #[test]
    fn completed_unlinked_retry_backoff_values() {
        assert_eq!(completed_unlinked_retry_minutes(1), 5);
        assert_eq!(completed_unlinked_retry_minutes(2), 15);
        assert_eq!(completed_unlinked_retry_minutes(3), 45);
        assert_eq!(completed_unlinked_retry_minutes(4), 120);
        assert_eq!(completed_unlinked_retry_minutes(5), 120); // capped
                                                              // Edge: 0 or negative treated as attempt 1
        assert_eq!(completed_unlinked_retry_minutes(0), 5);
    }

    #[test]
    fn extract_btih_reads_hash_from_magnet() {
        let magnet = "magnet:?xt=urn:btih:ABC123DEF456&dn=Example";
        assert_eq!(extract_btih(magnet), Some("ABC123DEF456".to_string()));
    }

    #[test]
    fn queue_block_reason_prefers_failed_torrents() {
        let torrent = DecypharrTorrent {
            info_hash: "abc".to_string(),
            name: "Broken".to_string(),
            state: "error".to_string(),
            status: "error".to_string(),
            progress: 0.0,
            is_complete: false,
            bad: true,
            category: "sonarr".to_string(),
            mount_path: String::new(),
            save_path: String::new(),
            content_path: String::new(),
            last_error: "slot full".to_string(),
            added_on: None,
            completed_at: None,
        };

        let (_, reason) = queue_block_reason(&[torrent], 3).unwrap();
        assert!(reason.contains("Broken"));
        assert!(reason.contains("DMM/Decypharr"));
    }

    #[test]
    fn queue_block_reason_flags_capacity_separately() {
        let torrent = DecypharrTorrent {
            info_hash: "abc".to_string(),
            name: "Busy".to_string(),
            state: "downloading".to_string(),
            status: "downloading".to_string(),
            progress: 0.2,
            is_complete: false,
            bad: false,
            category: "radarr".to_string(),
            mount_path: String::new(),
            save_path: String::new(),
            content_path: String::new(),
            last_error: String::new(),
            added_on: None,
            completed_at: None,
        };

        let (guard, _) = queue_block_reason(&[torrent], 1).unwrap();
        assert!(matches!(guard, QueueGuard::Capacity));
    }

    #[test]
    fn find_matching_torrent_falls_back_to_recent_token_match() {
        let tracker = TorrentTracker {
            category: "sonarr".to_string(),
            info_hash: None,
            query_tokens: vec![
                "breaking".to_string(),
                "bad".to_string(),
                "s01e01".to_string(),
            ],
            added_after: Utc.with_ymd_and_hms(2026, 3, 11, 0, 0, 0).unwrap(),
        };
        let torrents = vec![DecypharrTorrent {
            info_hash: "abc".to_string(),
            name: "Breaking.Bad.S01E01.1080p".to_string(),
            state: "downloading".to_string(),
            status: "downloading".to_string(),
            progress: 42.0,
            is_complete: false,
            bad: false,
            category: "sonarr".to_string(),
            mount_path: String::new(),
            save_path: String::new(),
            content_path: String::new(),
            last_error: String::new(),
            added_on: Some(Utc.with_ymd_and_hms(2026, 3, 11, 0, 0, 5).unwrap()),
            completed_at: None,
        }];

        let matched = find_matching_torrent(&torrents, &tracker).unwrap();
        assert_eq!(matched.name, "Breaking.Bad.S01E01.1080p");
    }

    #[test]
    fn normalize_arr_name_ignores_separators() {
        assert_eq!(normalize_arr_name("sonarr_anime"), "sonarranime");
        assert_eq!(normalize_arr_name("sonarr-anime"), "sonarranime");
    }

    #[test]
    fn parse_media_episode_value_roundtrips() {
        assert_eq!(
            parse_media_episode_value("tvdb-12345|1|9").unwrap(),
            ("tvdb-12345".to_string(), 1, 9)
        );
    }

    #[test]
    fn candidate_queries_include_label_and_yearless_fallbacks() {
        let request = AutoAcquireRequest {
            label: "The Darwin Incident (2026) S01E10 upgrade".to_string(),
            query: "Darwins Incident 10".to_string(),
            imdb_id: None,
            categories: vec![5070],
            arr: "sonarr-anime".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_check: RelinkCheck::MediaEpisode {
                media_id: "tvdb-123".to_string(),
                season: 1,
                episode: 10,
            },
        };

        let queries = build_candidate_queries(&request);
        assert_eq!(queries[0], "Darwins Incident 10");
        assert!(queries.contains(&"The Darwin Incident (2026) S01E10".to_string()));
        assert!(queries.contains(&"The Darwin Incident S01E10".to_string()));
        assert!(queries.contains(&"The Darwin Incident S01".to_string()));
        assert!(queries.contains(&"The Darwin Incident".to_string()));
        assert!(queries.contains(&"Darwins Incident".to_string()));
    }

    #[test]
    fn exact_imdb_movie_hits_survive_zero_token_overlap() {
        let request = AutoAcquireRequest {
            label: "1917".to_string(),
            query: "1917".to_string(),
            imdb_id: Some("tt8579674".to_string()),
            categories: vec![2000],
            arr: "radarr".to_string(),
            library_filter: Some("Movies".to_string()),
            relink_check: RelinkCheck::MediaId("tmdb-530915".to_string()),
        };
        let title_hit = DmmTitleCandidate {
            title: "1917".to_string(),
            imdb_id: "tt8579674".to_string(),
            year: Some(2019),
        };
        let ranked = rank_dmm_movie_results(
            &request,
            &title_hit,
            vec![DmmTorrentResult {
                title: "Sam.Mendes.War.Film.2019.2160p".to_string(),
                hash: "abc123".to_string(),
                file_size: 42,
            }],
        );

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].hash, "abc123");
    }

    #[test]
    fn strip_year_tokens_removes_parenthesized_years_only() {
        assert_eq!(
            strip_year_tokens("The Darwin Incident (2026) S01E10"),
            "The Darwin Incident S01E10"
        );
        assert_eq!(strip_year_tokens("Jujutsu Kaisen 3"), "Jujutsu Kaisen 3");
    }

    #[test]
    fn season_token_matches_common_anime_second_season_forms() {
        let normalized = "frieren 2nd season 01 12 complete";
        let tokens: HashSet<_> = normalized.split_whitespace().collect();
        assert!(season_token_matches(&tokens, normalized, 2));

        let normalized = "frieren second season batch";
        let tokens: HashSet<_> = normalized.split_whitespace().collect();
        assert!(season_token_matches(&tokens, normalized, 2));
    }

    #[test]
    fn anime_batch_fallbacks_reduce_episode_queries() {
        assert_eq!(
            anime_batch_fallbacks("Frieren S01E15"),
            vec!["Frieren S01".to_string(), "Frieren".to_string()]
        );
        assert_eq!(
            anime_batch_fallbacks("Jujutsu Kaisen 3"),
            vec!["Jujutsu Kaisen".to_string()]
        );
    }

    fn fake_hit(title: &str, seeders: i32, size_gb: i64) -> ProwlarrResult {
        ProwlarrResult {
            guid: format!("guid-{title}"),
            title: title.to_string(),
            indexer_id: 1,
            indexer: "test".to_string(),
            size: size_gb * 1024 * 1024 * 1024,
            seeders: Some(seeders),
            leechers: Some(0),
            download_url: Some("http://example.invalid/download".to_string()),
            magnet_url: Some("magnet:?xt=urn:btih:ABCDEF0123456789".to_string()),
            categories: Vec::new(),
            protocol: "torrent".to_string(),
        }
    }

    #[test]
    fn anime_ranking_prefers_exact_episode_over_packs() {
        let request = AutoAcquireRequest {
            label: "Frieren S01E15".to_string(),
            query: "Frieren S01E15".to_string(),
            imdb_id: None,
            categories: vec![5070],
            arr: "sonarr-anime".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_check: RelinkCheck::MediaEpisode {
                media_id: "tvdb-123".to_string(),
                season: 1,
                episode: 15,
            },
        };

        let ranked = rank_candidate_hits(
            &request,
            "Frieren S01E15",
            vec![
                fake_hit("[SubsPlease] Sousou no Frieren - 15", 22, 2),
                fake_hit("[SubsPlease] Sousou no Frieren S01 01-28 Complete", 120, 28),
            ],
        );

        assert_eq!(ranked[0].title, "[SubsPlease] Sousou no Frieren - 15");
    }

    #[test]
    fn anime_ranking_filters_wrong_absolute_episode() {
        let request = AutoAcquireRequest {
            label: "Frieren S01E15".to_string(),
            query: "Sousou no Frieren 15".to_string(),
            imdb_id: None,
            categories: vec![5070],
            arr: "sonarr-anime".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_check: RelinkCheck::MediaEpisode {
                media_id: "tvdb-123".to_string(),
                season: 1,
                episode: 15,
            },
        };

        let ranked = rank_candidate_hits(
            &request,
            "Sousou no Frieren",
            vec![
                fake_hit("[SubsPlease] Sousou no Frieren - 14", 300, 2),
                fake_hit("[SubsPlease] Sousou no Frieren - 15", 25, 2),
            ],
        );

        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].title, "[SubsPlease] Sousou no Frieren - 15");
    }

    #[test]
    fn anime_ranking_keeps_season_pack_when_exact_missing() {
        let request = AutoAcquireRequest {
            label: "Tales of Wedding Rings S02E13 upgrade".to_string(),
            query: "Tales of Wedding Rings S02E13".to_string(),
            imdb_id: None,
            categories: vec![5070],
            arr: "sonarr-anime".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_check: RelinkCheck::MediaEpisode {
                media_id: "tvdb-999".to_string(),
                season: 2,
                episode: 13,
            },
        };

        let ranked = rank_candidate_hits(
            &request,
            "Tales of Wedding Rings S02",
            vec![
                fake_hit("Tales of Wedding Rings S02 01-13 Complete 1080p", 18, 20),
                fake_hit("Tales of Wedding Rings S01 01-12 Complete 1080p", 200, 20),
            ],
        );

        assert_eq!(ranked.len(), 1);
        assert_eq!(
            ranked[0].title,
            "Tales of Wedding Rings S02 01-13 Complete 1080p"
        );
    }
}
