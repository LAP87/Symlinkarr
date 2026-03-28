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

        // Determine content type request from config
        let content_type = match source.media_type.as_str() {
            "anime" => Some(ContentType::Anime),
            "tv" => Some(ContentType::Tv),
            "movie" => Some(ContentType::Movie),
            _ => None,
        };

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

            let item = if let Some(ct) = content_type {
                self.parse_filename_with_type(&path, ct)
            } else {
                // Default behavior
                self.parse_filename(&path)
            };

            if let Some(item) = item {
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

        // Determine content type request from config
        let content_type = match source.media_type.as_str() {
            "anime" => Some(ContentType::Anime),
            "tv" => Some(ContentType::Tv),
            "movie" => Some(ContentType::Movie),
            _ => None,
        };

        let mut items = Vec::new();

        for entry in WalkDir::new(&source.path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                    let item = if let Some(ct) = content_type {
                        self.parse_filename_with_type(path, ct)
                    } else {
                        // Default behavior for "auto" or unidentified
                        self.parse_filename(path)
                    };

                    if let Some(item) = item {
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

    /// Scan all configured sources and return a combined list.
    pub fn scan_all(&self, sources: &[SourceConfig]) -> Vec<SourceItem> {
        let mut all_items = Vec::new();
        for source in sources {
            all_items.extend(self.scan_source(source));
        }
        info!("Total {} media files across all sources", all_items.len());
        all_items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Standard TV/Movie tests ──

    #[test]
    fn test_parse_standard_episode() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Breaking.Bad.S01E01.720p.BluRay.x264.mkv");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.parsed_title, "Breaking Bad");
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.quality, Some("720p".to_string()));
        assert_eq!(item.extension, "mkv");
    }

    #[test]
    fn test_parse_alt_episode_format() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/The.Office.1x05.Something.mkv");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.parsed_title, "The Office");
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(5));
    }

    #[test]
    fn test_resolution_token_not_parsed_as_season_episode() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Some.Show.1920x1080.WEB-DL.mkv");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.season, None);
        assert_eq!(item.episode, None);
        assert!(item.parsed_title.starts_with("Some Show"));
    }

    #[test]
    fn test_parse_movie() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/The.Matrix.1999.1080p.BluRay.x264.mp4");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.parsed_title, "The Matrix");
        assert_eq!(item.season, None);
        assert_eq!(item.episode, None);
        assert_eq!(item.year, Some(1999));
        assert_eq!(item.quality, Some("1080p".to_string()));
    }

    #[test]
    fn test_parse_4k_quality() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Dune.2021.2160p.WEB-DL.mkv");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.parsed_title, "Dune");
        assert_eq!(item.quality, Some("2160p".to_string()));
        assert_eq!(item.year, Some(2021));
    }

    #[test]
    fn test_parse_short_movie_title_with_release_year() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Up.2009.1080p.BluRay.x264.mkv");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.parsed_title, "Up");
        assert_eq!(item.year, Some(2009));
    }

    #[test]
    fn test_non_video_extension_skipped_by_scan() {
        use tempfile::TempDir;

        let scanner = SourceScanner::new();
        let dir = TempDir::new().unwrap();
        // Create a non-video file
        std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();
        // Create a video file
        std::fs::write(dir.path().join("movie.mkv"), "data").unwrap();

        let source = crate::config::SourceConfig {
            name: "Test".to_string(),
            path: dir.path().to_path_buf(),
            media_type: "auto".to_string(),
        };
        let results = scanner.scan_source(&source);

        // Only the .mkv file should be included
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].extension, "mkv");
    }

    #[test]
    fn test_extract_title_with_underscores() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/The_Big_Bang_Theory_S01E01.mkv");
        let item = scanner.parse_filename(&path).unwrap();

        assert_eq!(item.parsed_title, "The Big Bang Theory");
    }

    // ── Anime parser tests ──

    #[test]
    fn test_anime_subgroup_bare_episode() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[SubsPlease] Jujutsu Kaisen - 03 (1080p) [ABC123].mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Jujutsu Kaisen");
        assert_eq!(item.season, None); // Absolute numbering
        assert_eq!(item.episode, Some(3));
        assert_eq!(item.quality, Some("1080p".to_string()));
    }

    #[test]
    fn test_anime_subgroup_with_inner_trailing_space_is_parsed() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[SubsPlease ]Sousou no Frieren - 01.mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Sousou no Frieren");
        assert_eq!(item.season, None);
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.quality, None);
    }

    #[test]
    fn test_anime_subgroup_standard_subsplease_name_is_parsed() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[SubsPlease] Sousou no Frieren - 01.mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Sousou no Frieren");
        assert_eq!(item.season, None);
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.quality, None);
    }

    #[test]
    fn test_parse_release_title_variants_for_prowlarr_titles() {
        let scanner = SourceScanner::new();
        let anime = scanner
            .parse_release_title_variants("[SubsPlease] Sousou no Frieren - 01")
            .into_iter()
            .find(|(kind, _)| *kind == ParserKind::Anime)
            .map(|(_, item)| item)
            .unwrap();

        assert_eq!(anime.parsed_title, "Sousou no Frieren");
        assert_eq!(anime.season, None);
        assert_eq!(anime.episode, Some(1));
    }

    #[test]
    fn test_anime_standard_sxxexx() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[Erai-raws] Frieren - S01E15 [1080p].mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Frieren");
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(15));
        assert_eq!(item.quality, Some("1080p".to_string()));
    }

    #[test]
    fn test_anime_no_subgroup() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Naruto Shippuuden - 365 (1080p).mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Naruto Shippuuden");
        assert_eq!(item.season, None);
        assert_eq!(item.episode, Some(365));
        assert_eq!(item.quality, Some("1080p".to_string()));
    }

    #[test]
    fn test_anime_version_suffix() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[Judas] Vinland Saga - 03v2 (BDRip 1920x1080).mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Vinland Saga");
        assert_eq!(item.episode, Some(3));
        assert_eq!(item.quality, Some("1080p".to_string())); // 1920x1080 → 1080p
    }

    #[test]
    fn test_anime_horrible_subs() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[HorribleSubs] My Hero Academia - 88 [720p].mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "My Hero Academia");
        assert_eq!(item.episode, Some(88));
        assert_eq!(item.quality, Some("720p".to_string()));
    }

    #[test]
    fn test_anime_separate_season() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Title S2 - 03 (1080p).mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();

        assert_eq!(item.parsed_title, "Title");
        assert_eq!(item.season, Some(2));
        assert_eq!(item.episode, Some(3));
    }

    // ── Content-type dispatch test ──

    #[test]
    fn test_dispatch_anime_vs_tv() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[SubsPlease] Jujutsu Kaisen - 03 (1080p) [ABC123].mkv");

        // Anime parser gets it right
        let anime_item = scanner
            .parse_filename_with_type(&path, ContentType::Anime)
            .unwrap();
        assert_eq!(anime_item.parsed_title, "Jujutsu Kaisen");
        assert_eq!(anime_item.episode, Some(3));

        // TV parser would misparse — gets some title but misses the episode
        let tv_item = scanner
            .parse_filename_with_type(&path, ContentType::Tv)
            .unwrap();
        // The TV parser won't find S01E03 pattern, so episode will be None
        assert_eq!(tv_item.episode, None);
    }

    #[test]
    fn test_parse_dual_variants_contains_anime_and_standard() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[ExampleGroup] Some Anime - 03 (1080p).mkv");
        let variants = scanner.parse_dual_variants(&path);

        assert_eq!(variants.len(), 2);

        let standard = variants
            .iter()
            .find(|(kind, _)| *kind == ParserKind::Standard)
            .map(|(_, item)| item)
            .unwrap();
        let anime = variants
            .iter()
            .find(|(kind, _)| *kind == ParserKind::Anime)
            .map(|(_, item)| item)
            .unwrap();

        assert_eq!(standard.episode, None);
        assert_eq!(anime.episode, Some(3));
    }

    // ── M-20: year-like season regression ──

    #[test]
    fn test_year_like_value_not_parsed_as_season_via_alt_format() {
        let scanner = SourceScanner::new();
        // 2024x01 must NOT parse as season=2024, episode=1
        let path = PathBuf::from("/mnt/rd/Some.Show.2024x01.mkv");
        let item = scanner.parse_filename(&path).unwrap();
        assert_eq!(item.season, None, "2024x01 should not parse as a season");
        assert_eq!(item.episode, None, "2024x01 should not parse as an episode");
    }

    // ── H-07: multi-episode parsing ──

    #[test]
    fn test_multi_episode_contiguous_two() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Show.S01E01E02.mkv");
        let item = scanner.parse_filename(&path).unwrap();
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.episode_end, Some(2));
    }

    #[test]
    fn test_multi_episode_contiguous_three() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Show.S01E01E02E03.mkv");
        let item = scanner.parse_filename(&path).unwrap();
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.episode_end, Some(3));
    }

    #[test]
    fn test_multi_episode_dash_two() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Show.S01E01-E02.mkv");
        let item = scanner.parse_filename(&path).unwrap();
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.episode_end, Some(2));
    }

    #[test]
    fn test_multi_episode_dash_range() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Show.S01E01-E03.mkv");
        let item = scanner.parse_filename(&path).unwrap();
        assert_eq!(item.season, Some(1));
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.episode_end, Some(3));
    }

    #[test]
    fn test_single_episode_has_no_episode_end() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Show.S02E05.mkv");
        let item = scanner.parse_filename(&path).unwrap();
        assert_eq!(item.season, Some(2));
        assert_eq!(item.episode, Some(5));
        assert_eq!(item.episode_end, None);
    }

    // ── Anime edge cases ──

    #[test]
    fn test_anime_bare_episode_with_subgroup_and_resolution() {
        // [SubsPlease] Jujutsu Kaisen - 03 (720p) [ABC123].mkv
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[SubsPlease] Jujutsu Kaisen - 03 (720p) [ABC123].mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();
        assert_eq!(item.parsed_title, "Jujutsu Kaisen");
        assert_eq!(item.episode, Some(3));
        assert_eq!(item.quality, Some("720p".to_string()));
    }

    #[test]
    fn test_anime_episode_v2_not_double_counted() {
        // v2 should not affect episode number parsing
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[Era] Show - 05v2.mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();
        assert_eq!(item.episode, Some(5));
    }

    #[test]
    fn test_anime_quality_from_resolution_4k() {
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/Show - 01 (3840x2160).mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();
        assert_eq!(item.quality, Some("2160p".to_string()));
    }

    #[test]
    fn test_anime_japanese_title_romanized() {
        // Romanized Japanese titles should parse correctly
        let scanner = SourceScanner::new();
        let path = PathBuf::from("/mnt/rd/[Subs] Sousou no Frieren - 01 (BD 1080p).mkv");
        let item = scanner.parse_filename_anime(&path).unwrap();
        assert_eq!(item.parsed_title, "Sousou no Frieren");
        assert_eq!(item.episode, Some(1));
        assert_eq!(item.quality, Some("1080p".to_string()));
    }
}
