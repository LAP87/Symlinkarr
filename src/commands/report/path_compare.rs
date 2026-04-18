use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::LibraryConfig;
use crate::media_servers::plex_db;

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
}

struct FilesystemSymlinkScan {
    paths: HashSet<PathBuf>,
    missing_source_paths: HashSet<PathBuf>,
}

pub(super) const PATH_SAMPLE_LIMIT: usize = 10;

pub(super) async fn build_path_compare(
    libraries: &[&LibraryConfig],
    roots: &[PathBuf],
    link_records: &[crate::models::LinkRecord],
    plex_db_path: Option<&Path>,
) -> Result<PathCompareOutput> {
    let mut db_active_links: HashSet<PathBuf> = HashSet::new();
    let mut known_missing_source_paths: HashSet<PathBuf> = HashSet::new();
    for link in link_records
        .iter()
        .filter(|link| link.status == crate::models::LinkStatus::Active)
    {
        db_active_links.insert(link.target_path.clone());
        if !link.source_path.exists() {
            known_missing_source_paths.insert(link.target_path.clone());
        }
    }

    if plex_db_path.is_none() {
        let filesystem_scan = collect_filesystem_symlink_paths(libraries);
        let filesystem_symlinks = filesystem_scan.paths;
        known_missing_source_paths.extend(filesystem_scan.missing_source_paths);

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
        });
    }

    let plex_path_records = plex_db::load_path_records(plex_db_path.unwrap(), roots).await?;
    let filesystem_scan = collect_filesystem_symlink_paths(libraries);
    let filesystem_symlinks = filesystem_scan.paths;
    known_missing_source_paths.extend(filesystem_scan.missing_source_paths);

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
    })
}

fn collect_filesystem_symlink_paths(libraries: &[&LibraryConfig]) -> FilesystemSymlinkScan {
    let results: Vec<_> = libraries
        .par_iter()
        .map(|lib| {
            let mut paths = HashSet::new();
            let mut missing_source_paths = HashSet::new();
            for entry in WalkDir::new(&lib.path).follow_links(false) {
                let Ok(entry) = entry else {
                    continue;
                };
                if entry.file_type().is_symlink() {
                    let path = entry.path().to_path_buf();
                    if symlink_source_missing(&path) {
                        missing_source_paths.insert(path.clone());
                    }
                    paths.insert(path);
                }
            }
            (paths, missing_source_paths)
        })
        .collect();

    let mut all_paths = HashSet::new();
    let mut all_missing = HashSet::new();
    for (paths, missing) in results {
        all_paths.extend(paths);
        all_missing.extend(missing);
    }

    FilesystemSymlinkScan {
        paths: all_paths,
        missing_source_paths: all_missing,
    }
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
    let resolved = if target.is_absolute() {
        target
    } else {
        path.parent()
            .map(|parent| parent.join(&target))
            .unwrap_or(target)
    };
    !resolved.exists()
}
