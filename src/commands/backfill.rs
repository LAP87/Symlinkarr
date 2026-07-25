use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures_util::{stream, StreamExt, TryStreamExt};
use serde::Serialize;
use tracing::warn;

use crate::anime_identity::{AnimeIdentityGraph, ANIME_LISTS_CACHE_TTL_HOURS};
use crate::api::radarr::{RadarrClient, RadarrMovie};
use crate::api::sonarr::{SonarrClient, SonarrEpisode, SonarrSeries, SonarrWantedMissingRecord};
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::auto_acquire::{
    process_auto_acquire_queue, AutoAcquireBatchSummary, AutoAcquireRequest, RelinkCheck,
};
use crate::commands::{
    decypharr_arr_name, ensure_runtime_directories_healthy, is_safe_auto_acquire_query, print_json,
    prowlarr_categories, selected_libraries,
};
use crate::config::{Config, ContentType, LibraryConfig};
use crate::db::Database;
use crate::linker::Linker;
use crate::matcher::Matcher;
use crate::media_servers::{has_configured_invalidation_server, invalidate_after_mutation};
use crate::models::{LibraryItem, MatchResult, MediaId, MediaType};
use crate::{BackfillArr, OutputFormat};

const LARGE_BACKFILL_SCOPE_WARNING_THRESHOLD: usize = 1_000;

