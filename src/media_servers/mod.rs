use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{Config, LibraryConfig, MediaBrowserConfig};

pub(crate) mod emby;
pub(crate) mod jellyfin;
mod plex;
pub(crate) mod plex_db;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LibraryRefreshTelemetry {
    pub requested_paths: usize,
    pub unique_paths: usize,
    pub planned_batches: usize,
    pub coalesced_batches: usize,
    pub coalesced_paths: usize,
    pub refreshed_batches: usize,
    pub refreshed_paths_covered: usize,
    pub skipped_batches: usize,
    pub unresolved_paths: usize,
    pub capped_batches: usize,
    pub aborted_due_to_cap: bool,
    #[serde(default)]
    pub deferred_due_to_lock: bool,
    pub failed_batches: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LibraryRefreshOutcome {
    pub aggregate: LibraryRefreshTelemetry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<LibraryInvalidationServerOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MediaServerKind {
    Plex,
    Emby,
    Jellyfin,
}

impl std::fmt::Display for MediaServerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plex => write!(f, "Plex"),
            Self::Emby => write!(f, "Emby"),
            Self::Jellyfin => write!(f, "Jellyfin"),
        }
    }
}

impl MediaServerKind {
    pub(crate) fn service_key(self) -> &'static str {
        match self {
            Self::Plex => "plex",
            Self::Emby => "emby",
            Self::Jellyfin => "jellyfin",
        }
    }
}

