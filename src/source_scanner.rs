use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::config::{ContentType, SourceConfig};
use crate::models::SourceItem;
use crate::utils::{fast_path_health, VIDEO_EXTENSIONS};

/// Matches S01E01, S1E1, s01e01, etc.
static SEASON_EPISODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[Ss](\d{1,2})[Ee](\d{1,3})").unwrap());

/// Matches 1x01, 01x01, etc.
static ALT_SEASON_EPISODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})x(\d{2,3})\b").unwrap());

/// Matches quality tags like 1080p, 2160p, 720p, etc.
static QUALITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(2160p|1080p|720p|480p|4[Kk])").unwrap());

/// Matches a 4-digit year in parentheses or standalone.
static YEAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\.\s\(]?((?:19|20)\d{2})[\.\s\)\]]?").unwrap());

/// Matches one or more leading [Tag] blocks.
static MULTI_SUBGROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\[[^\]]*\]\s*)+").unwrap());

/// Matches bare episode: " - 03", " - 03v2"
static ANIME_EPISODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s-\s(\d{1,4})(?:v\d)?(?:\s|$|\()").unwrap());

/// Matches "S2 - 03" (separate season and episode)
static ANIME_SEASON_EPISODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[Ss](\d{1,2})\s*-\s*(\d{1,3})(?:\s|$|\()").unwrap());

/// Matches resolution like 1920x1080.
static RESOLUTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{3,4})x(\d{3,4})").unwrap());

/// Case-insensitive release tag detector.
static RELEASE_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:repack|proper|bluray|web[- ]?dl|webrip|hdtv|bdrip|dvdrip|x264|x265|hevc|h\.?264|h\.?265|aac|dts)\b",
    )
    .unwrap()
});

/// Space normalization regex reused across parses.
static SPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// S01E01 with optional multi-episode suffix chains (S01E01E02, S01E01-E03).
static MULTI_EP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[Ss](\d{1,2})[Ee](\d{1,3})(?:(?:[Ee]\d{1,3})+|(?:-[Ee]\d{1,3})+)?").unwrap()
});

/// Matches a single E<num> token (used to find last episode in multi-ep match).
static EP_NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[Ee](\d{1,3})").unwrap());

/// Parser variant used for source filename interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParserKind {
    Standard,
    Anime,
}

/// Scans Real-Debrid mount directories for media files and parses their
/// filenames to extract title, season, episode, quality, and year information.
pub struct SourceScanner;

impl SourceScanner {
    pub fn new() -> Self {
        Self
    }

    /// Scan a single source using the RD cache (avoids filesystem walk).
    pub async fn scan_source_with_cache(
        &self,
        source: &SourceConfig,
        cache: &crate::cache::TorrentCache<'_>,
    ) -> anyhow::Result<Vec<SourceItem>> {
        info!(
            "Scanning source via cache: {} at {:?}",
            source.name, source.path
        );

        // Get all files from cache, mapped to local mount path
        let files = cache.get_files(&source.path).await?;
        debug!(
            "Cache returned {} files for source {}",
            files.len(),
            source.name
        );

        let mut items = Vec::new();

        for (path, _size) in files {
            // Check extension
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if !VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                    continue;
                }
            } else {
                continue;
            }

