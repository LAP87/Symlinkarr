use super::*;

pub(super) async fn load_persistent_queue(
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
                request: job_to_request(&job)?,
            }),
            AcquisitionJobStatus::Downloading => downloading.push(job_to_submitted(&job)?),
            AcquisitionJobStatus::Relinking => relinking.push(job_to_relinking(&job)?),
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
        query_hints: request.query_hints.clone(),
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

fn job_to_request(job: &AcquisitionJobRecord) -> Result<AutoAcquireRequest> {
    let relink_check = match job.relink_kind {
        AcquisitionRelinkKind::MediaId => RelinkCheck::MediaId(job.relink_value.clone()),
        AcquisitionRelinkKind::MediaEpisode => {
            let (media_id, season, episode) = parse_media_episode_value(&job.relink_value)
                .with_context(|| {
                    format!(
                        "corrupt media-episode relink value '{}' in job {}",
                        job.relink_value, job.id
                    )
                })?;
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

    Ok(AutoAcquireRequest {
        label: job.label.clone(),
        query: job.query.clone(),
        query_hints: job.query_hints.clone(),
        imdb_id: job.imdb_id.clone(),
        categories: job.categories.clone(),
        arr: job.arr.clone(),
        library_filter: job.library_filter.clone(),
        relink_check,
    })
}

fn job_to_submitted(job: &AcquisitionJobRecord) -> Result<SubmittedAcquire> {
    let submitted_at = job.submitted_at.unwrap_or_else(Utc::now);
    Ok(SubmittedAcquire {
        job_id: job.id,
        attempts: job.attempts,
        request: job_to_request(job)?,
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
    })
}

fn job_to_relinking(job: &AcquisitionJobRecord) -> Result<RelinkPendingAcquire> {
    let submitted = job_to_submitted(job)?;
    Ok(RelinkPendingAcquire {
        submitted,
        completed_at: job.completed_at.unwrap_or_else(Utc::now),
    })
}

pub(super) async fn persist_terminal_outcome(
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

pub(super) async fn submit_request(
    cfg: &Config,
    decypharr: &DecypharrClient,
    dmm: Option<&DmmClient>,
    dmm_session: &mut DmmSearchSession,
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
                reason_code: "auto_acquire_queue_failing",
                release_title: None,
                message: reason,
            }),
        });
    }

    let candidate_queries = build_candidate_queries(request);
    let lookup = search_download_candidates(
        cfg,
        prowlarr.as_ref(),
        dmm,
        dmm_session,
        request,
        &candidate_queries,
    )
    .await?;

    let (search_query, source, candidates) = match lookup {
        CandidateLookup::Hits {
            query,
            source,
            candidates,
        } => (query, source, candidates),
        CandidateLookup::Pending(message) => {
            return Ok(SubmitAttempt::Immediate(AutoAcquireOutcome {
                status: AutoAcquireStatus::Blocked,
                reason_code: "auto_acquire_provider_pending",
                release_title: None,
                message,
            }))
        }
        CandidateLookup::Empty => {
            let (reason_code, message) = match (cfg.has_prowlarr(), cfg.has_dmm()) {
                (true, true) => (
                    "auto_acquire_no_result_provider_fallback_exhausted",
                    format!(
                    "no Prowlarr result for '{}' ({} query variant(s) tried); DMM cache fallback also returned no usable result",
                    request.label,
                    candidate_queries.len()
                ),
                ),
                (true, false) => (
                    "auto_acquire_no_result_prowlarr_empty",
                    format!(
                    "no Prowlarr result for '{}' ({} query variant(s) tried)",
                    request.label,
                    candidate_queries.len()
                ),
                ),
                (false, true) => (
                    "auto_acquire_no_result_dmm_empty",
                    format!(
                    "DMM cache fallback found no usable result for '{}' ({} title variant(s) tried)",
                    request.label,
                    candidate_queries.len()
                ),
                ),
                (false, false) => (
                    "auto_acquire_no_provider_configured",
                    "no external acquisition provider configured".to_string(),
                ),
            };
            return Ok(SubmitAttempt::Immediate(AutoAcquireOutcome {
                status: AutoAcquireStatus::NoResult,
                reason_code,
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
                reason_code: "auto_acquire_dry_run_preview",
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
        reason_code: "auto_acquire_submit_failed",
        release_title: None,
        message: last_add_error
            .unwrap_or_else(|| format!("no usable downloadable result from {}", source)),
    }))
}
