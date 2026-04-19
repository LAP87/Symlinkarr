use std::path::PathBuf;

use anyhow::Result;
use futures_util::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};

use self::deferred::{
    dedup_non_empty_paths, deferred_refresh_telemetry, emit_refresh_lock_contention,
    merge_dedup_paths, merge_deferred_refresh_entries, pending_deferred_refresh_count,
    queue_deferred_refresh_targets, take_deferred_refresh_targets, try_acquire_media_refresh_guard,
    DeferredRefreshQueueServer,
};
pub(crate) use self::deferred::{deferred_refresh_summary, has_pending_deferred_refreshes};
use crate::config::{Config, LibraryConfig, MediaBrowserConfig};

mod deferred;
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

#[derive(Debug, Clone)]
struct PlannedServerRefresh {
    server: MediaServerKind,
    targets: Vec<PathBuf>,
    requested_targets: usize,
    coalesced_batches: usize,
    coalesced_paths: usize,
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

async fn execute_planned_server_refreshes(
    cfg: &Config,
    planned_refreshes: Vec<PlannedServerRefresh>,
    emit_text: bool,
) -> (
    LibraryRefreshTelemetry,
    Vec<LibraryInvalidationServerOutcome>,
    Vec<DeferredRefreshQueueServer>,
) {
    let mut aggregate = LibraryRefreshTelemetry::default();
    let mut server_outcomes = Vec::new();
    let mut remaining_queue = Vec::new();
    let mut pending = FuturesUnordered::new();

    for planned in planned_refreshes {
        let cfg = cfg.clone();
        pending.push(async move {
            let result =
                refresh_paths_for_server(&cfg, planned.server, &planned.targets, emit_text).await;
            (planned, result)
        });
    }

    while let Some((planned, result)) = pending.next().await {
        match result {
            Ok(mut refresh) => {
                refresh.coalesced_batches += planned.coalesced_batches;
                refresh.coalesced_paths += planned.coalesced_paths;
                merge_refresh_telemetry(&mut aggregate, &refresh);
                server_outcomes.push(LibraryInvalidationServerOutcome {
                    server: planned.server,
                    requested_targets: planned.requested_targets,
                    refresh,
                });
            }
            Err(err) => {
                remaining_queue.push(DeferredRefreshQueueServer {
                    server: planned.server,
                    paths: planned.targets.clone(),
                });
                if emit_text {
                    crate::utils::user_println(format!(
                        "   ⚠️  {} refresh failed: {}",
                        planned.server, err
                    ));
                }
                let refresh = LibraryRefreshTelemetry {
                    requested_paths: planned.requested_targets,
                    coalesced_batches: planned.coalesced_batches,
                    coalesced_paths: planned.coalesced_paths,
                    skipped_batches: 1,
                    failed_batches: 1,
                    ..LibraryRefreshTelemetry::default()
                };
                merge_refresh_telemetry(&mut aggregate, &refresh);
                server_outcomes.push(LibraryInvalidationServerOutcome {
                    server: planned.server,
                    requested_targets: planned.requested_targets,
                    refresh,
                });
            }
        }
    }

    server_outcomes.sort_by_key(|entry| entry.server.service_key());
    remaining_queue.sort_by_key(|entry| entry.server.service_key());

    (aggregate, server_outcomes, remaining_queue)
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
        let deferred_summary = deferred_refresh_summary(cfg)?;
        emit_refresh_lock_contention(emit_text, &servers);
        let mut aggregate = LibraryRefreshTelemetry::default();
        let server_outcomes = servers
            .into_iter()
            .filter_map(|server| {
                let requested_paths = if !refresh_paths.is_empty() {
                    dedup_non_empty_paths(refresh_paths).len()
                } else {
                    deferred_summary
                        .servers
                        .iter()
                        .find(|entry| entry.server == server)
                        .map(|entry| entry.queued_targets)
                        .unwrap_or(0)
                };
                if requested_paths == 0 {
                    return None;
                }
                let refresh = deferred_refresh_telemetry(requested_paths);
                merge_refresh_telemetry(&mut aggregate, &refresh);
                Some(LibraryInvalidationServerOutcome {
                    server,
                    requested_targets: requested_paths,
                    refresh,
                })
            })
            .collect();
        return Ok(LibraryRefreshOutcome {
            aggregate,
            servers: server_outcomes,
        });
    };

    let deferred_queue = take_deferred_refresh_targets(cfg, &servers)?;
    let mut planned_refreshes = Vec::new();
    for server in servers {
        let old_deferred_paths = deferred_queue
            .iter()
            .find(|entry| entry.server == server)
            .map(|entry| entry.paths.clone())
            .unwrap_or_default();
        let mut server_targets = dedup_non_empty_paths(refresh_paths);
        if !old_deferred_paths.is_empty() {
            if emit_text {
                crate::utils::user_println(format!(
                    "   📺 {}: draining {} deferred refresh target(s) from an earlier locked run",
                    server,
                    old_deferred_paths.len()
                ));
            }
            merge_dedup_paths(&mut server_targets, &old_deferred_paths);
        }

        if server_targets.is_empty() {
            continue;
        }

        planned_refreshes.push(PlannedServerRefresh {
            server,
            requested_targets: server_targets.len(),
            targets: server_targets,
            coalesced_batches: 0,
            coalesced_paths: 0,
        });
    }
    let (aggregate, server_outcomes, failed_entries) =
        execute_planned_server_refreshes(cfg, planned_refreshes, emit_text).await;
    merge_deferred_refresh_entries(cfg, failed_entries)?;

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

        let deferred_queue = take_deferred_refresh_targets(cfg, &configured_servers)?;
        let mut planned_refreshes = Vec::new();
        for server in configured_servers.iter().copied() {
            let refresh_plan =
                refresh_targets_for_server(cfg, server, &refresh_roots, affected_paths);
            let mut effective_targets = refresh_plan.targets;
            let old_deferred_paths = deferred_queue
                .iter()
                .find(|entry| entry.server == server)
                .map(|entry| entry.paths.clone())
                .unwrap_or_default();
            if !old_deferred_paths.is_empty() {
                if emit_text {
                    crate::utils::user_println(format!(
                        "   📺 {}: draining {} deferred refresh target(s) from an earlier locked run",
                        server,
                        old_deferred_paths.len()
                    ));
                }
                merge_dedup_paths(&mut effective_targets, &old_deferred_paths);
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

            planned_refreshes.push(PlannedServerRefresh {
                server,
                requested_targets: effective_targets.len(),
                targets: effective_targets,
                coalesced_batches: refresh_plan.coalesced_batches,
                coalesced_paths: refresh_plan.coalesced_paths,
            });
        }
        let (aggregate, server_outcomes, failed_entries) =
            execute_planned_server_refreshes(cfg, planned_refreshes, emit_text).await;
        merge_deferred_refresh_entries(cfg, failed_entries)?;

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
mod tests;