#[derive(Debug, Clone)]
pub(crate) struct BackfillOptions {
    pub scope: BackfillArr,
    pub dry_run: bool,
    pub search_missing: bool,
    pub library_filter: Option<String>,
    pub item_filter: Option<String>,
    pub output: OutputFormat,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct BackfillSummary {
    pub scope: String,
    pub dry_run: bool,
    pub search_missing: bool,
    pub arr_items_seen: usize,
    pub empty_items_found: usize,
    pub whole_empty_items: usize,
    pub missing_episode_slots: usize,
    pub source_items_found: usize,
    pub matches_found: usize,
    pub linked_directly: u64,
    pub already_ok: usize,
    pub missing_search_needed: usize,
    pub ambiguous_manual_review: usize,
    pub failed: usize,
    pub links_created: u64,
    pub links_updated: u64,
    pub links_skipped: u64,
    pub auto_acquire_requests: usize,
    pub auto_acquire_submitted: usize,
    pub auto_acquire_completed_linked: usize,
    pub auto_acquire_completed_unlinked: usize,
    pub auto_acquire_no_result: usize,
    pub auto_acquire_blocked: usize,
    pub auto_acquire_failed: usize,
    pub auto_acquire_request_limit: usize,
    pub auto_acquire_candidates_considered: usize,
    pub auto_acquire_deferred_by_limit: usize,
    pub skipped: BTreeMap<String, u64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ArrBackfillCandidate {
    item: LibraryItem,
    source: BackfillArr,
    whole_item: bool,
    sonarr_series: Option<SonarrSeries>,
    radarr_movie: Option<RadarrMovie>,
    missing_episodes: Vec<SonarrWantedMissingRecord>,
}

#[derive(Debug, Clone, Default)]
struct CandidateCollection {
    candidates: Vec<ArrBackfillCandidate>,
    arr_items_seen: usize,
    whole_empty_items: usize,
    missing_episode_slots: usize,
    skipped: BTreeMap<String, u64>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct DestinationMatches {
    movie_ids: HashSet<String>,
    episode_slots: HashSet<(String, u32, u32)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CandidateClassification {
    already_ok: usize,
    missing_search_needed: usize,
}

#[derive(Debug, Clone, Copy)]
struct SonarrBackfillContext<'a> {
    selected_libraries: &'a [&'a LibraryConfig],
    source: BackfillArr,
    url: &'a str,
    api_key: &'a str,
    item_filter: Option<&'a str>,
}

struct BackfillLinkState {
    summary: BackfillSummary,
    candidates: Vec<ArrBackfillCandidate>,
    direct_matches: DestinationMatches,
    active_links: ActiveLinkSnapshot,
}

#[derive(Debug, Clone, Default)]
struct ActiveLinkSnapshot {
    targets_by_media_id: HashMap<String, Vec<String>>,
}

impl ActiveLinkSnapshot {
    async fn load(db: &Database, candidates: &[ArrBackfillCandidate]) -> Result<Self> {
        let media_ids = candidates
            .iter()
            .map(|candidate| candidate.item.id.to_string())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut targets_by_media_id = HashMap::<String, Vec<String>>::new();
        for link in db.get_active_links_for_media_ids(&media_ids).await? {
            targets_by_media_id
                .entry(link.media_id)
                .or_default()
                .push(link.target_path.to_string_lossy().to_ascii_uppercase());
        }
        Ok(Self {
            targets_by_media_id,
        })
    }

    fn has_media(&self, media_id: &str) -> bool {
        self.targets_by_media_id.contains_key(media_id)
    }

    fn has_episode(&self, media_id: &str, season: u32, episode: u32) -> bool {
        let slot = format!("S{season:02}E{episode:02}");
        self.targets_by_media_id
            .get(media_id)
            .is_some_and(|targets| targets.iter().any(|target| target.contains(&slot)))
    }
}

#[derive(Debug, Clone, Default)]
struct AutoAcquireRequestPlan {
    requests: Vec<AutoAcquireRequest>,
    request_limit: usize,
    candidates_considered: usize,
    deferred_by_limit: usize,
}

pub(crate) async fn run_backfill(
    cfg: &Config,
    db: &Database,
    options: BackfillOptions,
) -> Result<BackfillSummary> {
    let mut state = run_backfill_link_only(cfg, db, &options, true).await?;

    if options.search_missing {
        let tmdb = build_tmdb_client(cfg);
        let request_plan = build_auto_acquire_requests(
            cfg,
            db,
            tmdb.as_ref(),
            &state.candidates,
            &state.direct_matches,
            &state.active_links,
        )
        .await?;
        state.summary.auto_acquire_request_limit = request_plan.request_limit;
        state.summary.auto_acquire_candidates_considered = request_plan.candidates_considered;
        state.summary.auto_acquire_deferred_by_limit = request_plan.deferred_by_limit;
        let requests = request_plan.requests;
        state.summary.auto_acquire_requests = requests.len();

        if !requests.is_empty() {
            if state.summary.dry_run {
                state.summary.warnings.push(
                    "--dry-run with --search-missing may query DMM/Prowlarr for preview; Decypharr submissions are not sent"
                        .to_string(),
                );
            }
            if !cfg.has_decypharr() {
                state
                    .summary
                    .warnings
                    .push("--search-missing requested but Decypharr is not configured".to_string());
            } else if !cfg.has_prowlarr() && !cfg.has_dmm() {
                state.summary.warnings.push(
                    "--search-missing requested but neither Prowlarr nor DMM is configured"
                        .to_string(),
                );
            } else {
                let acquire_summary =
                    process_auto_acquire_queue(cfg, db, requests, state.summary.dry_run)
                        .await
                        .context("Arr backfill auto-acquire failed")?;
                apply_auto_acquire_summary(&mut state.summary, &acquire_summary);
            }
        }
    }

    let summary = state.summary;

    match options.output {
        OutputFormat::Json => print_json(&summary),
        OutputFormat::Text => print_text_summary(&summary),
    }

    Ok(summary)
}

pub(crate) async fn run_backfill_relink_for_filters(
    cfg: &Config,
    db: &Database,
    filters: &[Option<String>],
) -> Result<()> {
    if !cfg.has_radarr() && !cfg.has_sonarr() && !cfg.has_sonarr_anime() {
        return Ok(());
    }

    let mut unique_filters = Vec::<Option<String>>::new();
    if filters.iter().any(Option::is_none) {
        unique_filters.push(None);
    } else {
        for filter in filters {
            if !unique_filters.contains(filter) {
                unique_filters.push(filter.clone());
            }
        }
    }

    if unique_filters.is_empty() {
        unique_filters.push(None);
    }

    for filter in unique_filters {
        let options = BackfillOptions {
            scope: BackfillArr::All,
            dry_run: false,
            search_missing: false,
            library_filter: filter,
            item_filter: None,
            output: OutputFormat::Text,
        };

        if let Err(err) = run_backfill_link_only(cfg, db, &options, false).await {
            warn!("Arr backfill relink pass failed: {}", err);
        }
    }

    Ok(())
}

async fn run_backfill_link_only(
    cfg: &Config,
    db: &Database,
    options: &BackfillOptions,
    emit_text: bool,
) -> Result<BackfillLinkState> {
    let effective_dry_run = options.dry_run || cfg.symlink.dry_run;
    let selected = selected_libraries(cfg, options.library_filter.as_deref())?;
    ensure_runtime_directories_healthy(&selected, &cfg.sources, "arr backfill").await?;

    let collection = collect_candidates(
        cfg,
        options.scope,
        &selected,
        options.item_filter.as_deref(),
    )
    .await?;
    let mut summary = BackfillSummary {
        scope: options.scope.label().to_string(),
        dry_run: effective_dry_run,
        search_missing: options.search_missing,
        arr_items_seen: collection.arr_items_seen,
        empty_items_found: collection.candidates.len(),
        whole_empty_items: collection.whole_empty_items,
        missing_episode_slots: collection.missing_episode_slots,
        skipped: collection.skipped.clone(),
        warnings: collection.warnings.clone(),
        ..BackfillSummary::default()
    };

    warn_for_large_backfill_scope(
        &mut summary,
        collection.candidates.len(),
        options.library_filter.as_deref(),
        options.item_filter.as_deref(),
    );

    if collection.candidates.is_empty() {
        return Ok(BackfillLinkState {
            summary,
            candidates: collection.candidates,
            direct_matches: DestinationMatches::default(),
            active_links: ActiveLinkSnapshot::default(),
        });
    }

    let source_items = crate::commands::scan::collect_source_items_for_matching(cfg, db).await?;
    summary.source_items_found = source_items.len();

    let tmdb = build_tmdb_client(cfg);
    let tvdb = if cfg.has_tvdb() {
        Some(TvdbClient::new(
            &cfg.api.tvdb_api_key,
            cfg.api.cache_ttl_hours,
        ))
    } else {
        None
    };

    let library_items = collection
        .candidates
        .iter()
        .map(|candidate| candidate.item.clone())
        .collect::<Vec<_>>();

    let matcher = Matcher::new(
        tmdb.clone(),
        tvdb,
        cfg.matching.mode,
        cfg.matching.metadata_mode,
        cfg.matching.metadata_concurrency,
    )
    .with_multi_version(cfg.symlink.multi_version);

    let mut match_output = matcher
        .find_matches_with_telemetry(&library_items, &source_items, db)
        .await?;
    match_output.matches =
        filter_matches_to_arr_empty_slots(match_output.matches, &collection.candidates);
    summary.matches_found = match_output.matches.len();
    summary.ambiguous_manual_review = match_output.telemetry.ambiguous_skipped;
    merge_skip_counts(&mut summary.skipped, &match_output.telemetry.skip_reasons);

    matcher
        .enrich_episode_titles(&mut match_output.matches, db)
        .await?;

    let direct_matches = destination_matches(&match_output.matches);

    let linker = Linker::new_with_options(
        effective_dry_run,
        cfg.matching.mode.is_strict(),
        &cfg.symlink.naming_template,
        cfg.features.reconcile_links,
    )
    .with_multi_version(cfg.symlink.multi_version)
    .with_source_readiness_from_config(cfg);

    let link_summary = linker
        .process_matches(&match_output.matches, db, None)
        .await
        .context("failed to link Arr backfill matches")?;
    summary.links_created = link_summary.created;
    summary.links_updated = link_summary.updated;
    summary.links_skipped = link_summary.skipped;
    summary.linked_directly = link_summary.created + link_summary.updated;
    summary.already_ok = skip_count(&link_summary.skip_reasons, ALREADY_OK_SKIP_REASONS);
    summary.failed = skip_count(&link_summary.skip_reasons, FAILED_LINK_SKIP_REASONS);
    for (reason, count) in &link_summary.skip_reasons {
        *summary.skipped.entry(reason.clone()).or_insert(0) += count;
    }

    let linked_total = link_summary.created + link_summary.updated;
    if linked_total > 0 && !effective_dry_run && has_configured_invalidation_server(cfg) {
        if let Err(err) =
            invalidate_after_mutation(cfg, &selected, &link_summary.refresh_paths, emit_text).await
        {
            warn!("Arr backfill media-server refresh failed: {}", err);
            summary
                .warnings
                .push(format!("media-server refresh failed: {}", err));
        }
    }

    let active_links = ActiveLinkSnapshot::load(db, &collection.candidates).await?;
    let suppression_matches = if effective_dry_run {
        direct_matches.clone()
    } else {
        DestinationMatches::default()
    };
    let classification =
        classify_backfill_candidates(&collection.candidates, &suppression_matches, &active_links);
    summary.already_ok = summary.already_ok.saturating_add(classification.already_ok);
    summary.missing_search_needed = classification.missing_search_needed;

    Ok(BackfillLinkState {
        summary,
        candidates: collection.candidates,
        direct_matches: suppression_matches,
        active_links,
    })
}

async fn collect_candidates(
    cfg: &Config,
    scope: BackfillArr,
    selected_libraries: &[&LibraryConfig],
    item_filter: Option<&str>,
) -> Result<CandidateCollection> {
    let mut collection = CandidateCollection::default();

    if scope.includes(BackfillArr::Radarr) {
        if cfg.has_radarr() {
            collect_radarr_candidates(cfg, selected_libraries, item_filter, &mut collection)
                .await?;
        } else if scope != BackfillArr::All {
            collection
                .warnings
                .push("Radarr is not configured".to_string());
        }
    }

    if scope.includes(BackfillArr::Sonarr) {
        if cfg.has_sonarr() {
            collect_sonarr_candidates(
                SonarrBackfillContext {
                    selected_libraries,
                    source: BackfillArr::Sonarr,
                    url: &cfg.sonarr.url,
                    api_key: &cfg.sonarr.api_key,
                    item_filter,
                },
                &mut collection,
            )
            .await?;
        } else if scope != BackfillArr::All {
            collection
                .warnings
                .push("Sonarr is not configured".to_string());
        }
    }

    if scope.includes(BackfillArr::SonarrAnime) {
        if cfg.has_sonarr_anime() {
            collect_sonarr_candidates(
                SonarrBackfillContext {
                    selected_libraries,
                    source: BackfillArr::SonarrAnime,
                    url: &cfg.sonarr_anime.url,
                    api_key: &cfg.sonarr_anime.api_key,
                    item_filter,
                },
                &mut collection,
            )
            .await?;
        } else if scope != BackfillArr::All {
            collection
                .warnings
                .push("Sonarr Anime is not configured".to_string());
        }
    }

    collection.candidates.sort_by_key(|candidate| {
        (
            !candidate.whole_item,
            candidate.item.library_name.to_lowercase(),
            candidate.item.title.to_lowercase(),
        )
    });
    collection.candidates.dedup_by(|a, b| {
        a.item.id == b.item.id && a.item.path == b.item.path && a.source == b.source
    });

    Ok(collection)
}

fn build_tmdb_client(cfg: &Config) -> Option<TmdbClient> {
    cfg.has_tmdb().then(|| {
        TmdbClient::new(
            &cfg.api.tmdb_api_key,
            Some(&cfg.api.tmdb_read_access_token),
            cfg.api.cache_ttl_hours,
        )
    })
}

async fn collect_radarr_candidates(
    cfg: &Config,
    selected_libraries: &[&LibraryConfig],
    item_filter: Option<&str>,
    collection: &mut CandidateCollection,
) -> Result<()> {
    let radarr = RadarrClient::new(&cfg.radarr.url, &cfg.radarr.api_key);
    let movies = radarr.get_movies().await?;
    collection.arr_items_seen += movies.len();

    for movie in movies {
        if !movie.monitored {
            increment_skip(&mut collection.skipped, "radarr_unmonitored");
            continue;
        }
        if movie.has_file || movie.movie_file_id.unwrap_or_default() > 0 {
            continue;
        }
        if movie.tmdb_id <= 0 {
            increment_skip(&mut collection.skipped, "radarr_missing_tmdb_id");
            continue;
        }
        let media_id = MediaId::Tmdb(movie.tmdb_id as u64);
        if !arr_item_matches_filter(&movie.title, &movie.path, &media_id, item_filter) {
            continue;
        }

        let Some(item) = library_item_from_arr_path(
            selected_libraries,
            BackfillArr::Radarr,
            &movie.path,
            &movie.title,
            media_id,
        ) else {
            increment_skip(
                &mut collection.skipped,
                "radarr_path_outside_selected_libraries",
            );
            continue;
        };

        collection.candidates.push(ArrBackfillCandidate {
            item,
            source: BackfillArr::Radarr,
            whole_item: true,
            sonarr_series: None,
            radarr_movie: Some(movie),
            missing_episodes: Vec::new(),
        });
        collection.whole_empty_items += 1;
    }

    Ok(())
}

async fn collect_sonarr_candidates(
    ctx: SonarrBackfillContext<'_>,
    collection: &mut CandidateCollection,
) -> Result<()> {
    let sonarr = SonarrClient::new(ctx.url, ctx.api_key);
    let series = sonarr.get_series().await?;
    collection.arr_items_seen += series.len();
    let mut eligible = Vec::new();

    for series in series {
        if !series.monitored {
            increment_skip(&mut collection.skipped, "sonarr_unmonitored");
            continue;
        }

        let Some(media_id) = sonarr_media_id(&series) else {
            increment_skip(&mut collection.skipped, "sonarr_missing_media_id");
            continue;
        };
        if !arr_item_matches_filter(&series.title, &series.path, &media_id, ctx.item_filter) {
            continue;
        }
        let Some(item) = library_item_from_arr_path(
            ctx.selected_libraries,
            ctx.source,
            &series.path,
            &series.title,
            media_id,
        ) else {
            increment_skip(
                &mut collection.skipped,
                "sonarr_path_outside_selected_libraries",
            );
            continue;
        };

        eligible.push((series, item));
    }

    let fetched = stream::iter(eligible)
        .map(|(series, item)| {
            let sonarr = &sonarr;
            async move {
                let episodes = sonarr.get_episodes_for_series(series.id).await?;
                Ok::<_, anyhow::Error>((series, item, episodes))
            }
        })
        .buffer_unordered(8)
        .try_collect::<Vec<_>>()
        .await?;

    for (series, item, episodes) in fetched {
        let whole_item = sonarr_series_is_whole_empty(&series);
        let records = missing_records_from_episodes(&series, episodes);
        if !whole_item && records.is_empty() {
            continue;
        }

        if whole_item {
            collection.whole_empty_items += 1;
        }
        collection.missing_episode_slots += records.len();
        collection.candidates.push(ArrBackfillCandidate {
            item,
            source: ctx.source,
            whole_item,
            sonarr_series: Some(series),
            radarr_movie: None,
            missing_episodes: records,
        });
    }

    Ok(())
}

fn missing_records_from_episodes(
    series: &SonarrSeries,
    episodes: Vec<SonarrEpisode>,
) -> Vec<SonarrWantedMissingRecord> {
    episodes
        .into_iter()
        .filter_map(|episode| {
            if !episode.monitored
                || episode.has_file
                || episode.episode_file_id.unwrap_or_default() > 0
            {
                return None;
            }

            let record = SonarrWantedMissingRecord {
                series_id: series.id,
                tvdb_id: series.tvdb_id,
                season_number: episode.season_number,
                episode_number: episode.episode_number,
                absolute_episode_number: episode.absolute_episode_number,
                scene_season_number: episode.scene_season_number,
                scene_episode_number: episode.scene_episode_number,
                scene_absolute_episode_number: episode.scene_absolute_episode_number,
                title: episode.title,
                has_file: episode.has_file,
                episode_file_id: episode.episode_file_id,
                air_date_utc: episode.air_date_utc,
                monitored: episode.monitored,
            };

            (crate::anime_scanner::wanted_episode_is_searchable(&record)
                && crate::anime_scanner::wanted_episode_has_supported_numbering(&record))
            .then_some(record)
        })
        .collect()
}

fn sonarr_series_is_whole_empty(series: &SonarrSeries) -> bool {
    series
        .statistics
        .as_ref()
        .map(|statistics| statistics.episode_file_count == 0)
        .unwrap_or(false)
}

fn arr_item_matches_filter(
    title: &str,
    path: &str,
    media_id: &MediaId,
    item_filter: Option<&str>,
) -> bool {
    let Some(raw_filter) = item_filter else {
        return true;
    };
    let filter = crate::utils::normalize(raw_filter);
    if filter.is_empty() {
        return true;
    }

    if [title, path]
        .into_iter()
        .any(|candidate| item_filter_matches_text(candidate, raw_filter, &filter))
    {
        return true;
    }

    item_filter_allows_media_id_match(raw_filter, &filter)
        && media_id_matches_filter(media_id, raw_filter, &filter)
}

fn item_filter_matches_text(candidate: &str, raw_filter: &str, normalized_filter: &str) -> bool {
    if item_filter_is_short_numeric(raw_filter) {
        return crate::utils::normalize(candidate)
            .split_whitespace()
            .any(|token| token == normalized_filter);
    }

    crate::utils::normalize(candidate).contains(normalized_filter)
}

fn warn_for_large_backfill_scope(
    summary: &mut BackfillSummary,
    candidate_count: usize,
    library_filter: Option<&str>,
    item_filter: Option<&str>,
) {
    if candidate_count >= LARGE_BACKFILL_SCOPE_WARNING_THRESHOLD
        && library_filter.is_none()
        && item_filter.is_none()
    {
        summary.warnings.push(format!(
            "large backfill scope ({} candidates); use --library or --item for faster focused runs",
            candidate_count
        ));
    }
}

fn media_id_matches_filter(media_id: &MediaId, raw_filter: &str, normalized_filter: &str) -> bool {
    let media_id = media_id.to_string();
    if crate::utils::normalize(&media_id).contains(normalized_filter) {
        return true;
    }

    compact_alphanumeric(&media_id).contains(&compact_alphanumeric(raw_filter))
}

fn item_filter_allows_media_id_match(raw_filter: &str, normalized_filter: &str) -> bool {
    let has_provider_hint = normalized_filter
        .split_whitespace()
        .any(|token| matches!(token, "tvdb" | "tmdb" | "imdb"));
    if has_provider_hint {
        return true;
    }

    let compact = compact_alphanumeric(raw_filter);
    if ["tvdb", "tmdb", "imdb"]
        .iter()
        .any(|prefix| compact.starts_with(prefix))
    {
        return true;
    }

    let trimmed = raw_filter.trim();
    !item_filter_is_short_numeric(trimmed)
}

fn item_filter_is_short_numeric(raw_filter: &str) -> bool {
    let trimmed = raw_filter.trim();
    !trimmed.is_empty() && trimmed.len() < 4 && trimmed.chars().all(|ch| ch.is_ascii_digit())
}

fn compact_alphanumeric(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn library_item_from_arr_path(
    selected_libraries: &[&LibraryConfig],
    source: BackfillArr,
    path: &str,
    title: &str,
    media_id: MediaId,
) -> Option<LibraryItem> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return None;
    }

    let media_type = match source {
        BackfillArr::Radarr => MediaType::Movie,
        BackfillArr::Sonarr | BackfillArr::SonarrAnime | BackfillArr::All => MediaType::Tv,
    };

    let library = selected_libraries
        .iter()
        .copied()
        .filter(|library| library.media_type == media_type)
        .filter(|library| arr_source_matches_library(source, library))
        .filter(|library| path_is_under_library(&path, &library.path))
        .max_by_key(|library| library.path.components().count())?;

    let content_type = match source {
        BackfillArr::SonarrAnime => ContentType::Anime,
        BackfillArr::Radarr => ContentType::Movie,
        BackfillArr::Sonarr | BackfillArr::All => library
            .content_type
            .unwrap_or(ContentType::from_media_type(library.media_type)),
    };

    Some(LibraryItem {
        id: media_id,
        path,
        title: title.trim().to_string(),
        library_name: library.name.clone(),
        media_type,
        content_type,
    })
}

fn arr_source_matches_library(source: BackfillArr, library: &LibraryConfig) -> bool {
    let content_type = library
        .content_type
        .unwrap_or(ContentType::from_media_type(library.media_type));
    match source {
        BackfillArr::Radarr => library.media_type == MediaType::Movie,
        BackfillArr::SonarrAnime => {
            library.media_type == MediaType::Tv && content_type == ContentType::Anime
        }
        BackfillArr::Sonarr => {
            library.media_type == MediaType::Tv && content_type != ContentType::Anime
        }
        BackfillArr::All => true,
    }
}

fn path_is_under_library(path: &Path, library_root: &Path) -> bool {
    crate::utils::path_under_roots(path, &[library_root.to_path_buf()])
}

fn sonarr_media_id(series: &SonarrSeries) -> Option<MediaId> {
    if series.tvdb_id > 0 {
        Some(MediaId::Tvdb(series.tvdb_id as u64))
    } else if series.tmdb_id > 0 {
        Some(MediaId::Tmdb(series.tmdb_id as u64))
    } else {
        None
    }
}

fn filter_matches_to_arr_empty_slots(
    matches: Vec<MatchResult>,
    candidates: &[ArrBackfillCandidate],
) -> Vec<MatchResult> {
    let wanted_movies = candidates
        .iter()
        .filter(|candidate| candidate.item.media_type == MediaType::Movie)
        .map(|candidate| candidate.item.id.to_string())
        .collect::<HashSet<_>>();
    let wanted_whole_tv = candidates
        .iter()
        .filter(|candidate| candidate.item.media_type == MediaType::Tv && candidate.whole_item)
        .map(|candidate| candidate.item.id.to_string())
        .collect::<HashSet<_>>();
    let wanted_slots = wanted_episode_slots(candidates);

    matches
        .into_iter()
        .filter(|candidate| match candidate.library_item.media_type {
            MediaType::Movie => wanted_movies.contains(&candidate.library_item.id.to_string()),
            MediaType::Tv => {
                let media_id = candidate.library_item.id.to_string();
                if wanted_whole_tv.contains(&media_id) {
                    return true;
                }
                let Some(season) = candidate.source_item.season else {
                    return false;
                };
                let Some(episode) = candidate.source_item.episode else {
                    return false;
                };
                wanted_slots.contains(&(media_id, season, episode))
            }
        })
        .collect()
}

fn wanted_episode_slots(candidates: &[ArrBackfillCandidate]) -> HashSet<(String, u32, u32)> {
    let mut slots = HashSet::new();
    for candidate in candidates {
        let media_id = candidate.item.id.to_string();
        for episode in &candidate.missing_episodes {
            slots.insert((
                media_id.clone(),
                episode.season_number,
                episode.episode_number,
            ));
        }
    }
    slots
}

fn destination_matches(matches: &[MatchResult]) -> DestinationMatches {
    let mut result = DestinationMatches::default();
    for candidate in matches {
        let media_id = candidate.library_item.id.to_string();
        match candidate.library_item.media_type {
            MediaType::Movie => {
                result.movie_ids.insert(media_id);
            }
            MediaType::Tv => {
                if let (Some(season), Some(episode)) =
                    (candidate.source_item.season, candidate.source_item.episode)
                {
                    result.episode_slots.insert((media_id, season, episode));
                }
            }
        }
    }
    result
}

const ALREADY_OK_SKIP_REASONS: &[&str] = &["already_correct", "already_correct_disk"];
const FAILED_LINK_SKIP_REASONS: &[&str] = &[
    "source_missing_before_link",
    "source_unreadable_before_link",
    "regular_file_guard",
    "directory_guard",
];

fn classify_backfill_candidates(
    candidates: &[ArrBackfillCandidate],
    direct_matches: &DestinationMatches,
    active_links: &ActiveLinkSnapshot,
) -> CandidateClassification {
    let mut classification = CandidateClassification::default();

    for candidate in candidates {
        match candidate.item.media_type {
            MediaType::Movie => {
                classify_movie_candidate(
                    candidate,
                    direct_matches,
                    active_links,
                    &mut classification,
                );
            }
            MediaType::Tv => {
                classify_tv_candidate(candidate, direct_matches, active_links, &mut classification);
            }
        }
    }

    classification
}

fn classify_movie_candidate(
    candidate: &ArrBackfillCandidate,
    direct_matches: &DestinationMatches,
    active_links: &ActiveLinkSnapshot,
    classification: &mut CandidateClassification,
) {
    let media_id = candidate.item.id.to_string();
    if direct_matches.movie_ids.contains(&media_id) {
        return;
    }
    if active_links.has_media(&media_id) {
        classification.already_ok += 1;
    } else {
        classification.missing_search_needed += 1;
    }
}

fn classify_tv_candidate(
    candidate: &ArrBackfillCandidate,
    direct_matches: &DestinationMatches,
    active_links: &ActiveLinkSnapshot,
    classification: &mut CandidateClassification,
) {
    let media_id = candidate.item.id.to_string();
    if candidate.missing_episodes.is_empty() {
        let has_direct_match = direct_matches
            .episode_slots
            .iter()
            .any(|(slot_media_id, _, _)| slot_media_id == &media_id);
        if has_direct_match {
            return;
        }
        if active_links.has_media(&media_id) {
            classification.already_ok += 1;
        } else {
            classification.missing_search_needed += 1;
        }
        return;
    }

    for episode in &candidate.missing_episodes {
        let slot = (
            media_id.clone(),
            episode.season_number,
            episode.episode_number,
        );
        if direct_matches.episode_slots.contains(&slot) {
            continue;
        }
        if active_links.has_episode(&media_id, episode.season_number, episode.episode_number) {
            classification.already_ok += 1;
        } else {
            classification.missing_search_needed += 1;
        }
    }
}

async fn build_auto_acquire_requests(
    cfg: &Config,
    db: &Database,
    tmdb: Option<&TmdbClient>,
    candidates: &[ArrBackfillCandidate],
    direct_matches: &DestinationMatches,
    active_links: &ActiveLinkSnapshot,
) -> Result<AutoAcquireRequestPlan> {
    let mut plan = AutoAcquireRequestPlan {
        request_limit: cfg.decypharr.effective_max_requests_per_run(),
        ..AutoAcquireRequestPlan::default()
    };
    let mut queued_keys = HashSet::<String>::new();
    let anime_identity = if candidates
        .iter()
        .any(|candidate| candidate.item.content_type == ContentType::Anime)
    {
        AnimeIdentityGraph::load_with_ttl(db, ANIME_LISTS_CACHE_TTL_HOURS).await
    } else {
        None
    };

    for candidate in candidates {
        match candidate.item.media_type {
            MediaType::Movie => {
                let media_id = candidate.item.id.to_string();
                if direct_matches.movie_ids.contains(&media_id) || active_links.has_media(&media_id)
                {
                    continue;
                }
                if movie_auto_acquire_query(candidate).is_none() {
                    continue;
                }
                let key =
                    crate::auto_acquire::auto_acquire_request_key(&RelinkCheck::MediaId(media_id))?;
                if !queued_keys.insert(key) {
                    continue;
                }
                plan.candidates_considered += 1;
                if plan.requests.len() >= plan.request_limit {
                    plan.deferred_by_limit += 1;
                    continue;
                }
                let Some(request) = movie_auto_acquire_request(cfg, db, tmdb, candidate).await?
                else {
                    continue;
                };
                plan.requests.push(request);
            }
            MediaType::Tv => {
                let request_episodes =
                    auto_acquire_episode_anchors(candidate, direct_matches, active_links);
                for episode in request_episodes {
                    let media_id = candidate.item.id.to_string();
                    let slot = (
                        media_id.clone(),
                        episode.season_number,
                        episode.episode_number,
                    );
                    if direct_matches.episode_slots.contains(&slot)
                        || active_links.has_episode(
                            &media_id,
                            episode.season_number,
                            episode.episode_number,
                        )
                    {
                        continue;
                    }
                    if episode_auto_acquire_query(candidate, episode).is_none() {
                        continue;
                    }
                    let key = crate::auto_acquire::auto_acquire_request_key(
                        &RelinkCheck::MediaEpisode {
                            media_id: media_id.clone(),
                            season: episode.season_number,
                            episode: episode.episode_number,
                        },
                    )?;
                    if !queued_keys.insert(key) {
                        continue;
                    }
                    plan.candidates_considered += 1;
                    if plan.requests.len() >= plan.request_limit {
                        plan.deferred_by_limit += 1;
                        continue;
                    }
                    let Some(request) = episode_auto_acquire_request(
                        cfg,
                        db,
                        tmdb,
                        candidate,
                        episode,
                        anime_identity.as_ref(),
                    )
                    .await?
                    else {
                        continue;
                    };
                    plan.requests.push(request);
                }
            }
        }
    }

    Ok(plan)
}

fn auto_acquire_episode_anchors<'a>(
    candidate: &'a ArrBackfillCandidate,
    direct_matches: &DestinationMatches,
    active_links: &ActiveLinkSnapshot,
) -> Vec<&'a SonarrWantedMissingRecord> {
    if !candidate.whole_item {
        return candidate.missing_episodes.iter().collect();
    }

    let media_id = candidate.item.id.to_string();
    let mut anchors = BTreeMap::<u32, &SonarrWantedMissingRecord>::new();
    for episode in &candidate.missing_episodes {
        let slot = (
            media_id.clone(),
            episode.season_number,
            episode.episode_number,
        );
        if direct_matches.episode_slots.contains(&slot)
            || active_links.has_episode(&media_id, episode.season_number, episode.episode_number)
        {
            continue;
        }

        anchors.entry(episode.season_number).or_insert(episode);
    }

    anchors.into_values().collect()
}

