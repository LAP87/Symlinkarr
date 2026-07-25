use super::*;

use super::anime::{
    anime_pack_score, anime_parsed_variant_score, build_anime_request_context, is_numbering_token,
    query_has_specific_numbering, tv_release_score,
};

pub(super) async fn search_dmm_candidates(
    cfg: &Config,
    dmm: &DmmClient,
    dmm_session: &mut DmmSearchSession,
    request: &AutoAcquireRequest,
) -> Result<CandidateLookup> {
    let Some(plan) = build_dmm_lookup_plan(request) else {
        return Ok(CandidateLookup::Empty);
    };

    let mut pending_reason = None::<String>;

    if let Some(imdb_id) = request.imdb_id.as_deref() {
        info!(
            "DMM: trying direct IMDb lookup {} for '{}'",
            imdb_id, request.label
        );
        match fetch_dmm_candidates_for_imdb(cfg, request, &plan, dmm, dmm_session, imdb_id).await? {
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
            let Some(lookup) = dmm_session
                .fetch_lookup(dmm, plan.kind, &title_hit.imdb_id, plan.season)
                .await?
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

pub(super) async fn fetch_dmm_by_kind(
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
    dmm_session: &mut DmmSearchSession,
    imdb_id: &str,
) -> Result<DmmImdbLookup> {
    let Some(lookup) = dmm_session
        .fetch_lookup(dmm, plan.kind, imdb_id, plan.season)
        .await?
    else {
        return Ok(DmmImdbLookup::Empty);
    };

    match lookup {
        DmmTorrentLookup::Results(results) => {
            let synthetic_title_hit = DmmTitleCandidate {
                title: request.label.clone(),
                imdb_id: imdb_id.to_string(),
                year: requested_year(request),
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
    let slot = request_season_episode(request);
    let kind = if normalize_arr_name(&request.arr) == "radarr" {
        DmmMediaKind::Movie
    } else if slot.is_some() || matches!(request.relink_check, RelinkCheck::MediaId(_)) {
        DmmMediaKind::Show
    } else {
        return None;
    };

    let season = slot.map(|(season, _)| season);

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
    for hint in &request.query_hints {
        for (_, parsed) in scanner.parse_release_title_variants(hint) {
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

pub(super) fn rank_dmm_candidates(
    request: &AutoAcquireRequest,
    search_query: &str,
    title_hit: &DmmTitleCandidate,
    results: Vec<DmmTorrentResult>,
    max_results: usize,
) -> Vec<DownloadCandidate> {
    let inferred_request = request_season_episode(request).and_then(|(season, episode)| {
        matches!(request.relink_check, RelinkCheck::SymlinkPath(_)).then(|| {
            let mut inferred = request.clone();
            inferred.relink_check = RelinkCheck::MediaEpisode {
                media_id: "self-heal".to_string(),
                season,
                episode,
            };
            inferred
        })
    });
    let ranking_request = inferred_request.as_ref().unwrap_or(request);
    let ranked = if normalize_arr_name(&ranking_request.arr) == "sonarranime" {
        rank_dmm_anime_results(ranking_request, search_query, results)
    } else if relink_season_episode(&ranking_request.relink_check).is_some() {
        rank_dmm_tv_results(ranking_request, results)
    } else {
        rank_dmm_movie_results(ranking_request, title_hit, results)
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

fn request_season_episode(request: &AutoAcquireRequest) -> Option<(u32, u32)> {
    relink_season_episode(&request.relink_check).or_else(|| {
        let scanner = SourceScanner::new();
        std::iter::once(request.query.as_str())
            .chain(request.query_hints.iter().map(String::as_str))
            .flat_map(|value| scanner.parse_release_title_variants(value))
            .find_map(|(_, parsed)| Some((parsed.season?, parsed.episode?)))
    })
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

pub(super) fn rank_dmm_movie_results(
    request: &AutoAcquireRequest,
    title_hit: &DmmTitleCandidate,
    results: Vec<DmmTorrentResult>,
) -> Vec<DmmTorrentResult> {
    let query_tokens = dmm_query_title_tokens(request);
    let requested_year = requested_year(request).or(title_hit.year);
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
    let Some((season, episode)) = relink_season_episode(&request.relink_check) else {
        return Vec::new();
    };

    let scanner = SourceScanner::new();
    let upgrade = request.label.contains("upgrade");
    let prefer_pack = request_prefers_season_pack(request);
    rank_by_score(
        results,
        |result| {
            tv_release_score(
                &scanner,
                &result.title,
                season,
                episode,
                upgrade,
                prefer_pack,
            )
        },
        |r| r.file_size,
        None,
    )
}

pub(super) fn rank_dmm_anime_results(
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

            if let Some(mut score) = anime_pack_score(&context, &result.title, title_matches) {
                if context.prefer_pack {
                    score += 1_500;
                }
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

fn dmm_query_title_tokens(request: &AutoAcquireRequest) -> Vec<String> {
    let title = strip_numbering_tokens(&strip_year_tokens(&clean_request_label(&request.label)));
    normalized_tokens(&title)
}
