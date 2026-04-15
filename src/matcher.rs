use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::anime_identity::{AnimeIdentityGraph, ANIME_LISTS_CACHE_TTL_HOURS};
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::config::{ContentType, MatchingMode, MetadataMode};
use crate::db::Database;
use crate::models::{ContentMetadata, LibraryItem, MatchResult, MediaId, MediaType, SourceItem};
use crate::source_scanner::{ParserKind, SourceScanner};
use crate::utils::{normalize, user_println, ProgressLine};

#[derive(Debug, Clone)]
struct MatchCandidate {
    source_idx: usize,
    library_idx: usize,
    media_id: String,
    score: f64,
    alias: String,
    source_item: SourceItem,
}

#[derive(Debug, Default)]
struct MatchChunkResult {
    processed: usize,
    ambiguous_skipped: usize,
    prefiltered_library_candidates: usize,
    scored_candidates: usize,
    exact_id_hits: usize,
    skip_reasons: BTreeMap<String, u64>,
    best_per_source: Vec<MatchCandidate>,
}

#[derive(Debug, Clone, Default)]
pub struct MatchTelemetry {
    pub metadata_alias_prep: Duration,
    pub candidate_scan: Duration,
    pub destination_reduce: Duration,
    pub metadata_errors: usize,
    pub worker_count: usize,
    pub prefiltered_library_candidates: usize,
    pub scored_candidates: usize,
    pub exact_id_hits: usize,
    pub ambiguous_skipped: usize,
    pub skip_reasons: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
pub struct MatchRunOutput {
    pub matches: Vec<MatchResult>,
    pub telemetry: MatchTelemetry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DestinationKey {
    Tv {
        media_id: String,
        season: u32,
        episode: u32,
    },
    Movie {
        media_id: String,
    },
}

#[derive(Debug, Deserialize)]
struct CachedMetadataEnvelope {
    #[serde(default)]
    _symlinkarr_not_found: bool,
    title: String,
    #[serde(default)]
    aliases: Vec<String>,
    year: Option<u32>,
    #[serde(default)]
    seasons: Vec<crate::models::SeasonInfo>,
}

enum MetadataCacheState {
    Miss,
    Hit(ContentMetadata),
    NegativeHit,
}

#[derive(Debug, Default)]
struct MatchSkipDiagnostics {
    exact_id_incompatible: bool,
    any_resolved_candidate: bool,
    any_shape_compatible: bool,
    any_metadata_compatible: bool,
    any_non_empty_title: bool,
    any_aliases_available: bool,
    any_alias_score: bool,
}

fn increment_skip_reason(skip_reasons: &mut BTreeMap<String, u64>, reason: &str) {
    *skip_reasons.entry(reason.to_string()).or_insert(0) += 1;
}

fn dominant_match_skip_reason(
    diagnostics: &MatchSkipDiagnostics,
    candidate_count: usize,
) -> &'static str {
    if candidate_count == 0 {
        return "matcher_no_library_candidates";
    }
    if diagnostics.exact_id_incompatible {
        return "matcher_exact_id_incompatible";
    }
    if !diagnostics.any_resolved_candidate {
        return "matcher_episode_mapping_unresolved";
    }
    if !diagnostics.any_shape_compatible {
        return "matcher_media_shape_mismatch";
    }
    if !diagnostics.any_metadata_compatible {
        return "matcher_metadata_mismatch";
    }
    if !diagnostics.any_non_empty_title {
        return "matcher_empty_parsed_title";
    }
    if !diagnostics.any_aliases_available {
        return "matcher_missing_aliases";
    }
    if !diagnostics.any_alias_score {
        return "matcher_alias_score_below_threshold";
    }
    "matcher_no_candidate"
}

/// The core matching engine of Symlinkarr.
///
/// For each library item (with a known metadata ID), fetches all known
/// titles/aliases from TMDB/TVDB, then matches source items (from RD mount)
/// against those aliases using deterministic candidate selection.
pub struct Matcher {
    tmdb: Option<TmdbClient>,
    tvdb: Option<Arc<Mutex<TvdbClient>>>,
    mode: MatchingMode,
    metadata_mode: MetadataMode,
    metadata_concurrency: usize,
}

impl Matcher {
    pub fn new(
        tmdb: Option<TmdbClient>,
        tvdb: Option<TvdbClient>,
        mode: MatchingMode,
        metadata_mode: MetadataMode,
        metadata_concurrency: usize,
    ) -> Self {
        Self {
            tmdb,
            tvdb: tvdb.map(|t| Arc::new(Mutex::new(t))),
            mode,
            metadata_mode,
            metadata_concurrency,
        }
    }

