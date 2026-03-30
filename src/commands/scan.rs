use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::time::sleep;
use tracing::info;

use crate::api::bazarr::BazarrClient;
use crate::api::plex::{plan_refresh_batches, PlexClient};
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::auto_acquire::{
    process_auto_acquire_queue, AutoAcquireBatchSummary, AutoAcquireRequest, RelinkCheck,
};
use crate::commands::{
    decypharr_arr_name, ensure_runtime_directories_healthy, ensure_runtime_sources_healthy,
    is_safe_auto_acquire_query, prowlarr_categories, selected_libraries,
};
use crate::config::Config;
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::linker::{LinkProcessSummary, Linker};
use crate::matcher::{MatchRunOutput, MatchTelemetry, Matcher};
use crate::models::{LibraryItem, MatchResult, MediaId, MediaType, SourceItem};
use crate::source_scanner::SourceScanner;
use crate::utils::{stdout_text_guard, user_println};
use crate::OutputFormat;

#[derive(Debug, Clone, Default)]
struct SourceInventoryTelemetry {
    cache_enabled: bool,
    cache_downloaded_torrents: Option<usize>,
    cache_total_torrents: Option<usize>,
    cache_coverage: Option<f64>,
    cached_sources: usize,
    filesystem_sources: usize,
    cached_items: usize,
    filesystem_items: usize,
}

impl SourceInventoryTelemetry {
    fn cache_hit_ratio(&self) -> Option<f64> {
        let total_items = self.cached_items + self.filesystem_items;
        (total_items > 0).then_some(self.cached_items as f64 / total_items as f64)
    }
}

#[derive(Debug, Clone, Default)]
struct PlexRefreshTelemetry {
    requested_paths: usize,
    unique_paths: usize,
    planned_batches: usize,
    coalesced_batches: usize,
    coalesced_paths: usize,
    refreshed_batches: usize,
    refreshed_paths_covered: usize,
    skipped_batches: usize,
    unresolved_paths: usize,
    capped_batches: usize,
    failed_batches: usize,
}

#[derive(Debug, Clone, Default)]
struct ScanTelemetry {
    runtime_checks: Duration,
    library_scan: Duration,
    source_inventory: Duration,
    source_inventory_stats: SourceInventoryTelemetry,
    match_total: Duration,
    match_stats: MatchTelemetry,
    episode_title_enrichment: Duration,
    linking: Duration,
    plex_refresh: Duration,
    plex_refresh_stats: PlexRefreshTelemetry,
    dead_link_sweep: Duration,
}