async fn movie_auto_acquire_request(
    cfg: &Config,
    db: &Database,
    tmdb: Option<&TmdbClient>,
    candidate: &ArrBackfillCandidate,
) -> Result<Option<AutoAcquireRequest>> {
    let Some(query) = movie_auto_acquire_query(candidate) else {
        return Ok(None);
    };

    let imdb_id = candidate
        .radarr_movie
        .as_ref()
        .and_then(|movie| non_empty_string(&movie.imdb_id));
    let imdb_id = if imdb_id.is_some() {
        imdb_id
    } else {
        crate::commands::scan::lookup_item_imdb_id(tmdb, db, &candidate.item).await
    };

    Ok(Some(AutoAcquireRequest {
        label: query.clone(),
        query,
        query_hints: Vec::new(),
        imdb_id,
        categories: prowlarr_categories(candidate.item.media_type, candidate.item.content_type),
        arr: decypharr_arr_name(cfg, candidate.item.media_type, candidate.item.content_type)
            .to_string(),
        library_filter: Some(candidate.item.library_name.clone()),
        relink_check: RelinkCheck::MediaId(candidate.item.id.to_string()),
    }))
}

fn movie_auto_acquire_query(candidate: &ArrBackfillCandidate) -> Option<String> {
    let title = candidate.item.title.trim();
    let year = candidate
        .radarr_movie
        .as_ref()
        .and_then(|movie| (movie.year > 0).then_some(movie.year));
    let query = if let Some(year) = year {
        format!("{} {}", title, year)
    } else {
        title.to_string()
    };

    is_safe_auto_acquire_query(&query).then_some(query)
}