    pub async fn find_matches_with_telemetry(
        &self,
        library_items: &[LibraryItem],
        source_items: &[SourceItem],
        db: &Database,
    ) -> Result<MatchRunOutput> {
        info!(
            "Starting matching ({:?}, metadata={:?}): {} library items, {} source files",
            self.mode,
            self.metadata_mode,
            library_items.len(),
            source_items.len()
        );

        // Step 1: Build alias lookup for each library item (parallel metadata fetches)
        let mut alias_map: HashMap<usize, Vec<String>> = HashMap::new();
        let mut metadata_map: HashMap<usize, Option<ContentMetadata>> = HashMap::new();
        let mut metadata_errors = 0usize;
        let metadata_started = Instant::now();
        let mut metadata_progress = ProgressLine::new("Metadata alias prep:");
        user_println(format!(
            "   🧠 Building metadata alias map for {} library item(s)...",
            library_items.len()
        ));

        if self.metadata_mode == MetadataMode::Off {
            // No metadata to fetch — populate alias_map with folder titles only.
            for (idx, item) in library_items.iter().enumerate() {
                let all_titles = vec![normalize(&item.title)];
                alias_map.insert(idx, all_titles);
                metadata_map.insert(idx, None);
            }
        } else {
            let concurrency = self.metadata_concurrency.max(1);
            let semaphore = Arc::new(Semaphore::new(concurrency));
            let mut join_set: JoinSet<(usize, String, Result<Option<ContentMetadata>>)> =
                JoinSet::new();

            for (idx, item) in library_items.iter().enumerate() {
                let sem = Arc::clone(&semaphore);
                let tmdb = self.tmdb.clone();
                let tvdb = self.tvdb.clone();
                let metadata_mode = self.metadata_mode;
                let item = item.clone();
                let db = db.clone();

                join_set.spawn(async move {
                    let _permit = sem
                        .acquire()
                        .await
                        .expect("metadata semaphore unexpectedly closed");
                    let result =
                        fetch_metadata_static(&tmdb, tvdb.as_ref(), metadata_mode, &item, &db)
                            .await;
                    (idx, item.title.clone(), result)
                });
            }

            let total = library_items.len();
            let mut completed = 0usize;
            let mut last_metadata_progress = Instant::now();

            while let Some(join_result) = join_set.join_next().await {
                completed += 1;

                // Progress reporting in the collector loop only.
                if completed > 0 && last_metadata_progress.elapsed() >= Duration::from_secs(5) {
                    let pct = (completed as f64 / total.max(1) as f64) * 100.0;
                    if !metadata_progress.is_tty() {
                        info!(
                            "Metadata alias progress: {}/{} ({:.1}%)",
                            completed, total, pct
                        );
                    }
                    metadata_progress.update(format!("{}/{} ({:.1}%)", completed, total, pct));
                    last_metadata_progress = Instant::now();
                }

                let (idx, title, result) = match join_result {
                    Ok(r) => r,
                    Err(err) => {
                        metadata_errors += 1;
                        if metadata_errors <= 20 {
                            warn!("Metadata task panicked: {}. Skipping.", err);
                        }
                        continue;
                    }
                };

                let metadata = match result {
                    Ok(m) => m,
                    Err(err) => {
                        metadata_errors += 1;
                        if metadata_errors <= 20 {
                            warn!(
                                "Metadata lookup failed for '{}': {}. Using folder title only.",
                                title, err
                            );
                        }
                        None
                    }
                };

                // Rebuild all_titles from library_items using the returned idx.
                let item_title = &library_items[idx].title;
                let mut all_titles = vec![normalize(item_title)];
                if let Some(meta) = metadata.as_ref() {
                    all_titles.push(normalize(&meta.title));
                    for alias in &meta.aliases {
                        all_titles.push(normalize(alias));
                    }
                }
                all_titles.retain(|t| !t.is_empty());
                all_titles.sort();
                all_titles.dedup();

                debug!(
                    "Library '{}' has {} title variant(s): {:?}",
                    item_title,
                    all_titles.len(),
                    all_titles
                );
                alias_map.insert(idx, all_titles);
                metadata_map.insert(idx, metadata);
            }
        }

        metadata_progress.finish(format!(
            "{}/{} (100.0%) in {:.1}s",
            library_items.len(),
            library_items.len(),
            metadata_started.elapsed().as_secs_f64()
        ));
        let metadata_alias_prep = metadata_started.elapsed();
        if metadata_errors > 0 {
            warn!(
                "Metadata lookup failed for {} library item(s); continued with folder-title aliases only",
                metadata_errors
            );
        }
        let alias_token_index = build_alias_token_index(&alias_map);
        let anime_identity =
            AnimeIdentityGraph::load_with_ttl(db, ANIME_LISTS_CACHE_TTL_HOURS).await;

        // Step 2: Build deterministic best candidate per source
        let candidate_started = Instant::now();
        let allow_global_fallback = !library_items
            .iter()
            .all(|item| item.content_type == ContentType::Anime);
        let available = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let worker_count = if source_items.len() >= 2_000 {
            ((available * 3) / 4).max(1)
        } else {
            1
        };

        let (
            best_per_source,
            ambiguous_skipped,
            prefiltered_library_candidates,
            scored_candidates,
            exact_id_hits,
            skip_reasons,
        ) = if worker_count <= 1 {
            let chunk = match_source_slice(
                0,
                source_items,
                library_items,
                &alias_map,
                &metadata_map,
                &alias_token_index,
                self.mode,
                allow_global_fallback,
                anime_identity.as_ref(),
            );
            (
                chunk.best_per_source,
                chunk.ambiguous_skipped,
                chunk.prefiltered_library_candidates,
                chunk.scored_candidates,
                chunk.exact_id_hits,
                chunk.skip_reasons,
            )
        } else {
            user_println(format!(
                "   ⚙  Matching candidates using {} worker(s)",
                worker_count
            ));
            let library_items = Arc::new(library_items.to_vec());
            let alias_map = Arc::new(alias_map);
            let metadata_map = Arc::new(metadata_map);
            let alias_token_index = Arc::new(alias_token_index);
            let anime_identity = anime_identity.map(Arc::new);
            let chunk_size = source_items.len().div_ceil(worker_count);
            let mut workers = JoinSet::new();

            for (chunk_idx, chunk) in source_items.chunks(chunk_size.max(1)).enumerate() {
                let start_idx = chunk_idx * chunk_size.max(1);
                let chunk = chunk.to_vec();
                let library_items = Arc::clone(&library_items);
                let alias_map = Arc::clone(&alias_map);
                let metadata_map = Arc::clone(&metadata_map);
                let alias_token_index = Arc::clone(&alias_token_index);
                let anime_identity = anime_identity.clone();
                let mode = self.mode;

                workers.spawn_blocking(move || {
                    match_source_slice(
                        start_idx,
                        &chunk,
                        library_items.as_ref(),
                        alias_map.as_ref(),
                        metadata_map.as_ref(),
                        alias_token_index.as_ref(),
                        mode,
                        allow_global_fallback,
                        anime_identity.as_deref(),
                    )
                });
            }

            let mut progress = ProgressLine::new("Matching candidates:");
            let mut processed = 0usize;
            let mut ambiguous_skipped = 0usize;
            let mut prefiltered_library_candidates = 0usize;
            let mut scored_candidates = 0usize;
            let mut exact_id_hits = 0usize;
            let mut skip_reasons = BTreeMap::new();
            let mut best_per_source = Vec::new();

            while let Some(result) = workers.join_next().await {
                let chunk = result?;
                processed += chunk.processed;
                ambiguous_skipped += chunk.ambiguous_skipped;
                prefiltered_library_candidates += chunk.prefiltered_library_candidates;
                scored_candidates += chunk.scored_candidates;
                exact_id_hits += chunk.exact_id_hits;
                for (reason, count) in chunk.skip_reasons {
                    *skip_reasons.entry(reason).or_insert(0) += count;
                }
                best_per_source.extend(chunk.best_per_source);
                progress.update(format!(
                    "{}/{} ({:.1}%)",
                    processed,
                    source_items.len(),
                    (processed as f64 / source_items.len().max(1) as f64) * 100.0
                ));
            }

            progress.finish(format!(
                "{}/{} (100.0%)",
                source_items.len(),
                source_items.len()
            ));

            (
                best_per_source,
                ambiguous_skipped,
                prefiltered_library_candidates,
                scored_candidates,
                exact_id_hits,
                skip_reasons,
            )
        };
        let candidate_scan = candidate_started.elapsed();

        // Step 3: Enforce one link per destination slot (media_id+episode or media_id movie).
        // Multi-episode files (episode_end is set) are expanded into one candidate per
        // episode in the range so that destination reduction correctly resolves conflicts
        // between multi-ep and single-ep source files for the same episode slot.
        let destination_started = Instant::now();
        let mut by_destination: HashMap<DestinationKey, MatchCandidate> = HashMap::new();

        for candidate in best_per_source {
            let item = &library_items[candidate.library_idx];
            let episode_slots = expand_episode_slots(&candidate.source_item);

            if episode_slots.is_empty() {
                // Movie or source without episode info — use as-is.
                let Some(key) = destination_key(item, &candidate.source_item) else {
                    continue;
                };
                insert_or_replace(&mut by_destination, key, candidate);
            } else {
                // TV episode (possibly multi-episode) — insert one candidate per slot.
                for ep in episode_slots {
                    let mut slot_source = candidate.source_item.clone();
                    slot_source.episode = Some(ep);

                    let Some(key) = destination_key(item, &slot_source) else {
                        continue;
                    };

                    let slot_candidate = MatchCandidate {
                        source_item: slot_source,
                        ..candidate.clone()
                    };

                    insert_or_replace(&mut by_destination, key, slot_candidate);
                }
            }
        }

        // Step 4: Build final match results
        let mut final_candidates: Vec<MatchCandidate> = by_destination.into_values().collect();
        final_candidates.sort_by(|a, b| {
            a.library_idx
                .cmp(&b.library_idx)
                .then_with(|| a.source_idx.cmp(&b.source_idx))
        });

        let matches = final_candidates
            .into_iter()
            .map(|c| MatchResult {
                library_item: library_items[c.library_idx].clone(),
                source_item: c.source_item,
                confidence: c.score,
                matched_alias: c.alias,
                episode_title: None,
            })
            .collect::<Vec<_>>();
        let destination_reduce = destination_started.elapsed();

        let telemetry = MatchTelemetry {
            metadata_alias_prep,
            candidate_scan,
            destination_reduce,
            metadata_errors,
            worker_count,
            prefiltered_library_candidates,
            scored_candidates,
            exact_id_hits,
            ambiguous_skipped,
            skip_reasons,
        };

        info!(
            "Matching complete: {} confirmed matches ({:.0}% of source files; {} ambiguous skipped, {} candidate slots, {} scored candidates, {} exact-id hits)",
            matches.len(),
            if source_items.is_empty() {
                0.0
            } else {
                (matches.len() as f64 / source_items.len() as f64) * 100.0
            },
            telemetry.ambiguous_skipped,
            telemetry.prefiltered_library_candidates,
            telemetry.scored_candidates,
            telemetry.exact_id_hits,
        );
        Ok(MatchRunOutput { matches, telemetry })
    }

    /// Fetch content metadata for a library item (for episode renaming, etc.)
    #[cfg(test)]
    pub async fn get_metadata(
        &self,
        item: &LibraryItem,
        db: &Database,
    ) -> Result<Option<ContentMetadata>> {
        fetch_metadata_static(&self.tmdb, self.tvdb.as_ref(), self.metadata_mode, item, db).await
    }

    /// Pre-resolve episode titles for TV matches so the linker
    /// does not need access to the matcher at link-creation time.
    pub async fn enrich_episode_titles(
        &self,
        matches: &mut [MatchResult],
        db: &crate::db::Database,
    ) -> anyhow::Result<()> {
        // Collect indices that need enrichment.
        let needs_enrich: Vec<(usize, u32, u32)> = matches
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if m.library_item.media_type != crate::models::MediaType::Tv {
                    return None;
                }
                let season = m.source_item.season?;
                let episode = m.source_item.episode?;
                Some((i, season, episode))
            })
            .collect();

        if needs_enrich.is_empty() {
            return Ok(());
        }

        let concurrency = self.metadata_concurrency.max(1);
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut join_set: JoinSet<(usize, u32, u32, Option<String>)> = JoinSet::new();

