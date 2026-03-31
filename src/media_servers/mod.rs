use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{Config, LibraryConfig};

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
    pub failed_batches: usize,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LibraryInvalidationOutcome {
    pub server: Option<MediaServerKind>,
    pub requested_library_roots: usize,
    pub configured: bool,
    pub refresh: Option<LibraryRefreshTelemetry>,
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

        let refresh = self.refresh.as_ref()?;
        let server = self.server.unwrap_or(MediaServerKind::Plex);
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

pub(crate) async fn refresh_library_paths(
    cfg: &Config,
    refresh_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryRefreshTelemetry> {
    if refresh_paths.is_empty() {
        return Ok(LibraryRefreshTelemetry::default());
    }

    if let Some(server) = configured_invalidation_server(cfg) {
        return match server {
            MediaServerKind::Plex => {
                plex::refresh_library_paths(cfg, refresh_paths, emit_text).await
            }
            MediaServerKind::Emby => {
                emby::refresh_library_paths(cfg, refresh_paths, emit_text).await
            }
            MediaServerKind::Jellyfin => {
                jellyfin::refresh_library_paths(cfg, refresh_paths, emit_text).await
            }
        };
    }

    Ok(LibraryRefreshTelemetry {
        requested_paths: refresh_paths.len(),
        ..LibraryRefreshTelemetry::default()
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
    let configured_server = configured_invalidation_server(cfg);
    let configured = configured_server.is_some();
    let refresh_targets = match configured_server {
        Some(MediaServerKind::Plex) | None => refresh_roots.clone(),
        Some(MediaServerKind::Emby) | Some(MediaServerKind::Jellyfin) => {
            let mut paths = affected_paths.to_vec();
            paths.sort();
            paths.dedup();
            paths
        }
    };

    if refresh_roots.is_empty() || refresh_targets.is_empty() {
        return Ok(LibraryInvalidationOutcome {
            configured,
            ..LibraryInvalidationOutcome::default()
        });
    }

    if configured {
        let refresh = refresh_library_paths(cfg, &refresh_targets, emit_text).await?;
        return Ok(LibraryInvalidationOutcome {
            server: configured_server,
            requested_library_roots: refresh_roots.len(),
            configured,
            refresh: Some(refresh),
        });
    }

    Ok(LibraryInvalidationOutcome {
        server: None,
        requested_library_roots: refresh_roots.len(),
        configured,
        refresh: None,
    })
}

pub(crate) fn configured_invalidation_server(cfg: &Config) -> Option<MediaServerKind> {
    let enabled = configured_refresh_backends(cfg);
    match enabled.as_slice() {
        [server] => Some(*server),
        _ => None,
    }
}

pub(crate) fn has_configured_invalidation_server(cfg: &Config) -> bool {
    configured_invalidation_server(cfg).is_some()
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
    use crate::config::{ContentType, LibraryConfig};
    use crate::models::MediaType;

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
}