async fn episode_auto_acquire_request(
    cfg: &Config,
    db: &Database,
    tmdb: Option<&TmdbClient>,
    candidate: &ArrBackfillCandidate,
    episode: &SonarrWantedMissingRecord,
    anime_identity: Option<&AnimeIdentityGraph>,
) -> Result<Option<AutoAcquireRequest>> {
    let Some(series) = candidate.sonarr_series.as_ref() else {
        return Ok(None);
    };

    let Some(query) = episode_auto_acquire_query(candidate, episode) else {
        return Ok(None);
    };

    let media_id = candidate.item.id.to_string();
    let mut imdb_id = non_empty_string(&series.imdb_id);
    if imdb_id.is_none() && series.tmdb_id > 0 {
        if let Some(tmdb) = tmdb {
            imdb_id = tmdb
                .get_tv_imdb_id(series.tmdb_id as u64, db)
                .await
                .ok()
                .flatten();
        }
    }
    let imdb_id = if imdb_id.is_some() {
        imdb_id
    } else {
        crate::commands::scan::lookup_item_imdb_id(tmdb, db, &candidate.item).await
    };

    let query_hints = if candidate.item.content_type == ContentType::Anime {
        let mut hints =
            crate::anime_scanner::anime_query_hints(&candidate.item, episode, anime_identity, None);
        if candidate.whole_item {
            if let Some(episode_query) =
                crate::anime_scanner::build_anime_missing_search_query(series, episode, None)
            {
                push_unique_query_hint(&mut hints, episode_query);
            }
        }
        hints
    } else {
        Vec::new()
    };

    let label = if candidate.whole_item {
        format!(
            "{} S{:02} season pack",
            candidate.item.title, episode.season_number
        )
    } else {
        format!(
            "{} S{:02}E{:02}",
            candidate.item.title, episode.season_number, episode.episode_number
        )
    };
    let relink_check = if candidate.whole_item {
        let mut episodes = candidate
            .missing_episodes
            .iter()
            .filter(|missing| missing.season_number == episode.season_number)
            .map(|missing| missing.episode_number)
            .collect::<Vec<_>>();
        episodes.sort_unstable();
        episodes.dedup();
        RelinkCheck::MediaSeason {
            media_id,
            season: episode.season_number,
            episodes,
        }
    } else {
        RelinkCheck::MediaEpisode {
            media_id,
            season: episode.season_number,
            episode: episode.episode_number,
        }
    };

    Ok(Some(AutoAcquireRequest {
        label,
        query,
        query_hints,
        imdb_id,
        categories: prowlarr_categories(candidate.item.media_type, candidate.item.content_type),
        arr: decypharr_arr_name(cfg, candidate.item.media_type, candidate.item.content_type)
            .to_string(),
        library_filter: Some(candidate.item.library_name.clone()),
        relink_check,
    }))
}

