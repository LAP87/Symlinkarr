use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::ContentType;

/// Type of media content
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Tv,
    Movie,
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaType::Tv => write!(f, "tv"),
            MediaType::Movie => write!(f, "movie"),
        }
    }
}

/// A metadata identifier (TMDB or TVDB)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaId {
    Tvdb(u64),
    Tmdb(u64),
}

impl MediaId {
    /// Returns the numeric ID value
    #[allow(dead_code)] // Planned for future use
    pub fn id_value(&self) -> u64 {
        match self {
            MediaId::Tvdb(id) | MediaId::Tmdb(id) => *id,
        }
    }

    /// Returns the provider name ("tvdb" or "tmdb")
    #[allow(dead_code)] // Planned for future use
    pub fn provider(&self) -> &'static str {
        match self {
            MediaId::Tvdb(_) => "tvdb",
            MediaId::Tmdb(_) => "tmdb",
        }
    }
}

impl std::fmt::Display for MediaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaId::Tvdb(id) => write!(f, "tvdb-{}", id),
            MediaId::Tmdb(id) => write!(f, "tmdb-{}", id),
        }
    }
}

/// A folder in the Plex library that has been identified with a metadata ID
#[derive(Debug, Clone)]
pub struct LibraryItem {
    /// The metadata ID extracted from the folder name
    pub id: MediaId,
    /// The full path to the library folder
    pub path: PathBuf,
    /// The display name of the folder (without the ID tag)
    pub title: String,
    /// Which library this belongs to
    #[allow(dead_code)] // Populated for context, not directly read yet
    pub library_name: String,
    /// The type of media
    pub media_type: MediaType,
    /// Content type used to choose parsing strategy (tv/anime/movie)
    pub content_type: ContentType,
}

/// A media file found on the Real-Debrid mount
#[derive(Debug, Clone)]
pub struct SourceItem {
    /// Full path to the source file
    pub path: PathBuf,
    /// Parsed title from the filename
    pub parsed_title: String,
    /// Season number (for TV)
    pub season: Option<u32>,
    /// Episode number (for TV)
    pub episode: Option<u32>,
    /// Last episode number for multi-episode files (e.g., S01E01-E03 → episode=1, episode_end=Some(3))
    pub episode_end: Option<u32>,
    /// Video quality (e.g., "1080p", "2160p")
    #[allow(dead_code)] // Populated by parser, not used in matching yet
    pub quality: Option<String>,
    /// File extension (e.g., "mkv", "mp4")
    pub extension: String,
    /// Year (for movies, or to disambiguate)
    pub year: Option<u32>,
}

/// A confirmed match between a library item and a source item
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// The library item (destination)
    pub library_item: LibraryItem,
    /// The source item (origin on RD mount)
    pub source_item: SourceItem,
    /// Confidence score (0.0 - 1.0)
    #[allow(dead_code)] // Populated for diagnostics/future use
    pub confidence: f64,
    /// Which alias or title variant matched
    #[allow(dead_code)] // Populated for diagnostics/future use
    pub matched_alias: String,
    /// Pre-resolved episode title from metadata API (TV only).
    /// Populated during the enrichment phase so the linker
    /// does not need access to the matcher.
    pub episode_title: Option<String>,
}

/// Status of a symlink record in the database
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkStatus {
    /// Symlink is active and valid
    Active,
    /// Source file has disappeared (dead link)
    Dead,
    /// Symlink was manually removed
    Removed,
}

/// A persisted symlink record
#[derive(Debug, Clone)]
pub struct LinkRecord {
    /// Database row ID
    #[allow(dead_code)] // Populated from DB row
    pub id: Option<i64>,
    /// Path to the source file (RD mount)
    pub source_path: PathBuf,
    /// Path to the symlink target (Plex library)
    pub target_path: PathBuf,
    /// The metadata ID this link is associated with
    pub media_id: String,
    /// Type of media
    pub media_type: MediaType,
    /// Current status
    pub status: LinkStatus,
    /// When this link was created
    #[allow(dead_code)] // Populated from DB row
    pub created_at: Option<String>,
    /// When this link was last verified
    #[allow(dead_code)] // Populated from DB row
    pub updated_at: Option<String>,
}

/// Aliases and metadata fetched from TMDB/TVDB for a piece of content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentMetadata {
    /// Primary title
    pub title: String,
    /// All known aliases/alternative titles (international, etc.)
    pub aliases: Vec<String>,
    /// Year of release/premiere
    pub year: Option<u32>,
    /// For TV: list of seasons with episode counts
    pub seasons: Vec<SeasonInfo>,
}

/// Information about a single TV season
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeasonInfo {
    /// Season number
    pub season_number: u32,
    /// Episodes in this season
    pub episodes: Vec<EpisodeInfo>,
}

/// Information about a single episode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeInfo {
    /// Episode number
    pub episode_number: u32,
    /// Episode title
    pub title: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_type_display() {
        assert_eq!(MediaType::Movie.to_string(), "movie");
        assert_eq!(MediaType::Tv.to_string(), "tv");
    }

    #[test]
    fn test_media_id_display() {
        assert_eq!(MediaId::Tmdb(123).to_string(), "tmdb-123");
        assert_eq!(MediaId::Tvdb(456).to_string(), "tvdb-456");
    }

    #[test]
    fn test_media_id_id_value() {
        assert_eq!(MediaId::Tmdb(123).id_value(), 123);
        assert_eq!(MediaId::Tvdb(456).id_value(), 456);
    }

    #[test]
    fn test_media_id_provider() {
        assert_eq!(MediaId::Tmdb(123).provider(), "tmdb");
        assert_eq!(MediaId::Tvdb(456).provider(), "tvdb");
    }

    #[test]
    fn test_media_id_eq() {
        assert_eq!(MediaId::Tmdb(123), MediaId::Tmdb(123));
        assert_ne!(MediaId::Tmdb(123), MediaId::Tmdb(124));
        assert_ne!(MediaId::Tmdb(123), MediaId::Tvdb(123));
    }

    #[test]
    fn test_media_type_eq() {
        assert_eq!(MediaType::Movie, MediaType::Movie);
        assert_ne!(MediaType::Movie, MediaType::Tv);
    }

    #[test]
    fn test_link_status_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LinkStatus>();
        assert_send_sync::<MediaType>();
        assert_send_sync::<MediaId>();
    }
}