        for (match_idx, season, episode) in needs_enrich {
            let sem = Arc::clone(&semaphore);
            let tmdb = self.tmdb.clone();
            let tvdb = self.tvdb.clone();
            let metadata_mode = self.metadata_mode;
            let item = matches[match_idx].library_item.clone();
            let db = db.clone();

            join_set.spawn(async move {
                let _permit = sem.acquire().await;
                let title = if let Ok(Some(metadata)) =
                    fetch_metadata_static(&tmdb, tvdb.as_ref(), metadata_mode, &item, &db).await
                {
                    metadata
                        .seasons
                        .iter()
                        .find(|s| s.season_number == season)
                        .and_then(|s| s.episodes.iter().find(|ep| ep.episode_number == episode))
                        .map(|ep| ep.title.clone())
                        .filter(|t| !t.is_empty())
                } else {
                    None
                };
                (match_idx, season, episode, title)
            });
        }

        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((match_idx, _season, _episode, title)) => {
                    matches[match_idx].episode_title = title;
                }
                Err(err) => {
                    warn!("Episode title enrichment task panicked: {}", err);
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free (non-self) metadata helpers — used by parallel task spawns
// ---------------------------------------------------------------------------

pub(crate) async fn fetch_metadata_static(
    tmdb: &Option<TmdbClient>,
    tvdb: Option<&Arc<Mutex<TvdbClient>>>,
    metadata_mode: MetadataMode,
    item: &LibraryItem,
    db: &Database,
) -> Result<Option<ContentMetadata>> {
    match metadata_mode {
        MetadataMode::Off => Ok(None),
        MetadataMode::CacheOnly => match fetch_cached_metadata_static(item, db).await? {
            MetadataCacheState::Hit(metadata) => Ok(Some(metadata)),
            MetadataCacheState::Miss | MetadataCacheState::NegativeHit => Ok(None),
        },
        MetadataMode::Full => {
            match fetch_cached_metadata_static(item, db).await? {
                MetadataCacheState::Hit(metadata) => return Ok(Some(metadata)),
                MetadataCacheState::NegativeHit => return Ok(None),
                MetadataCacheState::Miss => {}
            }
            fetch_remote_metadata_static(tmdb, tvdb, metadata_mode, item, db).await
        }
    }
}

async fn fetch_cached_metadata_static(
    item: &LibraryItem,
    db: &Database,
) -> Result<MetadataCacheState> {
    let cache_key = match (&item.id, item.media_type) {
        (MediaId::Tmdb(id), MediaType::Tv) => format!("tmdb:tv:{}", id),
        (MediaId::Tmdb(id), MediaType::Movie) => format!("tmdb:movie:{}", id),
        (MediaId::Tvdb(id), _) => format!("tvdb:series:{}", id),
    };

    let Some(cached) = db.get_cached(&cache_key).await? else {
        return Ok(MetadataCacheState::Miss);
    };

    match serde_json::from_str::<CachedMetadataEnvelope>(&cached) {
        Ok(envelope) if envelope._symlinkarr_not_found => Ok(MetadataCacheState::NegativeHit),
        Ok(envelope) => Ok(MetadataCacheState::Hit(ContentMetadata {
            title: envelope.title,
            aliases: envelope.aliases,
            year: envelope.year,
            seasons: envelope.seasons,
        })),
        Err(err) => {
            warn!(
                "Metadata cache decode failed for key {} ({}); ignoring cache entry",
                cache_key, err
            );
            let _ = db.invalidate_cached(&cache_key).await;
            Ok(MetadataCacheState::Miss)
        }
    }
}

async fn fetch_remote_metadata_static(
    tmdb: &Option<TmdbClient>,
    tvdb: Option<&Arc<Mutex<TvdbClient>>>,
    metadata_mode: MetadataMode,
    item: &LibraryItem,
    db: &Database,
) -> Result<Option<ContentMetadata>> {
    if !metadata_mode.allows_network() {
        return Ok(None);
    }

    match &item.id {
        MediaId::Tmdb(id) => {
            if let Some(ref tmdb) = tmdb {
                let metadata = match item.media_type {
                    MediaType::Tv => tmdb.get_tv_metadata(*id, db).await?,
                    MediaType::Movie => tmdb.get_movie_metadata(*id, db).await?,
                };
                return Ok(Some(metadata));
            }
        }
        MediaId::Tvdb(tvdb_id) => {
            if let Some(tvdb_mutex) = tvdb {
                let mut tvdb = tvdb_mutex.lock().await;
                let metadata = tvdb.get_series_metadata(*tvdb_id, db).await?;
                return Ok(Some(metadata));
            } else {
                warn!(
                    "TVDB metadata requested for {} but no TVDB client configured",
                    tvdb_id
                );
            }
        }
    }

    Ok(None)
}

fn parser_kind_for_content(content_type: ContentType) -> ParserKind {
    match content_type {
        ContentType::Anime => ParserKind::Anime,
        ContentType::Tv | ContentType::Movie => ParserKind::Standard,
    }
}

fn resolve_source_for_library_item(
    item: &LibraryItem,
    parsed: &SourceItem,
    metadata: Option<&ContentMetadata>,
    anime_identity: Option<&AnimeIdentityGraph>,
) -> Option<SourceItem> {
    if item.media_type != MediaType::Tv {
        return Some(parsed.clone());
    }

    if item.content_type != ContentType::Anime {
        if parsed.season.is_some() && parsed.episode.is_some() {
            return Some(parsed.clone());
        }
        return None;
    }

    if let (Some(season), Some(episode)) = (parsed.season, parsed.episode) {
        let mut resolved = parsed.clone();
        if let Some((mapped_season, mapped_episode)) =
            resolve_anime_scene_episode_mapping(item, metadata, anime_identity, season, episode)
        {
            resolved.season = Some(mapped_season);
            resolved.episode = Some(mapped_episode);
        }
        return Some(resolved);
    }

    if parsed.season.is_some() {
        return None;
    }

    let absolute_episode = parsed.episode?;
    let (season, episode) = anime_identity
        .and_then(|graph| graph.resolve_absolute_episode(item, absolute_episode))
        .or_else(|| resolve_anime_episode_mapping(metadata, absolute_episode))?;

    let mut resolved = parsed.clone();
    resolved.season = Some(season);
    resolved.episode = Some(episode);
    Some(resolved)
}

fn source_shape_matches_media_type(item: &LibraryItem, parsed: &SourceItem) -> bool {
    match item.media_type {
        MediaType::Tv => parsed.season.is_some() && parsed.episode.is_some(),
        MediaType::Movie => parsed.season.is_none() && parsed.episode.is_none(),
    }
}

fn extract_title_year(title: &str) -> Option<u32> {
    fn parse_year(token: &str) -> Option<u32> {
        if token.len() != 4 {
            return None;
        }
        let year: u32 = token.parse().ok()?;
        (1900..=2099).contains(&year).then_some(year)
    }

    let mut digits = String::new();
    for ch in title.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if let Some(year) = parse_year(&digits) {
            return Some(year);
        }
        digits.clear();
    }

    parse_year(&digits)
}

fn candidate_release_year(item: &LibraryItem, metadata: Option<&ContentMetadata>) -> Option<u32> {
    metadata
        .and_then(|metadata| metadata.year)
        .or_else(|| extract_title_year(&item.title))
}

fn candidate_metadata_compatible(
    item: &LibraryItem,
    parsed: &SourceItem,
    metadata: Option<&ContentMetadata>,
) -> bool {
    if let (Some(source_year), Some(candidate_year)) =
        (parsed.year, candidate_release_year(item, metadata))
    {
        if source_year != candidate_year {
            return false;
        }
    }

    if item.media_type == MediaType::Tv && item.content_type == ContentType::Tv {
        if let (Some(source_season), Some(metadata)) = (parsed.season, metadata) {
            let has_season_metadata = metadata
                .seasons
                .iter()
                .any(|season| !season.episodes.is_empty());
            if has_season_metadata
                && !metadata
                    .seasons
                    .iter()
                    .any(|season| season.season_number == source_season)
            {
                return false;
            }
        }
    }

    true
}

fn resolve_anime_scene_episode_mapping(
    item: &LibraryItem,
    metadata: Option<&ContentMetadata>,
    anime_identity: Option<&AnimeIdentityGraph>,
    parsed_season: u32,
    parsed_episode: u32,
) -> Option<(u32, u32)> {
    if parsed_episode == 0 {
        return None;
    }

    if let Some(resolved) = anime_identity
        .and_then(|graph| graph.resolve_scene_episode(item, parsed_season, parsed_episode))
    {
        return Some(resolved);
    }

    let metadata = metadata?;
    let seasons = anime_regular_seasons(metadata);
    if seasons.is_empty() {
        return None;
    }

    if let Some(season) = seasons
        .iter()
        .find(|season| season.season_number == parsed_season)
    {
        if season_has_episode(season, parsed_episode) {
            return Some((parsed_season, parsed_episode));
        }

        return None;
    }

    let max_regular_season = seasons.iter().map(|season| season.season_number).max()?;
    if parsed_season <= max_regular_season {
        return None;
    }

    anime_identity
        .and_then(|graph| graph.resolve_absolute_episode(item, parsed_episode))
        .or_else(|| resolve_anime_episode_mapping(Some(metadata), parsed_episode))
}

fn resolve_anime_episode_mapping(
    metadata: Option<&ContentMetadata>,
    absolute_episode: u32,
) -> Option<(u32, u32)> {
    if absolute_episode == 0 {
        return None;
    }

    let metadata = metadata?;
    let seasons = anime_regular_seasons(metadata);
    if seasons.is_empty() {
        return None;
    }

    if seasons.len() == 1 && season_has_episode(seasons[0], absolute_episode) {
        return Some((seasons[0].season_number, absolute_episode));
    }

    let exact_matches: Vec<u32> = seasons
        .iter()
        .filter(|season| season_has_episode(season, absolute_episode))
        .map(|season| season.season_number)
        .collect();
    let cumulative = resolve_cumulative_anime_episode(&seasons, absolute_episode);

    match (exact_matches.as_slice(), cumulative) {
        ([season], Some((cum_season, cum_episode))) => {
            if *season == cum_season && absolute_episode == cum_episode {
                Some((cum_season, cum_episode))
            } else if absolute_episode > 50 {
                // Long-running anime often keep a high season-local episode number.
                Some((*season, absolute_episode))
            } else {
                Some((cum_season, cum_episode))
            }
        }
        ([season], None) => Some((*season, absolute_episode)),
        ([], Some((season, episode))) => Some((season, episode)),
        _ => None,
    }
}

fn anime_regular_seasons(metadata: &ContentMetadata) -> Vec<&crate::models::SeasonInfo> {
    let mut seasons: Vec<_> = metadata
        .seasons
        .iter()
        .filter(|season| season.season_number > 0 && !season.episodes.is_empty())
        .collect();
    seasons.sort_by_key(|season| season.season_number);
    seasons
}

fn season_has_episode(season: &crate::models::SeasonInfo, episode: u32) -> bool {
    season
        .episodes
        .iter()
        .any(|item| item.episode_number == episode)
}

fn resolve_cumulative_anime_episode(
    seasons: &[&crate::models::SeasonInfo],
    absolute_episode: u32,
) -> Option<(u32, u32)> {
    let mut consumed = 0u32;

    for season in seasons {
        let count = season.episodes.len() as u32;
        if count == 0 {
            continue;
        }
        if absolute_episode <= consumed + count {
            return Some((season.season_number, absolute_episode - consumed));
        }
        consumed += count;
    }

    None
}

fn insert_or_replace(
    by_destination: &mut HashMap<DestinationKey, MatchCandidate>,
    key: DestinationKey,
    candidate: MatchCandidate,
) {
    match by_destination.get(&key) {
        None => {
            by_destination.insert(key, candidate);
        }
        Some(existing) => {
            if should_replace_destination(existing, &candidate) {
                by_destination.insert(key, candidate);
            }
        }
    }
}

/// Expand a source item into its covered episode numbers.
/// Single-episode files return `[episode]`; multi-episode files return `[start..=end]`.
/// Returns an empty vec for movies or items without episode info.
/// Capped at 24 episodes per file to prevent pathological expansion from parser bugs.
fn expand_episode_slots(source: &SourceItem) -> Vec<u32> {
    const MAX_MULTI_EPISODE_SPAN: u32 = 24;

    match (source.episode, source.episode_end) {
        (Some(start), Some(end)) if end > start && (end - start) < MAX_MULTI_EPISODE_SPAN => {
            (start..=end).collect()
        }
        (Some(start), Some(end)) if end > start => {
            warn!(
                "Multi-episode span too large ({}-{}); capping at first episode only: {:?}",
                start, end, source.path
            );
            vec![start]
        }
        (Some(ep), _) => vec![ep],
        _ => vec![], // Movies or items without episode info
    }
}

fn destination_key(item: &LibraryItem, source: &SourceItem) -> Option<DestinationKey> {
    let media_id = item.id.to_string();
    match item.media_type {
        MediaType::Movie => Some(DestinationKey::Movie { media_id }),
        MediaType::Tv => Some(DestinationKey::Tv {
            media_id,
            season: source.season?,
            episode: source.episode?,
        }),
    }
}

fn should_replace_destination(existing: &MatchCandidate, challenger: &MatchCandidate) -> bool {
    candidate_cmp(challenger, existing).is_gt()
}

pub(crate) fn best_alias_score(
    mode: MatchingMode,
    aliases: &[String],
    normalized_source: &str,
) -> Option<(f64, String)> {
    let mut best: Option<(f64, String)> = None;

    for alias in aliases {
        if alias.is_empty() {
            continue;
        }

        let score = if alias == normalized_source {
            1.0
        } else {
            match mode {
                MatchingMode::Strict => prefix_word_boundary_score(alias, normalized_source, 0.70),
                MatchingMode::Balanced => {
                    prefix_word_boundary_score(alias, normalized_source, 0.60)
                        .max(prefix_any_score(alias, normalized_source, 0.70) * 0.9)
                }
                MatchingMode::Aggressive => {
                    let s1 = prefix_any_score(alias, normalized_source, 0.55);
                    let s2 = contains_score(alias, normalized_source, 0.55);
                    s1.max(s2 * 0.8)
                }
            }
        };

        if score <= 0.0 {
            continue;
        }

        match &best {
            None => best = Some((score, alias.clone())),
            Some((current, current_alias)) => {
                let better = score > *current
                    || (score == *current && alias.len() > current_alias.len())
                    || (score == *current
                        && alias.len() == current_alias.len()
                        && alias < current_alias);
                if better {
                    best = Some((score, alias.clone()));
                }
            }
        }
    }

    best
}

fn prefix_word_boundary_score(alias: &str, source: &str, min_ratio: f64) -> f64 {
    if let Some(rest) = source.strip_prefix(alias) {
        if rest.starts_with(' ') {
            let ratio = alias.len() as f64 / source.len() as f64;
            if ratio >= min_ratio {
                return ratio;
            }
        }
    }
    0.0
}

fn prefix_any_score(alias: &str, source: &str, min_ratio: f64) -> f64 {
    if source.starts_with(alias) {
        let ratio = alias.len() as f64 / source.len() as f64;
        if ratio >= min_ratio {
            return ratio;
        }
    }
    0.0
}

fn contains_score(alias: &str, source: &str, min_ratio: f64) -> f64 {
    if source.contains(alias) {
        let ratio = alias.len() as f64 / source.len() as f64;
        if ratio >= min_ratio {
            return ratio;
        }
    }
    0.0
}

fn build_alias_token_index(alias_map: &HashMap<usize, Vec<String>>) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();

