
use super::*;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn test_config() -> Config {
    Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/tmp/library"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/tmp/source"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: default_db_path(),
        log_level: default_log_level(),
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
fn candidate_paths_include_app_config_when_no_explicit_path_is_set() {
    let _guard = env_lock().lock().unwrap();
    std::env::remove_var("SYMLINKARR_CONFIG");

    let paths = candidate_config_paths(None);
    assert_eq!(
        paths,
        vec![
            PathBuf::from("config.yaml"),
            PathBuf::from("/app/config/config.yaml")
        ]
    );
}

#[test]
fn candidate_paths_prioritize_env_override() {
    let _guard = env_lock().lock().unwrap();
    std::env::set_var("SYMLINKARR_CONFIG", "/tmp/symlinkarr-env.yaml");

    let paths = candidate_config_paths(None);
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/tmp/symlinkarr-env.yaml"),
            PathBuf::from("config.yaml"),
            PathBuf::from("/app/config/config.yaml")
        ]
    );

    std::env::remove_var("SYMLINKARR_CONFIG");
}

#[test]
fn load_dotenv_file_collects_missing_vars_without_overwriting_existing_env() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join(".env");
    std::fs::write(
        &env_path,
        "SYMLINKARR_FROM_FILE=file-value\nexport SYMLINKARR_QUOTED=\"quoted value\"\n",
    )
    .unwrap();

    std::env::remove_var("SYMLINKARR_FROM_FILE");
    std::env::set_var("SYMLINKARR_QUOTED", "shell-value");

    let mut overlay = DotenvOverlay::new();
    let loaded = load_dotenv_file(&env_path, &mut overlay).unwrap();
    assert_eq!(loaded, 1);
    assert_eq!(
        overlay.get("SYMLINKARR_FROM_FILE").map(String::as_str),
        Some("file-value")
    );
    assert_eq!(overlay.get("SYMLINKARR_QUOTED"), None);
    assert_eq!(
        std::env::var("SYMLINKARR_QUOTED").unwrap(),
        "shell-value".to_string()
    );

    std::env::remove_var("SYMLINKARR_FROM_FILE");
    std::env::remove_var("SYMLINKARR_QUOTED");
}

#[test]
fn resolve_secret_reads_secretfile() {
    let dir = tempfile::tempdir().unwrap();
    let secret_path = dir.path().join("api.key");
    std::fs::write(&secret_path, "secret-value\n").unwrap();

    let value = resolve_secret(
        &format!("secretfile:{}", secret_path.display()),
        "api.tmdb_api_key",
        true,
        None,
        None,
    )
    .unwrap();
    assert_eq!(value, "secret-value");
}

#[test]
fn resolve_secret_rejects_plaintext_when_provider_is_required() {
    let err = resolve_secret("plaintext", "api.tmdb_api_key", true, None, None).unwrap_err();
    assert!(err.to_string().contains("Plaintext secret is not allowed"));
}

#[test]
fn resolve_secretfile_relative_to_config_directory() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("tmdb.key"), "tmdb-secret").unwrap();

    let value = resolve_secret(
        "secretfile:tmdb.key",
        "api.tmdb_api_key",
        true,
        Some(&config_dir),
        None,
    )
    .unwrap();
    assert_eq!(value, "tmdb-secret");
}

#[test]
fn resolve_secret_prefers_real_env_over_dotenv_overlay() {
    let _guard = env_lock().lock().unwrap();
    std::env::set_var("SYMLINKARR_DOTENV_TEST", "real-env");
    let overlay = DotenvOverlay::from([(
        "SYMLINKARR_DOTENV_TEST".to_string(),
        "dotenv-value".to_string(),
    )]);

    let value = resolve_secret(
        "env:SYMLINKARR_DOTENV_TEST",
        "realdebrid.api_token",
        true,
        None,
        Some(&overlay),
    )
    .unwrap();
    assert_eq!(value, "real-env");
    std::env::remove_var("SYMLINKARR_DOTENV_TEST");
}

#[test]
fn resolve_secret_uses_dotenv_overlay_when_real_env_missing() {
    let _guard = env_lock().lock().unwrap();
    std::env::remove_var("SYMLINKARR_DOTENV_TEST");
    let overlay = DotenvOverlay::from([(
        "SYMLINKARR_DOTENV_TEST".to_string(),
        "dotenv-value".to_string(),
    )]);

    let value = resolve_secret(
        "env:SYMLINKARR_DOTENV_TEST",
        "realdebrid.api_token",
        true,
        None,
        Some(&overlay),
    )
    .unwrap();
    assert_eq!(value, "dotenv-value");
}

