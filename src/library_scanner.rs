use std::sync::LazyLock;
use regex::Regex;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::config::{ContentType, LibraryConfig};
#[cfg(test)]
use crate::models::MediaType;
use crate::models::{LibraryItem, MediaId};

/// Static regexes compiled once at program start.
static ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(tvdb|tmdb)-([0-9]+)\}").unwrap());
static TITLE_CLEANUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\{(?:tvdb|tmdb)-[0-9]+\}\s*").unwrap());

/// Scans Plex/Jellyfin library directories for folders tagged with
/// `{tvdb-XXXXX}` or `{tmdb-XXXXX}` metadata IDs.
pub struct LibraryScanner;

impl LibraryScanner {
    pub fn new() -> Self {
        Self
    }

    /// Scan a single library directory and return all ID-tagged folders found.
    pub fn scan_library(&self, lib: &LibraryConfig) -> Vec<LibraryItem> {
        info!("Scanning library: {} at {:?}", lib.name, lib.path);

        if !lib.path.exists() {
            warn!("Library path does not exist: {:?}", lib.path);
            return Vec::new();
        }

        let mut items = Vec::new();

        for entry in WalkDir::new(&lib.path)
            .min_depth(1)
            .max_depth(lib.depth)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
        {
            if let Some(file_name) = entry.file_name().to_str() {
                if let Some(item) = self.parse_folder(file_name, entry.path(), lib) {
                    items.push(item);
                }
            }
        }

        info!("Found {} folders with ID tags in {}", items.len(), lib.name);
        items
    }

    /// Parse a folder name for a metadata ID tag.
    fn parse_folder(
        &self,
        folder_name: &str,
        path: &std::path::Path,
        lib: &LibraryConfig,
    ) -> Option<LibraryItem> {
        let caps = ID_REGEX.captures(folder_name)?;

        let id_type = caps.get(1)?.as_str();
        let id_val: u64 = caps.get(2)?.as_str().parse().ok()?;

        let media_id = match id_type {
            "tvdb" => MediaId::Tvdb(id_val),
            "tmdb" => MediaId::Tmdb(id_val),
            _ => return None,
        };

        // Extract the clean title by removing the ID tag
        let title = TITLE_CLEANUP_REGEX
            .replace(folder_name, "")
            .trim()
            .to_string();

        Some(LibraryItem {
            id: media_id,
            path: path.to_path_buf(),
            title,
            library_name: lib.name.clone(),
            media_type: lib.media_type,
            content_type: lib
                .content_type
                .unwrap_or(ContentType::from_media_type(lib.media_type)),
        })
    }

    /// Scan all configured libraries and return a combined list.
    #[allow(dead_code)] // Retained for compatibility with existing integrations
    pub fn scan_all(&self, libraries: &[LibraryConfig]) -> Vec<LibraryItem> {
        let mut all_items = Vec::new();
        for lib in libraries {
            all_items.extend(self.scan_library(lib));
        }

        // Sort alphabetically by title so that the logs and processing order are predictable.
        all_items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));

        info!(
            "Total {} identified folders across all libraries",
            all_items.len()
        );
        all_items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn test_lib(dir: &std::path::Path, media_type: MediaType) -> LibraryConfig {
        LibraryConfig {
            name: "Test".to_string(),
            path: dir.to_path_buf(),
            media_type,
            content_type: None,
            depth: 1,
        }
    }

    #[test]
    fn test_parse_tvdb_folder() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("Breaking Bad {tvdb-81189}");
        fs::create_dir(&folder).unwrap();

        let lib = test_lib(dir.path(), MediaType::Tv);
        let results = scanner.scan_library(&lib);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, MediaId::Tvdb(81189));
        assert_eq!(results[0].title, "Breaking Bad");
    }

    #[test]
    fn test_parse_tmdb_folder() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("The Matrix {tmdb-603}");
        fs::create_dir(&folder).unwrap();

        let lib = test_lib(dir.path(), MediaType::Movie);
        let results = scanner.scan_library(&lib);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, MediaId::Tmdb(603));
        assert_eq!(results[0].title, "The Matrix");
    }

    #[test]
    fn test_no_id_folder_is_skipped() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("Some Random Folder");
        fs::create_dir(&folder).unwrap();

        let lib = test_lib(dir.path(), MediaType::Tv);
        let results = scanner.scan_library(&lib);

        assert!(results.is_empty());
    }

    #[test]
    fn test_files_not_included() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        // Create a file, not a directory
        fs::write(dir.path().join("Not A Dir {tvdb-12345}"), "").unwrap();

        let lib = test_lib(dir.path(), MediaType::Tv);
        let results = scanner.scan_library(&lib);

        assert!(results.is_empty());
    }

    #[test]
    fn test_multiple_folders_in_library() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("Breaking Bad {tvdb-81189}")).unwrap();
        fs::create_dir(dir.path().join("The Matrix {tmdb-603}")).unwrap();
        fs::create_dir(dir.path().join("Unknown {imdb-tt123}")).unwrap(); // Unknown type, should be skipped

        let lib = test_lib(dir.path(), MediaType::Tv);
        let results = scanner.scan_library(&lib);

        // Scanner picks up all valid tvdb/tmdb folders regardless of library type
        assert_eq!(results.len(), 2);
        let ids: Vec<_> = results.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&MediaId::Tvdb(81189)));
        assert!(ids.contains(&MediaId::Tmdb(603)));
    }

    #[test]
    fn test_title_cleanup_removes_id_tag() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        // Folder with extra whitespace around the ID tag
        fs::create_dir(dir.path().join("  Movie Title  {tmdb-999}  ")).unwrap();

        let lib = test_lib(dir.path(), MediaType::Movie);
        let results = scanner.scan_library(&lib);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Movie Title");
    }

    #[test]
    fn test_tvdb_and_tmdb_in_same_directory() {
        let scanner = LibraryScanner::new();
        let dir = TempDir::new().unwrap();
        // Scanner picks up both tvdb and tmdb regardless of library type
        fs::create_dir(dir.path().join("Movie {tmdb-100}")).unwrap();
        fs::create_dir(dir.path().join("Show {tvdb-200}")).unwrap();

        let lib = test_lib(dir.path(), MediaType::Movie);
        let results = scanner.scan_library(&lib);

        assert_eq!(results.len(), 2);
        let ids: Vec<_> = results.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&MediaId::Tmdb(100)));
        assert!(ids.contains(&MediaId::Tvdb(200)));
    }
}