    for (library_idx, aliases) in alias_map {
        for alias in aliases {
            for token in alias_lookup_tokens(alias) {
                index.entry(token).or_default().push(*library_idx);
            }
        }
    }

    for indices in index.values_mut() {
        indices.sort_unstable();
        indices.dedup();
    }

    index
}

fn alias_lookup_tokens(title: &str) -> Vec<String> {
    title
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_string())
        .collect()
}

fn source_lookup_tokens(variants: &HashMap<ParserKind, SourceItem>) -> Vec<String> {
    let mut tokens = HashSet::new();

    for parsed in variants.values() {
        let normalized = normalize(&parsed.parsed_title);
        if normalized.is_empty() {
            continue;
        }
        for token in alias_lookup_tokens(&normalized) {
            tokens.insert(token);
        }
    }

    let mut sorted: Vec<String> = tokens.into_iter().collect();
    sorted.sort();
    sorted
}

/// Maximum number of library candidates passed to the scoring phase per source item.
/// Prevents O(n²) blowup when many library items share common tokens like "the" or "a".
const MAX_CANDIDATES_PER_SOURCE: usize = 50;

fn candidate_library_indices(
    variants: &HashMap<ParserKind, SourceItem>,
    alias_token_index: &HashMap<String, Vec<usize>>,
    library_count: usize,
    allow_global_fallback: bool,
) -> Vec<usize> {
    let tokens = source_lookup_tokens(variants);
    if tokens.is_empty() {
        return if allow_global_fallback {
            (0..library_count).collect()
        } else {
            Vec::new()
        };
    }

    // Count token overlaps per library index.
    let mut overlap_counts: HashMap<usize, usize> = HashMap::new();
    for token in &tokens {
        if let Some(indices) = alias_token_index.get(token) {
            for idx in indices {
                *overlap_counts.entry(*idx).or_insert(0) += 1;
            }
        }
    }

    if overlap_counts.is_empty() {
        return if allow_global_fallback {
            (0..library_count).collect()
        } else {
            Vec::new()
        };
    }

    // Sort by overlap count descending, then by index for determinism, and cap.
    let mut ranked: Vec<(usize, usize)> = overlap_counts.into_iter().collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(MAX_CANDIDATES_PER_SOURCE);

    let mut indices: Vec<usize> = ranked.into_iter().map(|(idx, _)| idx).collect();
    indices.sort_unstable();
    indices
}