#[test]
fn config_load_reads_env_file_from_config_directory() {
    let _guard = env_lock().lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.yaml");
    let env_path = dir.path().join(".env");
    std::env::remove_var("SYMLINKARR_RD_TOKEN");

    std::fs::write(&env_path, "SYMLINKARR_RD_TOKEN=rd-from-dotenv\n").unwrap();
    std::fs::write(
        &config_path,
        r#"
libraries:
  - name: Movies
    path: "/tmp/library"
    media_type: movie
sources:
  - name: RD
    path: "/tmp/source"
    media_type: auto
realdebrid:
  api_token: "env:SYMLINKARR_RD_TOKEN"
"#,
    )
    .unwrap();

    let cfg = Config::load(Some(config_path.display().to_string())).unwrap();
    assert_eq!(cfg.realdebrid.api_token, "rd-from-dotenv");

    std::env::remove_var("SYMLINKARR_RD_TOKEN");
}

#[test]
fn validate_rejects_zero_daemon_interval() {
    let mut cfg = test_config();
    cfg.daemon.interval_minutes = 0;

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .contains(&"daemon.interval_minutes must be greater than 0".to_string()));
}

#[test]
fn validate_rejects_unknown_source_media_type() {
    let mut cfg = test_config();
    cfg.sources[0].media_type = "tvv".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(report.errors.iter().any(|error| {
        error.contains("Source 'RD' media_type must be one of: auto, anime, tv, movie")
    }));
}

#[test]
fn validate_rejects_invalid_vacuum_hour() {
    let mut cfg = test_config();
    cfg.daemon.vacuum_hour_local = 24;

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .contains(&"daemon.vacuum_hour_local must be between 0 and 23".to_string()));
}

#[test]
fn validate_rejects_relative_library_and_source_paths() {
    let mut cfg = test_config();
    cfg.libraries = vec![LibraryConfig {
        name: "Relative Library".to_string(),
        path: PathBuf::from("relative/library"),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Tv),
        depth: 1,
    }];
    cfg.sources = vec![SourceConfig {
        name: "Relative Source".to_string(),
        path: PathBuf::from("relative/source"),
        media_type: "auto".to_string(),
    }];

    let report = cfg.validate();
    assert!(report
        .errors
        .iter()
        .any(|error: &String| error.contains("Library 'Relative Library' path must be absolute")));
    assert!(report
        .errors
        .iter()
        .any(|error: &String| error.contains("Source 'Relative Source' path must be absolute")));
}

#[test]
fn validate_treats_missing_paths_as_errors() {
    let security = SecurityConfig {
        enforce_secure_permissions: false,
        ..SecurityConfig::default()
    };
    let cfg = Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/definitely/missing/library"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/definitely/missing/source"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: default_db_path(),
        log_level: default_log_level(),
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
        security,
        cleanup: CleanupPolicyConfig::default(),
        web: WebConfig::default(),
        loaded_from: None,
        secret_files: Vec::new(),
    };

    let report = cfg.validate();
    assert_eq!(report.errors.len(), 2);
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("Library 'Movies' path does not exist")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("Source 'RD' path does not exist")));
}

#[test]
fn validate_runtime_settings_skips_missing_path_errors() {
    let security = SecurityConfig {
        enforce_secure_permissions: false,
        ..SecurityConfig::default()
    };
    let cfg = Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/definitely/missing/library"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/definitely/missing/source"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: default_db_path(),
        log_level: default_log_level(),
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
        security,
        cleanup: CleanupPolicyConfig::default(),
        web: WebConfig::default(),
        loaded_from: None,
        secret_files: Vec::new(),
    };

    let report = cfg.validate_runtime_settings();
    assert!(report.errors.is_empty());
}

#[test]
fn validate_rejects_zero_decypharr_polling_settings() {
    let mut cfg = Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/tmp/library"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/tmp/source"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: default_db_path(),
        log_level: default_log_level(),
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
    };
    cfg.decypharr.poll_interval_seconds = 0;
    cfg.decypharr.completion_timeout_minutes = 0;
    cfg.decypharr.relink_timeout_minutes = 0;
    cfg.decypharr.max_in_flight = 0;
    cfg.decypharr.max_requests_per_run = 0;
    cfg.decypharr.queue_page_size = 0;

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("decypharr.poll_interval_seconds")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("decypharr.completion_timeout_minutes")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("decypharr.relink_timeout_minutes")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("decypharr.max_in_flight")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("decypharr.max_requests_per_run")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("decypharr.queue_page_size")));
}