fn episode_auto_acquire_query(
    candidate: &ArrBackfillCandidate,
    episode: &SonarrWantedMissingRecord,
) -> Option<String> {
    let series = candidate.sonarr_series.as_ref()?;
    if candidate.item.content_type == ContentType::Anime {
        if candidate.whole_item {
            build_tv_season_pack_search_query(series, episode)
        } else {
            crate::anime_scanner::build_anime_missing_search_query(series, episode, None)
        }
    } else {
        build_tv_search_query(series, episode, candidate.whole_item)
    }
}

fn build_tv_search_query(
    series: &SonarrSeries,
    episode: &SonarrWantedMissingRecord,
    whole_item: bool,
) -> Option<String> {
    let title = series.title.trim();
    if title.is_empty() {
        return None;
    }

    if whole_item {
        return build_tv_season_pack_search_query(series, episode);
    }

    let query = if let (Some(scene_season), Some(scene_episode)) =
        (episode.scene_season_number, episode.scene_episode_number)
    {
        format!("{} S{:02}E{:02}", title, scene_season, scene_episode)
    } else {
        format!(
            "{} S{:02}E{:02}",
            title, episode.season_number, episode.episode_number
        )
    };

    is_safe_auto_acquire_query(&query).then_some(query)
}

fn build_tv_season_pack_search_query(
    series: &SonarrSeries,
    episode: &SonarrWantedMissingRecord,
) -> Option<String> {
    let title = series.title.trim();
    if title.is_empty() {
        return None;
    }

    let season = episode.scene_season_number.unwrap_or(episode.season_number);
    let query = format!("{} S{:02} Complete", title, season);
    is_safe_auto_acquire_query(&query).then_some(query)
}