pub(crate) fn display_server_list(servers: &[MediaServerKind]) -> String {
    servers
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LibraryInvalidationOutcome {
    pub server: Option<MediaServerKind>,
    pub requested_library_roots: usize,
    pub configured: bool,
    pub refresh: Option<LibraryRefreshTelemetry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<LibraryInvalidationServerOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LibraryInvalidationServerOutcome {
    pub server: MediaServerKind,
    pub requested_targets: usize,
    pub refresh: LibraryRefreshTelemetry,
}

impl LibraryInvalidationOutcome {
    pub(crate) fn summary_suffix(&self) -> Option<String> {
        if self.requested_library_roots == 0 {
            return None;
        }

        if !self.configured {
            return Some(format!(
                "no media-server refresh configured for {} changed library root(s)",
                self.requested_library_roots
            ));
        }

        if !self.servers.is_empty() {
            let labels = self
                .servers
                .iter()
                .map(|entry| entry.server.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let aggregate = self.refresh.as_ref()?;
            if aggregate.deferred_due_to_lock {
                return Some(format!(
                    "{} refresh deferred because another Symlinkarr process is already refreshing a media server",
                    labels
                ));
            }
            if aggregate.aborted_due_to_cap {
                return Some(format!(
                    "{} refresh partially aborted by cap guard across {} server(s)",
                    labels,
                    self.servers.len()
                ));
            }
            if aggregate.refreshed_batches > 0 {
                return Some(format!(
                    "{} refresh queued for {} request(s) across {} server(s)",
                    labels,
                    aggregate.refreshed_batches,
                    self.servers.len()
                ));
            }
            if aggregate.failed_batches > 0 || aggregate.skipped_batches > 0 {
                return Some(format!(
                    "{} refresh attempted with {} skipped and {} failed request(s) across {} server(s)",
                    labels,
                    aggregate.skipped_batches,
                    aggregate.failed_batches,
                    self.servers.len()
                ));
            }
        }

        let refresh = self.refresh.as_ref()?;
        if self.server.is_none() {
            if refresh.deferred_due_to_lock {
                return Some(format!(
                    "media-server refresh deferred because another Symlinkarr process is already refreshing a media server for {} changed library root(s)",
                    self.requested_library_roots
                ));
            }
            if refresh.aborted_due_to_cap {
                return Some(format!(
                    "media-server refresh aborted by cap guard for {} changed library root(s)",
                    self.requested_library_roots
                ));
            }
            if refresh.refreshed_batches > 0 {
                return Some(format!(
                    "media-server refresh queued for {} request(s)",
                    refresh.refreshed_batches
                ));
            }
            if refresh.failed_batches > 0 || refresh.skipped_batches > 0 {
                return Some(format!(
                    "media-server refresh attempted with {} skipped and {} failed request(s)",
                    refresh.skipped_batches, refresh.failed_batches
                ));
            }
            return None;
        }

        let server = self.server.unwrap_or(MediaServerKind::Plex);
        if refresh.deferred_due_to_lock {
            return Some(format!(
                "{} refresh deferred because another Symlinkarr process is already refreshing a media server for {} changed library root(s)",
                server, self.requested_library_roots
            ));
        }
        if refresh.aborted_due_to_cap {
            return Some(format!(
                "{} refresh aborted by cap guard for {} changed library root(s)",
                server, self.requested_library_roots
            ));
        }
        if refresh.refreshed_batches > 0 {
            return Some(format!(
                "{} refresh queued for {} request(s)",
                server, refresh.refreshed_batches
            ));
        }
        if refresh.failed_batches > 0 || refresh.skipped_batches > 0 {
            return Some(format!(
                "{} refresh attempted with {} skipped and {} failed request(s)",
                server, refresh.skipped_batches, refresh.failed_batches
            ));
        }

        None
    }
}

fn merge_refresh_telemetry(
    aggregate: &mut LibraryRefreshTelemetry,
    addition: &LibraryRefreshTelemetry,
) {
    aggregate.requested_paths += addition.requested_paths;
    aggregate.unique_paths += addition.unique_paths;
    aggregate.planned_batches += addition.planned_batches;
    aggregate.coalesced_batches += addition.coalesced_batches;
    aggregate.coalesced_paths += addition.coalesced_paths;
    aggregate.refreshed_batches += addition.refreshed_batches;
    aggregate.refreshed_paths_covered += addition.refreshed_paths_covered;
    aggregate.skipped_batches += addition.skipped_batches;
    aggregate.unresolved_paths += addition.unresolved_paths;
    aggregate.capped_batches += addition.capped_batches;
    aggregate.aborted_due_to_cap |= addition.aborted_due_to_cap;
    aggregate.deferred_due_to_lock |= addition.deferred_due_to_lock;
    aggregate.failed_batches += addition.failed_batches;
}

#[derive(Debug)]
struct MediaRefreshGuard {
    _file: File,
}

fn media_refresh_lock_path(cfg: &Config) -> PathBuf {
    let base = if cfg.backup.path.is_absolute() {
        cfg.backup.path.clone()
    } else {
        Path::new(&cfg.db_path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    base.join(".media-server-refresh.lock")
}

fn media_refresh_queue_path(cfg: &Config) -> PathBuf {
    let base = if cfg.backup.path.is_absolute() {
        cfg.backup.path.clone()
    } else {
        Path::new(&cfg.db_path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    base.join(".media-server-refresh.queue.json")
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
fn try_acquire_media_refresh_guard(cfg: &Config) -> Result<Option<MediaRefreshGuard>> {
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
fn try_acquire_media_refresh_guard(cfg: &Config) -> Result<Option<MediaRefreshGuard>> {
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
impl Drop for MediaRefreshGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn emit_refresh_lock_contention(emit_text: bool, servers: &[MediaServerKind]) {
    if emit_text {
        crate::utils::user_println(format!(
            "   ⚠️  Media refresh deferred: another Symlinkarr process already holds the refresh lock for {}",
            display_server_list(servers)
        ));
    }
}

fn deferred_refresh_telemetry(requested_paths: usize) -> LibraryRefreshTelemetry {
    LibraryRefreshTelemetry {
        requested_paths,
        deferred_due_to_lock: true,
        ..LibraryRefreshTelemetry::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DeferredRefreshQueue {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    servers: Vec<DeferredRefreshQueueServer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeferredRefreshQueueServer {
    server: MediaServerKind,
    paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeferredRefreshSummary {
    pub pending_targets: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<DeferredRefreshServerSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeferredRefreshServerSummary {
    pub server: MediaServerKind,
    pub queued_targets: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RefreshTargetPlan {
    targets: Vec<PathBuf>,
    coalesced_paths: usize,
    coalesced_batches: usize,
    root_fallback_applied: bool,
}

fn dedup_non_empty_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut unique = paths
        .iter()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    unique.sort();
    unique.dedup();
    unique
}

fn merge_dedup_paths(into: &mut Vec<PathBuf>, paths: &[PathBuf]) {
    into.extend(
        paths
            .iter()
            .filter(|path| !path.as_os_str().is_empty())
            .cloned(),
    );
    into.sort();
    into.dedup();
}

fn load_deferred_refresh_queue(cfg: &Config) -> Result<DeferredRefreshQueue> {
    let queue_path = media_refresh_queue_path(cfg);
    if !queue_path.exists() {
        return Ok(DeferredRefreshQueue::default());
    }

    let raw = std::fs::read_to_string(queue_path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn store_deferred_refresh_queue(cfg: &Config, queue: &DeferredRefreshQueue) -> Result<()> {
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

fn queue_deferred_refresh_targets(
    cfg: &Config,
    entries: &[(MediaServerKind, Vec<PathBuf>)],
) -> Result<()> {
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

fn take_deferred_refresh_queue(cfg: &Config) -> Result<DeferredRefreshQueue> {
    let queue = load_deferred_refresh_queue(cfg)?;
    let queue_path = media_refresh_queue_path(cfg);
    if queue_path.exists() {
        std::fs::remove_file(queue_path)?;
    }
    Ok(queue)
}

fn pending_deferred_refresh_count(cfg: &Config) -> Result<usize> {
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

fn planned_media_browser_batches(cfg: &MediaBrowserConfig, paths: &[PathBuf]) -> usize {
    let unique = dedup_non_empty_paths(paths);
    if unique.is_empty() {
        return 0;
    }

    let batch_size = cfg.refresh_batch_size.max(1);
    unique.len().div_ceil(batch_size)
}

fn select_media_browser_refresh_targets(
    cfg: &MediaBrowserConfig,
    refresh_roots: &[PathBuf],
    affected_paths: &[PathBuf],
) -> RefreshTargetPlan {
    let targeted = dedup_non_empty_paths(affected_paths);
    if targeted.is_empty() {
        return RefreshTargetPlan::default();
    }

    if !cfg.abort_refresh_when_capped || !cfg.fallback_to_library_roots_when_capped {
        return RefreshTargetPlan {
            targets: targeted,
            ..RefreshTargetPlan::default()
        };
    }

    let targeted_batches = planned_media_browser_batches(cfg, &targeted);
    if cfg.max_refresh_batches_per_run == 0 || targeted_batches <= cfg.max_refresh_batches_per_run {
        return RefreshTargetPlan {
            targets: targeted,
            ..RefreshTargetPlan::default()
        };
    }

    let roots = dedup_non_empty_paths(refresh_roots);
    if roots.is_empty() {
        return RefreshTargetPlan {
            targets: targeted,
            ..RefreshTargetPlan::default()
        };
    }

    let root_batches = planned_media_browser_batches(cfg, &roots);
    if root_batches == 0
        || root_batches > cfg.max_refresh_batches_per_run
        || root_batches >= targeted_batches
    {
        return RefreshTargetPlan {
            targets: targeted,
            ..RefreshTargetPlan::default()
        };
    }

    RefreshTargetPlan {
        targets: roots.clone(),
        coalesced_paths: targeted.len().saturating_sub(roots.len()),
        coalesced_batches: targeted_batches.saturating_sub(root_batches),
        root_fallback_applied: true,
    }
}

fn refresh_targets_for_server(
    cfg: &Config,
    server: MediaServerKind,
    refresh_roots: &[PathBuf],
    affected_paths: &[PathBuf],
) -> RefreshTargetPlan {
    match server {
        MediaServerKind::Plex => RefreshTargetPlan {
            targets: dedup_non_empty_paths(refresh_roots),
            ..RefreshTargetPlan::default()
        },
        MediaServerKind::Emby => {
            select_media_browser_refresh_targets(&cfg.emby, refresh_roots, affected_paths)
        }
        MediaServerKind::Jellyfin => {
            select_media_browser_refresh_targets(&cfg.jellyfin, refresh_roots, affected_paths)
        }
    }
}

async fn refresh_paths_for_server(
    cfg: &Config,
    server: MediaServerKind,
    refresh_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryRefreshTelemetry> {
    match server {
        MediaServerKind::Plex => plex::refresh_library_paths(cfg, refresh_paths, emit_text).await,
        MediaServerKind::Emby => emby::refresh_library_paths(cfg, refresh_paths, emit_text).await,
        MediaServerKind::Jellyfin => {
            jellyfin::refresh_library_paths(cfg, refresh_paths, emit_text).await
        }
    }
}

pub(crate) async fn refresh_library_paths(
    cfg: &Config,
    refresh_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryRefreshTelemetry> {
    Ok(
        refresh_library_paths_detailed(cfg, refresh_paths, emit_text)
            .await?
            .aggregate,
    )
}

pub(crate) async fn refresh_library_paths_detailed(
    cfg: &Config,
    refresh_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryRefreshOutcome> {
    let pending_deferred_paths = pending_deferred_refresh_count(cfg)?;
    if refresh_paths.is_empty() && pending_deferred_paths == 0 {
        return Ok(LibraryRefreshOutcome::default());
    }

    let servers = configured_refresh_backends(cfg);
    if servers.is_empty() {
        return Ok(LibraryRefreshOutcome {
            aggregate: LibraryRefreshTelemetry {
                requested_paths: refresh_paths.len(),
                ..LibraryRefreshTelemetry::default()
            },
            servers: Vec::new(),
        });
    }

    let Some(_guard) = try_acquire_media_refresh_guard(cfg)? else {
        if !refresh_paths.is_empty() {
            let deferred_entries = servers
                .iter()
                .copied()
                .map(|server| (server, dedup_non_empty_paths(refresh_paths)))
                .collect::<Vec<_>>();
            queue_deferred_refresh_targets(cfg, &deferred_entries)?;
        }
        emit_refresh_lock_contention(emit_text, &servers);
        let mut aggregate = LibraryRefreshTelemetry::default();
        let requested_paths = if !refresh_paths.is_empty() {
            refresh_paths.len()
        } else {
            pending_deferred_paths
        };
        let server_outcomes = servers
            .into_iter()
            .map(|server| {
                let refresh = deferred_refresh_telemetry(requested_paths);
                merge_refresh_telemetry(&mut aggregate, &refresh);
                LibraryInvalidationServerOutcome {
                    server,
                    requested_targets: requested_paths,
                    refresh,
                }
            })
            .collect();
        return Ok(LibraryRefreshOutcome {
            aggregate,
            servers: server_outcomes,
        });
    };

    let deferred_queue = take_deferred_refresh_queue(cfg)?;
    let mut aggregate = LibraryRefreshTelemetry::default();
    let mut server_outcomes = Vec::new();
    for server in servers {
        let mut server_targets = dedup_non_empty_paths(refresh_paths);
        if let Some(deferred) = deferred_queue
            .servers
            .iter()
            .find(|entry| entry.server == server)
        {
            if emit_text && !deferred.paths.is_empty() {
                crate::utils::user_println(format!(
                    "   📺 {}: draining {} deferred refresh target(s) from an earlier locked run",
                    server,
                    deferred.paths.len()
                ));
            }
            merge_dedup_paths(&mut server_targets, &deferred.paths);
        }

        if server_targets.is_empty() {
            continue;
        }

        match refresh_paths_for_server(cfg, server, &server_targets, emit_text).await {
            Ok(telemetry) => {
                merge_refresh_telemetry(&mut aggregate, &telemetry);
                server_outcomes.push(LibraryInvalidationServerOutcome {
                    server,
                    requested_targets: server_targets.len(),
                    refresh: telemetry,
                });
            }
            Err(err) => {
                if emit_text {
                    crate::utils::user_println(format!(
                        "   ⚠️  {} refresh failed: {}",
                        server, err
                    ));
                }
                let failed = LibraryRefreshTelemetry {
                    requested_paths: server_targets.len(),
                    failed_batches: 1,
                    skipped_batches: 1,
                    ..LibraryRefreshTelemetry::default()
                };
                merge_refresh_telemetry(&mut aggregate, &failed);
                server_outcomes.push(LibraryInvalidationServerOutcome {
                    server,
                    requested_targets: server_targets.len(),
                    refresh: failed,
                });
            }
        }
    }

    Ok(LibraryRefreshOutcome {
        aggregate,
        servers: server_outcomes,
    })
}

pub(crate) async fn refresh_selected_library_roots(
    cfg: &Config,
    libraries: &[&LibraryConfig],
    emit_text: bool,
) -> Result<LibraryRefreshTelemetry> {
    let roots = selected_library_root_paths(libraries);
    refresh_library_paths(cfg, &roots, emit_text).await
}

pub(crate) fn refresh_root_paths_for_affected_paths(
    libraries: &[&LibraryConfig],
    affected_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    for path in affected_paths {
        let best = libraries
            .iter()
            .filter(|library| path.starts_with(&library.path))
            .max_by_key(|library| library.path.components().count())
            .map(|library| library.path.clone());
        if let Some(root) = best {
            roots.push(root);
        }
    }

    roots.sort();
    roots.dedup();
    roots
}

pub(crate) async fn invalidate_after_mutation(
    cfg: &Config,
    libraries: &[&LibraryConfig],
    affected_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryInvalidationOutcome> {
    let refresh_roots = refresh_root_paths_for_affected_paths(libraries, affected_paths);
    let configured_servers = configured_refresh_backends(cfg);
    let configured = !configured_servers.is_empty();

    if refresh_roots.is_empty() {
        return Ok(LibraryInvalidationOutcome {
            server: configured_servers
                .first()
                .copied()
                .filter(|_| configured_servers.len() == 1),
            configured,
            ..LibraryInvalidationOutcome::default()
        });
    }

    if configured {
        let Some(_guard) = try_acquire_media_refresh_guard(cfg)? else {
            let mut deferred_entries = Vec::new();
            for server in configured_servers.iter().copied() {
                let refresh_plan =
                    refresh_targets_for_server(cfg, server, &refresh_roots, affected_paths);
                if refresh_plan.targets.is_empty() {
                    continue;
                }
                deferred_entries.push((server, refresh_plan.targets.clone()));
            }
            queue_deferred_refresh_targets(cfg, &deferred_entries)?;
            emit_refresh_lock_contention(emit_text, &configured_servers);
            let mut aggregate = LibraryRefreshTelemetry::default();
            let mut server_outcomes = Vec::new();
            for server in configured_servers.iter().copied() {
                let refresh_plan =
                    refresh_targets_for_server(cfg, server, &refresh_roots, affected_paths);
                if refresh_plan.targets.is_empty() {
                    continue;
                }

                let mut refresh = deferred_refresh_telemetry(refresh_plan.targets.len());
                refresh.coalesced_batches = refresh_plan.coalesced_batches;
                refresh.coalesced_paths = refresh_plan.coalesced_paths;
                merge_refresh_telemetry(&mut aggregate, &refresh);
                server_outcomes.push(LibraryInvalidationServerOutcome {
                    server,
                    requested_targets: refresh_plan.targets.len(),
                    refresh,
                });
            }

            return Ok(LibraryInvalidationOutcome {
                server: configured_servers
                    .first()
                    .copied()
                    .filter(|_| configured_servers.len() == 1),
                requested_library_roots: refresh_roots.len(),
                configured,
                refresh: Some(aggregate),
                servers: server_outcomes,
            });
        };

        let deferred_queue = take_deferred_refresh_queue(cfg)?;
        let mut aggregate = LibraryRefreshTelemetry::default();
        let mut server_outcomes = Vec::new();
        for server in configured_servers.iter().copied() {
            let refresh_plan =
                refresh_targets_for_server(cfg, server, &refresh_roots, affected_paths);
            let mut effective_targets = refresh_plan.targets;
            if let Some(deferred) = deferred_queue
                .servers
                .iter()
                .find(|entry| entry.server == server)
            {
                if emit_text && !deferred.paths.is_empty() {
                    crate::utils::user_println(format!(
                        "   📺 {}: draining {} deferred refresh target(s) from an earlier locked run",
                        server,
                        deferred.paths.len()
                    ));
                }
                merge_dedup_paths(&mut effective_targets, &deferred.paths);
            }

            if effective_targets.is_empty() {
                continue;
            }

            if emit_text && refresh_plan.root_fallback_applied {
                crate::utils::user_println(format!(
                    "   📺 {}: falling back to {} library-root invalidation target(s) to stay under the cap; avoided {} targeted path(s) and {} queued request(s)",
                    server,
                    effective_targets.len(),
                    refresh_plan.coalesced_paths,
                    refresh_plan.coalesced_batches
                ));
            }

            match refresh_paths_for_server(cfg, server, &effective_targets, emit_text).await {
                Ok(mut refresh) => {
                    refresh.coalesced_paths += refresh_plan.coalesced_paths;
                    refresh.coalesced_batches += refresh_plan.coalesced_batches;
                    merge_refresh_telemetry(&mut aggregate, &refresh);
                    server_outcomes.push(LibraryInvalidationServerOutcome {
                        server,
                        requested_targets: effective_targets.len(),
                        refresh,
                    });
                }
                Err(err) => {
                    if emit_text {
                        crate::utils::user_println(format!(
                            "   ⚠️  {} refresh failed: {}",
                            server, err
                        ));
                    }
                    let refresh = LibraryRefreshTelemetry {
                        requested_paths: effective_targets.len(),
                        coalesced_batches: refresh_plan.coalesced_batches,
                        coalesced_paths: refresh_plan.coalesced_paths,
                        skipped_batches: 1,
                        failed_batches: 1,
                        ..LibraryRefreshTelemetry::default()
                    };
                    merge_refresh_telemetry(&mut aggregate, &refresh);
                    server_outcomes.push(LibraryInvalidationServerOutcome {
                        server,
                        requested_targets: effective_targets.len(),
                        refresh,
                    });
                }
            }
        }

        return Ok(LibraryInvalidationOutcome {
            server: configured_servers
                .first()
                .copied()
                .filter(|_| configured_servers.len() == 1),
            requested_library_roots: refresh_roots.len(),
            configured,
            refresh: Some(aggregate),
            servers: server_outcomes,
        });
    }

    Ok(LibraryInvalidationOutcome {
        server: None,
        requested_library_roots: refresh_roots.len(),
        configured,
        refresh: None,
        servers: Vec::new(),
    })
}

pub(crate) fn has_configured_invalidation_server(cfg: &Config) -> bool {
    !configured_refresh_backends(cfg).is_empty()
}

pub(crate) fn configured_refresh_backends(cfg: &Config) -> Vec<MediaServerKind> {
    let mut enabled = Vec::new();
    if cfg.has_plex_refresh() {
        enabled.push(MediaServerKind::Plex);
    }
    if cfg.has_emby_refresh() {
        enabled.push(MediaServerKind::Emby);
    }
    if cfg.has_jellyfin_refresh() {
        enabled.push(MediaServerKind::Jellyfin);
    }
    enabled
}

pub(crate) fn configured_media_servers(cfg: &Config) -> Vec<MediaServerKind> {
    let mut servers = Vec::new();
    if cfg.has_plex() {
        servers.push(MediaServerKind::Plex);
    }
    if cfg.has_emby() {
        servers.push(MediaServerKind::Emby);
    }
    if cfg.has_jellyfin() {
        servers.push(MediaServerKind::Jellyfin);
    }
    servers
}

pub(crate) async fn probe_media_server(
    cfg: &Config,
    server: MediaServerKind,
) -> Option<Result<usize>> {
    match server {
        MediaServerKind::Plex => {
            if cfg.has_plex() {
                Some(plex::probe_sections(cfg).await)
            } else {
                None
            }
        }
        MediaServerKind::Emby => {
            if cfg.has_emby() {
                Some(emby::probe_libraries(cfg).await)
            } else {
                None
            }
        }
        MediaServerKind::Jellyfin => {
            if cfg.has_jellyfin() {
                Some(jellyfin::probe_libraries(cfg).await)
            } else {
                None
            }
        }
    }
}

pub(crate) fn selected_library_root_paths(libraries: &[&LibraryConfig]) -> Vec<PathBuf> {
    let mut roots: Vec<_> = libraries
        .iter()
        .map(|library| library.path.clone())
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::models::MediaType;

    fn test_config() -> Config {
        Config {
            libraries: vec![LibraryConfig {
                name: "Anime".to_string(),
                path: PathBuf::from("/mnt/storage/plex/anime"),
                media_type: MediaType::Tv,
                content_type: Some(ContentType::Anime),
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: PathBuf::from("/mnt/zurg/__all__"),
                media_type: "auto".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            backup: BackupConfig::default(),
            db_path: "/tmp/test.sqlite".to_string(),
            log_level: "info".to_string(),
            daemon: DaemonConfig::default(),
            symlink: SymlinkConfig::default(),
            matching: MatchingConfig::default(),
            prowlarr: ProwlarrConfig::default(),
            bazarr: BazarrConfig::default(),
            tautulli: TautulliConfig::default(),
            plex: PlexConfig::default(),
            emby: MediaBrowserConfig::default(),
            jellyfin: MediaBrowserConfig::default(),
            radarr: RadarrConfig::default(),
            sonarr: SonarrConfig::default(),
            sonarr_anime: SonarrConfig::default(),
            features: FeaturesConfig::default(),
            security: SecurityConfig::default(),
            cleanup: CleanupPolicyConfig::default(),
            web: WebConfig::default(),
            loaded_from: None,
            secret_files: Vec::new(),
        }
    }

    #[test]
    fn selected_library_root_paths_dedupes_and_sorts() {
        let movie = LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/mnt/storage/plex/movies"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        };
        let anime = LibraryConfig {
            name: "Anime".to_string(),
            path: PathBuf::from("/mnt/storage/plex/anime"),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        };

        let roots = selected_library_root_paths(&[&movie, &anime, &movie]);
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/mnt/storage/plex/anime"),
                PathBuf::from("/mnt/storage/plex/movies"),
            ]
        );
    }

    #[test]
    fn refresh_root_paths_for_affected_paths_prefers_longest_matching_root() {
        let root = LibraryConfig {
            name: "Root".to_string(),
            path: PathBuf::from("/mnt/storage/plex"),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Tv),
            depth: 1,
        };
        let anime = LibraryConfig {
            name: "Anime".to_string(),
            path: PathBuf::from("/mnt/storage/plex/anime"),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        };

        let roots = refresh_root_paths_for_affected_paths(
            &[&root, &anime],
            &[
                PathBuf::from("/mnt/storage/plex/anime/Show/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/serier/Show/Season 01/E01.mkv"),
            ],
        );

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/mnt/storage/plex"),
                PathBuf::from("/mnt/storage/plex/anime"),
            ]
        );
    }

    #[test]
    fn configured_refresh_backends_returns_empty_without_backend() {
        let cfg = test_config();
        assert!(configured_refresh_backends(&cfg).is_empty());
        assert!(!has_configured_invalidation_server(&cfg));
    }

    #[test]
    fn configured_refresh_backends_supports_multiple_backends() {
        let mut cfg = test_config();
        cfg.plex.url = "http://localhost:32400".to_string();
        cfg.plex.token = "plex-token".to_string();
        cfg.emby.url = "http://localhost:8096".to_string();
        cfg.emby.api_key = "emby-key".to_string();

        assert_eq!(
            configured_refresh_backends(&cfg),
            vec![MediaServerKind::Plex, MediaServerKind::Emby]
        );
        assert!(has_configured_invalidation_server(&cfg));
    }

    #[test]
    fn media_browser_target_plan_falls_back_to_library_roots_when_cap_would_abort() {
        let mut cfg = test_config();
        cfg.emby.url = "http://localhost:8096".to_string();
        cfg.emby.api_key = "emby-key".to_string();
        cfg.emby.refresh_batch_size = 2;
        cfg.emby.max_refresh_batches_per_run = 1;
        cfg.emby.abort_refresh_when_capped = true;
        cfg.emby.fallback_to_library_roots_when_capped = true;

        let plan = refresh_targets_for_server(
            &cfg,
            MediaServerKind::Emby,
            &[PathBuf::from("/mnt/storage/plex/anime")],
            &[
                PathBuf::from("/mnt/storage/plex/anime/Show 1/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/anime/Show 2/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/anime/Show 3/Season 01/E01.mkv"),
            ],
        );

        assert_eq!(plan.targets, vec![PathBuf::from("/mnt/storage/plex/anime")]);
        assert!(plan.root_fallback_applied);
        assert_eq!(plan.coalesced_paths, 2);
        assert_eq!(plan.coalesced_batches, 1);
    }

    #[test]
    fn media_browser_target_plan_keeps_targeted_paths_when_fallback_disabled() {
        let mut cfg = test_config();
        cfg.jellyfin.url = "http://localhost:8097".to_string();
        cfg.jellyfin.api_key = "jellyfin-key".to_string();
        cfg.jellyfin.refresh_batch_size = 2;
        cfg.jellyfin.max_refresh_batches_per_run = 1;
        cfg.jellyfin.abort_refresh_when_capped = true;
        cfg.jellyfin.fallback_to_library_roots_when_capped = false;

        let plan = refresh_targets_for_server(
            &cfg,
            MediaServerKind::Jellyfin,
            &[PathBuf::from("/mnt/storage/plex/anime")],
            &[
                PathBuf::from("/mnt/storage/plex/anime/Show 1/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/anime/Show 2/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/anime/Show 3/Season 01/E01.mkv"),
            ],
        );

        assert_eq!(plan.targets.len(), 3);
        assert!(!plan.root_fallback_applied);
        assert_eq!(plan.coalesced_paths, 0);
        assert_eq!(plan.coalesced_batches, 0);
    }

    #[test]
    fn media_browser_target_plan_skips_fallback_when_roots_still_exceed_cap() {
        let mut cfg = test_config();
        cfg.emby.url = "http://localhost:8096".to_string();
        cfg.emby.api_key = "emby-key".to_string();
        cfg.emby.refresh_batch_size = 1;
        cfg.emby.max_refresh_batches_per_run = 1;
        cfg.emby.abort_refresh_when_capped = true;
        cfg.emby.fallback_to_library_roots_when_capped = true;

        let plan = refresh_targets_for_server(
            &cfg,
            MediaServerKind::Emby,
            &[
                PathBuf::from("/mnt/storage/plex/anime"),
                PathBuf::from("/mnt/storage/plex/series"),
            ],
            &[
                PathBuf::from("/mnt/storage/plex/anime/Show 1/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/series/Show 2/Season 01/E01.mkv"),
                PathBuf::from("/mnt/storage/plex/series/Show 3/Season 01/E01.mkv"),
            ],
        );

        assert_eq!(plan.targets.len(), 3);
        assert!(!plan.root_fallback_applied);
    }

    #[tokio::test]
    async fn invalidate_after_mutation_reports_unconfigured_when_no_backend_exists() {
        let cfg = test_config();
        let library = &cfg.libraries[0];
        let outcome = invalidate_after_mutation(
            &cfg,
            &[library],
            &[PathBuf::from(
                "/mnt/storage/plex/anime/Show/Season 01/E01.mkv",
            )],
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome.server, None);
        assert!(!outcome.configured);
        assert_eq!(outcome.requested_library_roots, 1);
        assert!(outcome.refresh.is_none());
        assert!(outcome.servers.is_empty());
    }

    #[test]
    fn summary_suffix_mentions_multiple_backends_when_present() {
        let outcome = LibraryInvalidationOutcome {
            server: None,
            requested_library_roots: 2,
            configured: true,
            refresh: Some(LibraryRefreshTelemetry {
                refreshed_batches: 3,
                ..LibraryRefreshTelemetry::default()
            }),
            servers: vec![
                LibraryInvalidationServerOutcome {
                    server: MediaServerKind::Plex,
                    requested_targets: 2,
                    refresh: LibraryRefreshTelemetry {
                        refreshed_batches: 1,
                        ..LibraryRefreshTelemetry::default()
                    },
                },
                LibraryInvalidationServerOutcome {
                    server: MediaServerKind::Emby,
                    requested_targets: 4,
                    refresh: LibraryRefreshTelemetry {
                        refreshed_batches: 2,
                        ..LibraryRefreshTelemetry::default()
                    },
                },
            ],
        };

        let summary = outcome.summary_suffix().unwrap();
        assert!(summary.contains("Plex, Emby"));
        assert!(summary.contains("across 2 server(s)"));
    }

    #[test]
    fn summary_suffix_mentions_lock_deferral_when_present() {
        let outcome = LibraryInvalidationOutcome {
            server: None,
            requested_library_roots: 2,
            configured: true,
            refresh: Some(LibraryRefreshTelemetry {
                deferred_due_to_lock: true,
                ..LibraryRefreshTelemetry::default()
            }),
            servers: vec![LibraryInvalidationServerOutcome {
                server: MediaServerKind::Plex,
                requested_targets: 2,
                refresh: LibraryRefreshTelemetry {
                    requested_paths: 2,
                    deferred_due_to_lock: true,
                    ..LibraryRefreshTelemetry::default()
                },
            }],
        };

        let summary = outcome.summary_suffix().unwrap();
        assert!(summary.contains("deferred"));
        assert!(summary.contains("already refreshing"));
    }

    #[tokio::test]
    async fn refresh_library_paths_detailed_defers_when_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.plex.url = "http://localhost:32400".to_string();
        cfg.plex.token = "plex-token".to_string();
        cfg.backup.path = dir.path().join("backups");
        std::fs::create_dir_all(&cfg.backup.path).unwrap();

        let lock_path = media_refresh_lock_path(&cfg);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0);

        let outcome = refresh_library_paths_detailed(
            &cfg,
            &[PathBuf::from(
                "/mnt/storage/plex/anime/Show/Season 01/E01.mkv",
            )],
            false,
        )
        .await
        .unwrap();

        assert!(outcome.aggregate.deferred_due_to_lock);
        assert_eq!(outcome.servers.len(), 1);
        assert!(outcome.servers[0].refresh.deferred_due_to_lock);
        assert!(media_refresh_queue_path(&cfg).exists());
    }

    #[test]
    fn try_acquire_media_refresh_guard_creates_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.backup.path = dir.path().join("nested/backups");

        let guard = try_acquire_media_refresh_guard(&cfg).unwrap();
        assert!(guard.is_some());
        assert!(media_refresh_lock_path(&cfg).exists());
    }

    #[test]
    fn deferred_refresh_queue_roundtrip_dedupes_per_server() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.backup.path = dir.path().join("backups");
        std::fs::create_dir_all(&cfg.backup.path).unwrap();

        queue_deferred_refresh_targets(
            &cfg,
            &[
                (
                    MediaServerKind::Emby,
                    vec![
                        PathBuf::from("/mnt/storage/plex/anime"),
                        PathBuf::from("/mnt/storage/plex/anime"),
                    ],
                ),
                (
                    MediaServerKind::Plex,
                    vec![PathBuf::from("/mnt/storage/plex/series")],
                ),
            ],
        )
        .unwrap();
        queue_deferred_refresh_targets(
            &cfg,
            &[(
                MediaServerKind::Emby,
                vec![PathBuf::from("/mnt/storage/plex/movies")],
            )],
        )
        .unwrap();

        let queue = take_deferred_refresh_queue(&cfg).unwrap();
        assert_eq!(queue.servers.len(), 2);
        assert_eq!(
            queue.servers[0],
            DeferredRefreshQueueServer {
                server: MediaServerKind::Emby,
                paths: vec![
                    PathBuf::from("/mnt/storage/plex/anime"),
                    PathBuf::from("/mnt/storage/plex/movies")
                ]
            }
        );
        assert_eq!(
            queue.servers[1],
            DeferredRefreshQueueServer {
                server: MediaServerKind::Plex,
                paths: vec![PathBuf::from("/mnt/storage/plex/series")]
            }
        );
        assert!(!media_refresh_queue_path(&cfg).exists());
    }
}