#[test]
fn validate_rejects_zero_realdebrid_pagination_limits() {
    let mut cfg = Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/tmp/library"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/tmp/source"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: default_db_path(),
        log_level: default_log_level(),
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
    };
    cfg.realdebrid.api_token = "token".to_string();
    cfg.realdebrid.torrents_page_limit = 0;
    cfg.realdebrid.max_pages = 0;

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("realdebrid.torrents_page_limit")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("realdebrid.max_pages")));
}

fn runtime_config_fixture() -> Config {
    Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: PathBuf::from("/tmp/library"),
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/tmp/source"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: default_db_path(),
        log_level: default_log_level(),
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
fn validate_rejects_zero_api_cache_ttl() {
    let mut cfg = runtime_config_fixture();
    cfg.api.cache_ttl_hours = 0;

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("api.cache_ttl_hours")));
}

#[test]
fn validate_accepts_high_api_cache_ttl() {
    let mut cfg = runtime_config_fixture();
    cfg.api.cache_ttl_hours = 87600; // high-but-valid override

    let report = cfg.validate_runtime_settings();
    assert!(
        report.warnings.is_empty()
            || !report
                .warnings
                .iter()
                .any(|warning| warning.contains("api.cache_ttl_hours"))
    );
}

#[test]
fn web_config_defaults_to_loopback_bind_address() {
    let web = WebConfig::default();
    assert_eq!(web.bind_address, "127.0.0.1");
    assert!(!web.allow_remote);
    assert_eq!(web.port, 8726);
}

#[test]
fn plex_config_defaults_to_safe_refresh_limits() {
    let plex = PlexConfig::default();
    assert!(plex.refresh_enabled);
    assert_eq!(plex.refresh_delay_ms, 250);
    assert_eq!(plex.refresh_coalesce_threshold, 8);
    assert_eq!(plex.max_refresh_batches_per_run, 12);
    assert!(plex.abort_refresh_when_capped);
}

#[test]
fn media_browser_config_defaults_to_safe_refresh_limits() {
    let media_browser = MediaBrowserConfig::default();
    assert!(media_browser.refresh_enabled);
    assert_eq!(media_browser.refresh_delay_ms, 250);
    assert_eq!(media_browser.refresh_batch_size, 64);
    assert_eq!(media_browser.max_refresh_batches_per_run, 12);
    assert!(media_browser.abort_refresh_when_capped);
}

#[test]
fn has_plex_refresh_respects_refresh_enabled_flag() {
    let mut cfg = runtime_config_fixture();
    cfg.plex.url = "http://localhost:32400".to_string();
    cfg.plex.token = "token".to_string();
    assert!(cfg.has_plex_refresh());

    cfg.plex.refresh_enabled = false;
    assert!(!cfg.has_plex_refresh());
    assert!(cfg.has_plex());
}

#[test]
fn has_emby_and_jellyfin_refresh_respect_refresh_enabled_flags() {
    let mut cfg = runtime_config_fixture();

    cfg.emby.url = "http://localhost:8096".to_string();
    cfg.emby.api_key = "emby-key".to_string();
    assert!(cfg.has_emby_refresh());
    cfg.emby.refresh_enabled = false;
    assert!(!cfg.has_emby_refresh());
    assert!(cfg.has_emby());

    cfg.jellyfin.url = "http://localhost:8097".to_string();
    cfg.jellyfin.api_key = "jellyfin-key".to_string();
    assert!(cfg.has_jellyfin_refresh());
    cfg.jellyfin.refresh_enabled = false;
    assert!(!cfg.has_jellyfin_refresh());
    assert!(cfg.has_jellyfin());
}

#[test]
fn validate_runtime_settings_allows_multiple_media_refresh_backends() {
    let mut cfg = runtime_config_fixture();
    cfg.plex.url = "http://localhost:32400".to_string();
    cfg.plex.token = "plex-token".to_string();
    cfg.emby.url = "http://localhost:8096".to_string();
    cfg.emby.api_key = "emby-key".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(!report.errors.iter().any(|err| {
        err.contains("Only one media-server refresh backend may be enabled at a time")
    }));
}

