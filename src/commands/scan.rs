use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use tracing::info;

use crate::api::bazarr::BazarrClient;
use crate::api::plex::{find_section_for_path, PlexClient};
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::auto_acquire::{process_auto_acquire_queue, AutoAcquireRequest, RelinkCheck};
use crate::commands::{
    decypharr_arr_name, is_safe_auto_acquire_query, prowlarr_categories, runtime_source_health,
    runtime_source_probe_path, selected_libraries, DIRECTORY_PROBE_TIMEOUT,
};
use crate::config::{Config, LibraryConfig};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::linker::Linker;
use crate::matcher::Matcher;
use crate::models::{LibraryItem, MediaId, MediaType};
use crate::source_scanner::SourceScanner;
use crate::utils::{directory_path_health_with_timeout, stdout_text_guard, user_println};
use crate::OutputFormat;

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

    let selected_libraries = selected_libraries(cfg, library_filter)?;
    ensure_runtime_directories_healthy(&selected_libraries, &cfg.sources).await?;

    // Step 1: Scan libraries
    let lib_scanner = LibraryScanner::new();
    let mut library_items = Vec::new();
    for lib in &selected_libraries {
        library_items.extend(lib_scanner.scan_library(lib));
    }
    library_items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    info!("Step 1/4: {} library items identified", library_items.len());

    // Step 2: Scan sources
    let src_scanner = SourceScanner::new();
    let source_items = if !cfg.realdebrid.api_token.is_empty() {
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
                if coverage >= MIN_CACHE_COVERAGE {
                    info!(
                        "Using cached source data ({}/{} downloaded torrents, {:.0}% coverage)",
                        cached, total, coverage * 100.0
                    );
                    true
                } else {
                    info!(
                        "Cache coverage too low ({}/{} = {:.0}%), walking filesystem instead",
                        cached, total, coverage * 100.0
                    );
                    false
                }
            }
            Ok(_) => {
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

        let mut all_items = Vec::new();
        for source in &cfg.sources {
            if cache_available {
                match src_scanner.scan_source_with_cache(source, &cache).await {
                    Ok(items) => all_items.extend(items),
                    Err(e) => {
                        tracing::error!(
                            "Failed to read cache for source {}: {}. Falling back to filesystem scan.",
                            source.name,
                            e
                        );
                        all_items.extend(src_scanner.scan_source(source));
                    }
                }
            } else {
                all_items.extend(src_scanner.scan_source(source));
            }
        }
        all_items
    } else {
        src_scanner.scan_all(&cfg.sources)
    };
    info!("Step 2/4: {} source files found", source_items.len());

    // Step 3: Match
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
    let mut matches = matcher
        .find_matches(&library_items, &source_items, db)
        .await?;
    info!("Step 3/4: {} matches confirmed", matches.len());

    // Step 4: Create symlinks
    let effective_dry_run = dry_run || cfg.symlink.dry_run;
    let linker = Linker::new_with_options(
        effective_dry_run,
        cfg.matching.mode.is_strict(),
        &cfg.symlink.naming_template,
        cfg.features.reconcile_links,
    );
    matcher.enrich_episode_titles(&mut matches, db).await?;
    let link_summary = linker.process_matches(&matches, db).await?;
    info!(
        "Step 4/4: symlinks created={}, updated={}, skipped={}",
        link_summary.created, link_summary.updated, link_summary.skipped
    );

    // Bazarr: trigger subtitle search for newly linked content
    let linked_total = link_summary.created + link_summary.updated;
    if linked_total > 0 && !effective_dry_run && cfg.has_bazarr() {
        let bazarr = BazarrClient::new(&cfg.bazarr);
        match bazarr.trigger_sync().await {
            Ok(_) => user_println("   📝 Bazarr: subtitle search triggered for new content"),
            Err(e) => user_println(format!("   ⚠️  Bazarr subtitle trigger failed: {}", e)),
        }
    }

    if linked_total > 0 && !effective_dry_run && cfg.has_plex() {
        if let Err(e) = trigger_plex_refresh(cfg, &link_summary.refresh_paths).await {
            user_println(format!("   ⚠️  Plex refresh failed: {}", e));
        }
    }

    // Record scan in history
    db.record_scan(
        library_items.len() as i64,
        source_items.len() as i64,
        matches.len() as i64,
        linked_total as i64,
    )
    .await?;

    let dead = if search_missing {
        let library_roots: Vec<_> = selected_libraries
            .iter()
            .map(|lib| lib.path.clone())
            .collect();
        let dead = linker
            .check_dead_links_scoped(db, Some(&library_roots))
            .await?;
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

    // External acquire providers
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

            let anime_missing_requests = match crate::anime_scanner::build_anime_episode_requests(
                crate::anime_scanner::AnimeEpisodeKind::Missing,
                cfg,
                db,
                tmdb.as_ref(),
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
                        cfg,
                        db,
                        tmdb.as_ref(),
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
        library_items_found: library_items.len() as i64,
        source_items_found: source_items.len() as i64,
        matches_found: matches.len() as i64,
        links_created: link_summary.created as i64,
        links_updated: link_summary.updated as i64,
        dead_marked: dead.dead_marked as i64,
        links_removed: dead.removed as i64,
        links_skipped: (link_summary.skipped + dead.skipped) as i64,
        ambiguous_skipped: 0,
    })
    .await?;

    info!("=== Scan Complete ===");
    Ok((linked_total as i64, dead.removed as i64))
}

