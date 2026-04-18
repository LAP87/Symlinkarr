use super::*;

pub(super) fn anime_batch_fallbacks(query: &str) -> Vec<String> {
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

pub(super) fn rank_candidate_hits(
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

pub(super) fn build_anime_request_context(
    request: &AutoAcquireRequest,
) -> Option<AnimeRequestContext> {
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

pub(super) fn is_numbering_token(token: &str) -> bool {
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

pub(super) fn anime_parsed_variant_score(
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

pub(super) fn anime_quality_bonus(quality: Option<&str>, upgrade: bool) -> i64 {
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

pub(super) fn anime_pack_score(
    context: &AnimeRequestContext,
    title: &str,
    title_matches: i64,
) -> Option<i64> {
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

pub(super) fn has_conflicting_explicit_season(
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

pub(super) fn season_token_matches(
    tokens: &HashSet<&str>,
    normalized_title: &str,
    season: u32,
) -> bool {
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

pub(super) fn contains_complete_marker(tokens: &HashSet<&str>) -> bool {
    tokens.contains("complete") || tokens.contains("batch") || tokens.contains("end")
}

pub(super) fn episode_ranges(title: &str) -> Vec<(u32, u32)> {
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

pub(super) fn query_has_specific_numbering(query: &str) -> bool {
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

pub(super) fn parse_season_token(token: &str) -> Option<u32> {
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

pub(super) fn is_episode_number_token(token: &str) -> bool {
    !is_year_token(token)
        && token.len() <= 4
        && token.chars().all(|ch| ch.is_ascii_digit())
        && token.parse::<u32>().map(|value| value > 0).unwrap_or(false)
}