fn source_path_contains_media_id(source_path: &str, media_id: &str) -> bool {
    source_path.match_indices(media_id).any(|(idx, _)| {
        let before = source_path[..idx].chars().next_back();
        let after = source_path[idx + media_id.len()..].chars().next();
        let before_ok = before.is_none_or(|ch| !ch.is_ascii_alphanumeric());
        let after_ok = after.is_none_or(|ch| !ch.is_ascii_alphanumeric());
        before_ok && after_ok
    })
}

#[allow(clippy::too_many_arguments)]
fn match_source_slice(
    start_idx: usize,
    source_items: &[SourceItem],
    library_items: &[LibraryItem],
    alias_map: &HashMap<usize, Vec<String>>,
    metadata_map: &HashMap<usize, Option<ContentMetadata>>,
    alias_token_index: &HashMap<String, Vec<usize>>,
    mode: MatchingMode,
    allow_global_fallback: bool,
    anime_identity: Option<&AnimeIdentityGraph>,
) -> MatchChunkResult {
    let parser = SourceScanner::new();
    let mut best_per_source = Vec::new();
    let mut ambiguous_skipped = 0usize;
    let mut prefiltered_library_candidates = 0usize;
    let mut scored_candidates = 0usize;
    let mut exact_id_hits = 0usize;
    let mut skip_reasons = BTreeMap::new();

    for (offset, source) in source_items.iter().enumerate() {
        let source_idx = start_idx + offset;
        let mut variants: HashMap<ParserKind, SourceItem> = HashMap::new();
        for (kind, parsed) in parser.parse_dual_variants(&source.path) {
            variants.insert(kind, parsed);
        }
        if variants.is_empty() {
            variants.insert(ParserKind::Standard, source.clone());
        }

        let mut diagnostics = MatchSkipDiagnostics::default();
        let mut candidates_by_media: HashMap<String, MatchCandidate> = HashMap::new();
        let candidate_library_indices = candidate_library_indices(
            &variants,
            alias_token_index,
            library_items.len(),
            allow_global_fallback,
        );
        let candidate_count = candidate_library_indices.len();
        prefiltered_library_candidates += candidate_count;

        // Early-exit: if the source file path contains a library item's exact media ID
        // (e.g. "tvdb-81189" embedded in the RD path), skip scoring and use it directly.
        let source_path_str = source.path.to_string_lossy();
        if let Some(exact_idx) = candidate_library_indices.iter().copied().find(|&lib_idx| {
            let id_str = library_items[lib_idx].id.to_string();
            source_path_contains_media_id(source_path_str.as_ref(), id_str.as_str())
        }) {
            let item = &library_items[exact_idx];
            let parser_kind = parser_kind_for_content(item.content_type);
            let metadata = metadata_map.get(&exact_idx).and_then(|meta| meta.as_ref());
            let parsed = variants
                .get(&parser_kind)
                .or_else(|| variants.get(&ParserKind::Standard))
                .or_else(|| variants.values().next());
            if let Some(parsed) = parsed {
                let parsed =
                    resolve_source_for_library_item(item, parsed, metadata, anime_identity);
                if let Some(parsed) = parsed {
                    diagnostics.any_resolved_candidate = true;
                    if !source_shape_matches_media_type(item, &parsed)
                    {
                        diagnostics.exact_id_incompatible = true;
                        continue;
                    }
                    diagnostics.any_shape_compatible = true;
                    if !candidate_metadata_compatible(item, &parsed, metadata) {
                        diagnostics.exact_id_incompatible = true;
                        continue;
                    }
                    diagnostics.any_metadata_compatible = true;
                    let media_id = item.id.to_string();
                    let aliases = alias_map
                        .get(&exact_idx)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    let normalized_source = normalize(&parsed.parsed_title);
                    if !normalized_source.is_empty() {
                        diagnostics.any_non_empty_title = true;
                    }
                    if !aliases.is_empty() {
                        diagnostics.any_aliases_available = true;
                    }
                    let matched_alias = aliases
                        .first()
                        .cloned()
                        .unwrap_or_else(|| item.title.clone());
                    let score = if normalized_source.is_empty() {
                        1.0
                    } else {
                        best_alias_score(mode, aliases, &normalized_source)
                            .map(|(s, _)| s)
                            .unwrap_or(1.0)
                    };
                    diagnostics.any_alias_score = true;
                    best_per_source.push(MatchCandidate {
                        source_idx,
                        library_idx: exact_idx,
                        media_id,
                        score,
                        alias: matched_alias,
                        source_item: parsed,
                    });
                    exact_id_hits += 1;
                    continue;
                }
            }
        }

        for library_idx in candidate_library_indices {
            let item = &library_items[library_idx];
            let parser_kind = parser_kind_for_content(item.content_type);
            let parsed = variants
                .get(&parser_kind)
                .or_else(|| variants.get(&ParserKind::Standard))
                .or_else(|| variants.values().next());

            let Some(parsed) = parsed else {
                continue;
            };
            let metadata = metadata_map
                .get(&library_idx)
                .and_then(|meta| meta.as_ref());
            let parsed = resolve_source_for_library_item(item, parsed, metadata, anime_identity);
            let Some(parsed) = parsed else {
                continue;
            };
            diagnostics.any_resolved_candidate = true;

            if !source_shape_matches_media_type(item, &parsed) {
                continue;
            }
            diagnostics.any_shape_compatible = true;
            if !candidate_metadata_compatible(item, &parsed, metadata) {
                continue;
            }
            diagnostics.any_metadata_compatible = true;

            let normalized_source = normalize(&parsed.parsed_title);
            if normalized_source.is_empty() {
                continue;
            }
            diagnostics.any_non_empty_title = true;

            let Some(aliases) = alias_map.get(&library_idx) else {
                continue;
            };
            if !aliases.is_empty() {
                diagnostics.any_aliases_available = true;
            }

            let Some((score, matched_alias)) = best_alias_score(mode, aliases, &normalized_source)
            else {
                continue;
            };
            diagnostics.any_alias_score = true;
            scored_candidates += 1;

            let candidate = MatchCandidate {
                source_idx,
                library_idx,
                media_id: item.id.to_string(),
                score,
                alias: matched_alias,
                source_item: parsed,
            };

            match candidates_by_media.get(&candidate.media_id) {
                Some(existing) if !is_better_candidate(&candidate, existing) => {}
                _ => {
                    candidates_by_media.insert(candidate.media_id.clone(), candidate);
                }
            }
        }

        let candidates: Vec<MatchCandidate> = candidates_by_media.into_values().collect();
        if candidates.is_empty() {
            increment_skip_reason(
                &mut skip_reasons,
                dominant_match_skip_reason(&diagnostics, candidate_count),
            );
            continue;
        }

        let (best, second) = select_top_two(&candidates);
        let Some(best) = best else {
            continue;
        };
        if let Some(second) = second {
            if should_reject_ambiguous_scores(mode, best.score, second.score) {
                debug!(
                    "Ambiguous source skipped: {:?} (top={:.3}, second={:.3})",
                    source.path, best.score, second.score
                );
                ambiguous_skipped += 1;
                increment_skip_reason(&mut skip_reasons, "ambiguous_match");
                continue;
            }
        }

        best_per_source.push(best);
    }

    MatchChunkResult {
        processed: source_items.len(),
        ambiguous_skipped,
        prefiltered_library_candidates,
        scored_candidates,
        exact_id_hits,
        skip_reasons,
        best_per_source,
    }
}

fn candidate_cmp(a: &MatchCandidate, b: &MatchCandidate) -> Ordering {
    a.score
        .partial_cmp(&b.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            quality_rank(a.source_item.quality.as_deref())
                .cmp(&quality_rank(b.source_item.quality.as_deref()))
        })
        .then_with(|| a.alias.len().cmp(&b.alias.len()))
        .then_with(|| {
            let a_path = a.source_item.path.to_string_lossy();
            let b_path = b.source_item.path.to_string_lossy();
            b_path.cmp(&a_path)
        })
}

