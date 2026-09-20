//! Decypharr's `__bad__` quarantine: torrents the provider removed or that failed. Decypharr
//! keeps mirroring them under `__all__`, where they stat fine but read as empty, so a link
//! into one of those folders is dead even though the path exists.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::SourceConfig;

/// A folder identified in Decypharr's `__bad__` quarantine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuarantinedFolderDetail {
    pub name: String,
    pub source_name: String,
    pub quarantine_path: String,
    pub full_path: String,
    pub modified_at: Option<String>,
}

/// Source-root paths (`<root>/<torrent folder>`) of every quarantined torrent visible from
/// the configured sources. Empty when no source has a readable `__bad__` sibling.
#[derive(Debug, Default, Clone)]
pub struct QuarantinedFolders {
    folders: HashSet<PathBuf>,
}

impl QuarantinedFolders {
    pub fn load_from_paths<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> Self {
        let mut folders = HashSet::new();
        for path in paths {
            let path_ref = path.as_ref();
            for bad_dir in quarantine_dirs_for(path_ref) {
                let entries = match fs::read_dir(&bad_dir) {
                    Ok(entries) => entries,
                    Err(err) => {
                        debug!("No quarantine dir at {}: {}", bad_dir.display(), err);
                        continue;
                    }
                };
                let before = folders.len();
                for entry in entries.flatten() {
                    folders.insert(path_ref.join(entry.file_name()));
                }
                let added = folders.len() - before;
                if added > 0 {
                    warn!(
                        "Quarantine {} lists {} torrent folder(s); links into them are treated as dead",
                        bad_dir.display(),
                        added
                    );
                }
            }
        }
        Self { folders }
    }

    pub fn load(sources: &[SourceConfig]) -> Self {
        Self::load_from_paths(sources.iter().map(|s| &s.path))
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    pub fn len(&self) -> usize {
        self.folders.len()
    }

    /// True when `path` is inside a quarantined torrent folder.
    pub fn contains(&self, path: &Path) -> bool {
        if self.folders.is_empty() {
            return false;
        }
        path.ancestors()
            .skip(1)
            .any(|dir| self.folders.contains(dir))
    }

    /// List all quarantined folders found across configured sources with detailed metadata.
    pub fn list_details(sources: &[SourceConfig]) -> Vec<QuarantinedFolderDetail> {
        let mut details = Vec::new();
        let mut seen = HashSet::new();

        for src in sources {
            for bad_dir in quarantine_dirs_for(&src.path) {
                let entries = match fs::read_dir(&bad_dir) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let file_name = entry.file_name();
                    let name = file_name.to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    let modified_at =
                        entry
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .map(|time| {
                                let dt: chrono::DateTime<chrono::Utc> = time.into();
                                dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
                            });
                    details.push(QuarantinedFolderDetail {
                        name,
                        source_name: src.name.clone(),
                        quarantine_path: bad_dir.display().to_string(),
                        full_path: path.display().to_string(),
                        modified_at,
                    });
                }
            }
        }
        details.sort_by(|a, b| a.name.cmp(&b.name));
        details
    }
}

/// Where Decypharr's `__bad__` view lives relative to a configured source root: next to
/// an `__all__` root, or directly inside a root that is the mount itself.
fn quarantine_dirs_for(source_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if source_root.file_name().is_some_and(|n| n == "__all__") {
        if let Some(parent) = source_root.parent() {
            dirs.push(parent.join("__bad__"));
        }
    }
    dirs.push(source_root.join("__bad__"));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &Path) -> SourceConfig {
        SourceConfig {
            name: "RD".to_string(),
            path: path.to_path_buf(),
            media_type: "auto".to_string(),
        }
    }

    #[test]
    fn loads_bad_siblings_of_an_all_root_and_matches_files_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path();
        fs::create_dir_all(mount.join("__all__/Good.Pack")).unwrap();
        fs::create_dir_all(mount.join("__all__/Bad.Pack")).unwrap();
        fs::create_dir_all(mount.join("__bad__/Bad.Pack")).unwrap();

        let q = QuarantinedFolders::load(&[source(&mount.join("__all__"))]);
        assert_eq!(q.len(), 1);
        assert!(q.contains(&mount.join("__all__/Bad.Pack/ep01.mkv")));
        assert!(q.contains(&mount.join("__all__/Bad.Pack/Season 1/ep01.mkv")));
        assert!(!q.contains(&mount.join("__all__/Good.Pack/ep01.mkv")));
        assert!(
            !q.contains(&mount.join("__all__/Bad.Pack")),
            "the folder itself is not inside itself"
        );
    }

    #[test]
    fn missing_quarantine_dir_means_nothing_is_quarantined() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("plain")).unwrap();
        let q = QuarantinedFolders::load(&[source(&tmp.path().join("plain"))]);
        assert!(q.is_empty());
        assert!(!q.contains(&tmp.path().join("plain/x/y.mkv")));
    }

    #[test]
    fn list_details_returns_quarantined_items_with_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path();
        fs::create_dir_all(mount.join("__all__/Good.Pack")).unwrap();
        fs::create_dir_all(mount.join("__bad__/Bad.Pack")).unwrap();
        fs::create_dir_all(mount.join("__bad__/Corrupt.Movie")).unwrap();

        let details = QuarantinedFolders::list_details(&[source(&mount.join("__all__"))]);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].name, "Bad.Pack");
        assert_eq!(details[0].source_name, "RD");
        assert_eq!(details[1].name, "Corrupt.Movie");
    }
}