/// Run a single scan → match → link cycle.
pub(crate) async fn run_scan(
    cfg: &Config,
    db: &Database,
    dry_run: bool,
    search_missing: bool,
    output: OutputFormat,
    library_filter: Option<&str>,
) -> Result<(i64, i64)> {
    let _stdout_guard = stdout_text_guard(output != OutputFormat::Json);
    info!("=== Symlinkarr Scan ===");
    let mut telemetry = ScanTelemetry::default();
    let mut auto_acquire_summary = AutoAcquireBatchSummary::default();
    let mut auto_acquire_missing_requests = 0usize;
    let mut auto_acquire_cutoff_requests = 0usize;

    let selected_libraries = selected_libraries(cfg, library_filter)?;

    let runtime_checks_started = Instant::now();
    ensure_runtime_directories_healthy(&selected_libraries, &cfg.sources, "scan startup").await?;
    telemetry.runtime_checks = runtime_checks_started.elapsed();

    let library_scan_started = Instant::now();
    let lib_scanner = LibraryScanner::new();
    let mut library_items = Vec::new();
    for lib in &selected_libraries {
        library_items.extend(lib_scanner.scan_library(lib));
    }
    library_items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    telemetry.library_scan = library_scan_started.elapsed();
    info!(
        "Step 1/4: {} library items identified in {}",
        library_items.len(),
        fmt_duration(telemetry.library_scan)
    );

    let source_inventory_started = Instant::now();
    let (source_items, source_inventory_stats) = collect_source_items(cfg, db).await?;
    telemetry.source_inventory = source_inventory_started.elapsed();
    telemetry.source_inventory_stats = source_inventory_stats;
    info!(
        "Step 2/4: {} source files found in {}",
        source_items.len(),
        fmt_duration(telemetry.source_inventory)
    );

    let tmdb = if cfg.has_tmdb() {
        Some(TmdbClient::new(
            &cfg.api.tmdb_api_key,
            Some(&cfg.api.tmdb_read_access_token),
            cfg.api.cache_ttl_hours,
        ))
    } else {
        if cfg.matching.metadata_mode.allows_network() {
            tracing::warn!("No TMDB API key configured — matching limited to local titles");
        } else {
            tracing::info!(
                "TMDB API key not configured; metadata mode {:?} avoids network lookups",
                cfg.matching.metadata_mode
            );
        }
        None
    };

    let tvdb = if cfg.has_tvdb() {
        Some(TvdbClient::new(
            &cfg.api.tvdb_api_key,
            cfg.api.cache_ttl_hours,
        ))
    } else {
        None
    };

    let matcher = Matcher::new(
        tmdb.clone(),
        tvdb,
        cfg.matching.mode,
        cfg.matching.metadata_mode,
        cfg.matching.metadata_concurrency,
    );

    let matching_started = Instant::now();
    let MatchRunOutput {
        mut matches,
        telemetry: match_telemetry,
    } = matcher
        .find_matches_with_telemetry(&library_items, &source_items, db)
        .await?;
    telemetry.match_total = matching_started.elapsed();
    telemetry.match_stats = match_telemetry;
    info!(
        "Step 3/4: {} matches confirmed in {}",
        matches.len(),
        fmt_duration(telemetry.match_total)
    );

    let effective_dry_run = dry_run || cfg.symlink.dry_run;
    let linker = Linker::new_with_options(
        effective_dry_run,
        cfg.matching.mode.is_strict(),
        &cfg.symlink.naming_template,
        cfg.features.reconcile_links,
    );

    let title_enrichment_started = Instant::now();
    matcher.enrich_episode_titles(&mut matches, db).await?;
    telemetry.episode_title_enrichment = title_enrichment_started.elapsed();

    let linking_started = Instant::now();
    let link_summary = linker.process_matches(&matches, db).await?;
    telemetry.linking = linking_started.elapsed();
    info!(
        "Step 4/4: symlinks created={}, updated={}, skipped={} in {}",
        link_summary.created,
        link_summary.updated,
        link_summary.skipped,
        fmt_duration(telemetry.linking)
    );

    let linked_total = link_summary.created + link_summary.updated;
    if linked_total > 0 && !effective_dry_run && cfg.has_bazarr() {
        let bazarr = BazarrClient::new(&cfg.bazarr);
        match bazarr.trigger_sync().await {
            Ok(_) => user_println("   📝 Bazarr: subtitle search triggered for new content"),
            Err(e) => user_println(format!("   ⚠️  Bazarr subtitle trigger failed: {}", e)),
        }
    }

    if linked_total > 0 && !effective_dry_run && cfg.has_plex() {
        let plex_refresh_started = Instant::now();
        match trigger_plex_refresh(cfg, &link_summary.refresh_paths).await {
            Ok(plex_stats) => {
                telemetry.plex_refresh = plex_refresh_started.elapsed();
                telemetry.plex_refresh_stats = plex_stats;
            }
            Err(e) => {
                telemetry.plex_refresh = plex_refresh_started.elapsed();
                telemetry.plex_refresh_stats.requested_paths = link_summary.refresh_paths.len();
                telemetry.plex_refresh_stats.skipped_batches = 1;
                user_println(format!("   ⚠️  Plex refresh failed: {}", e));
            }
        }
    }

    db.record_scan(
        library_items.len() as i64,
        source_items.len() as i64,
        matches.len() as i64,
        linked_total as i64,
    )
    .await?;

    let dead = if search_missing {
        ensure_runtime_sources_healthy(&cfg.sources, "scan dead-link sweep").await?;
        let dead_started = Instant::now();
        let library_roots: Vec<_> = selected_libraries
            .iter()
            .map(|lib| lib.path.clone())
            .collect();
        let dead = linker
            .check_dead_links_scoped(db, Some(&library_roots))
            .await?;
        telemetry.dead_link_sweep = dead_started.elapsed();
        if dead.dead_marked > 0 {
            info!(
                "Dead links: marked={}, removed={}, skipped={}",
                dead.dead_marked, dead.removed, dead.skipped
            );
        }
        dead
    } else {
        info!("Skipping full dead-link sweep during scan for performance");
        crate::linker::DeadLinkSummary::default()
    };

    log_scan_telemetry(&telemetry, &matches, &link_summary);

    if search_missing && (cfg.has_prowlarr() || cfg.has_dmm()) && cfg.has_decypharr() {
        if !effective_dry_run {
            user_println(
                "\n   ⚠️  --search-missing triggers external grabs. Ensure you intended side effects.",
            );
        }

        let matched_ids: HashSet<_> = matches.iter().map(|m| &m.library_item.id).collect();
        let matched_media_ids: HashSet<String> = matches
            .iter()
            .map(|m| m.library_item.id.to_string())
            .collect();
        let unmatched: Vec<_> = library_items
            .iter()
            .filter(|item| !matched_ids.contains(&item.id))
            .collect();
        let mut requests = Vec::new();
        let max_grabs = cfg.decypharr.max_requests_per_run;

        if !unmatched.is_empty() {
            user_println(format!(
                "\n   🔍 Auto-acquire: evaluating {} unmatched library items...",
                unmatched.len()
            ));
            let mut attempted_queries = HashSet::new();

            for item in &unmatched {
                if requests.len() >= max_grabs {
                    user_println(format!(
                        "\n   ⚠️  Auto-acquire: reached safety limit of {} requests. Stopping queue build.",
                        max_grabs
                    ));
                    break;
                }
                if item.media_type == MediaType::Tv {
                    continue;
                }

                let Some(query) = build_missing_search_query(item) else {
                    user_println(format!(
                        "      ⚠️  '{}' → skipped auto-grab (query too ambiguous)",
                        item.title
                    ));
                    continue;
                };
                if !attempted_queries.insert(query.clone()) {
                    continue;
                }
                let cats = prowlarr_categories(item.media_type, item.content_type);
                let request = AutoAcquireRequest {
                    label: item.title.clone(),
                    query: query.clone(),
                    query_hints: Vec::new(),
                    imdb_id: lookup_item_imdb_id(tmdb.as_ref(), db, item).await,
                    categories: cats,
                    arr: decypharr_arr_name(cfg, item.media_type, item.content_type).to_string(),
                    library_filter: Some(item.library_name.clone()),
                    relink_check: RelinkCheck::MediaId(item.id.to_string()),
                };
                requests.push(request);
            }
        }

        if requests.len() < max_grabs {
            let remaining = max_grabs - requests.len();
            let cutoff_budget = if remaining >= 4 { remaining / 2 } else { 0 };
            let missing_budget = remaining.saturating_sub(cutoff_budget);
            let anime_identity = crate::anime_identity::AnimeIdentityGraph::load(cfg, db).await;
            let anime_ctx = crate::anime_scanner::AnimeAcquireContext {
                cfg,
                db,
                tmdb: tmdb.as_ref(),
                anime_identity: anime_identity.as_ref(),
            };

            let anime_missing_requests = match crate::anime_scanner::build_anime_episode_requests(
                crate::anime_scanner::AnimeEpisodeKind::Missing,
                anime_ctx,
                &library_items,
                &matched_media_ids,
                missing_budget,
            )
            .await
            {
                Ok(requests) => requests,
                Err(err) => {
                    user_println(format!(
                        "   ⚠️  Sonarr Anime missing lookup failed: {}. Continuing without episode-missing acquire.",
                        err
                    ));
                    Vec::new()
                }
            };
            auto_acquire_missing_requests = anime_missing_requests.len();
            if !anime_missing_requests.is_empty() {
                user_println(format!(
                    "   🎌 Sonarr Anime: queued {} episode-specific missing request(s)",
                    anime_missing_requests.len()
                ));
                requests.extend(anime_missing_requests);
            }

            let remaining = max_grabs - requests.len();
            if remaining > 0 {
                let anime_cutoff_requests =
                    match crate::anime_scanner::build_anime_episode_requests(
                        crate::anime_scanner::AnimeEpisodeKind::CutoffUpgrade,
                        anime_ctx,
                        &library_items,
                        &matched_media_ids,
                        remaining,
                    )
                    .await
                    {
                        Ok(requests) => requests,
                        Err(err) => {
                            user_println(format!(
                                "   ⚠️  Sonarr Anime cutoff lookup failed: {}. Continuing without cutoff upgrades.",
                                err
                            ));
                            Vec::new()
                        }
                    };
                auto_acquire_cutoff_requests = anime_cutoff_requests.len();
                if !anime_cutoff_requests.is_empty() {
                    user_println(format!(
                        "   🎌 Sonarr Anime: queued {} cutoff-upgrade request(s)",
                        anime_cutoff_requests.len()
                    ));
                    requests.extend(anime_cutoff_requests);
                }
            }
        }

        match process_auto_acquire_queue(cfg, db, requests, effective_dry_run).await {
            Ok(summary) => {
                auto_acquire_summary = summary.clone();
                if summary.total > 0 {
                    user_println(format!(
                        "\n   📡 Auto-acquire summary: submitted={}, linked={}, completed_unlinked={}, no_result={}, blocked={}, failed={}",
                        summary.submitted,
                        summary.completed_linked,
                        summary.completed_unlinked,
                        summary.no_result,
                        summary.blocked,
                        summary.failed
                    ));
                }
            }
            Err(err) => {
                user_println(format!(
                    "\n   ⚠️  Auto-acquire failed: {}. Scan completed without external acquisition.",
                    err
                ));
            }
        }
    } else if search_missing && !cfg.has_prowlarr() && !cfg.has_dmm() {
        user_println(
            "\n   ⚠️  --search-missing specified but neither Prowlarr nor DMM is configured",
        );
    } else if search_missing && !cfg.has_decypharr() {
        user_println("\n   ⚠️  --search-missing specified but Decypharr not configured");
    }

    db.record_scan_run(&crate::db::ScanRunRecord {
        dry_run: effective_dry_run,
        library_filter: library_filter.map(str::to_string),
        search_missing,
        library_items_found: library_items.len() as i64,
        source_items_found: source_items.len() as i64,
        matches_found: matches.len() as i64,
        links_created: link_summary.created as i64,
        links_updated: link_summary.updated as i64,
        dead_marked: dead.dead_marked as i64,
        links_removed: dead.removed as i64,
        links_skipped: (link_summary.skipped + dead.skipped) as i64,
        ambiguous_skipped: telemetry.match_stats.ambiguous_skipped as i64,
        runtime_checks_ms: duration_ms_i64(telemetry.runtime_checks),
        library_scan_ms: duration_ms_i64(telemetry.library_scan),
        source_inventory_ms: duration_ms_i64(telemetry.source_inventory),
        matching_ms: duration_ms_i64(telemetry.match_total),
        title_enrichment_ms: duration_ms_i64(telemetry.episode_title_enrichment),
        linking_ms: duration_ms_i64(telemetry.linking),
        plex_refresh_ms: duration_ms_i64(telemetry.plex_refresh),
        plex_refresh_requested_paths: telemetry.plex_refresh_stats.requested_paths as i64,
        plex_refresh_unique_paths: telemetry.plex_refresh_stats.unique_paths as i64,
        plex_refresh_planned_batches: telemetry.plex_refresh_stats.planned_batches as i64,
        plex_refresh_coalesced_batches: telemetry.plex_refresh_stats.coalesced_batches as i64,
        plex_refresh_coalesced_paths: telemetry.plex_refresh_stats.coalesced_paths as i64,
        plex_refresh_refreshed_batches: telemetry.plex_refresh_stats.refreshed_batches as i64,
        plex_refresh_refreshed_paths_covered: telemetry.plex_refresh_stats.refreshed_paths_covered
            as i64,
        plex_refresh_skipped_batches: telemetry.plex_refresh_stats.skipped_batches as i64,
        plex_refresh_unresolved_paths: telemetry.plex_refresh_stats.unresolved_paths as i64,
        plex_refresh_capped_batches: telemetry.plex_refresh_stats.capped_batches as i64,
        plex_refresh_failed_batches: telemetry.plex_refresh_stats.failed_batches as i64,
        dead_link_sweep_ms: duration_ms_i64(telemetry.dead_link_sweep),
        cache_hit_ratio: telemetry.source_inventory_stats.cache_hit_ratio(),
        candidate_slots: telemetry.match_stats.prefiltered_library_candidates as i64,
        scored_candidates: telemetry.match_stats.scored_candidates as i64,
        exact_id_hits: telemetry.match_stats.exact_id_hits as i64,
        auto_acquire_requests: auto_acquire_summary.total as i64,
        auto_acquire_missing_requests: auto_acquire_missing_requests as i64,
        auto_acquire_cutoff_requests: auto_acquire_cutoff_requests as i64,
        auto_acquire_dry_run_hits: auto_acquire_summary.dry_run as i64,
        auto_acquire_submitted: auto_acquire_summary.submitted as i64,
        auto_acquire_no_result: auto_acquire_summary.no_result as i64,
        auto_acquire_blocked: auto_acquire_summary.blocked as i64,
        auto_acquire_failed: auto_acquire_summary.failed as i64,
        auto_acquire_completed_linked: auto_acquire_summary.completed_linked as i64,
        auto_acquire_completed_unlinked: auto_acquire_summary.completed_unlinked as i64,
    })
    .await?;

    info!("=== Scan Complete ===");
    Ok((linked_total as i64, dead.removed as i64))
}