fn push_unique_query_hint(hints: &mut Vec<String>, hint: String) {
    let normalized = crate::utils::normalize(&hint);
    if normalized.is_empty()
        || hints
            .iter()
            .any(|existing| crate::utils::normalize(existing) == normalized)
    {
        return;
    }
    hints.push(hint);
}

fn apply_auto_acquire_summary(
    summary: &mut BackfillSummary,
    acquire_summary: &AutoAcquireBatchSummary,
) {
    summary.auto_acquire_submitted = acquire_summary.submitted;
    summary.auto_acquire_completed_linked = acquire_summary.completed_linked;
    summary.auto_acquire_completed_unlinked = acquire_summary.completed_unlinked;
    summary.auto_acquire_no_result = acquire_summary.no_result;
    summary.auto_acquire_blocked = acquire_summary.blocked;
    summary.auto_acquire_failed = acquire_summary.failed;
    summary.failed = summary.failed.saturating_add(acquire_summary.failed);
    for (reason, count) in &acquire_summary.reason_counts {
        *summary.skipped.entry(reason.clone()).or_insert(0) += count;
    }
}

fn print_text_summary(summary: &BackfillSummary) {
    for line in text_summary_lines(summary) {
        println!("{line}");
    }
}

fn text_summary_lines(summary: &BackfillSummary) -> Vec<String> {
    let mut lines = vec![
        String::new(),
        "Arr backfill summary".to_string(),
        format!("  Scope: {}", summary.scope),
        format!("  Dry run: {}", summary.dry_run),
        format!("  Arr items scanned: {}", summary.arr_items_seen),
        format!(
            "  Empty/missing folders: {} (whole folders={}, missing episode slots={})",
            summary.empty_items_found, summary.whole_empty_items, summary.missing_episode_slots
        ),
        format!(
            "  Existing source files scanned: {}",
            summary.source_items_found
        ),
        format!("  Matched existing files: {}", summary.matches_found),
        format!(
            "  Linked directly: {} (created={}, updated={})",
            summary.linked_directly, summary.links_created, summary.links_updated
        ),
        format!("  Already OK: {}", summary.already_ok),
        format!("  Missing/search-needed: {}", summary.missing_search_needed),
        format!(
            "  Ambiguous/manual-review: {}",
            summary.ambiguous_manual_review
        ),
        format!("  Failed: {}", summary.failed),
        format!("  Link skips: {}", summary.links_skipped),
    ];

    if summary.search_missing {
        lines.push(format!(
            "  Search/add: requests={} (limit={}, deferred_by_limit={}), submitted={}, linked={}, completed_unlinked={}, no_result={}, blocked={}, failed={}",
            summary.auto_acquire_requests,
            format_request_limit(summary.auto_acquire_request_limit),
            summary.auto_acquire_deferred_by_limit,
            summary.auto_acquire_submitted,
            summary.auto_acquire_completed_linked,
            summary.auto_acquire_completed_unlinked,
            summary.auto_acquire_no_result,
            summary.auto_acquire_blocked,
            summary.auto_acquire_failed
        ));
    }
    if !summary.skipped.is_empty() {
        lines.push("  Skip reasons:".to_string());
        for (reason, count) in &summary.skipped {
            lines.push(format!("    {reason} = {count}"));
        }
    }
    if !summary.warnings.is_empty() {
        lines.push("  Warnings:".to_string());
        for warning in &summary.warnings {
            lines.push(format!("    {warning}"));
        }
    }

    lines.push("  Next steps:".to_string());
    for step in backfill_next_steps(summary) {
        lines.push(format!("    - {step}"));
    }

    lines
}

fn format_request_limit(limit: usize) -> String {
    if limit == usize::MAX {
        "unlimited".to_string()
    } else {
        limit.to_string()
    }
}

fn backfill_next_steps(summary: &BackfillSummary) -> Vec<String> {
    if summary.empty_items_found == 0 {
        return vec!["No empty/missing Arr folders found for this scope.".to_string()];
    }

    let mut steps = Vec::new();
    if summary.dry_run && summary.linked_directly > 0 {
        steps.push("Re-run without --dry-run to create/update the planned links.".to_string());
    }
    if summary.search_missing && summary.auto_acquire_deferred_by_limit > 0 {
        steps.push(format!(
            "Re-run backfill --search-missing later; {} request(s) were deferred by max_requests_per_run.",
            summary.auto_acquire_deferred_by_limit
        ));
    }
    if summary.missing_search_needed > 0 && !summary.search_missing {
        steps.push(
            "Re-run with --search-missing to submit missing known gaps through Prowlarr/DMM and Decypharr."
                .to_string(),
        );
    } else if summary.search_missing && summary.auto_acquire_submitted > 0 {
        steps.push(
            "Let Decypharr finish submitted items, then run backfill again to link completed files."
                .to_string(),
        );
    } else if summary.search_missing
        && summary.missing_search_needed > 0
        && summary.auto_acquire_requests == 0
    {
        steps.push(
            "Review missing candidates; no safe search/add request could be built for them."
                .to_string(),
        );
    }
    if summary.ambiguous_manual_review > 0 {
        steps.push(
            "Review ambiguous/manual items before linking; strict matching skipped them."
                .to_string(),
        );
    }
    if summary.failed > 0 {
        steps.push(
            "Check failed skip reasons below, fix source/path issues, then rerun backfill."
                .to_string(),
        );
    }
    if summary.linked_directly > 0 && !summary.dry_run {
        steps.push(
            "New links were created; media-server refresh was requested when configured."
                .to_string(),
        );
    }
    if steps.is_empty() {
        steps.push("No operator action needed from this backfill run.".to_string());
    }

    steps
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then_some(value.to_string())
}

fn increment_skip(skipped: &mut BTreeMap<String, u64>, reason: &str) {
    *skipped.entry(reason.to_string()).or_insert(0) += 1;
}

fn merge_skip_counts(target: &mut BTreeMap<String, u64>, source: &BTreeMap<String, u64>) {
    for (reason, count) in source {
        *target.entry(reason.clone()).or_insert(0) += count;
    }
}

fn skip_count(skipped: &BTreeMap<String, u64>, reasons: &[&str]) -> usize {
    reasons
        .iter()
        .filter_map(|reason| skipped.get(*reason))
        .fold(0usize, |total, count| {
            total.saturating_add((*count).try_into().unwrap_or(usize::MAX))
        })
}