            if let Some(item) = self.parse_path_for_source(&path, source) {
                items.push(item);
            }
        }

        info!(
            "Found {} media files in {} (via cache)",
            items.len(),
            source.name
        );
        Ok(items)
    }

    /// Scan a single source directory and return all video files found.
    pub fn scan_source(&self, source: &SourceConfig) -> Vec<SourceItem> {
        info!("Scanning source: {} at {:?}", source.name, source.path);

        if !fast_path_health(&source.path).is_healthy() {
            warn!("Source path is not healthy: {:?}", source.path);
            return Vec::new();
        }

        let mut items = Vec::new();

        for entry in WalkDir::new(&source.path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                    if let Some(item) = self.parse_path_for_source(path, source) {
                        items.push(item);
                    }
                }
            }
        }

        info!("Found {} media files in {}", items.len(), source.name);
        items
    }

    /// Parse a video filename using the appropriate parser for the content type.
    /// This is the primary public API for content-type-aware parsing.
    pub fn parse_filename_with_type(
        &self,
        path: &std::path::Path,
        content_type: ContentType,
    ) -> Option<SourceItem> {
        match content_type {
            ContentType::Anime => self.parse_filename_with_kind(path, ParserKind::Anime),
            ContentType::Tv | ContentType::Movie => {
                self.parse_filename_with_kind(path, ParserKind::Standard)
            }
        }
    }

    /// Parse a path using the parser mode implied by a source config.
    /// Unknown or "auto" source modes fall back to the standard parser.
    pub fn parse_path_for_source(
        &self,
        path: &std::path::Path,
        source: &SourceConfig,
    ) -> Option<SourceItem> {
        match source.media_type.as_str() {
            "anime" => self.parse_filename_with_type(path, ContentType::Anime),
            "tv" => self.parse_filename_with_type(path, ContentType::Tv),
            "movie" => self.parse_filename_with_type(path, ContentType::Movie),
            _ => self.parse_filename(path),
        }
    }

    /// Parse a filename with a specific parser kind.
    pub fn parse_filename_with_kind(
        &self,
        path: &std::path::Path,
        kind: ParserKind,
    ) -> Option<SourceItem> {
        match kind {
            ParserKind::Standard => self.parse_filename(path),
            ParserKind::Anime => self.parse_filename_anime(path),
        }
    }

    /// Parse a filename with both parsers and return all successful variants.
    /// In strict matching this enables parser selection per library content type.
    pub fn parse_dual_variants(&self, path: &std::path::Path) -> Vec<(ParserKind, SourceItem)> {
        let mut variants = Vec::new();

        if let Some(item) = self.parse_filename_with_kind(path, ParserKind::Standard) {
            variants.push((ParserKind::Standard, item));
        }
        if let Some(item) = self.parse_filename_with_kind(path, ParserKind::Anime) {
            variants.push((ParserKind::Anime, item));
        }

        variants
    }

    /// Parse a raw release title with both parser variants.
    pub fn parse_release_title_variants(&self, title: &str) -> Vec<(ParserKind, SourceItem)> {
        let virtual_path = Self::virtual_release_path(title);
        self.parse_dual_variants(&virtual_path)
    }

    fn virtual_release_path(title: &str) -> PathBuf {
        let sanitized = title
            .chars()
            .map(|ch| {
                if matches!(ch, '/' | '\\' | '\0') {
                    ' '
                } else {
                    ch
                }
            })
            .collect::<String>();
        PathBuf::from(format!("/virtual/{}.mkv", sanitized))
    }

    // ─── Standard TV/Movie parser ────────────────────────────────────

    /// Parse a video filename into a SourceItem (standard TV/movie format).
    fn parse_filename(&self, path: &std::path::Path) -> Option<SourceItem> {
        let file_stem = path.file_stem()?.to_str()?;
        let extension = path.extension()?.to_str()?.to_lowercase();

        // Extract season/episode
        let (season, episode, episode_end) = self.extract_season_episode(file_stem);

        // Extract quality
        let quality = QUALITY_RE
            .captures(file_stem)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        // Extract year
        let year = self.extract_year(file_stem);

        // Extract title and drop a trailing release year token when present.
        let parsed_title = self.strip_trailing_release_year(&self.extract_title(file_stem), year);

        debug!(
            "Parsed: {:?} → title={}, S{:?}E{:?}",
            path.file_name(),
            parsed_title,
            season,
            episode
        );

        Some(SourceItem {
            path: path.to_path_buf(),
            parsed_title,
            season,
            episode,
            episode_end,
            quality,
            extension,
            year,
        })
    }

    /// Extract season, episode, and episode_end numbers from a filename.
    ///
    /// For multi-episode files (S01E01E02, S01E01-E03), episode holds the first
    /// episode and episode_end holds the last. Single-episode files have episode_end=None.
    fn extract_season_episode(&self, filename: &str) -> (Option<u32>, Option<u32>, Option<u32>) {
        if let Some(caps) = MULTI_EP_RE.captures(filename) {
            let season = caps.get(1).and_then(|m| m.as_str().parse().ok());
            let episode: Option<u32> = caps.get(2).and_then(|m| m.as_str().parse().ok());
            let matched_text = caps.get(0).map(|m| m.as_str()).unwrap_or("");

            // Find the last E<num> in the matched text; if it differs from the
            // first episode it is the end of a multi-episode range.
            let last: Option<u32> = EP_NUM_RE
                .captures_iter(matched_text)
                .last()
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse().ok());

            let episode_end = if last != episode { last } else { None };

            return (season, episode, episode_end);
        }

        // Try 1x01 format (no multi-episode support for this format)
        if let Some(caps) = ALT_SEASON_EPISODE_RE.captures(filename) {
            let season = caps.get(1).and_then(|m| m.as_str().parse().ok());
            let episode = caps.get(2).and_then(|m| m.as_str().parse().ok());
            return (season, episode, None);
        }

        (None, None, None)
    }

    /// Extract a year from a filename.
    fn extract_year(&self, filename: &str) -> Option<u32> {
        // Find year, but be careful not to pick up resolution-like numbers
        YEAR_RE
            .captures(filename)
            .and_then(|c| c.get(1))
            .and_then(|m| {
                let year: u32 = m.as_str().parse().ok()?;
                // Sanity check: valid year range
                if (1900..=2099).contains(&year) {
                    Some(year)
                } else {
                    None
                }
            })
    }

    /// Extract the title portion of a filename.
    /// Takes everything before the first S01E01, quality tag, or known separator.
    fn extract_title(&self, filename: &str) -> String {
        let mut title = filename.to_string();

        // Find the earliest position of season/episode or quality markers
        let mut cutoff = title.len();

        if let Some(m) = SEASON_EPISODE_RE.find(&title) {
            cutoff = cutoff.min(m.start());
        }
        if let Some(m) = ALT_SEASON_EPISODE_RE.find(&title) {
            cutoff = cutoff.min(m.start());
        }
        if let Some(m) = QUALITY_RE.find(&title) {
            cutoff = cutoff.min(m.start());
        }

        // Also cut at known release group tags
        if let Some(m) = RELEASE_TAG_RE.find(&title) {
            cutoff = cutoff.min(m.start());
        }

        title = title[..cutoff].to_string();

        // Replace dots, underscores with spaces and clean up
        title = title.replace(['.', '_'], " ");
        // Remove trailing hyphens and whitespace
        title = title
            .trim_end_matches(|c: char| c == '-' || c.is_whitespace())
            .to_string();
        // Collapse multiple spaces
        title = SPACE_RE.replace_all(&title, " ").trim().to_string();

        title
    }

    fn strip_trailing_release_year(&self, title: &str, year: Option<u32>) -> String {
        let Some(year) = year else {
            return title.to_string();
        };
        let year = year.to_string();
        let trimmed = title.trim();
        let Some((prefix, suffix)) = trimmed.rsplit_once(' ') else {
            return trimmed.to_string();
        };

        if suffix == year && !prefix.trim().is_empty() {
            prefix.trim().to_string()
        } else {
            trimmed.to_string()
        }
    }

    // ─── Anime parser ────────────────────────────────────────────────

    /// Parse a video filename using anime naming conventions.
    ///
    /// Handles formats:
    ///   [SubsPlease] Jujutsu Kaisen - 03 (1080p) [hash].mkv
    ///   [Erai-raws] Frieren - S01E15 [1080p].mkv
    ///   Naruto Shippuuden - 365 (1080p).mkv
    ///   [Judas] Title - 03v2 (BDRip 1920x1080).mkv
    ///   Title S2 - 03 (1080p).mkv
    fn parse_filename_anime(&self, path: &std::path::Path) -> Option<SourceItem> {
        let file_stem = path.file_stem()?.to_str()?;
        let extension = path.extension()?.to_str()?.to_lowercase();

        // Step 1: Strip one or more leading [SubGroup]-style tags
        let cleaned = MULTI_SUBGROUP_RE.replace(file_stem, "").to_string();

        // Step 2: Try standard S01E01 first (some anime uses it), with multi-episode support
        let (season, episode, episode_end) = if SEASON_EPISODE_RE.is_match(&cleaned) {
            // Delegate to extract_season_episode for multi-episode detection
            self.extract_season_episode(&cleaned)
        }
        // Try "S2 - 03" format (separate season marker)
        else if let Some(caps) = ANIME_SEASON_EPISODE_RE.captures(&cleaned) {
            (
                caps.get(1).and_then(|m| m.as_str().parse().ok()),
                caps.get(2).and_then(|m| m.as_str().parse().ok()),
                None,
            )
        }
        // Try bare " - 03" format (absolute episode number, no season)
        else if let Some(caps) = ANIME_EPISODE_RE.captures(&cleaned) {
            (
                None, // No season for absolute numbering
                caps.get(1).and_then(|m| m.as_str().parse().ok()),
                None,
            )
        } else {
            (None, None, None)
        };

        // Step 3: Extract quality from [1080p], (1080p), or 1920x1080
        let quality = QUALITY_RE
            .captures(&cleaned)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                // Try resolution format (1920x1080 → 1080p)
                RESOLUTION_RE.captures(&cleaned).and_then(|c| {
                    let height: u32 = c[2].parse().ok()?;
                    match height {
                        2160 => Some("2160p".to_string()),
                        1080 => Some("1080p".to_string()),
                        720 => Some("720p".to_string()),
                        480 => Some("480p".to_string()),
                        _ => Some(format!("{}p", height)),
                    }
                })
            });

        // Step 4: Extract year
        let year = self.extract_year(&cleaned);

        // Step 5: Extract title — everything before the first marker
        let parsed_title = self.extract_anime_title(&cleaned);

        debug!(
            "Parsed (anime): {:?} → title={}, S{:?}E{:?}, quality={:?}",
            path.file_name(),
            parsed_title,
            season,
            episode,
            quality
        );

        Some(SourceItem {
            path: path.to_path_buf(),
            parsed_title,
            season,
            episode,
            episode_end,
            quality,
            extension,
            year,
        })
    }

    /// Extract the title from an anime filename (after subgroup stripping).
    ///
    /// Cuts at the first:
    ///   - " - " followed by a digit (episode separator)
    ///   - S01E01 pattern
    ///   - [quality] or (quality) bracket
    ///   - Known tags
    fn extract_anime_title(&self, cleaned: &str) -> String {
        let mut cutoff = cleaned.len();

        // Cut at " - <digits>" (episode separator)
        if let Some(m) = ANIME_EPISODE_RE.find(cleaned) {
            cutoff = cutoff.min(m.start());
        }
        // Cut at "S2 - 03"
        if let Some(m) = ANIME_SEASON_EPISODE_RE.find(cleaned) {
            cutoff = cutoff.min(m.start());
        }
        // Cut at standard S01E01
        if let Some(m) = SEASON_EPISODE_RE.find(cleaned) {
            cutoff = cutoff.min(m.start());
        }
        // Cut at quality brackets: [1080p] or (1080p)
        if let Some(pos) = cleaned.find('[') {
            cutoff = cutoff.min(pos);
        }
        if let Some(pos) = cleaned.find('(') {
            // Only cut at ( if it contains quality info or a year
            let rest = &cleaned[pos..];
            if rest.starts_with("(1")
                || rest.starts_with("(2")
                || rest.starts_with("(7")
                || rest.starts_with("(4")
                || rest.starts_with("(BD")
            {
                cutoff = cutoff.min(pos);
            }
        }

        let title = cleaned[..cutoff].trim();
        // Remove trailing " -" if present
        let title = title.trim_end_matches(" -").trim_end_matches('-').trim();

        title.to_string()
    }
}

#[cfg(test)]
mod tests;