async fn collect_source_items(
    cfg: &Config,
    db: &Database,
) -> Result<(Vec<SourceItem>, SourceInventoryTelemetry)> {
    let src_scanner = SourceScanner::new();
    let mut telemetry = SourceInventoryTelemetry::default();

    if !cfg.realdebrid.api_token.is_empty() {
        use crate::api::realdebrid::RealDebridClient;
        use crate::cache::TorrentCache;

        info!("Initializing Real-Debrid cache...");
        let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
        let cache = TorrentCache::new(db, &rd_client);

        match cache.sync().await {
            Ok(_) => info!("Real-Debrid cache synced successfully"),
            Err(e) => tracing::error!(
                "Failed to sync Real-Debrid cache: {}. Using existing cache if available.",
                e
            ),
        }

        const MIN_CACHE_COVERAGE: f64 = 0.80;
        let cache_available = match db.get_rd_torrent_counts().await {
            Ok((cached, total)) if total > 0 => {
                let coverage = cached as f64 / total as f64;
                telemetry.cache_downloaded_torrents = Some(cached as usize);
                telemetry.cache_total_torrents = Some(total as usize);
                telemetry.cache_coverage = Some(coverage);
                if coverage >= MIN_CACHE_COVERAGE {
                    info!(
                        "Using cached source data ({}/{} downloaded torrents, {:.0}% coverage)",
                        cached,
                        total,
                        coverage * 100.0
                    );
                    true
                } else {
                    info!(
                        "Cache coverage too low ({}/{} = {:.0}%), walking filesystem instead",
                        cached,
                        total,
                        coverage * 100.0
                    );
                    false
                }
            }
            Ok((cached, total)) => {
                telemetry.cache_downloaded_torrents = Some(cached as usize);
                telemetry.cache_total_torrents = Some(total as usize);
                info!("Cache unavailable (no downloaded torrents), walking filesystem");
                false
            }
            Err(e) => {
                tracing::warn!(
                    "Could not query RD torrent count: {}. Walking filesystem.",
                    e
                );
                false
            }
        };
        telemetry.cache_enabled = cache_available;

        let mut all_items = Vec::new();
        for source in &cfg.sources {
            if cache_available {
                match src_scanner.scan_source_with_cache(source, &cache).await {
                    Ok(items) => {
                        telemetry.cached_sources += 1;
                        telemetry.cached_items += items.len();
                        all_items.extend(items);
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to read cache for source {}: {}. Falling back to filesystem scan.",
                            source.name,
                            e
                        );
                        let items = src_scanner.scan_source(source);
                        telemetry.filesystem_sources += 1;
                        telemetry.filesystem_items += items.len();
                        all_items.extend(items);
                    }
                }
            } else {
                let items = src_scanner.scan_source(source);
                telemetry.filesystem_sources += 1;
                telemetry.filesystem_items += items.len();
                all_items.extend(items);
            }
        }

        Ok((all_items, telemetry))
    } else {
        let mut all_items = Vec::new();
        for source in &cfg.sources {
            let items = src_scanner.scan_source(source);
            telemetry.filesystem_sources += 1;
            telemetry.filesystem_items += items.len();
            all_items.extend(items);
        }
        Ok((all_items, telemetry))
    }
}

