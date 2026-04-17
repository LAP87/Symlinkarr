use super::*;

pub(super) fn build_library_indices_by_path(
    library_items: &[LibraryItem],
) -> HashMap<PathBuf, usize> {
    library_items
        .iter()
        .enumerate()
        .map(|(idx, item)| (item.path.clone(), idx))
        .collect()
}

pub(super) fn build_library_indices_by_id(library_items: &[LibraryItem]) -> HashMap<String, usize> {
    library_items
        .iter()
        .enumerate()
        .map(|(idx, item)| (item.id.to_string(), idx))
        .collect()
}

pub(super) fn find_owner_library_item<'a>(
    symlink_path: &Path,
    library_items: &'a [LibraryItem],
    library_indices_by_path: &HashMap<PathBuf, usize>,
) -> Option<&'a LibraryItem> {
    let mut current = symlink_path.parent();
    while let Some(path) = current {
        if let Some(idx) = library_indices_by_path.get(path) {
            return library_items.get(*idx);
        }
        current = path.parent();
    }

    None
}

pub(super) fn build_aliases(
    item: &LibraryItem,
    metadata: Option<&Option<ContentMetadata>>,
) -> Vec<String> {
    let mut aliases = vec![normalize(&item.title)];

    if let Some(Some(meta)) = metadata {
        aliases.push(normalize(&meta.title));
        aliases.extend(meta.aliases.iter().map(|alias| normalize(alias)));
    }

    aliases.sort();
    aliases.dedup();
    aliases
}

pub(super) fn build_aliases_by_index(
    library_items: &[LibraryItem],
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
) -> Vec<Vec<String>> {
    library_items
        .iter()
        .map(|item| build_aliases(item, metadata_map.get(&item.id.to_string())))
        .collect()
}

pub(super) fn build_alias_token_index(
    alias_map_by_index: &[Vec<String>],
) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, aliases) in alias_map_by_index.iter().enumerate() {
        let mut seen = HashSet::new();
        for alias in aliases {
            for token in title_lookup_tokens(alias) {
                if seen.insert(token.clone()) {
                    index.entry(token).or_default().push(idx);
                }
            }
        }
    }

    for indices in index.values_mut() {
        indices.sort_unstable();
        indices.dedup();
    }

    index
}

pub(super) fn owner_title_matches(
    content_type: ContentType,
    aliases: &[String],
    normalized_parsed: &str,
) -> bool {
    if normalized_parsed.is_empty() {
        return true;
    }

    match content_type {
        ContentType::Anime => aliases
            .iter()
            .any(|alias| tokenized_title_match(alias, normalized_parsed)),
        ContentType::Tv | ContentType::Movie => {
            let parsed_variants = title_match_variants(normalized_parsed);
            aliases.iter().any(|alias| {
                let alias_variants = title_match_variants(alias);
                alias_variants.iter().any(|alias_variant| {
                    parsed_variants.iter().any(|parsed_variant| {
                        strict_owner_alias_match(alias_variant, parsed_variant)
                            || (content_type == ContentType::Tv
                                && tv_alias_with_embedded_episode_marker(
                                    alias_variant,
                                    parsed_variant,
                                ))
                    })
                })
            })
        }
    }
}

