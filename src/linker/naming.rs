use tracing::warn;

/// Truncate `s` to at most `max_bytes` bytes, respecting UTF-8 char boundaries.
pub(super) fn truncate_str_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk back from max_bytes until we land on a char boundary.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Ensure a constructed episode filename is <= 250 bytes.
///
/// Strategy:
/// 1. Truncate `episode_title` first (most variable).
/// 2. If still over, truncate `title` as a last resort.
pub(super) fn truncate_filename_to_limit(
    filename: String,
    title: &str,
    episode_title: &str,
    season: u32,
    episode: u32,
    extension: &str,
) -> String {
    const LIMIT: usize = 250;

    if filename.len() <= LIMIT {
        return filename;
    }

    // Step 1: truncate episode_title
    let excess = filename.len() - LIMIT;
    let ep_title_bytes = episode_title.len();
    let new_ep_len = ep_title_bytes.saturating_sub(excess);
    let truncated_ep = truncate_str_bytes(episode_title, new_ep_len).trim_end();

    let candidate = if truncated_ep.is_empty() {
        format!("{} - S{:02}E{:02}.{}", title, season, episode, extension)
    } else {
        format!(
            "{} - S{:02}E{:02} - {}.{}",
            title, season, episode, truncated_ep, extension
        )
    };

    if candidate.len() <= LIMIT {
        warn!(
            "Episode filename exceeded 250 bytes; truncated episode title to {:?}",
            truncated_ep
        );
        return candidate;
    }

    // Step 2: truncate title as last resort
    let excess2 = candidate.len() - LIMIT;
    let truncated_title = truncate_str_bytes(title, title.len().saturating_sub(excess2)).trim_end();
    let final_name = if truncated_ep.is_empty() {
        format!(
            "{} - S{:02}E{:02}.{}",
            truncated_title, season, episode, extension
        )
    } else {
        format!(
            "{} - S{:02}E{:02} - {}.{}",
            truncated_title, season, episode, truncated_ep, extension
        )
    };
    warn!(
        "Episode filename still exceeded 250 bytes after episode title truncation; \
         also truncated show title from {:?} to {:?}",
        title, truncated_title
    );
    final_name
}

/// Remove invalid filename characters (Windows-compatible for safety).
pub(super) fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}