fn log_scan_telemetry(
    telemetry: &ScanTelemetry,
    matches: &[MatchResult],
    link_summary: &LinkProcessSummary,
) {
    info!(
        "Scan phase telemetry: runtime_checks={}, library_scan={}, source_inventory={}, matching={}, title_enrichment={}, linking={}, plex_refresh={}, dead_link_sweep={}",
        fmt_duration(telemetry.runtime_checks),
        fmt_duration(telemetry.library_scan),
        fmt_duration(telemetry.source_inventory),
        fmt_duration(telemetry.match_total),
        fmt_duration(telemetry.episode_title_enrichment),
        fmt_duration(telemetry.linking),
        fmt_duration(telemetry.plex_refresh),
        fmt_duration(telemetry.dead_link_sweep),
    );

    info!(
        "Scan telemetry details: cache_hit_ratio={}, cached_items={}, filesystem_items={}, metadata_alias_prep={}, candidate_scan={}, destination_reduce={}, metadata_errors={}, worker_count={}, candidate_slots={}, scored_candidates={}, exact_id_hits={}, ambiguous_skipped={}, refresh_requested_paths={}, refresh_unique_paths={}, refresh_batches={}, coalesced_batches={}, refreshed_batches={}, refreshed_paths_covered={}, skipped_refresh_batches={}, capped_refresh_batches={}, failed_refresh_batches={}, unresolved_refresh_paths={}",
        telemetry
            .source_inventory_stats
            .cache_hit_ratio()
            .map(|ratio| format!("{:.0}%", ratio * 100.0))
            .unwrap_or_else(|| "n/a".to_string()),
        telemetry.source_inventory_stats.cached_items,
        telemetry.source_inventory_stats.filesystem_items,
        fmt_duration(telemetry.match_stats.metadata_alias_prep),
        fmt_duration(telemetry.match_stats.candidate_scan),
        fmt_duration(telemetry.match_stats.destination_reduce),
        telemetry.match_stats.metadata_errors,
        telemetry.match_stats.worker_count,
        telemetry.match_stats.prefiltered_library_candidates,
        telemetry.match_stats.scored_candidates,
        telemetry.match_stats.exact_id_hits,
        telemetry.match_stats.ambiguous_skipped,
        telemetry.plex_refresh_stats.requested_paths,
        telemetry.plex_refresh_stats.unique_paths,
        telemetry.plex_refresh_stats.planned_batches,
        telemetry.plex_refresh_stats.coalesced_batches,
        telemetry.plex_refresh_stats.refreshed_batches,
        telemetry.plex_refresh_stats.refreshed_paths_covered,
        telemetry.plex_refresh_stats.skipped_batches,
        telemetry.plex_refresh_stats.capped_batches,
        telemetry.plex_refresh_stats.failed_batches,
        telemetry.plex_refresh_stats.unresolved_paths,
    );

    user_println(format!(
        "   📊 Scan telemetry: checks={} | library={} | source={} | match={} | titles={} | link={} | plex={} | dead={}",
        fmt_duration(telemetry.runtime_checks),
        fmt_duration(telemetry.library_scan),
        fmt_duration(telemetry.source_inventory),
        fmt_duration(telemetry.match_total),
        fmt_duration(telemetry.episode_title_enrichment),
        fmt_duration(telemetry.linking),
        fmt_duration(telemetry.plex_refresh),
        fmt_duration(telemetry.dead_link_sweep),
    ));
    user_println(format!(
        "   📊 Scan details: matches={} created={} updated={} skipped={} ambiguous={} candidates={} scored={} exact-id={} cache-hit={} refresh={}/{} skipped={} capped={}",
        matches.len(),
        link_summary.created,
        link_summary.updated,
        link_summary.skipped,
        telemetry.match_stats.ambiguous_skipped,
        telemetry.match_stats.prefiltered_library_candidates,
        telemetry.match_stats.scored_candidates,
        telemetry.match_stats.exact_id_hits,
        telemetry
            .source_inventory_stats
            .cache_hit_ratio()
            .map(|ratio| format!("{:.0}%", ratio * 100.0))
            .unwrap_or_else(|| "n/a".to_string()),
        telemetry.plex_refresh_stats.refreshed_batches,
        telemetry.plex_refresh_stats.planned_batches,
        telemetry.plex_refresh_stats.skipped_batches,
        telemetry.plex_refresh_stats.capped_batches,
    ));
}

