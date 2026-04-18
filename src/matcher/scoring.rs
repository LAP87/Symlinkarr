use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::config::MatchingMode;
use crate::models::{LibraryItem, MediaType, SourceItem};
use crate::source_scanner::ParserKind;
use crate::utils::normalize;

use super::{DestinationKey, MatchCandidate};

pub(super) fn insert_or_replace(
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
pub(super) fn expand_episode_slots(source: &SourceItem) -> Vec<u32> {
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
        _ => vec![],
    }
}

pub(super) fn destination_key(item: &LibraryItem, source: &SourceItem) -> Option<DestinationKey> {
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

pub(super) fn should_replace_destination(
    existing: &MatchCandidate,
    challenger: &MatchCandidate,
) -> bool {
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
                    s1.max(s2 * 0.85)
                }
            }
        };

        if score > 0.0 {
            match &best {
                Some((best_score, best_alias)) if *best_score > score => {}
                Some((best_score, best_alias))
                    if (*best_score - score).abs() < f64::EPSILON
                        && best_alias.len() <= alias.len() => {}
                _ => best = Some((score, alias.clone())),
            }
        }
    }

    best
}

fn prefix_word_boundary_score(alias: &str, source: &str, min_ratio: f64) -> f64 {
    if let Some(rest) = source.strip_prefix(alias) {
        if rest.is_empty()
            || rest.starts_with([' ', '.', '-', '_', '(', '['])
            || rest.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        {
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

pub(super) fn build_alias_token_index(
    alias_map: &HashMap<usize, Vec<String>>,
) -> HashMap<String, Vec<usize>> {
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

pub(super) fn candidate_library_indices(
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

    let mut ranked: Vec<(usize, usize)> = overlap_counts.into_iter().collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(MAX_CANDIDATES_PER_SOURCE);

    let mut indices: Vec<usize> = ranked.into_iter().map(|(idx, _)| idx).collect();
    indices.sort_unstable();
    indices
}

pub(super) fn source_path_contains_media_id(source_path: &str, media_id: &str) -> bool {
    source_path.match_indices(media_id).any(|(idx, _)| {
        let before = source_path[..idx].chars().next_back();
        let after = source_path[idx + media_id.len()..].chars().next();
        let before_ok = before.is_none_or(|ch| !ch.is_ascii_alphanumeric());
        let after_ok = after.is_none_or(|ch| !ch.is_ascii_alphanumeric());
        before_ok && after_ok
    })
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

pub(super) fn is_better_candidate(challenger: &MatchCandidate, existing: &MatchCandidate) -> bool {
    candidate_cmp(challenger, existing).is_gt()
}

pub(super) fn select_top_two(
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

pub(super) fn should_reject_ambiguous_scores(mode: MatchingMode, best: f64, second: f64) -> bool {
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

#[allow(dead_code)]
pub(super) fn should_reject_ambiguous(mode: MatchingMode, candidates: &[MatchCandidate]) -> bool {
    let (best, second) = select_top_two(candidates);
    let (Some(best), Some(second)) = (best, second) else {
        return false;
    };

    should_reject_ambiguous_scores(mode, best.score, second.score)
}