impl BackfillArr {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Radarr => "radarr",
            Self::Sonarr => "sonarr",
            Self::SonarrAnime => "sonarr-anime",
        }
    }

    fn includes(self, other: BackfillArr) -> bool {
        self == BackfillArr::All || self == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, DaemonConfig, DecypharrConfig,
        DmmConfig, FeaturesConfig, MatchingConfig, MediaBrowserConfig, PlexConfig, ProwlarrConfig,
        RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig,
        TautulliConfig, WebConfig,
    };
    use crate::models::{LinkRecord, LinkStatus, SourceItem};

    fn test_config(max_requests_per_run: usize) -> Config {
        Config {
            libraries: vec![library(
                "Series",
                "/plex/series",
                MediaType::Tv,
                Some(ContentType::Tv),
            )],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: PathBuf::from("/rd"),
                media_type: "tv".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig {
                max_requests_per_run,
                ..DecypharrConfig::default()
            },
            dmm: DmmConfig::default(),
            backup: BackupConfig::default(),
            db_path: ":memory:".to_string(),
            log_level: "info".to_string(),
            daemon: DaemonConfig::default(),
            symlink: SymlinkConfig::default(),
            matching: MatchingConfig::default(),
            prowlarr: ProwlarrConfig::default(),
            bazarr: BazarrConfig::default(),
            tautulli: TautulliConfig::default(),
            plex: PlexConfig::default(),
            emby: MediaBrowserConfig::default(),
            jellyfin: MediaBrowserConfig::default(),
            radarr: RadarrConfig::default(),
            sonarr: SonarrConfig::default(),
            sonarr_anime: SonarrConfig::default(),
            features: FeaturesConfig::default(),
            security: SecurityConfig::default(),
            cleanup: CleanupPolicyConfig::default(),
            web: WebConfig::default(),
            loaded_from: None,
            secret_files: Vec::new(),
        }
    }

    fn library(
        name: &str,
        path: &str,
        media_type: MediaType,
        content_type: Option<ContentType>,
    ) -> LibraryConfig {
        LibraryConfig {
            name: name.to_string(),
            path: PathBuf::from(path),
            media_type,
            content_type,
            depth: 1,
        }
    }

    fn tv_library_item(media_id: MediaId, title: &str) -> LibraryItem {
        LibraryItem {
            id: media_id,
            path: PathBuf::from(format!("/plex/series/{title}")),
            title: title.to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        }
    }

    fn missing_record(season: u32, episode: u32) -> SonarrWantedMissingRecord {
        SonarrWantedMissingRecord {
            series_id: 1,
            tvdb_id: 12345,
            season_number: season,
            episode_number: episode,
            absolute_episode_number: None,
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: format!("Episode {episode}"),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        }
    }

    fn sonarr_series(id: i64, tvdb_id: i64, title: &str) -> SonarrSeries {
        SonarrSeries {
            id,
            title: title.to_string(),
            path: format!("/plex/series/{title}"),
            alternate_titles: Vec::new(),
            tvdb_id,
            tmdb_id: 0,
            imdb_id: "tt1234567".to_string(),
            monitored: true,
            statistics: None,
            use_scene_numbering: false,
        }
    }

    fn tv_candidate(
        item: LibraryItem,
        missing_episodes: Vec<SonarrWantedMissingRecord>,
    ) -> ArrBackfillCandidate {
        ArrBackfillCandidate {
            item,
            source: BackfillArr::Sonarr,
            whole_item: false,
            sonarr_series: None,
            radarr_movie: None,
            missing_episodes,
        }
    }

    fn tv_candidate_with_series(
        item: LibraryItem,
        series: SonarrSeries,
        whole_item: bool,
        missing_episodes: Vec<SonarrWantedMissingRecord>,
    ) -> ArrBackfillCandidate {
        ArrBackfillCandidate {
            item,
            source: BackfillArr::Sonarr,
            whole_item,
            sonarr_series: Some(series),
            radarr_movie: None,
            missing_episodes,
        }
    }

    fn tv_match(item: LibraryItem, season: u32, episode: u32) -> MatchResult {
        MatchResult {
            library_item: item,
            source_item: SourceItem {
                path: PathBuf::from(format!("/rd/show/S{season:02}E{episode:02}.mkv")),
                parsed_title: "Show".to_string(),
                season: Some(season),
                episode: Some(episode),
                episode_end: None,
                quality: Some("1080p".to_string()),
                video_codec: None,
                hdr_formats: Vec::new(),
                edition: None,
                extension: "mkv".to_string(),
                year: None,
            },
            confidence: 0.99,
            matched_alias: "show".to_string(),
            episode_title: None,
        }
    }

    fn active_tv_link(media_id: &str, season: u32, episode: u32) -> LinkRecord {
        LinkRecord {
            id: None,
            source_path: PathBuf::from(format!("/rd/show/S{season:02}E{episode:02}.mkv")),
            target_path: PathBuf::from(format!("/plex/series/Show/S{season:02}E{episode:02}.mkv")),
            media_id: media_id.to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn item_filter_short_numeric_matches_title_and_path_but_not_media_id_only() {
        assert!(arr_item_matches_filter(
            "86 Eighty-Six",
            "/plex/anime/Eighty-Six",
            &MediaId::Tvdb(12345),
            Some("86"),
        ));
        assert!(arr_item_matches_filter(
            "Different Show",
            "/plex/anime/86 eighty six",
            &MediaId::Tvdb(12345),
            Some("86"),
        ));
        assert!(!arr_item_matches_filter(
            "Different Show 1986",
            "/plex/anime/different-show-tvdb-1986",
            &MediaId::Tvdb(12345),
            Some("86"),
        ));
        assert!(!arr_item_matches_filter(
            "Different Show",
            "/plex/anime/different-show",
            &MediaId::Tvdb(86),
            Some("86"),
        ));
    }

    #[test]
    fn item_filter_provider_or_long_numeric_matches_media_id() {
        assert!(arr_item_matches_filter(
            "Different Show",
            "/plex/anime/different-show",
            &MediaId::Tvdb(86),
            Some("tvdb-86"),
        ));
        assert!(arr_item_matches_filter(
            "Different Show",
            "/plex/anime/different-show",
            &MediaId::Tvdb(281633),
            Some("281633"),
        ));
        assert!(arr_item_matches_filter(
            "Different Movie",
            "/plex/movies/different-movie",
            &MediaId::Tmdb(550),
            Some("tmdb550"),
        ));
    }

    #[test]
    fn arr_path_builds_library_item_without_folder_id_tag() {
        let anime = library(
            "Anime",
            "/plex/anime",
            MediaType::Tv,
            Some(ContentType::Anime),
        );
        let selected = vec![&anime];

        let item = library_item_from_arr_path(
            &selected,
            BackfillArr::SonarrAnime,
            "/plex/anime/Parasyte -the maxim-",
            "Parasyte -the maxim-",
            MediaId::Tvdb(281633),
        )
        .unwrap();

        assert_eq!(item.library_name, "Anime");
        assert_eq!(item.content_type, ContentType::Anime);
        assert_eq!(item.path, PathBuf::from("/plex/anime/Parasyte -the maxim-"));
        assert_eq!(item.id, MediaId::Tvdb(281633));
    }

    #[test]
    fn sonarr_anime_path_rejects_non_anime_library() {
        let series = library(
            "Series",
            "/plex/series",
            MediaType::Tv,
            Some(ContentType::Tv),
        );
        let selected = vec![&series];

        let item = library_item_from_arr_path(
            &selected,
            BackfillArr::SonarrAnime,
            "/plex/series/Parasyte -the maxim-",
            "Parasyte -the maxim-",
            MediaId::Tvdb(281633),
        );

        assert!(item.is_none());
    }

    #[test]
    fn arr_path_rejects_parent_directory_escape() {
        let movies = library(
            "Movies",
            "/plex/movies",
            MediaType::Movie,
            Some(ContentType::Movie),
        );
        let selected = vec![&movies];

        let item = library_item_from_arr_path(
            &selected,
            BackfillArr::Radarr,
            "/plex/movies/../outside/Escape",
            "Escape",
            MediaId::Tmdb(1),
        );

        assert!(item.is_none());
    }

    #[test]
    fn text_summary_reports_operator_counts_and_next_steps() {
        let mut skipped = BTreeMap::new();
        skipped.insert("ambiguous_match".to_string(), 3);
        skipped.insert("source_missing_before_link".to_string(), 1);
        let summary = BackfillSummary {
            scope: "all".to_string(),
            dry_run: true,
            search_missing: false,
            arr_items_seen: 12,
            empty_items_found: 5,
            whole_empty_items: 2,
            missing_episode_slots: 4,
            source_items_found: 99,
            matches_found: 6,
            linked_directly: 2,
            already_ok: 1,
            missing_search_needed: 3,
            ambiguous_manual_review: 3,
            failed: 1,
            links_created: 2,
            links_skipped: 1,
            skipped,
            ..BackfillSummary::default()
        };

        let output = text_summary_lines(&summary).join("\n");

        assert!(output.contains("Arr items scanned: 12"));
        assert!(output.contains("Empty/missing folders: 5"));
        assert!(output.contains("Matched existing files: 6"));
        assert!(output.contains("Linked directly: 2"));
        assert!(output.contains("Already OK: 1"));
        assert!(output.contains("Missing/search-needed: 3"));
        assert!(output.contains("Ambiguous/manual-review: 3"));
        assert!(output.contains("Failed: 1"));
        assert!(output.contains("Re-run without --dry-run"));
        assert!(output.contains("Re-run with --search-missing"));
    }

    #[test]
    fn text_summary_reports_search_missing_cap_and_deferred_next_step() {
        let summary = BackfillSummary {
            scope: "sonarr-anime".to_string(),
            dry_run: true,
            search_missing: true,
            empty_items_found: 20,
            missing_search_needed: 50,
            auto_acquire_requests: 10,
            auto_acquire_request_limit: 10,
            auto_acquire_candidates_considered: 42,
            auto_acquire_deferred_by_limit: 32,
            auto_acquire_no_result: 3,
            ..BackfillSummary::default()
        };

        let output = text_summary_lines(&summary).join("\n");

        assert!(output.contains("Search/add: requests=10 (limit=10, deferred_by_limit=32)"));
        assert!(output.contains(
            "Re-run backfill --search-missing later; 32 request(s) were deferred by max_requests_per_run."
        ));

        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["auto_acquire_request_limit"], 10);
        assert_eq!(json["auto_acquire_candidates_considered"], 42);
        assert_eq!(json["auto_acquire_deferred_by_limit"], 32);
    }

    #[test]
    fn text_summary_reports_unlimited_search_missing_cap() {
        let summary = BackfillSummary {
            scope: "all".to_string(),
            search_missing: true,
            auto_acquire_requests: 42,
            auto_acquire_request_limit: usize::MAX,
            ..BackfillSummary::default()
        };

        let output = text_summary_lines(&summary).join("\n");

        assert!(output.contains("Search/add: requests=42 (limit=unlimited, deferred_by_limit=0)"));
    }

    #[test]
    fn large_unfiltered_backfill_scope_adds_operator_warning() {
        let mut summary = BackfillSummary::default();
        warn_for_large_backfill_scope(
            &mut summary,
            LARGE_BACKFILL_SCOPE_WARNING_THRESHOLD,
            None,
            None,
        );

        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("large backfill scope")));

        let mut scoped_summary = BackfillSummary::default();
        warn_for_large_backfill_scope(
            &mut scoped_summary,
            LARGE_BACKFILL_SCOPE_WARNING_THRESHOLD,
            Some("Anime"),
            None,
        );
        warn_for_large_backfill_scope(
            &mut scoped_summary,
            LARGE_BACKFILL_SCOPE_WARNING_THRESHOLD,
            None,
            Some("86-eighty-six"),
        );

        assert!(scoped_summary.warnings.is_empty());
    }

    #[tokio::test]
    async fn auto_acquire_request_builder_counts_deferred_episode_anchors_after_cap() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let cfg = test_config(2);

        let whole_item = tv_library_item(MediaId::Tvdb(11111), "Whole Show");
        let partial_item = tv_library_item(MediaId::Tvdb(22222), "Partial Show");
        let candidates = vec![
            tv_candidate_with_series(
                whole_item,
                sonarr_series(1, 11111, "Whole Show"),
                true,
                vec![
                    missing_record(1, 1),
                    missing_record(1, 2),
                    missing_record(2, 1),
                    missing_record(2, 2),
                ],
            ),
            tv_candidate_with_series(
                partial_item,
                sonarr_series(2, 22222, "Partial Show"),
                false,
                vec![missing_record(1, 1), missing_record(1, 2)],
            ),
        ];

        let plan = build_auto_acquire_requests(
            &cfg,
            &db,
            None,
            &candidates,
            &DestinationMatches::default(),
            &ActiveLinkSnapshot::default(),
        )
        .await
        .unwrap();

        assert_eq!(plan.request_limit, 2);
        assert_eq!(plan.requests.len(), 2);
        assert_eq!(plan.candidates_considered, 4);
        assert_eq!(plan.deferred_by_limit, 2);
        match &plan.requests[0].relink_check {
            RelinkCheck::MediaSeason {
                media_id,
                season,
                episodes,
            } => {
                assert_eq!(media_id, "tvdb-11111");
                assert_eq!(*season, 1);
                assert_eq!(episodes, &[1, 2]);
            }
            other => panic!("expected season relink check, got {other:?}"),
        }
        match &plan.requests[1].relink_check {
            RelinkCheck::MediaSeason {
                media_id,
                season,
                episodes,
            } => {
                assert_eq!(media_id, "tvdb-11111");
                assert_eq!(*season, 2);
                assert_eq!(episodes, &[1, 2]);
            }
            other => panic!("expected season relink check, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn auto_acquire_request_builder_treats_zero_request_cap_as_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let cfg = test_config(0);

        let whole_item = tv_library_item(MediaId::Tvdb(11111), "Whole Show");
        let partial_item = tv_library_item(MediaId::Tvdb(22222), "Partial Show");
        let candidates = vec![
            tv_candidate_with_series(
                whole_item,
                sonarr_series(1, 11111, "Whole Show"),
                true,
                vec![
                    missing_record(1, 1),
                    missing_record(1, 2),
                    missing_record(2, 1),
                    missing_record(2, 2),
                ],
            ),
            tv_candidate_with_series(
                partial_item,
                sonarr_series(2, 22222, "Partial Show"),
                false,
                vec![missing_record(1, 1), missing_record(1, 2)],
            ),
        ];

        let plan = build_auto_acquire_requests(
            &cfg,
            &db,
            None,
            &candidates,
            &DestinationMatches::default(),
            &ActiveLinkSnapshot::default(),
        )
        .await
        .unwrap();

        assert_eq!(plan.request_limit, usize::MAX);
        assert_eq!(plan.requests.len(), 4);
        assert_eq!(plan.candidates_considered, 4);
        assert_eq!(plan.deferred_by_limit, 0);
    }

    #[tokio::test]
    async fn candidate_classification_separates_direct_active_and_missing_slots() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let item = tv_library_item(MediaId::Tvdb(12345), "Show");
        db.insert_link(&active_tv_link("tvdb-12345", 1, 2))
            .await
            .unwrap();
        let candidate = tv_candidate(
            item.clone(),
            vec![
                missing_record(1, 1),
                missing_record(1, 2),
                missing_record(1, 3),
            ],
        );
        let direct_matches = destination_matches(&[tv_match(item, 1, 1)]);

        let active_links = ActiveLinkSnapshot::load(&db, std::slice::from_ref(&candidate))
            .await
            .unwrap();
        let classification =
            classify_backfill_candidates(&[candidate], &direct_matches, &active_links);

        assert_eq!(
            classification,
            CandidateClassification {
                already_ok: 1,
                missing_search_needed: 1,
            }
        );
    }
}