fn quality_rank(quality: Option<&str>) -> u8 {
    match quality.map(|value| value.to_ascii_lowercase()) {
        Some(value) if value == "2160p" || value == "4k" => 4,
        Some(value) if value == "1080p" => 3,
        Some(value) if value == "720p" => 2,
        Some(value) if value == "480p" => 1,
        Some(_) => 1,
        None => 0,
    }
}

fn is_better_candidate(challenger: &MatchCandidate, existing: &MatchCandidate) -> bool {
    candidate_cmp(challenger, existing).is_gt()
}

fn select_top_two(
    candidates: &[MatchCandidate],
) -> (Option<MatchCandidate>, Option<MatchCandidate>) {
    let mut best: Option<&MatchCandidate> = None;
    let mut second: Option<&MatchCandidate> = None;

    for candidate in candidates {
        match best {
            None => {
                best = Some(candidate);
            }
            Some(current_best) if is_better_candidate(candidate, current_best) => {
                second = best;
                best = Some(candidate);
            }
            _ => match second {
                None => second = Some(candidate),
                Some(current_second) if is_better_candidate(candidate, current_second) => {
                    second = Some(candidate);
                }
                _ => {}
            },
        }
    }

    (best.cloned(), second.cloned())
}

fn should_reject_ambiguous_scores(mode: MatchingMode, best: f64, second: f64) -> bool {
    let threshold = match mode {
        MatchingMode::Strict => Some(0.08),
        MatchingMode::Balanced => Some(0.04),
        MatchingMode::Aggressive => None,
    };

    let Some(threshold) = threshold else {
        return false;
    };

    (best - second) < threshold
}

