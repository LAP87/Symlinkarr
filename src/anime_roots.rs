use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use crate::config::LibraryConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnimeRootDuplicateGroup {
    pub normalized_title: String,
    pub tagged_roots: Vec<PathBuf>,
    pub untagged_roots: Vec<PathBuf>,
}

static TAGGED_ANIME_ROOT_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r" \((?:19|20)\d{2}\) \{(?:tvdb|tmdb)-\d+\}$").expect("valid anime root regex")
});

pub(crate) fn collect_anime_root_duplicate_groups(
    libraries: &[&LibraryConfig],
) -> Vec<AnimeRootDuplicateGroup> {
    #[derive(Default)]
    struct Bucket {
        tagged_roots: Vec<PathBuf>,
        untagged_roots: Vec<PathBuf>,
    }

    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();

    for library in libraries {
        let Ok(entries) = std::fs::read_dir(&library.path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }

            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            let is_tagged = TAGGED_ANIME_ROOT_SUFFIX_RE.is_match(name);
            let normalized_title = TAGGED_ANIME_ROOT_SUFFIX_RE.replace(name, "").to_string();
            let bucket = buckets.entry(normalized_title).or_default();
            if is_tagged {
                bucket.tagged_roots.push(path);
            } else {
                bucket.untagged_roots.push(path);
            }
        }
    }

    let mut groups = Vec::new();
    for (normalized_title, mut bucket) in buckets {
        if bucket.tagged_roots.is_empty() || bucket.untagged_roots.is_empty() {
            continue;
        }

        bucket.tagged_roots.sort();
        bucket.untagged_roots.sort();
        groups.push(AnimeRootDuplicateGroup {
            normalized_title,
            tagged_roots: bucket.tagged_roots,
            untagged_roots: bucket.untagged_roots,
        });
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ContentType, LibraryConfig};
    use crate::models::MediaType;

    #[test]
    fn collect_anime_root_duplicate_groups_detects_mixed_tagged_and_untagged_roots() {
        let dir = tempfile::TempDir::new().unwrap();
        let anime_root = dir.path().join("anime");
        std::fs::create_dir_all(&anime_root).unwrap();
        std::fs::create_dir_all(anime_root.join("Show")).unwrap();
        std::fs::create_dir_all(anime_root.join("Show (2024) {tvdb-123}")).unwrap();
        std::fs::create_dir_all(anime_root.join("Other (2024) {tvdb-456}")).unwrap();

        let library = LibraryConfig {
            name: "Anime".to_string(),
            path: anime_root,
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        };

        let groups = collect_anime_root_duplicate_groups(&[&library]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].normalized_title, "Show");
        assert_eq!(groups[0].tagged_roots.len(), 1);
        assert_eq!(groups[0].untagged_roots.len(), 1);
    }
}