#[test]
fn config_load_parses_web_bind_address() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.yaml");

    std::fs::write(
        &config_path,
        r#"
libraries:
  - name: Movies
    path: "/tmp/library"
    media_type: movie
sources:
  - name: RD
    path: "/tmp/source"
    media_type: auto
web:
  enabled: true
  bind_address: "0.0.0.0"
  allow_remote: true
  port: 9999
"#,
    )
    .unwrap();

    let cfg = Config::load(Some(config_path.display().to_string())).unwrap();
    assert!(cfg.web.enabled);
    assert_eq!(cfg.web.bind_address, "0.0.0.0");
    assert!(cfg.web.allow_remote);
    assert_eq!(cfg.web.port, 9999);
}

#[test]
fn validate_rejects_empty_web_bind_address_when_enabled() {
    let mut cfg = runtime_config_fixture();
    cfg.web.enabled = true;
    cfg.web.bind_address.clear();

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("web.bind_address")));
}

#[test]
fn validate_rejects_remote_web_bind_without_explicit_opt_in() {
    let mut cfg = runtime_config_fixture();
    cfg.web.enabled = true;
    cfg.web.bind_address = "0.0.0.0".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("requires web.allow_remote=true")));
}

#[test]
fn validate_allows_remote_web_bind_with_basic_auth() {
    let mut cfg = runtime_config_fixture();
    cfg.web.enabled = true;
    cfg.web.bind_address = "0.0.0.0".to_string();
    cfg.web.allow_remote = true;
    cfg.web.username = "admin".to_string();
    cfg.web.password = "secret".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(!report
        .errors
        .iter()
        .any(|err| err.contains("web.username/web.password")));
    assert!(!report
        .errors
        .iter()
        .any(|err| err.contains("built-in HTML UI")));
}

#[test]
fn validate_rejects_partial_basic_web_auth() {
    let mut cfg = runtime_config_fixture();
    cfg.web.enabled = true;
    cfg.web.username = "admin".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("web.username and web.password")));
}

#[test]
fn validate_rejects_remote_web_with_only_api_key_auth() {
    let mut cfg = runtime_config_fixture();
    cfg.web.enabled = true;
    cfg.web.bind_address = "0.0.0.0".to_string();
    cfg.web.allow_remote = true;
    cfg.web.api_key = "token".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("web.username/web.password")));
}

#[test]
fn validate_rejects_whitespace_only_web_password() {
    let mut cfg = runtime_config_fixture();
    cfg.web.enabled = true;
    cfg.web.username = "admin".to_string();
    cfg.web.password = "   ".to_string();

    let report = cfg.validate_runtime_settings();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("web.username and web.password")));
}

#[test]
fn raw_plaintext_secret_fields_detects_unwrapped_secrets() {
    let value: serde_yml::Value = serde_yml::from_str(
        r#"
api:
  tmdb_api_key: "plaintext"
  tmdb_read_access_token: "env:TMDB_TOKEN"
realdebrid:
  api_token: "secretfile:rd.token"
sonarr:
  api_key: "another-plaintext"
web:
  api_key: "web-plaintext"
"#,
    )
    .unwrap();

    let fields = raw_plaintext_secret_fields(&value);
    assert_eq!(
        fields,
        vec!["api.tmdb_api_key", "web.api_key", "sonarr.api_key"]
    );
}

#[test]
fn collect_secret_file_paths_resolves_relative_entries() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();
    let value: serde_yml::Value = serde_yml::from_str(
        r#"
api:
  tmdb_api_key: "secretfile:tmdb.key"
prowlarr:
  api_key: "env:PROWLARR_API_KEY"
tautulli:
  api_key: "secretfile:/tmp/tautulli.key"
web:
  password: "secretfile:web.pass"
"#,
    )
    .unwrap();

    let paths = collect_secret_file_paths(&value, Some(&config_dir));
    assert_eq!(
        paths,
        vec![
            config_dir.join("tmdb.key"),
            config_dir.join("web.pass"),
            PathBuf::from("/tmp/tautulli.key")
        ]
    );
}