pub(super) fn extract_title_year(title: &str) -> Option<u32> {
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

pub(super) fn candidate_release_year(
    item: &LibraryItem,
    metadata: Option<&ContentMetadata>,
) -> Option<u32> {
    metadata
        .and_then(|metadata| metadata.year)
        .or_else(|| extract_title_year(&item.title))
}

pub(super) fn candidate_metadata_compatible(
    item: &LibraryItem,
    entry: &WorkingEntry,
    metadata: Option<&ContentMetadata>,
) -> bool {
    if let (Some(source_year), Some(candidate_year)) =
        (entry.year, candidate_release_year(item, metadata))
    {
        if source_year != candidate_year {
            return false;
        }
    }

    if item.media_type == MediaType::Tv && item.content_type == ContentType::Tv {
        if let (Some(source_season), Some(metadata)) = (entry.season, metadata) {
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

const MAX_ALTERNATE_MATCH_CANDIDATES: usize = 50;

pub(super) fn title_lookup_tokens(title: &str) -> Vec<String> {
    title
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_string())
        .collect()
}

pub(super) fn candidate_library_indices_for_title(
    normalized_title: &str,
    alias_token_index: &HashMap<String, Vec<usize>>,
) -> Vec<usize> {
    let tokens = title_lookup_tokens(normalized_title);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut overlap_counts: HashMap<usize, usize> = HashMap::new();
    for token in tokens {
        if let Some(indices) = alias_token_index.get(&token) {
            for idx in indices {
                *overlap_counts.entry(*idx).or_insert(0) += 1;
            }
        }
    }

    let mut ranked: Vec<(usize, usize)> = overlap_counts.into_iter().collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(MAX_ALTERNATE_MATCH_CANDIDATES);
    ranked.into_iter().map(|(idx, _)| idx).collect()
}

pub(super) fn find_alternate_library_match(
    owner_idx: usize,
    entry: &WorkingEntry,
    normalized_parsed: &str,
    library_items: &[LibraryItem],
    alias_map_by_index: &[Vec<String>],
    alias_token_index: &HashMap<String, Vec<usize>>,
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
) -> Option<AlternateMatchContext> {
    if normalized_parsed.is_empty() {
        return None;
    }

    let parsed_variants = title_match_variants(normalized_parsed);
    let mut best: Option<(usize, f64)> = None;

    for idx in candidate_library_indices_for_title(normalized_parsed, alias_token_index) {
        if idx == owner_idx {
            continue;
        }

        let Some(candidate) = library_items.get(idx) else {
            continue;
        };
        if candidate.media_type != entry.media_type || candidate.content_type != entry.content_type
        {
            continue;
        }
        let candidate_metadata = metadata_map
            .get(&candidate.id.to_string())
            .and_then(|meta| meta.as_ref());
        if !candidate_metadata_compatible(candidate, entry, candidate_metadata) {
            continue;
        }

        let aliases = alias_map_by_index
            .get(idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let Some(score) = best_variant_alias_score(aliases, &parsed_variants) else {
            continue;
        };
        if score < 0.70 {
            continue;
        }

        match best {
            None => best = Some((idx, score)),
            Some((best_idx, best_score)) => {
                let replace = score > best_score
                    || (score == best_score
                        && candidate.title.len() > library_items[best_idx].title.len())
                    || (score == best_score
                        && candidate.title.len() == library_items[best_idx].title.len()
                        && candidate.title < library_items[best_idx].title);
                if replace {
                    best = Some((idx, score));
                }
            }
        }
    }

    best.and_then(|(idx, score)| {
        library_items.get(idx).map(|item| AlternateMatchContext {
            media_id: item.id.to_string(),
            title: item.title.clone(),
            score,
        })
    })
}

pub(super) fn best_variant_alias_score(
    aliases: &[String],
    parsed_variants: &[String],
) -> Option<f64> {
    let mut best: Option<f64> = None;

    for alias in aliases {
        for alias_variant in title_match_variants(alias) {
            for parsed_variant in parsed_variants {
                let Some((score, _)) = best_alias_score(
                    crate::config::MatchingMode::Strict,
                    std::slice::from_ref(&alias_variant),
                    parsed_variant,
                ) else {
                    continue;
                };
                if best.is_none_or(|current| score > current) {
                    best = Some(score);
                }
            }
        }
    }

    best
}

pub(super) fn strict_owner_alias_match(alias: &str, normalized_parsed: &str) -> bool {
    if alias.is_empty() || normalized_parsed.is_empty() {
        return false;
    }

    let alias = alias.trim();
    let normalized_parsed = normalized_parsed.trim();
    if alias.is_empty() || normalized_parsed.is_empty() {
        return false;
    }

    best_alias_score(
        crate::config::MatchingMode::Strict,
        &[alias.to_string()],
        normalized_parsed,
    )
    .is_some()
}

pub(super) fn tv_alias_with_embedded_episode_marker(alias: &str, normalized_parsed: &str) -> bool {
    if alias.is_empty() || normalized_parsed.is_empty() {
        return false;
    }

    let alias = alias.trim();
    let normalized_parsed = normalized_parsed.trim();
    let Some(rest) = normalized_parsed.strip_prefix(alias) else {
        return false;
    };
    if !rest.starts_with(' ') {
        return false;
    }

    let Some(marker) = rest.split_whitespace().next() else {
        return false;
    };

    is_episode_marker_token(marker)
}

pub(super) fn is_episode_marker_token(token: &str) -> bool {
    let token = token.trim();
    if token.is_empty() {
        return false;
    }

    if let Some(rest) = token.strip_prefix('s') {
        return is_numeric_episode_pair(rest, 'e') || is_numeric_episode_pair(rest, 'x');
    }

    is_numeric_episode_pair(token, 'x')
}

pub(super) fn is_numeric_episode_pair(value: &str, separator: char) -> bool {
    let Some((left, right)) = value.split_once(separator) else {
        return false;
    };

    !left.is_empty()
        && !right.is_empty()
        && left.chars().all(|c| c.is_ascii_digit())
        && right.chars().all(|c| c.is_ascii_digit())
}

pub(super) fn strip_leading_article(value: &str) -> String {
    for article in ["the ", "a ", "an "] {
        if let Some(stripped) = value.strip_prefix(article) {
            return stripped.trim().to_string();
        }
    }

    value.trim().to_string()
}

pub(super) fn strip_trailing_year(value: &str) -> String {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    if tokens.is_empty() {
        return String::new();
    }

    let mut end = tokens.len();
    while end > 0 {
        let token = tokens[end - 1];
        let Some(year) = token.parse::<u32>().ok() else {
            break;
        };
        if !(1900..=2099).contains(&year) {
            break;
        }
        end -= 1;
    }

    if end == 0 {
        return value.trim().to_string();
    }

    tokens[..end].join(" ")
}

pub(super) fn title_match_variants(value: &str) -> Vec<String> {
    let base = value.trim();
    if base.is_empty() {
        return Vec::new();
    }

    let mut variants = vec![base.to_string()];
    let no_article = strip_leading_article(base);
    if !no_article.is_empty() {
        variants.push(no_article.clone());
    }

    let no_year = strip_trailing_year(base);
    if !no_year.is_empty() {
        variants.push(no_year.clone());
    }

    let no_article_no_year = strip_trailing_year(&no_article);
    if !no_article_no_year.is_empty() {
        variants.push(no_article_no_year);
    }

    variants.sort();
    variants.dedup();
    variants
}

const SEASON_COUNT_ANOMALY_RATIO_THRESHOLD: f64 = 1.2;
const SEASON_COUNT_ANOMALY_EXCESS_RATIO: f64 = 0.15;
const SEASON_COUNT_ANOMALY_MIN_EXCESS: usize = 2;

pub(super) fn is_season_count_anomaly(actual: usize, expected: usize) -> bool {
    // Count anomalies are only relevant for excess links in this season slot.
    // Lower-than-expected counts are common for partial libraries and should not flag.
    if expected == 0 || actual <= expected {
        return false;
    }

    let ratio = actual as f64 / expected as f64;
    if ratio < SEASON_COUNT_ANOMALY_RATIO_THRESHOLD {
        return false;
    }

    let excess = actual - expected;
    let ratio_min_excess = (expected as f64 * SEASON_COUNT_ANOMALY_EXCESS_RATIO).ceil() as usize;
    let min_excess = ratio_min_excess.max(SEASON_COUNT_ANOMALY_MIN_EXCESS);

    excess >= min_excess
}

pub(super) fn expected_count_for_season(
    media_id: &str,
    season: u32,
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
    arr_map: &HashMap<String, ArrSeriesSnapshot>,
) -> Option<usize> {
    if let Some(arr) = arr_map.get(media_id) {
        if let Some(count) = arr.season_counts.get(&season) {
            return Some(*count);
        }
    }

    metadata_map
        .get(media_id)
        .and_then(|meta| meta.as_ref())
        .and_then(|meta| {
            meta.seasons
                .iter()
                .find(|s| s.season_number == season)
                .map(|s| s.episodes.len())
        })
}

pub(super) fn classify_severity(reasons: &BTreeSet<FindingReason>) -> FindingSeverity {
    if reasons.contains(&FindingReason::BrokenSource)
        || reasons.contains(&FindingReason::MovieEpisodeSource)
        || reasons.contains(&FindingReason::EpisodeOutOfRange)
        || (reasons.contains(&FindingReason::AlternateLibraryMatch)
            && reasons.contains(&FindingReason::ParserTitleMismatch))
        || (reasons.contains(&FindingReason::ArrUntracked)
            && reasons.contains(&FindingReason::ParserTitleMismatch))
    {
        return FindingSeverity::Critical;
    }

    if reasons.contains(&FindingReason::NonRdSourcePath)
        || reasons.contains(&FindingReason::ArrUntracked)
        || reasons.contains(&FindingReason::AlternateLibraryMatch)
        || reasons.contains(&FindingReason::ParserTitleMismatch)
        || (reasons.contains(&FindingReason::DuplicateEpisodeSlot) && reasons.len() > 1)
    {
        return FindingSeverity::High;
    }

    FindingSeverity::Warning
}

pub(super) fn classify_confidence(reasons: &BTreeSet<FindingReason>) -> f64 {
    let mut score = 0.0;

    for reason in reasons {
        let weight = match reason {
            FindingReason::BrokenSource => 1.0,
            FindingReason::LegacyAnimeRootDuplicate => 0.55,
            FindingReason::AlternateLibraryMatch => 0.98,
            FindingReason::MovieEpisodeSource => 0.95,
            FindingReason::EpisodeOutOfRange => 0.9,
            FindingReason::NonRdSourcePath => 0.8,
            FindingReason::ArrUntracked => 0.7,
            FindingReason::DuplicateEpisodeSlot => 0.65,
            FindingReason::ParserTitleMismatch => 0.6,
            FindingReason::SeasonCountAnomaly => 0.4,
        };
        if weight > score {
            score = weight;
        }
    }

    score
}

pub(super) fn episode_out_of_range(meta: &ContentMetadata, season: u32, episode: u32) -> bool {
    let Some(season_info) = meta.seasons.iter().find(|s| s.season_number == season) else {
        // Many providers omit/reshape specials; treat unknown S00 as "unknown" instead of hard error.
        return season != 0;
    };

    if season_info.episodes.is_empty() {
        return false;
    }

    let max_episode = season_info
        .episodes
        .iter()
        .map(|e| e.episode_number)
        .max()
        .unwrap_or(0);

    episode == 0 || episode > max_episode
}

pub(super) fn tokenized_title_match(alias: &str, parsed: &str) -> bool {
    if alias == parsed {
        return true;
    }

    token_window_contains(parsed, alias) || token_window_contains(alias, parsed)
}

pub(super) fn token_window_contains(haystack: &str, needle: &str) -> bool {
    if haystack.is_empty() || needle.is_empty() {
        return false;
    }

    let hay_tokens: Vec<_> = haystack.split_whitespace().collect();
    let needle_tokens: Vec<_> = needle.split_whitespace().collect();

    if needle_tokens.len() > hay_tokens.len() {
        return false;
    }

    hay_tokens
        .windows(needle_tokens.len())
        .any(|window| window == needle_tokens)
}

pub(super) fn effective_content_type(lib: &LibraryConfig) -> ContentType {
    lib.content_type
        .unwrap_or(ContentType::from_media_type(lib.media_type))
}
