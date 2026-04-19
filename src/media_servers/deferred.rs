use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Config;

use super::{
    display_server_list, DeferredRefreshServerSummary, DeferredRefreshSummary,
    LibraryRefreshTelemetry, MediaServerKind,
};

#[derive(Debug)]
pub(super) struct MediaRefreshGuard {
    _file: File,
}

#[derive(Debug)]
struct DeferredRefreshQueueGuard {
    _file: File,
}

pub(super) fn media_refresh_lock_base(cfg: &Config) -> PathBuf {
    if cfg.backup.path.is_absolute() {
        cfg.backup.path.clone()
    } else {
        Path::new(&cfg.db_path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

pub(super) fn media_refresh_lock_path(cfg: &Config) -> PathBuf {
    media_refresh_lock_base(cfg).join(".media-server-refresh.lock")
}

pub(super) fn media_refresh_queue_path(cfg: &Config) -> PathBuf {
    media_refresh_lock_base(cfg).join(".media-server-refresh.queue.json")
}

fn media_refresh_queue_lock_path(cfg: &Config) -> PathBuf {
    media_refresh_lock_base(cfg).join(".media-server-refresh.queue.lock")
}

fn ensure_parent_dir_exists(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn try_acquire_media_refresh_guard(cfg: &Config) -> Result<Option<MediaRefreshGuard>> {
    let lock_path = media_refresh_lock_path(cfg);
    ensure_parent_dir_exists(&lock_path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(MediaRefreshGuard { _file: file }));
    }

    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(err.into()),
    }
}

#[cfg(not(unix))]
pub(super) fn try_acquire_media_refresh_guard(cfg: &Config) -> Result<Option<MediaRefreshGuard>> {
    let lock_path = media_refresh_lock_path(cfg);
    ensure_parent_dir_exists(&lock_path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    Ok(Some(MediaRefreshGuard { _file: file }))
}

#[cfg(unix)]
fn acquire_deferred_refresh_queue_guard(cfg: &Config) -> Result<DeferredRefreshQueueGuard> {
    let lock_path = media_refresh_queue_lock_path(cfg);
    ensure_parent_dir_exists(&lock_path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc == 0 {
        return Ok(DeferredRefreshQueueGuard { _file: file });
    }
    Err(std::io::Error::last_os_error().into())
}

#[cfg(not(unix))]
fn acquire_deferred_refresh_queue_guard(cfg: &Config) -> Result<DeferredRefreshQueueGuard> {
    let lock_path = media_refresh_queue_lock_path(cfg);
    ensure_parent_dir_exists(&lock_path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    Ok(DeferredRefreshQueueGuard { _file: file })
}

#[cfg(unix)]
impl Drop for MediaRefreshGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(unix)]
impl Drop for DeferredRefreshQueueGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(super) fn emit_refresh_lock_contention(emit_text: bool, servers: &[MediaServerKind]) {
    if emit_text {
        crate::utils::user_println(format!(
            "   ⚠️  Media refresh deferred: another Symlinkarr process already holds the refresh lock for {}",
            display_server_list(servers)
        ));
    }
}

pub(super) fn deferred_refresh_telemetry(requested_paths: usize) -> LibraryRefreshTelemetry {
    LibraryRefreshTelemetry {
        requested_paths,
        deferred_due_to_lock: true,
        ..LibraryRefreshTelemetry::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DeferredRefreshQueue {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) servers: Vec<DeferredRefreshQueueServer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DeferredRefreshQueueServer {
    pub(super) server: MediaServerKind,
    pub(super) paths: Vec<PathBuf>,
}

pub(super) fn dedup_non_empty_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut unique = paths
        .iter()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    unique.sort();
    unique.dedup();
    unique
}

pub(super) fn merge_dedup_paths(into: &mut Vec<PathBuf>, paths: &[PathBuf]) {
    into.extend(
        paths
            .iter()
            .filter(|path| !path.as_os_str().is_empty())
            .cloned(),
    );
    into.sort();
    into.dedup();
}

pub(super) fn load_deferred_refresh_queue(cfg: &Config) -> Result<DeferredRefreshQueue> {
    let queue_path = media_refresh_queue_path(cfg);
    if !queue_path.exists() {
        return Ok(DeferredRefreshQueue::default());
    }

    let raw = std::fs::read_to_string(queue_path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub(super) fn store_deferred_refresh_queue(
    cfg: &Config,
    queue: &DeferredRefreshQueue,
) -> Result<()> {
    let queue_path = media_refresh_queue_path(cfg);
    if queue.servers.is_empty() {
        if queue_path.exists() {
            std::fs::remove_file(queue_path)?;
        }
        return Ok(());
    }

    if let Some(parent) = queue_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(queue_path, serde_json::to_vec_pretty(queue)?)?;
    Ok(())
}

pub(super) fn queue_deferred_refresh_targets(
    cfg: &Config,
    entries: &[(MediaServerKind, Vec<PathBuf>)],
) -> Result<()> {
    let _guard = acquire_deferred_refresh_queue_guard(cfg)?;
    let mut queue = load_deferred_refresh_queue(cfg)?;
    for (server, paths) in entries {
        let normalized = dedup_non_empty_paths(paths);
        if normalized.is_empty() {
            continue;
        }

        if let Some(existing) = queue
            .servers
            .iter_mut()
            .find(|entry| entry.server == *server)
        {
            merge_dedup_paths(&mut existing.paths, &normalized);
        } else {
            queue.servers.push(DeferredRefreshQueueServer {
                server: *server,
                paths: normalized,
            });
        }
    }
    queue
        .servers
        .sort_by_key(|entry| entry.server.service_key());
    store_deferred_refresh_queue(cfg, &queue)
}

pub(super) fn take_deferred_refresh_targets(
    cfg: &Config,
    servers: &[MediaServerKind],
) -> Result<Vec<DeferredRefreshQueueServer>> {
    if servers.is_empty() {
        return Ok(Vec::new());
    }

    let _guard = acquire_deferred_refresh_queue_guard(cfg)?;
    let queue = load_deferred_refresh_queue(cfg)?;
    let mut retained = DeferredRefreshQueue::default();
    let mut taken = Vec::new();

    for entry in queue.servers {
        if servers.contains(&entry.server) {
            taken.push(entry);
        } else {
            retained.servers.push(entry);
        }
    }

    retained
        .servers
        .sort_by_key(|entry| entry.server.service_key());
    taken.sort_by_key(|entry| entry.server.service_key());
    store_deferred_refresh_queue(cfg, &retained)?;
    Ok(taken)
}

pub(super) fn merge_deferred_refresh_entries(
    cfg: &Config,
    entries: Vec<DeferredRefreshQueueServer>,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let _guard = acquire_deferred_refresh_queue_guard(cfg)?;
    let mut queue = load_deferred_refresh_queue(cfg)?;
    for entry in entries {
        let normalized = dedup_non_empty_paths(&entry.paths);
        if normalized.is_empty() {
            continue;
        }

        if let Some(existing) = queue
            .servers
            .iter_mut()
            .find(|existing| existing.server == entry.server)
        {
            merge_dedup_paths(&mut existing.paths, &normalized);
        } else {
            queue.servers.push(DeferredRefreshQueueServer {
                server: entry.server,
                paths: normalized,
            });
        }
    }

    queue
        .servers
        .sort_by_key(|entry| entry.server.service_key());
    store_deferred_refresh_queue(cfg, &queue)
}

pub(super) fn pending_deferred_refresh_count(cfg: &Config) -> Result<usize> {
    Ok(load_deferred_refresh_queue(cfg)?
        .servers
        .iter()
        .map(|entry| entry.paths.len())
        .sum())
}

pub(crate) fn has_pending_deferred_refreshes(cfg: &Config) -> Result<bool> {
    Ok(pending_deferred_refresh_count(cfg)? > 0)
}

pub(crate) fn deferred_refresh_summary(cfg: &Config) -> Result<DeferredRefreshSummary> {
    let queue = load_deferred_refresh_queue(cfg)?;
    let mut servers = queue
        .servers
        .into_iter()
        .map(|entry| DeferredRefreshServerSummary {
            server: entry.server,
            queued_targets: entry.paths.len(),
        })
        .filter(|entry| entry.queued_targets > 0)
        .collect::<Vec<_>>();
    servers.sort_by_key(|entry| entry.server.service_key());
    let pending_targets = servers.iter().map(|entry| entry.queued_targets).sum();
    Ok(DeferredRefreshSummary {
        pending_targets,
        servers,
    })
}
