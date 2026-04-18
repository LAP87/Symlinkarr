use super::*;

pub(super) fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace(['.', '_', '-'], " ")
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn title_tokens(normalized_title: &str) -> Vec<String> {
    normalized_title
        .split_whitespace()
        .filter(|token| token.len() >= 2 && !token_is_lookup_noise(token))
        .map(|token| token.to_string())
        .collect()
}

pub(super) fn token_is_lookup_noise(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();

    if let Some(num) = lower.strip_suffix('p') {
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }

    if let Some(rest) = lower.strip_prefix('s') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    if let Some(rest) = lower.strip_prefix('e') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }

    matches!(
        lower.as_str(),
        "x264" | "x265" | "hevc" | "webrip" | "webdl" | "bluray" | "bdrip" | "hdtv"
    )
}
fn quality_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(2160|1080|720|480)p").unwrap())
}

fn year_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(?((?:19|20)\d{2})\)?").unwrap())
}

/// Extract quality (e.g., "1080p") from a torrent-style filename
pub(super) fn extract_quality(filename: &str) -> Option<String> {
    quality_tag_regex()
        .captures(filename)
        .map(|c| format!("{}p", &c[1]))
}

/// Extract year (e.g., "(2008)") from a filename string.
pub(super) fn extract_year(filename: &str) -> Option<u32> {
    year_tag_regex()
        .captures(filename)
        .and_then(|c| c[1].parse::<u32>().ok())
}

/// Calculate match score between a dead link and a candidate replacement.
///
/// Scoring breakdown (max 1.0):
///   - Title match:    0.35 (exact) / 0.20 (containment)
///   - Season match:   0.25 (TV only, required)
///   - Episode match:  0.25 (TV only, required)
///   - Year match:     0.15 (movies only, bonus)
///   - Quality match:  0.10
///   - Size proximity: 0.05
pub(super) struct MatchScoreInput<'a> {
    pub(super) search_title: &'a str,
    pub(super) candidate_title: &'a str,
    pub(super) search_season: Option<u32>,
    pub(super) search_episode: Option<u32>,
    pub(super) candidate_season: Option<u32>,
    pub(super) candidate_episode: Option<u32>,
    pub(super) search_quality: &'a Option<String>,
    pub(super) candidate_quality: &'a Option<String>,
    pub(super) search_size: Option<u64>,
    pub(super) candidate_size: Option<u64>,
    pub(super) media_type: MediaType,
    pub(super) search_year: Option<u32>,
    pub(super) candidate_year: Option<u32>,
}

pub(super) fn calculate_match_score(input: MatchScoreInput<'_>) -> f64 {
    let mut score = 0.0;
    let MatchScoreInput {
        search_title,
        candidate_title,
        search_season,
        search_episode,
        candidate_season,
        candidate_episode,
        search_quality,
        candidate_quality,
        search_size,
        candidate_size,
        media_type,
        search_year,
        candidate_year,
    } = input;

    // ── Title match (0.35 max) ──
    if search_title == candidate_title {
        score += 0.35;
    } else if search_title.contains(candidate_title) || candidate_title.contains(search_title) {
        let ratio = search_title.len().min(candidate_title.len()) as f64
            / search_title.len().max(candidate_title.len()) as f64;
        score += 0.20 * ratio;
    } else {
        return 0.0; // No title match → discard
    }

    // ── Season match (0.25 max, mandatory for TV) ──
    match (search_season, candidate_season) {
        (Some(a), Some(b)) if a == b => score += 0.25,
        (Some(_), Some(_)) => return 0.0, // Wrong season → discard
        (Some(_), None) if media_type == MediaType::Tv => return 0.0, // TV needs season
        _ => {}
    }

    // ── Episode match (0.25 max, mandatory for TV) ──
    match (search_episode, candidate_episode) {
        (Some(a), Some(b)) if a == b => score += 0.25,
        (Some(_), Some(_)) => return 0.0, // Wrong episode → discard
        (Some(_), None) if media_type == MediaType::Tv => return 0.0, // TV needs episode
        _ => {}
    }

    // ── Year match (0.15 bonus for movies) ──
    if media_type == MediaType::Movie {
        match (search_year, candidate_year) {
            (Some(y1), Some(y2)) if y1 == y2 => score += 0.15,
            (Some(_), Some(_)) => return 0.0, // Wrong year on a movie → discard
            _ => {}                           // Unknown year, no penalty
        }
    }

    // ── Quality match (0.10 bonus) ──
    match (search_quality, candidate_quality) {
        (Some(q1), Some(q2)) if q1.to_lowercase() == q2.to_lowercase() => {
            score += 0.10;
        }
        _ => {} // No penalty for unknown quality
    }

    // ── File size proximity (0.05 bonus) ──
    if let (Some(s1), Some(s2)) = (search_size, candidate_size) {
        if s1 > 0 && s2 > 0 {
            let ratio = s1.min(s2) as f64 / s1.max(s2) as f64;
            if ratio > (1.0 - SIZE_TOLERANCE) {
                score += 0.05;
            }
        }
    }

    score
}
