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
    aggregate.failed_batches += addition.failed_batches;
}

fn refresh_targets_for_server(
    server: MediaServerKind,
    refresh_roots: &[PathBuf],
    affected_paths: &[PathBuf],
) -> Vec<PathBuf> {
    match server {
        MediaServerKind::Plex => refresh_roots.to_vec(),
        MediaServerKind::Emby | MediaServerKind::Jellyfin => {
            let mut paths = affected_paths.to_vec();
            paths.sort();
            paths.dedup();
            paths
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
    if refresh_paths.is_empty() {
        return Ok(LibraryRefreshTelemetry::default());
    }

    let servers = configured_refresh_backends(cfg);
    if servers.is_empty() {
        return Ok(LibraryRefreshTelemetry {
            requested_paths: refresh_paths.len(),
            ..LibraryRefreshTelemetry::default()
        });
    }

    let mut aggregate = LibraryRefreshTelemetry::default();
    for server in servers {
        match refresh_paths_for_server(cfg, server, refresh_paths, emit_text).await {
            Ok(telemetry) => merge_refresh_telemetry(&mut aggregate, &telemetry),
            Err(err) => {
                if emit_text {
                    crate::utils::user_println(format!(
                        "   ⚠️  {} refresh failed: {}",
                        server, err
                    ));
                }
                aggregate.requested_paths += refresh_paths.len();
                aggregate.failed_batches += 1;
                aggregate.skipped_batches += 1;
            }
        }
    }

    Ok(aggregate)
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
        let mut aggregate = LibraryRefreshTelemetry::default();
        let mut server_outcomes = Vec::new();
        for server in configured_servers.iter().copied() {
            let refresh_targets =
                refresh_targets_for_server(server, &refresh_roots, affected_paths);
            if refresh_targets.is_empty() {
                continue;
            }

            match refresh_paths_for_server(cfg, server, &refresh_targets, emit_text).await {
                Ok(refresh) => {
                    merge_refresh_telemetry(&mut aggregate, &refresh);
                    server_outcomes.push(LibraryInvalidationServerOutcome {
                        server,
                        requested_targets: refresh_targets.len(),
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
                        requested_paths: refresh_targets.len(),
                        skipped_batches: 1,
                        failed_batches: 1,
                        ..LibraryRefreshTelemetry::default()
                    };
                    merge_refresh_telemetry(&mut aggregate, &refresh);
                    server_outcomes.push(LibraryInvalidationServerOutcome {
                        server,
                        requested_targets: refresh_targets.len(),
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
}