#[allow(dead_code)] // Covered via unit tests and kept for diagnostic reuse
fn should_reject_ambiguous(mode: MatchingMode, candidates: &[MatchCandidate]) -> bool {
    let (best, second) = select_top_two(candidates);
    let (Some(best), Some(second)) = (best, second) else {
        return false;
    };

    should_reject_ambiguous_scores(mode, best.score, second.score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    use crate::anime_identity::AnimeIdentityGraph;
    use crate::db::Database;
    use crate::models::{EpisodeInfo, SeasonInfo};

    fn metadata_with_seasons(seasons: &[(u32, u32)]) -> ContentMetadata {
        ContentMetadata {
            title: "Example Anime".to_string(),
            aliases: Vec::new(),
            year: Some(2024),
            seasons: seasons
                .iter()
                .map(|(season_number, episode_count)| SeasonInfo {
                    season_number: *season_number,
                    episodes: (1..=*episode_count)
                        .map(|episode_number| EpisodeInfo {
                            episode_number,
                            title: format!("Episode {}", episode_number),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn candidate(path: &str, score: f64) -> MatchCandidate {
        MatchCandidate {
            source_idx: 0,
            library_idx: 0,
            media_id: "tvdb-1".to_string(),
            score,
            alias: "x".to_string(),
            source_item: SourceItem {
                path: PathBuf::from(path),
                parsed_title: "title".to_string(),
                season: Some(1),
                episode: Some(1),
                episode_end: None,
                quality: None,
                extension: "mkv".to_string(),
                year: None,
            },
        }
    }

    fn candidate_with_quality(path: &str, score: f64, quality: Option<&str>) -> MatchCandidate {
        let mut candidate = candidate(path, score);
        candidate.source_item.quality = quality.map(|value| value.to_string());
        candidate
    }

    fn movie_item(title: &str, tmdb_id: u64) -> LibraryItem {
        LibraryItem {
            title: title.to_string(),
            path: PathBuf::from(format!("/library/{title} {{tmdb-{tmdb_id}}}")),
            id: MediaId::Tmdb(tmdb_id),
            library_name: "Movies".to_string(),
            media_type: MediaType::Movie,
            content_type: ContentType::Movie,
        }
    }

    fn tv_item(title: &str, tvdb_id: u64) -> LibraryItem {
        LibraryItem {
            title: title.to_string(),
            path: PathBuf::from(format!("/library/{title} {{tvdb-{tvdb_id}}}")),
            id: MediaId::Tvdb(tvdb_id),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        }
    }

    fn tv_metadata(title: &str, year: u32, seasons: &[u32]) -> ContentMetadata {
        ContentMetadata {
            title: title.to_string(),
            aliases: Vec::new(),
            year: Some(year),
            seasons: seasons
                .iter()
                .map(|season_number| SeasonInfo {
                    season_number: *season_number,
                    episodes: vec![EpisodeInfo {
                        episode_number: 1,
                        title: "Episode 1".to_string(),
                    }],
                })
                .collect(),
        }
    }

    fn movie_metadata(title: &str, year: u32) -> ContentMetadata {
        ContentMetadata {
            title: title.to_string(),
            aliases: Vec::new(),
            year: Some(year),
            seasons: Vec::new(),
        }
    }

    fn anime_item(title: &str, tvdb_id: u64) -> LibraryItem {
        LibraryItem {
            title: title.to_string(),
            path: PathBuf::from(format!("/library/{title} {{tvdb-{tvdb_id}}}")),
            id: MediaId::Tvdb(tvdb_id),
            library_name: "Anime".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Anime,
        }
    }

    fn parsed_standard_source(path: &str) -> SourceItem {
        SourceScanner::new()
            .parse_filename_with_type(Path::new(path), ContentType::Tv)
            .expect("expected source to parse")
    }

    fn parsed_movie_source(path: &str) -> SourceItem {
        SourceScanner::new()
            .parse_filename_with_type(Path::new(path), ContentType::Movie)
            .expect("expected movie source to parse")
    }

    #[test]
    fn test_strict_no_substring_group_collision() {
        let aliases = vec!["show".to_string()];
        assert!(best_alias_score(MatchingMode::Strict, &aliases, "showgroup 03").is_none());
        assert!(best_alias_score(MatchingMode::Strict, &aliases, "show").is_some());
    }

    #[test]
    fn test_parser_kind_for_content_type() {
        assert_eq!(
            parser_kind_for_content(ContentType::Anime),
            ParserKind::Anime
        );
        assert_eq!(
            parser_kind_for_content(ContentType::Tv),
            ParserKind::Standard
        );
    }

    #[test]
    fn test_ambiguous_rejected_in_strict() {
        let candidates = vec![candidate("/a", 0.90), candidate("/b", 0.85)];
        assert!(should_reject_ambiguous(MatchingMode::Strict, &candidates));
        assert!(!should_reject_ambiguous(
            MatchingMode::Aggressive,
            &candidates
        ));
    }

    #[test]
    fn test_destination_conflict_keeps_higher_score() {
        let existing = candidate("/old", 0.81);
        let challenger = candidate("/new", 0.92);
        assert!(should_replace_destination(&existing, &challenger));
    }

    #[test]
    fn test_destination_conflict_tie_is_deterministic() {
        let existing = candidate("/z-path", 0.90);
        let challenger = candidate("/a-path", 0.90);
        assert!(should_replace_destination(&existing, &challenger));
    }

    #[test]
    fn test_destination_conflict_prefers_higher_quality_when_scores_tie() {
        let existing = candidate_with_quality("/z-path", 0.90, Some("720p"));
        let challenger = candidate_with_quality("/a-path", 0.90, Some("1080p"));
        assert!(should_replace_destination(&existing, &challenger));
    }

    #[test]
    fn test_candidate_prefilter_matches_relevant_library_indices() {
        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["breaking bad".to_string()]);
        alias_map.insert(1usize, vec!["game of thrones".to_string()]);
        alias_map.insert(2usize, vec!["jujutsu kaisen".to_string()]);
        let index = build_alias_token_index(&alias_map);

        let mut variants = HashMap::new();
        variants.insert(
            ParserKind::Standard,
            SourceItem {
                path: PathBuf::from("/rd/Breaking.Bad.S01E01.mkv"),
                parsed_title: "Breaking Bad".to_string(),
                season: Some(1),
                episode: Some(1),
                episode_end: None,
                quality: None,
                extension: "mkv".to_string(),
                year: None,
            },
        );

        let indices = candidate_library_indices(&variants, &index, 3, true);
        assert_eq!(indices, vec![0]);
    }

    #[test]
    fn test_candidate_prefilter_falls_back_to_all_when_no_token_hit() {
        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["breaking bad".to_string()]);
        alias_map.insert(1usize, vec!["game of thrones".to_string()]);
        let index = build_alias_token_index(&alias_map);

        let mut variants = HashMap::new();
        variants.insert(
            ParserKind::Standard,
            SourceItem {
                path: PathBuf::from("/rd/Some.Unknown.Show.S01E01.mkv"),
                parsed_title: "Completely Unknown".to_string(),
                season: Some(1),
                episode: Some(1),
                episode_end: None,
                quality: None,
                extension: "mkv".to_string(),
                year: None,
            },
        );

        let indices = candidate_library_indices(&variants, &index, 2, true);
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn test_candidate_prefilter_can_skip_global_fallback() {
        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["breaking bad".to_string()]);
        alias_map.insert(1usize, vec!["game of thrones".to_string()]);
        let index = build_alias_token_index(&alias_map);

        let mut variants = HashMap::new();
        variants.insert(
            ParserKind::Standard,
            SourceItem {
                path: PathBuf::from("/rd/Some.Unknown.Show.S01E01.mkv"),
                parsed_title: "Completely Unknown".to_string(),
                season: Some(1),
                episode: Some(1),
                episode_end: None,
                quality: None,
                extension: "mkv".to_string(),
                year: None,
            },
        );

        let indices = candidate_library_indices(&variants, &index, 2, false);
        assert!(indices.is_empty());
    }

    #[test]
    fn test_resolve_anime_episode_mapping_single_season_bare_episode() {
        let metadata = metadata_with_seasons(&[(1, 12)]);
        assert_eq!(
            resolve_anime_episode_mapping(Some(&metadata), 3),
            Some((1, 3))
        );
    }

    #[test]
    fn test_resolve_anime_episode_mapping_multi_season_absolute_numbering() {
        let metadata = metadata_with_seasons(&[(1, 12), (2, 12)]);
        assert_eq!(
            resolve_anime_episode_mapping(Some(&metadata), 21),
            Some((2, 9))
        );
    }

    #[test]
    fn test_resolve_anime_episode_mapping_prefers_unique_high_episode_local_match() {
        let metadata = metadata_with_seasons(&[(1, 12), (20, 130)]);
        assert_eq!(
            resolve_anime_episode_mapping(Some(&metadata), 129),
            Some((20, 129))
        );
    }

    #[test]
    fn test_resolve_anime_scene_episode_mapping_falls_back_to_absolute_episode() {
        let metadata = metadata_with_seasons(&[(1, 12), (20, 130)]);
        let item = anime_item("Example Anime", 456);
        assert_eq!(
            resolve_anime_scene_episode_mapping(&item, Some(&metadata), None, 25, 129),
            Some((20, 129))
        );
    }

    #[test]
    fn test_resolve_anime_scene_episode_mapping_does_not_treat_in_season_numbers_as_absolute() {
        let metadata = metadata_with_seasons(&[(1, 12), (3, 12), (20, 130)]);
        let item = anime_item("Example Anime", 456);
        assert_eq!(
            resolve_anime_scene_episode_mapping(&item, Some(&metadata), None, 3, 129),
            None
        );
    }

    #[test]
    fn test_resolve_source_for_library_item_uses_anime_identity_for_absolute_numbering() {
        let metadata = metadata_with_seasons(&[(1, 12), (2, 12)]);
        let item = anime_item("Example Anime", 22222);
        let parsed = SourceItem {
            path: PathBuf::from("/rd/[SubsPlease] Example Anime - 15.mkv"),
            parsed_title: "Example Anime".to_string(),
            season: None,
            episode: Some(15),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        };
        let graph = AnimeIdentityGraph::from_xml(
            r#"<?xml version="1.0" encoding="utf-8"?>
<anime-list>
  <anime anidbid="101" tvdbid="22222" defaulttvdbseason="2">
    <name>Example Anime</name>
    <mapping-list>
      <mapping anidbseason="1" tvdbseason="2">;13-1;14-2;15-3;</mapping>
    </mapping-list>
  </anime>
</anime-list>
"#,
        )
        .unwrap();

        let resolved =
            resolve_source_for_library_item(&item, &parsed, Some(&metadata), Some(&graph)).unwrap();
        assert_eq!(resolved.season, Some(2));
        assert_eq!(resolved.episode, Some(3));
    }

    #[test]
    fn test_resolve_source_for_library_item_prefers_anime_identity_scene_mapping_over_metadata() {
        let metadata = metadata_with_seasons(&[(1, 24), (2, 12)]);
        let item = anime_item("Example Anime", 33333);
        let parsed = SourceItem {
            path: PathBuf::from("/rd/[SubsPlease] Example Anime S01E15.mkv"),
            parsed_title: "Example Anime".to_string(),
            season: Some(1),
            episode: Some(15),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        };
        let graph = AnimeIdentityGraph::from_xml(
            r#"<?xml version="1.0" encoding="utf-8"?>
<anime-list>
  <anime anidbid="102" tvdbid="33333" defaulttvdbseason="1">
    <name>Example Anime</name>
    <mapping-list>
      <mapping anidbseason="1" tvdbseason="2">;15-3;</mapping>
    </mapping-list>
  </anime>
</anime-list>
"#,
        )
        .unwrap();

        let resolved =
            resolve_source_for_library_item(&item, &parsed, Some(&metadata), Some(&graph)).unwrap();
        assert_eq!(resolved.season, Some(2));
        assert_eq!(resolved.episode, Some(3));
    }

    #[test]
    fn test_movie_source_shape_rejects_episode_like_source() {
        let item = movie_item("The Avengers", 24428);
        let source = parsed_standard_source("/rd/Avengers.Assemble.S01E01.mkv");
        assert!(!source_shape_matches_media_type(&item, &source));
    }

    #[test]
    fn test_match_source_slice_skips_movie_candidate_for_episode_source() {
        let library_items = vec![movie_item("The Avengers", 24428)];
        let source_items = vec![parsed_standard_source("/rd/Avengers.Assemble.S01E01.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["avengers assemble".to_string()]);
        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, None);
        let alias_token_index = build_alias_token_index(&alias_map);

        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert!(chunk.best_per_source.is_empty());
        assert_eq!(
            chunk.skip_reasons.get("matcher_media_shape_mismatch"),
            Some(&1)
        );
    }

    #[test]
    fn test_exact_id_path_respects_movie_episode_shape_guard() {
        let library_items = vec![movie_item("The Avengers", 24428)];
        let source_items = vec![parsed_standard_source(
            "/rd/tmdb-24428/Avengers.Assemble.S01E01.mkv",
        )];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["avengers assemble".to_string()]);
        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, None);
        let alias_token_index = build_alias_token_index(&alias_map);

        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert!(chunk.best_per_source.is_empty());
        assert_eq!(chunk.exact_id_hits, 0);
    }

    #[test]
    fn test_source_path_contains_media_id_requires_boundaries() {
        assert!(source_path_contains_media_id(
            "/rd/tmdb-24428/Movie.mkv",
            "tmdb-24428"
        ));
        assert!(source_path_contains_media_id(
            "/rd/[tmdb-24428]/Movie.mkv",
            "tmdb-24428"
        ));
        assert!(!source_path_contains_media_id(
            "/rd/tmdb-244281/Movie.mkv",
            "tmdb-24428"
        ));
        assert!(!source_path_contains_media_id(
            "/rd/pretmdb-24428/Movie.mkv",
            "tmdb-24428"
        ));
    }

    #[test]
    fn test_exact_id_path_does_not_match_prefix_of_longer_id() {
        let library_items = vec![movie_item("Example", 60), movie_item("Example", 6043)];
        let source_items = vec![parsed_standard_source("/rd/tmdb-6043/Example.2024.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["example".to_string()]);
        alias_map.insert(1usize, vec!["example".to_string()]);
        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, None);
        metadata_map.insert(1usize, None);
        let alias_token_index = build_alias_token_index(&alias_map);

        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert_eq!(chunk.best_per_source.len(), 1);
        assert_eq!(chunk.best_per_source[0].media_id, "tmdb-6043");
        assert_eq!(chunk.exact_id_hits, 1);
    }

    #[test]
    fn test_tv_same_title_year_selects_matching_series() {
        let library_items = vec![
            tv_item("Dark Matter (2024)", 2024),
            tv_item("Dark Matter (2015)", 2015),
        ];
        let source_items = vec![parsed_standard_source("/rd/Dark.Matter.2024.S01E01.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(
            0usize,
            vec!["dark matter 2024".to_string(), "dark matter".to_string()],
        );
        alias_map.insert(
            1usize,
            vec!["dark matter 2015".to_string(), "dark matter".to_string()],
        );

        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, Some(tv_metadata("Dark Matter", 2024, &[1])));
        metadata_map.insert(1usize, Some(tv_metadata("Dark Matter", 2015, &[1, 2, 3])));

        let alias_token_index = build_alias_token_index(&alias_map);
        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert_eq!(chunk.best_per_source.len(), 1);
        assert_eq!(chunk.best_per_source[0].media_id, "tvdb-2024");
    }

    #[test]
    fn test_tv_wrong_year_candidate_rejected_when_correct_show_lacks_plain_alias() {
        let library_items = vec![
            tv_item("Dark Matter (2024)", 2024),
            tv_item("Dark Matter (2015)", 2015),
        ];
        let source_items = vec![parsed_standard_source("/rd/Dark.Matter.2024.S01E01.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["dark matter 2024".to_string()]);
        alias_map.insert(
            1usize,
            vec!["dark matter 2015".to_string(), "dark matter".to_string()],
        );

        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, None);
        metadata_map.insert(1usize, Some(tv_metadata("Dark Matter", 2015, &[1, 2, 3])));

        let alias_token_index = build_alias_token_index(&alias_map);
        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert!(chunk.best_per_source.is_empty());
    }

    #[test]
    fn test_match_source_slice_records_no_library_candidates_reason() {
        let library_items = vec![anime_item("Example Anime", 1234)];
        let source_items = vec![parsed_standard_source("/rd/Completely.Different.S01E01.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["example anime".to_string()]);
        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, Some(tv_metadata("Example Anime", 2024, &[1])));
        let alias_token_index = build_alias_token_index(&alias_map);

        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            false,
            None,
        );

        assert!(chunk.best_per_source.is_empty());
        assert_eq!(
            chunk.skip_reasons.get("matcher_no_library_candidates"),
            Some(&1)
        );
    }

    #[test]
    fn test_match_source_slice_records_metadata_mismatch_reason() {
        let library_items = vec![movie_item("The Thing (1982)", 1982)];
        let source_items =
            vec![parsed_movie_source("/rd/The.Thing.2011.1080p.BluRay.x264-GROUP.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["the thing".to_string()]);
        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, Some(movie_metadata("The Thing", 1982)));
        let alias_token_index = build_alias_token_index(&alias_map);

        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert!(chunk.best_per_source.is_empty());
        assert_eq!(
            chunk.skip_reasons.get("matcher_metadata_mismatch"),
            Some(&1)
        );
    }

    #[test]
    fn test_match_source_slice_records_alias_threshold_reason() {
        let library_items = vec![movie_item("The Avengers", 24428)];
        let source_items = vec![parsed_movie_source("/rd/Avengers.of.Ultron.2015.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(0usize, vec!["the avengers".to_string()]);
        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, None);
        let alias_token_index = build_alias_token_index(&alias_map);

        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert!(chunk.best_per_source.is_empty());
        assert_eq!(
            chunk.skip_reasons.get("matcher_alias_score_below_threshold"),
            Some(&1)
        );
    }

    #[test]
    fn test_tv_same_title_season_guard_prefers_series_with_known_season() {
        let library_items = vec![
            tv_item("Dark Matter (2024)", 2024),
            tv_item("Dark Matter (2015)", 2015),
        ];
        let source_items = vec![parsed_standard_source("/rd/Dark.Matter.S03E01.mkv")];

        let mut alias_map = HashMap::new();
        alias_map.insert(
            0usize,
            vec!["dark matter 2024".to_string(), "dark matter".to_string()],
        );
        alias_map.insert(
            1usize,
            vec!["dark matter 2015".to_string(), "dark matter".to_string()],
        );

        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, Some(tv_metadata("Dark Matter", 2024, &[1])));
        metadata_map.insert(1usize, Some(tv_metadata("Dark Matter", 2015, &[1, 2, 3])));

        let alias_token_index = build_alias_token_index(&alias_map);
        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert_eq!(chunk.best_per_source.len(), 1);
        assert_eq!(chunk.best_per_source[0].media_id, "tvdb-2015");
    }

    #[test]
    fn test_movie_same_title_year_selects_matching_release() {
        let library_items = vec![
            movie_item("The Thing (1982)", 1982),
            movie_item("The Thing (2011)", 2011),
        ];
        let source_items = vec![parsed_movie_source(
            "/rd/The.Thing.2011.1080p.BluRay.x264-GROUP.mkv",
        )];

        let mut alias_map = HashMap::new();
        alias_map.insert(
            0usize,
            vec!["the thing 1982".to_string(), "the thing".to_string()],
        );
        alias_map.insert(
            1usize,
            vec!["the thing 2011".to_string(), "the thing".to_string()],
        );

        let mut metadata_map = HashMap::new();
        metadata_map.insert(0usize, Some(movie_metadata("The Thing", 1982)));
        metadata_map.insert(1usize, Some(movie_metadata("The Thing", 2011)));

        let alias_token_index = build_alias_token_index(&alias_map);
        let chunk = match_source_slice(
            0,
            &source_items,
            &library_items,
            &alias_map,
            &metadata_map,
            &alias_token_index,
            MatchingMode::Strict,
            true,
            None,
        );

        assert_eq!(chunk.best_per_source.len(), 1);
        assert_eq!(chunk.best_per_source[0].media_id, "tmdb-2011");
    }

    #[tokio::test]
    async fn test_metadata_off_returns_none() {
        let tmp = tempdir().unwrap();
        let db = Database::new(tmp.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let item = LibraryItem {
            title: "Example Show".to_string(),
            path: PathBuf::from("/library/Example Show {tmdb-123}"),
            id: MediaId::Tmdb(123),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        };

        let matcher = Matcher::new(None, None, MatchingMode::Strict, MetadataMode::Off, 1);
        let metadata = matcher.get_metadata(&item, &db).await.unwrap();
        assert!(metadata.is_none());
    }

    #[tokio::test]
    async fn test_metadata_cache_only_reads_cached_entry() {
        let tmp = tempdir().unwrap();
        let db = Database::new(tmp.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let cached = ContentMetadata {
            title: "Example Show".to_string(),
            aliases: vec!["Example Alias".to_string()],
            year: Some(2024),
            seasons: vec![],
        };
        db.set_cached("tmdb:tv:123", &serde_json::to_string(&cached).unwrap(), 24)
            .await
            .unwrap();

        let item = LibraryItem {
            title: "Example Show".to_string(),
            path: PathBuf::from("/library/Example Show {tmdb-123}"),
            id: MediaId::Tmdb(123),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        };

        let matcher = Matcher::new(None, None, MatchingMode::Strict, MetadataMode::CacheOnly, 1);
        let metadata = matcher.get_metadata(&item, &db).await.unwrap();
        assert_eq!(metadata.unwrap().aliases, vec!["Example Alias".to_string()]);
    }

    #[tokio::test]
    async fn test_negative_metadata_cache_skips_remote_lookup() {
        let tmp = tempdir().unwrap();
        let db = Database::new(tmp.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.set_cached(
            "tvdb:series:456",
            r#"{"_symlinkarr_not_found":true,"title":"","aliases":[],"year":null,"seasons":[]}"#,
            24,
        )
        .await
        .unwrap();

        let item = LibraryItem {
            title: "Example Anime".to_string(),
            path: PathBuf::from("/library/Example Anime {tvdb-456}"),
            id: MediaId::Tvdb(456),
            library_name: "Anime".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Anime,
        };

        let matcher = Matcher::new(None, None, MatchingMode::Strict, MetadataMode::Full, 1);
        let metadata = matcher.get_metadata(&item, &db).await.unwrap();
        assert!(metadata.is_none());
    }

    #[test]
    fn test_expand_episode_slots_single() {
        let source = SourceItem {
            path: PathBuf::from("/rd/Show.S01E05.mkv"),
            parsed_title: "show".to_string(),
            season: Some(1),
            episode: Some(5),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        };
        assert_eq!(expand_episode_slots(&source), vec![5]);
    }

    #[test]
    fn test_expand_episode_slots_multi() {
        let source = SourceItem {
            path: PathBuf::from("/rd/Show.S01E01E02E03.mkv"),
            parsed_title: "show".to_string(),
            season: Some(1),
            episode: Some(1),
            episode_end: Some(3),
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        };
        assert_eq!(expand_episode_slots(&source), vec![1, 2, 3]);
    }

    #[test]
    fn test_expand_episode_slots_movie() {
        let source = SourceItem {
            path: PathBuf::from("/rd/Movie.2024.mkv"),
            parsed_title: "movie".to_string(),
            season: None,
            episode: None,
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: Some(2024),
        };
        assert!(expand_episode_slots(&source).is_empty());
    }

    #[test]
    fn test_expand_episode_slots_caps_pathological_range() {
        let source = SourceItem {
            path: PathBuf::from("/rd/Show.S01E01-E999.mkv"),
            parsed_title: "show".to_string(),
            season: Some(1),
            episode: Some(1),
            episode_end: Some(999),
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        };
        // Should fall back to first episode only due to cap
        assert_eq!(expand_episode_slots(&source), vec![1]);
    }
}