fn fmt_duration(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

fn duration_ms_i64(duration: Duration) -> i64 {
    duration
        .as_millis()
        .min(i64::MAX as u128)
        .try_into()
        .unwrap_or(i64::MAX)
}

// ─── Scan-specific helpers ─────────────────────────────────────────

async fn trigger_plex_refresh(
    cfg: &Config,
    refresh_paths: &[PathBuf],
) -> Result<PlexRefreshTelemetry> {
    let mut telemetry = PlexRefreshTelemetry {
        requested_paths: refresh_paths.len(),
        ..PlexRefreshTelemetry::default()
    };
    if refresh_paths.is_empty() || !cfg.has_plex_refresh() {
        return Ok(telemetry);
    }

    let plex = PlexClient::new(&cfg.plex.url, &cfg.plex.token);
    let sections = plex.get_sections().await?;
    let planned = plan_refresh_batches(
        &sections,
        refresh_paths,
        cfg.plex.refresh_coalesce_threshold,
    );
    let (plan, dropped_batches) =
        enforce_refresh_batch_limit(planned, cfg.plex.max_refresh_batches_per_run);

    telemetry.unique_paths = plan.unique_paths;
    telemetry.planned_batches = plan.batches.len() + dropped_batches;
    telemetry.coalesced_batches = plan.coalesced_batches;
    telemetry.coalesced_paths = plan.coalesced_paths;
    telemetry.unresolved_paths = plan.unresolved_paths.len();
    telemetry.capped_batches = dropped_batches;

    for path in &plan.unresolved_paths {
        user_println(format!(
            "   ⚠️  Plex: no matching library section found for {}",
            path.display()
        ));
    }
    telemetry.skipped_batches += plan.unresolved_paths.len();
    if dropped_batches > 0 {
        telemetry.skipped_batches += dropped_batches;
        user_println(format!(
            "   ⚠️  Plex: capped refresh plan at {} request(s); {} request(s) skipped to reduce load",
            cfg.plex.max_refresh_batches_per_run, dropped_batches
        ));
    }

    let refresh_delay = Duration::from_millis(cfg.plex.refresh_delay_ms);
    let batch_count = plan.batches.len();
    for (idx, batch) in plan.batches.into_iter().enumerate() {
        match plex
            .refresh_path(&batch.section_key, &batch.refresh_path)
            .await
        {
            Ok(_) => {
                telemetry.refreshed_batches += 1;
                telemetry.refreshed_paths_covered += batch.covered_paths;
            }
            Err(err) => {
                user_println(format!(
                    "   ⚠️  Plex: refresh failed for {} (section '{}'): {}",
                    batch.refresh_path.display(),
                    batch.section_title,
                    err
                ));
                telemetry.failed_batches += 1;
                telemetry.skipped_batches += 1;
            }
        }

        if refresh_delay > Duration::ZERO && idx + 1 < batch_count {
            sleep(refresh_delay).await;
        }
    }

    if telemetry.refreshed_batches > 0 {
        user_println(format!(
            "   📺 Plex: targeted refresh queued for {} request(s) covering {} path(s)",
            telemetry.refreshed_batches, telemetry.refreshed_paths_covered
        ));
    }
    if telemetry.coalesced_batches > 0 {
        user_println(format!(
            "   📺 Plex: coalesced {} path(s) into {} library-root refresh(es)",
            telemetry.coalesced_paths, telemetry.coalesced_batches
        ));
    }
    if telemetry.skipped_batches > 0 {
        user_println(format!(
            "   ⚠️  Plex: {} refresh request(s) were not queued",
            telemetry.skipped_batches
        ));
    }

    Ok(telemetry)
}

fn enforce_refresh_batch_limit(
    mut plan: crate::api::plex::PlexRefreshPlan,
    max_batches: usize,
) -> (crate::api::plex::PlexRefreshPlan, usize) {
    if max_batches == 0 || plan.batches.len() <= max_batches {
        return (plan, 0);
    }

    let dropped_batches = plan.batches.len().saturating_sub(max_batches);
    plan.batches.truncate(max_batches);
    (plan, dropped_batches)
}

fn build_missing_search_query(item: &LibraryItem) -> Option<String> {
    if item.media_type == MediaType::Tv {
        return None;
    }
    let query = item.title.trim().to_string();
    is_safe_auto_acquire_query(&query).then_some(query)
}

pub(crate) async fn lookup_item_imdb_id(
    tmdb: Option<&TmdbClient>,
    db: &Database,
    item: &LibraryItem,
) -> Option<String> {
    let tmdb = tmdb?;
    match (&item.id, item.media_type) {
        (MediaId::Tmdb(id), MediaType::Movie) => {
            tmdb.get_movie_imdb_id(*id, db).await.ok().flatten()
        }
        (MediaId::Tmdb(id), MediaType::Tv) => tmdb.get_tv_imdb_id(*id, db).await.ok().flatten(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::plex::PlexRefreshPlan;
    use crate::config::ContentType;

    #[test]
    fn missing_auto_acquire_skips_tv_libraries() {
        let tv = LibraryItem {
            id: crate::models::MediaId::Tvdb(81189),
            path: std::path::PathBuf::from("/tmp/Breaking Bad {tvdb-81189}"),
            title: "Breaking Bad".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        };
        let movie = LibraryItem {
            id: crate::models::MediaId::Tmdb(603),
            path: std::path::PathBuf::from("/tmp/The Matrix {tmdb-603}"),
            title: "The Matrix 1999".to_string(),
            library_name: "Movies".to_string(),
            media_type: MediaType::Movie,
            content_type: ContentType::Movie,
        };

        assert_eq!(build_missing_search_query(&tv), None);
        assert_eq!(
            build_missing_search_query(&movie),
            Some("The Matrix 1999".to_string())
        );
    }

    #[test]
    fn cache_hit_ratio_is_based_on_item_mix() {
        let telemetry = SourceInventoryTelemetry {
            cached_items: 8,
            filesystem_items: 2,
            ..SourceInventoryTelemetry::default()
        };

        assert_eq!(telemetry.cache_hit_ratio(), Some(0.8));
    }

    #[test]
    fn enforce_refresh_batch_limit_keeps_small_plans_intact() {
        let plan = PlexRefreshPlan {
            batches: vec![
                crate::api::plex::PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/a"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
                crate::api::plex::PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/b"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
            ],
            ..PlexRefreshPlan::default()
        };

        let (limited, dropped) = enforce_refresh_batch_limit(plan, 2);
        assert_eq!(limited.batches.len(), 2);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn enforce_refresh_batch_limit_truncates_large_plans() {
        let plan = PlexRefreshPlan {
            batches: vec![
                crate::api::plex::PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/a"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
                crate::api::plex::PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/b"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
                crate::api::plex::PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/c"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
            ],
            ..PlexRefreshPlan::default()
        };

        let (limited, dropped) = enforce_refresh_batch_limit(plan, 2);
        assert_eq!(limited.batches.len(), 2);
        assert_eq!(limited.batches[0].refresh_path, PathBuf::from("/library/a"));
        assert_eq!(limited.batches[1].refresh_path, PathBuf::from("/library/b"));
        assert_eq!(dropped, 1);
    }
}
