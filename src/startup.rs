use std::path::Path;

use tracing::info;

use crate::config::Config;

const STARTUP_LOGO: &[&str] = &[
    r" ______   ____  __ _     ___ _   _ _  __    _    ____  ____  ",
    r"/ ___\ \ / /  \/  | |   |_ _| \ | | |/ /   / \  |  _ \|  _ \ ",
    r"\___ \\ V /| |\/| | |    | ||  \| | ' /   / _ \ | |_) | |_) |",
    r" ___) || | | |  | | |___ | || |\  | . \  / ___ \|  _ <|  _ < ",
    r"|____/ |_| |_|  |_|_____|___|_| \_|_|\_\/_/   \_\_| \_\_| \_\",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeMode {
    Daemon,
    Web,
}

impl RuntimeMode {
    fn label(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Web => "web",
        }
    }
}

pub(crate) fn emit_runtime_banner(
    cfg: &Config,
    mode: RuntimeMode,
    config_path: Option<&Path>,
    web_port_override: Option<u16>,
) {
    emit_banner_header();
    info!("mode: {}", mode.label());
    if let Some(path) = config_path {
        info!("config: {}", path.display());
    }
    info!("db: {}", cfg.db_path);
    info!(
        "media: {} libraries | {} sources",
        cfg.libraries.len(),
        cfg.sources.len()
    );
    info!("web: {}", web_summary(cfg, mode, web_port_override));
    info!("stack: {}", integration_summary(cfg));
    info!("{}", runtime_summary(cfg, mode));
    info!("------------------------------------------------------------");
}

pub(crate) fn emit_noconfig_banner(port: u16) {
    emit_banner_header();
    info!("mode: setup");
    info!("config: missing");
    info!("next: open http://127.0.0.1:{port} to bootstrap or restore state");
    info!("------------------------------------------------------------");
}

fn emit_banner_header() {
    info!("----------------------------------------------------------------------");
    for line in STARTUP_LOGO {
        info!("{line}");
    }
    info!(
        "v{} | local-first symlink manager",
        env!("CARGO_PKG_VERSION")
    );
    info!("----------------------------------------------------------------------");
}

fn web_summary(cfg: &Config, mode: RuntimeMode, web_port_override: Option<u16>) -> String {
    let web_active = matches!(mode, RuntimeMode::Web) || cfg.has_web();
    if !web_active {
        return "disabled".to_string();
    }

    let bind_address = cfg.web.normalized_bind_address();
    let port = web_port_override.unwrap_or(cfg.web.port);
    let auth = web_auth_summary(cfg);

    if matches!(bind_address.as_str(), "0.0.0.0" | "::") {
        format!("http://127.0.0.1:{port} (bind {bind_address}:{port}, auth {auth})")
    } else {
        format!("http://{bind_address}:{port} (auth {auth})")
    }
}

fn web_auth_summary(cfg: &Config) -> &'static str {
    match (
        cfg.web.has_partial_basic_auth(),
        cfg.web.has_basic_auth(),
        cfg.web.has_api_key_auth(),
    ) {
        (true, _, true) => "partial-basic + api-key",
        (true, _, false) => "partial-basic",
        (false, true, true) => "basic + api-key",
        (false, true, false) => "basic",
        (false, false, true) => "api-key",
        (false, false, false) => "open",
    }
}