// ─── Scan-specific helpers ─────────────────────────────────────────

async fn trigger_plex_refresh(cfg: &Config, refresh_paths: &[PathBuf]) -> Result<()> {
    if refresh_paths.is_empty() || !cfg.has_plex() {
        return Ok(());
    }

    let plex = PlexClient::new(&cfg.plex.url, &cfg.plex.token);
    let sections = plex.get_sections().await?;
    let mut unique_paths = refresh_paths.to_vec();
    unique_paths.sort();
    unique_paths.dedup();

    let mut refreshed = 0usize;
    let mut skipped = 0usize;

    for path in unique_paths {
        let Some(section) = find_section_for_path(&sections, &path) else {
            user_println(format!(
                "   ⚠️  Plex: no matching library section found for {}",
                path.display()
            ));
            skipped += 1;
            continue;
        };

        match plex.refresh_path(&section.key, &path).await {
            Ok(_) => refreshed += 1,
            Err(err) => {
                user_println(format!(
                    "   ⚠️  Plex: refresh failed for {} (section '{}'): {}",
                    path.display(),
                    section.title,
                    err
                ));
                skipped += 1;
            }
        }
    }

    if refreshed > 0 {
        user_println(format!(
            "   📺 Plex: targeted refresh queued for {} path(s)",
            refreshed
        ));
    }
    if skipped > 0 {
        user_println(format!(
            "   ⚠️  Plex: {} path(s) were not refreshed",
            skipped
        ));
    }

    Ok(())
}

async fn ensure_runtime_directories_healthy(
    libraries: &[&LibraryConfig],
    sources: &[crate::config::SourceConfig],
) -> Result<()> {
    for lib in libraries {
        let health =
            directory_path_health_with_timeout(lib.path.clone(), DIRECTORY_PROBE_TIMEOUT).await;
        if !health.is_healthy() {
            anyhow::bail!(
                "Library '{}' is not healthy: {}",
                lib.name,
                health.describe(&lib.path)
            );
        }
    }

    for src in sources {
        let probe_path = runtime_source_probe_path(&src.path);
        let health = runtime_source_health(&src.path, &probe_path).await;
        if !health.is_healthy() {
            anyhow::bail!(
                "Source '{}' is not healthy: {}",
                src.name,
                health.describe(&probe_path)
            );
        }
    }

    Ok(())
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
}
