use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use super::{parse_trash_filename, DeadLink};
use crate::config::ContentType;
use crate::models::LinkRecord;
use crate::utils::{cached_source_health, path_under_roots, PathHealth};

/// SAFETY: Verify a path is a symlink (not a regular file or directory)
/// before performing destructive operations. This prevents accidental
/// deletion of *arr-managed directories or real media files.
pub(super) fn assert_symlink_only(path: &std::path::Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        anyhow::bail!(
            "SAFETY GUARD: Attempted to operate on a directory instead of a symlink: {:?}. \
             Symlinkarr never touches directories — they are managed by *arr apps.",
            path
        );
    }
    if !meta.file_type().is_symlink() {
        anyhow::bail!(
            "SAFETY GUARD: {:?} is a regular file, not a symlink. \
             Symlinkarr only operates on symlinks.",
            path
        );
    }
    Ok(())
}

fn normalized_compare_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn is_streaming_symlink_match(dead_symlink_path: &Path, active_path: &str) -> bool {
    if dead_symlink_path.to_string_lossy() == active_path {
        return true;
    }

    let active = PathBuf::from(active_path);
    normalized_compare_path(dead_symlink_path) == normalized_compare_path(&active)
}

pub(super) fn collect_fresh_dead_links_chunk(
    links: Vec<LinkRecord>,
    allowed_symlink_roots: Option<Vec<PathBuf>>,
    processed: &AtomicUsize,
) -> Result<Vec<DeadLink>> {
    let mut source_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
    let mut parent_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
    let mut fresh = Vec::new();

    for link in links {
        if let Some(roots) = allowed_symlink_roots.as_ref() {
            if !path_under_roots(&link.target_path, roots) {
                processed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }

        let source_exists = destructive_source_exists(
            "repair dead-link detection",
            &link.source_path,
            &mut source_health_cache,
            &mut parent_health_cache,
        )?;
        let target_ok = match std::fs::symlink_metadata(&link.target_path) {
            Ok(meta) if meta.file_type().is_symlink() => std::fs::read_link(&link.target_path)
                .map(|resolved| resolved == link.source_path)
                .unwrap_or(false),
            _ => false,
        };

        if source_exists && target_ok {
            processed.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let symlink_name = link
            .target_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let meta = parse_trash_filename(symlink_name);

        fresh.push(DeadLink {
            symlink_path: link.target_path.clone(),
            original_source: link.source_path.clone(),
            media_id: link.media_id.clone(),
            media_type: link.media_type,
            content_type: ContentType::from_media_type(link.media_type),
            meta,
            original_size: if source_exists {
                std::fs::metadata(&link.source_path).ok().map(|m| m.len())
            } else {
                None
            },
        });
        processed.fetch_add(1, Ordering::Relaxed);
    }

    Ok(fresh)
}

pub(super) fn destructive_source_exists(
    operation: &str,
    source_path: &Path,
    source_health_cache: &mut HashMap<PathBuf, PathHealth>,
    parent_health_cache: &mut HashMap<PathBuf, PathHealth>,
) -> Result<bool> {
    let source_health = cached_source_health(source_path, source_health_cache, parent_health_cache);
    if source_health.blocks_destructive_ops() {
        anyhow::bail!(
            "Aborting {}: source path became unhealthy: {}",
            operation,
            source_health.describe(source_path)
        );
    }
    Ok(source_health.is_healthy())
}

pub(super) fn recommended_drift_workers(total_active: usize) -> usize {
    if total_active == 0 {
        return 1;
    }

    let available = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let target = (available * 3).div_ceil(4);
    target.max(1).min(total_active)
}