fn integration_summary(cfg: &Config) -> String {
    let mut enabled = Vec::new();

    if cfg.has_realdebrid() {
        enabled.push("RealDebrid");
    }
    if cfg.has_decypharr() {
        enabled.push("Decypharr");
    }
    if cfg.has_dmm() {
        enabled.push("DMM");
    }
    if cfg.has_prowlarr() {
        enabled.push("Prowlarr");
    }
    if cfg.has_sonarr() {
        enabled.push("Sonarr");
    }
    if cfg.has_sonarr_anime() {
        enabled.push("Sonarr Anime");
    }
    if cfg.has_radarr() {
        enabled.push("Radarr");
    }
    if cfg.has_plex() {
        enabled.push("Plex");
    }
    if cfg.has_emby() {
        enabled.push("Emby");
    }
    if cfg.has_jellyfin() {
        enabled.push("Jellyfin");
    }
    if cfg.has_bazarr() {
        enabled.push("Bazarr");
    }
    if cfg.has_tautulli() {
        enabled.push("Tautulli");
    }
    if cfg.has_tmdb() {
        enabled.push("TMDB");
    }
    if cfg.has_tvdb() {
        enabled.push("TVDB");
    }

    if enabled.is_empty() {
        return "local-only".to_string();
    }

    const LIMIT: usize = 6;
    if enabled.len() <= LIMIT {
        return enabled.join(", ");
    }

    let remaining = enabled.len() - LIMIT;
    format!("{} +{} more", enabled[..LIMIT].join(", "), remaining)
}

fn runtime_summary(cfg: &Config, mode: RuntimeMode) -> String {
    let backups = if cfg.backup.enabled { "on" } else { "off" };

    match mode {
        RuntimeMode::Daemon => format!(
            "daemon: every {}m | auto-acquire {} | backups {}",
            cfg.daemon.interval_minutes,
            on_off(cfg.daemon.search_missing),
            backups
        ),
        RuntimeMode::Web => format!("ui: standalone web mode | backups {backups}"),
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

#[cfg(test)]
mod tests {
    use super::{integration_summary, runtime_summary, web_summary, RuntimeMode};
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, DaemonConfig,
        DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::models::MediaType;
    use std::path::PathBuf;

    fn sample_config() -> Config {
        Config {
            libraries: vec![LibraryConfig {
                name: "Shows".to_string(),
                path: PathBuf::from("/library/shows"),
                media_type: MediaType::Tv,
                content_type: None,
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: PathBuf::from("/mnt/rd"),
                media_type: "tv".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            backup: BackupConfig {
                enabled: true,
                ..BackupConfig::default()
            },
            db_path: "/app/data/symlinkarr.db".to_string(),
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
    fn web_summary_uses_local_hint_for_wildcard_bind() {
        let mut cfg = sample_config();
        cfg.web.enabled = true;
        cfg.web.bind_address = "0.0.0.0".to_string();
        cfg.web.port = 8726;

        let summary = web_summary(&cfg, RuntimeMode::Daemon, None);
        assert!(summary.contains("http://127.0.0.1:8726"));
        assert!(summary.contains("bind 0.0.0.0:8726"));
        assert!(summary.contains("auth open"));
    }

    #[test]
    fn integration_summary_truncates_long_stacks() {
        let mut cfg = sample_config();
        cfg.realdebrid.api_token = "token".to_string();
        cfg.decypharr.url = "http://decypharr".to_string();
        cfg.dmm.url = "https://dmm.example".to_string();
        cfg.prowlarr.url = "http://prowlarr".to_string();
        cfg.prowlarr.api_key = "key".to_string();
        cfg.sonarr.url = "http://sonarr".to_string();
        cfg.sonarr.api_key = "key".to_string();
        cfg.sonarr_anime.url = "http://sonarr-anime".to_string();
        cfg.sonarr_anime.api_key = "key".to_string();
        cfg.radarr.url = "http://radarr".to_string();
        cfg.radarr.api_key = "key".to_string();

        let summary = integration_summary(&cfg);
        assert!(summary.contains("RealDebrid"));
        assert!(summary.contains("+1 more"));
    }

    #[test]
    fn runtime_summary_includes_daemon_interval() {
        let mut cfg = sample_config();
        cfg.daemon.interval_minutes = 60;
        cfg.daemon.search_missing = true;

        let summary = runtime_summary(&cfg, RuntimeMode::Daemon);
        assert!(summary.contains("every 60m"));
        assert!(summary.contains("auto-acquire on"));
        assert!(summary.contains("backups on"));
    }
}