#[cfg(unix)]
#[test]
fn validate_reports_insecure_runtime_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let library_dir = dir.path().join("library");
    let source_dir = dir.path().join("source");
    let backup_dir = dir.path().join("backups");
    let quarantine_dir = dir.path().join("quarantine");
    let db_path = dir.path().join("symlinkarr.db");
    let secret_path = dir.path().join("tmdb.key");

    std::fs::create_dir_all(&library_dir).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&backup_dir).unwrap();
    std::fs::create_dir_all(&quarantine_dir).unwrap();
    std::fs::write(&db_path, "").unwrap();
    std::fs::write(&secret_path, "secret").unwrap();

    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&backup_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&quarantine_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let cfg = Config {
        libraries: vec![LibraryConfig {
            name: "Movies".to_string(),
            path: library_dir,
            media_type: MediaType::Movie,
            content_type: Some(ContentType::Movie),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: source_dir,
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig {
            path: backup_dir,
            ..BackupConfig::default()
        },
        db_path: db_path.display().to_string(),
        log_level: default_log_level(),
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
        security: SecurityConfig {
            enforce_secure_permissions: true,
            ..SecurityConfig::default()
        },
        cleanup: CleanupPolicyConfig {
            prune: PrunePolicyConfig {
                quarantine_path: quarantine_dir,
                ..PrunePolicyConfig::default()
            },
        },
        web: WebConfig::default(),
        loaded_from: None,
        secret_files: vec![secret_path],
    };

    let report = cfg.validate();
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("db_path must not be group/world accessible")));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("backup.path must not be group/world accessible")));
    assert!(report.errors.iter().any(
        |err| err.contains("cleanup.prune.quarantine_path must not be group/world accessible")
    ));
    assert!(report
        .errors
        .iter()
        .any(|err| err.contains("secretfile must not be group/world accessible")));
}

#[test]
fn test_content_type_display() {
    assert_eq!(ContentType::Tv.to_string(), "tv");
    assert_eq!(ContentType::Anime.to_string(), "anime");
    assert_eq!(ContentType::Movie.to_string(), "movie");
}

#[test]
fn test_content_type_from_media_type() {
    assert_eq!(ContentType::from_media_type(MediaType::Tv), ContentType::Tv);
    assert_eq!(
        ContentType::from_media_type(MediaType::Movie),
        ContentType::Movie
    );
}

#[test]
fn test_matching_mode_display() {
    assert_eq!(MatchingMode::Strict.to_string(), "strict");
    assert_eq!(MatchingMode::Balanced.to_string(), "balanced");
    assert_eq!(MatchingMode::Aggressive.to_string(), "aggressive");
}

#[test]
fn test_matching_mode_is_strict() {
    assert!(MatchingMode::Strict.is_strict());
    assert!(!MatchingMode::Balanced.is_strict());
    assert!(!MatchingMode::Aggressive.is_strict());
}

#[test]
fn test_metadata_mode_display() {
    assert_eq!(MetadataMode::Full.to_string(), "full");
    assert_eq!(MetadataMode::CacheOnly.to_string(), "cache_only");
    assert_eq!(MetadataMode::Off.to_string(), "off");
}

#[test]
fn test_metadata_mode_allows_network() {
    assert!(MetadataMode::Full.allows_network());
    assert!(!MetadataMode::CacheOnly.allows_network());
    assert!(!MetadataMode::Off.allows_network());
}

#[test]
fn test_inspect_restore_targets_resolves_relative_paths_from_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.yaml");
    std::fs::write(
        &config_path,
        r#"
db_path: "./data/symlinkarr.db"
realdebrid:
  api_token: "secretfile:./secrets/rd-token"
prowlarr:
  url: "http://localhost:9696"
  api_key: "secretfile:./secrets/prowlarr-token"
"#,
    )
    .unwrap();

    let targets = inspect_restore_targets(&config_path).unwrap();

    assert_eq!(targets.db_path, config_dir.join("data/symlinkarr.db"));
    assert_eq!(targets.secret_files.len(), 2);
    assert!(targets
        .secret_files
        .iter()
        .any(|path| path.ends_with("secrets/rd-token")));
    assert!(targets
        .secret_files
        .iter()
        .any(|path| path.ends_with("secrets/prowlarr-token")));
}

#[test]
fn test_inspect_restore_targets_uses_default_db_path_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.yaml");
    std::fs::write(
        &config_path,
        r#"
realdebrid:
  api_token: "secretfile:./secrets/rd-token"
"#,
    )
    .unwrap();

    let targets = inspect_restore_targets(&config_path).unwrap();

    assert_eq!(targets.db_path, config_dir.join("symlinkarr.db"));
    assert_eq!(
        targets.secret_files,
        vec![config_dir.join("secrets/rd-token")]
    );
}
