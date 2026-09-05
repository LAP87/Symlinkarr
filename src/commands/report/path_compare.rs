use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use walkdir::WalkDir;

use crate::commands::DIRECTORY_PROBE_TIMEOUT;
use crate::config::LibraryConfig;
use crate::media_servers::plex_db;
use crate::utils::{path_under_roots, resolve_link_target, unreachable_source_roots};

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub(super) struct PathSample {
    pub(super) count: usize,
    pub(super) samples: Vec<PathBuf>,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub(super) struct PathCompareOutput {
    pub(super) filesystem_symlinks: usize,
    pub(super) db_active_links: usize,
    pub(super) plex_indexed_files: Option<usize>,
    pub(super) plex_deleted_paths: Option<usize>,
    pub(super) fs_not_in_db: PathSample,
    pub(super) db_not_on_fs: PathSample,
    pub(super) fs_not_in_plex: Option<PathSample>,
    pub(super) db_not_in_plex: Option<PathSample>,
    pub(super) plex_not_on_fs: Option<PathSample>,
    pub(super) plex_deleted_and_known_missing_source: Option<PathSample>,
    pub(super) plex_deleted_without_known_missing_source: Option<PathSample>,
    pub(super) all_three: Option<usize>,
    /// Links whose source root was unreachable (hung/disconnected mount) when
    /// the report ran: their sources could not be verified, so they are
    /// reported here instead of being classified as missing.
    /// `None` when every source root was reachable.
    pub(super) unreachable_sources: Option<PathSample>,
}

struct FilesystemSymlinkScan {
    paths: HashSet<PathBuf>,
    missing_source_paths: HashSet<PathBuf>,
    unreachable_source_paths: HashSet<PathBuf>,
}

pub(super) const PATH_SAMPLE_LIMIT: usize = 10;

pub(super) async fn build_path_compare(
    libraries: &[&LibraryConfig],
    roots: &[PathBuf],
    link_records: &[crate::models::LinkRecord],
    plex_db_path: Option<&Path>,
    source_roots: &[PathBuf],
) -> Result<PathCompareOutput> {
    // Probe each distinct source root once (with timeout) so a hung/disconnected
    // mount neither stalls the report nor makes healthy links look missing.
    let unreachable_roots = unreachable_source_roots(source_roots, DIRECTORY_PROBE_TIMEOUT).await;
    let mut unreachable_source_paths: HashSet<PathBuf> = HashSet::new();

    let mut db_active_links: HashSet<PathBuf> = HashSet::new();
    let mut known_missing_source_paths: HashSet<PathBuf> = HashSet::new();
    for link in link_records
        .iter()
        .filter(|link| link.status == crate::models::LinkStatus::Active)
    {
        db_active_links.insert(link.target_path.clone());
        if path_under_roots(&link.source_path, &unreachable_roots) {
            unreachable_source_paths.insert(link.target_path.clone());
        } else if !link.source_path.exists() {
            known_missing_source_paths.insert(link.target_path.clone());
        }
    }

    if plex_db_path.is_none() {
        let filesystem_scan = collect_filesystem_symlink_paths(libraries, &unreachable_roots);
        let filesystem_symlinks = filesystem_scan.paths;
        known_missing_source_paths.extend(filesystem_scan.missing_source_paths);
        unreachable_source_paths.extend(filesystem_scan.unreachable_source_paths);

        return Ok(PathCompareOutput {
            filesystem_symlinks: filesystem_symlinks.len(),
            db_active_links: db_active_links.len(),
            plex_indexed_files: None,
            plex_deleted_paths: None,
            fs_not_in_db: sample_difference(&filesystem_symlinks, &db_active_links),
            db_not_on_fs: sample_difference(&db_active_links, &filesystem_symlinks),
            fs_not_in_plex: None,
            db_not_in_plex: None,
            plex_not_on_fs: None,
            plex_deleted_and_known_missing_source: None,
            plex_deleted_without_known_missing_source: None,
            all_three: None,
            unreachable_sources: unreachable_sources_sample(
                &unreachable_roots,
                &unreachable_source_paths,
            ),
        });
    }

    let plex_path_records = plex_db::load_path_records(plex_db_path.unwrap(), roots).await?;
    let filesystem_scan = collect_filesystem_symlink_paths(libraries, &unreachable_roots);
    let filesystem_symlinks = filesystem_scan.paths;
    known_missing_source_paths.extend(filesystem_scan.missing_source_paths);
    unreachable_source_paths.extend(filesystem_scan.unreachable_source_paths);

    let plex_indexed_files = plex_path_records
        .iter()
        .map(|record| record.path.clone())
        .collect::<HashSet<_>>();
    let plex_deleted_paths: HashSet<PathBuf> = plex_path_records
        .iter()
        .filter(|record| record.deleted_only)
        .map(|record| record.path.clone())
        .collect();

    let all_three = Some(
        filesystem_symlinks
            .iter()
            .filter(|path| db_active_links.contains(*path) && plex_indexed_files.contains(*path))
            .count(),
    );

    Ok(PathCompareOutput {
        filesystem_symlinks: filesystem_symlinks.len(),
        db_active_links: db_active_links.len(),
        plex_indexed_files: Some(plex_indexed_files.len()),
        plex_deleted_paths: Some(plex_deleted_paths.len()),
        fs_not_in_db: sample_difference(&filesystem_symlinks, &db_active_links),
        db_not_on_fs: sample_difference(&db_active_links, &filesystem_symlinks),
        fs_not_in_plex: Some(sample_difference(&filesystem_symlinks, &plex_indexed_files)),
        db_not_in_plex: Some(sample_difference(&db_active_links, &plex_indexed_files)),
        plex_not_on_fs: Some(sample_difference(&plex_indexed_files, &filesystem_symlinks)),
        plex_deleted_and_known_missing_source: Some(sample_intersection(
            &plex_deleted_paths,
            &known_missing_source_paths,
        )),
        plex_deleted_without_known_missing_source: Some(sample_difference(
            &plex_deleted_paths,
            &known_missing_source_paths,
        )),
        all_three,
        unreachable_sources: unreachable_sources_sample(
            &unreachable_roots,
            &unreachable_source_paths,
        ),
    })
}

fn unreachable_sources_sample(
    unreachable_roots: &[PathBuf],
    unreachable_source_paths: &HashSet<PathBuf>,
) -> Option<PathSample> {
    if unreachable_roots.is_empty() {
        return None;
    }
    Some(sample_difference(unreachable_source_paths, &HashSet::new()))
}

fn collect_filesystem_symlink_paths(
    libraries: &[&LibraryConfig],
    unreachable_roots: &[PathBuf],
) -> FilesystemSymlinkScan {
    let results: Vec<_> = libraries
        .par_iter()
        .map(|lib| {
            let mut paths = HashSet::new();
            let mut missing_source_paths = HashSet::new();
            let mut unreachable_source_paths = HashSet::new();
            for entry in WalkDir::new(&lib.path).follow_links(false) {
                let Ok(entry) = entry else {
                    continue;
                };
                if entry.file_type().is_symlink() {
                    let path = entry.path().to_path_buf();
                    // Health-gate first: never touch sources under an
                    // unreachable root (a hung mount would stall the walk and
                    // healthy links would be misreported as missing).
                    if symlink_target_under_roots(&path, unreachable_roots) {
                        unreachable_source_paths.insert(path.clone());
                    } else if symlink_source_missing(&path) {
                        missing_source_paths.insert(path.clone());
                    }
                    paths.insert(path);
                }
            }
            (paths, missing_source_paths, unreachable_source_paths)
        })
        .collect();

    let mut all_paths = HashSet::new();
    let mut all_missing = HashSet::new();
    let mut all_unreachable = HashSet::new();
    for (paths, missing, unreachable) in results {
        all_paths.extend(paths);
        all_missing.extend(missing);
        all_unreachable.extend(unreachable);
    }

    FilesystemSymlinkScan {
        paths: all_paths,
        missing_source_paths: all_missing,
        unreachable_source_paths: all_unreachable,
    }
}

fn symlink_target_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    if roots.is_empty() {
        return false;
    }
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    path_under_roots(&resolve_link_target(path, &target), roots)
}

pub(super) fn sample_difference(left: &HashSet<PathBuf>, right: &HashSet<PathBuf>) -> PathSample {
    let mut diff: Vec<PathBuf> = left
        .iter()
        .filter(|path| !right.contains(*path))
        .cloned()
        .collect();
    diff.sort();
    PathSample {
        count: diff.len(),
        samples: diff.into_iter().take(PATH_SAMPLE_LIMIT).collect(),
    }
}

pub(super) fn sample_intersection(left: &HashSet<PathBuf>, right: &HashSet<PathBuf>) -> PathSample {
    let mut paths: Vec<PathBuf> = left
        .iter()
        .filter(|path| right.contains(*path))
        .cloned()
        .collect();
    paths.sort();
    PathSample {
        count: paths.len(),
        samples: paths.into_iter().take(PATH_SAMPLE_LIMIT).collect(),
    }
}

pub(super) fn symlink_source_missing(path: &Path) -> bool {
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    !resolve_link_target(path, &target).exists()
}
