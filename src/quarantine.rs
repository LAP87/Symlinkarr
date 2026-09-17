//! Decypharr's `__bad__` quarantine: torrents the provider removed or that failed. Decypharr
//! keeps mirroring them under `__all__`, where they stat fine but read as empty, so a link
//! into one of those folders is dead even though the path exists.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::config::SourceConfig;

/// Source-root paths (`<root>/<torrent folder>`) of every quarantined torrent visible from
/// the configured sources. Empty when no source has a readable `__bad__` sibling.
#[derive(Debug, Default, Clone)]
pub struct QuarantinedFolders {
    folders: HashSet<PathBuf>,
}

impl QuarantinedFolders {
    pub fn load(sources: &[SourceConfig]) -> Self {
        let mut folders = HashSet::new();
        for source in sources {
            for bad_dir in quarantine_dirs_for(&source.path) {
                let entries = match fs::read_dir(&bad_dir) {
                    Ok(entries) => entries,
                    Err(err) => {
                        debug!("No quarantine dir at {}: {}", bad_dir.display(), err);
                        continue;
                    }
                };
                let before = folders.len();
                for entry in entries.flatten() {
                    folders.insert(source.path.join(entry.file_name()));
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
}
