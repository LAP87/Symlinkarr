use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::models::MediaType;
use crate::utils::{fast_path_health, PathHealth};
use validation::validate_secure_permissions;

type DotenvOverlay = std::collections::HashMap<String, String>;

/// Content type that controls which filename parser to use.
/// Separate from MediaType — Anime maps to MediaType::Tv in the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    #[default]
    Tv,
    Anime,
    Movie,
}

impl std::fmt::Display for ContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentType::Tv => write!(f, "tv"),
            ContentType::Anime => write!(f, "anime"),
            ContentType::Movie => write!(f, "movie"),
        }
    }
}

impl ContentType {
    /// Derive content type from MediaType (fallback when not specified)
    pub fn from_media_type(mt: MediaType) -> Self {
        match mt {
            MediaType::Tv => ContentType::Tv,
            MediaType::Movie => ContentType::Movie,
        }
    }
}

/// Matching strictness policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MatchingMode {
    #[default]
    Strict,
    Balanced,
    Aggressive,
}

impl std::fmt::Display for MatchingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchingMode::Strict => write!(f, "strict"),
            MatchingMode::Balanced => write!(f, "balanced"),
            MatchingMode::Aggressive => write!(f, "aggressive"),
        }
    }
}

impl MatchingMode {
    pub fn is_strict(self) -> bool {
        matches!(self, MatchingMode::Strict)
    }
}

/// Metadata lookup policy for matching and title enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MetadataMode {
    /// Use API clients when cache is missing.
    #[default]
    Full,
    /// Use cache only (never perform new API requests).
    CacheOnly,
    /// Disable metadata entirely; local folder titles only.
    Off,
}

impl std::fmt::Display for MetadataMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataMode::Full => write!(f, "full"),
            MetadataMode::CacheOnly => write!(f, "cache_only"),
            MetadataMode::Off => write!(f, "off"),
        }
    }
}

impl MetadataMode {
    pub fn allows_network(self) -> bool {
        matches!(self, MetadataMode::Full)
    }
}

/// Matching configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchingConfig {
    /// Matching mode: strict, balanced, or aggressive
    #[serde(default)]
    pub mode: MatchingMode,
    /// Metadata lookup mode: full, cache_only, or off
    #[serde(default)]
    pub metadata_mode: MetadataMode,
    /// Maximum concurrent metadata API fetches (default: 8)
    #[serde(default = "default_metadata_concurrency")]
    pub metadata_concurrency: usize,
}

fn default_metadata_concurrency() -> usize {
    8
}

impl Default for MatchingConfig {
    fn default() -> Self {
        Self {
            mode: MatchingMode::Strict,
            metadata_mode: MetadataMode::Full,
            metadata_concurrency: default_metadata_concurrency(),
        }
    }
}

/// Web UI configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_bind_address")]
    pub bind_address: String,
    #[serde(default)]
    pub allow_remote: bool,
    #[serde(default = "default_web_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub api_key: String,
}

fn default_web_bind_address() -> String {
    "127.0.0.1".to_string()
}

fn default_web_port() -> u16 {
    8726
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_web_bind_address(),
            allow_remote: false,
            port: default_web_port(),
            username: String::new(),
            password: String::new(),
            api_key: String::new(),
        }
    }
}

impl WebConfig {
    pub fn normalized_bind_address(&self) -> String {
        let bind_address = self.bind_address.trim();
        if bind_address.is_empty() {
            default_web_bind_address()
        } else {
            bind_address.to_string()
        }
    }

    pub fn binds_loopback_only(&self) -> bool {
        let bind_address = self.normalized_bind_address();
        if bind_address.eq_ignore_ascii_case("localhost") {
            return true;
        }

        bind_address
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
    }

    pub fn requires_remote_ack(&self) -> bool {
        !self.binds_loopback_only()
    }

    pub fn has_basic_auth(&self) -> bool {
        !self.username.trim().is_empty() && !self.password.trim().is_empty()
    }

    pub fn has_partial_basic_auth(&self) -> bool {
        let has_username = !self.username.trim().is_empty();
        let has_password = !self.password.trim().is_empty();
        has_username ^ has_password
    }

    pub fn has_api_key_auth(&self) -> bool {
        !self.api_key.is_empty()
    }
}

/// Top-level application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Plex/Jellyfin library directories to scan for ID-tagged folders
    pub libraries: Vec<LibraryConfig>,
    /// Real-Debrid mount directories to scan for source files
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    /// API keys and settings
    #[serde(default)]
    pub api: ApiConfig,
    /// Real-Debrid API settings
    #[serde(default)]
    pub realdebrid: RealDebridConfig,
    /// Decypharr integration settings
    #[serde(default)]
    pub decypharr: DecypharrConfig,
    /// Debrid Media Manager integration (optional)
    #[serde(default)]
    pub dmm: DmmConfig,
    /// Symlink backup settings
    #[serde(default)]
    pub backup: BackupConfig,
    /// Path to the SQLite database file
    #[serde(default = "default_db_path")]
    pub db_path: String,
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Daemon/scheduler settings
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Symlink creation settings
    #[serde(default)]
    pub symlink: SymlinkConfig,
    /// Matching behavior settings
    #[serde(default)]
    pub matching: MatchingConfig,
    /// Prowlarr indexer integration (optional)
    #[serde(default)]
    pub prowlarr: ProwlarrConfig,
    /// Bazarr subtitle integration (optional)
    #[serde(default)]
    pub bazarr: BazarrConfig,
    /// Tautulli Plex stats integration (optional)
    #[serde(default)]
    pub tautulli: TautulliConfig,
    /// Plex integration for targeted library refresh (optional)
    #[serde(default)]
    pub plex: PlexConfig,
    /// Emby integration for targeted library invalidation (optional)
    #[serde(default)]
    pub emby: MediaBrowserConfig,
    /// Jellyfin integration for targeted library invalidation (optional)
    #[serde(default)]
    pub jellyfin: MediaBrowserConfig,
    /// Radarr integration (optional)
    #[serde(default)]
    pub radarr: RadarrConfig,
    /// Sonarr integration (optional)
    #[serde(default)]
    pub sonarr: SonarrConfig,
    /// Sonarr Anime instance (optional)
    #[serde(default)]
    pub sonarr_anime: SonarrConfig,
    /// Feature flags
    #[serde(default)]
    pub features: FeaturesConfig,
    /// Security policy
    #[serde(default)]
    pub security: SecurityConfig,
    /// Cleanup policy
    #[serde(default)]
    pub cleanup: CleanupPolicyConfig,
    /// Web UI settings
    #[serde(default)]
    pub web: WebConfig,
    /// Path of the config file that was loaded, when available
    #[serde(skip)]
    pub loaded_from: Option<PathBuf>,
    /// Resolved secret files referenced via `secretfile:`
    #[serde(skip)]
    pub secret_files: Vec<PathBuf>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreConfigTargets {
    pub db_path: PathBuf,
    pub secret_files: Vec<PathBuf>,
}

/// Configuration for a Plex/Jellyfin library directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryConfig {
    /// Human-readable name (e.g., "Serier", "Filmer", "Anime")
    pub name: String,
    /// Absolute path to the library root
    pub path: PathBuf,
    /// Media type: tv or movie (used in DB)
    #[serde(default = "default_media_type")]
    pub media_type: MediaType,
    /// Content type: tv, anime, or movie (controls filename parsing)
    /// If omitted, auto-derived from media_type
    pub content_type: Option<ContentType>,
    /// How deep to scan for ID-tagged folders (usually 1)
    #[serde(default = "default_depth")]
    pub depth: usize,
}

/// Configuration for a Real-Debrid source mount
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    /// Human-readable name (e.g., "RealDebrid")
    pub name: String,
    /// Absolute path to the arrow mount root
    pub path: PathBuf,
    /// Media type filter: auto, anime, tv, or movie
    #[serde(default = "default_source_media_type")]
    pub media_type: String,
}

/// API client configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// TMDB API key (required for alias matching)
    #[serde(default)]
    pub tmdb_api_key: String,
    /// Optional TMDB v4 read access token (preferred over api_key when set)
    #[serde(default)]
    pub tmdb_read_access_token: String,
    /// TVDB API key (optional)
    #[serde(default)]
    pub tvdb_api_key: String,
    /// How long to cache API responses (hours)
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_hours: u64,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            tmdb_api_key: String::new(),
            tmdb_read_access_token: String::new(),
            tvdb_api_key: String::new(),
            cache_ttl_hours: default_cache_ttl(),
        }
    }
}

fn is_valid_source_media_type(value: &str) -> bool {
    matches!(value.trim(), "auto" | "anime" | "tv" | "movie")
}

/// Daemon/scheduler configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Whether to run in daemon mode
    #[serde(default)]
    pub enabled: bool,
    /// Interval between scans in minutes
    #[serde(default = "default_interval")]
    pub interval_minutes: u64,
    /// Allow the daemon to search and acquire missing items
    #[serde(default)]
    pub search_missing: bool,
    /// Run SQLite VACUUM as a scheduled once-per-day maintenance task
    #[serde(default)]
    pub vacuum_enabled: bool,
    /// Local hour (0-23) when daemon-triggered VACUUM may run
    #[serde(default = "default_vacuum_hour_local")]
    pub vacuum_hour_local: u8,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: default_interval(),
            search_missing: false,
            vacuum_enabled: false,
            vacuum_hour_local: default_vacuum_hour_local(),
        }
    }
}

/// Symlink creation settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkConfig {
    /// Dry-run mode: log actions without creating symlinks
    #[serde(default)]
    pub dry_run: bool,
    /// Naming template for symlinked episodes
    #[serde(default = "default_naming_template")]
    pub naming_template: String,
    /// Probe Decypharr WebDAV readability before creating/updating live symlinks
    #[serde(default = "default_true")]
    pub verify_source_readability: bool,
    /// Timeout for the pre-link WebDAV readability probe
    #[serde(default = "default_source_probe_timeout_ms")]
    pub source_probe_timeout_ms: u64,
}

impl Default for SymlinkConfig {
    fn default() -> Self {
        Self {
            dry_run: false,
            naming_template: default_naming_template(),
            verify_source_readability: default_true(),
            source_probe_timeout_ms: default_source_probe_timeout_ms(),
        }
    }
}

/// Real-Debrid API configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealDebridConfig {
    /// RD API token (from https://real-debrid.com/apitoken)
    #[serde(default)]
    pub api_token: String,
    /// Max torrents to request per page from RD API pagination
    #[serde(default = "default_realdebrid_torrents_page_limit")]
    pub torrents_page_limit: u32,
    /// Delay between RD pagination requests in milliseconds
    #[serde(default = "default_realdebrid_pagination_delay_ms")]
    pub pagination_delay_ms: u64,
    /// Safety cap for maximum RD pagination pages per sync
    #[serde(default = "default_realdebrid_max_pages")]
    pub max_pages: u32,
}

impl Default for RealDebridConfig {
    fn default() -> Self {
        Self {
            api_token: String::new(),
            torrents_page_limit: default_realdebrid_torrents_page_limit(),
            pagination_delay_ms: default_realdebrid_pagination_delay_ms(),
            max_pages: default_realdebrid_max_pages(),
        }
    }
}

/// Decypharr integration configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecypharrConfig {
    /// Decypharr web UI URL (e.g., "http://localhost:8282")
    #[serde(default = "default_decypharr_url")]
    pub url: String,
    /// Decypharr API token (if auth is enabled)
    #[serde(default)]
    pub api_token: Option<String>,
    /// Seconds between queue/completion polls
    #[serde(default = "default_decypharr_poll_interval_seconds")]
    pub poll_interval_seconds: u64,
    /// Max minutes to wait for a download to complete
    #[serde(default = "default_decypharr_completion_timeout_minutes")]
    pub completion_timeout_minutes: u64,
    /// Max minutes to wait for Symlinkarr to relink completed content
    #[serde(default = "default_decypharr_relink_timeout_minutes")]
    pub relink_timeout_minutes: u64,
    /// Max incomplete torrents Symlinkarr allows in Decypharr before pausing new grabs
    #[serde(default = "default_decypharr_max_in_flight")]
    pub max_in_flight: usize,
    /// Max new acquisition requests Symlinkarr enqueues in a single run
    #[serde(default = "default_decypharr_max_requests_per_run")]
    pub max_requests_per_run: usize,
    /// Page size used when polling Decypharr queue endpoints
    #[serde(default = "default_decypharr_queue_page_size")]
    pub queue_page_size: usize,
    /// Arr instance name for movies (sent to Decypharr)
    #[serde(default = "default_arr_name_movie")]
    pub arr_name_movie: String,
    /// Arr instance name for TV shows (sent to Decypharr)
    #[serde(default = "default_arr_name_tv")]
    pub arr_name_tv: String,
    /// Arr instance name for anime (sent to Decypharr)
    #[serde(default = "default_arr_name_anime")]
    pub arr_name_anime: String,
}

impl Default for DecypharrConfig {
    fn default() -> Self {
        Self {
            url: default_decypharr_url(),
            api_token: None,
            poll_interval_seconds: default_decypharr_poll_interval_seconds(),
            completion_timeout_minutes: default_decypharr_completion_timeout_minutes(),
            relink_timeout_minutes: default_decypharr_relink_timeout_minutes(),
            max_in_flight: default_decypharr_max_in_flight(),
            max_requests_per_run: default_decypharr_max_requests_per_run(),
            queue_page_size: default_decypharr_queue_page_size(),
            arr_name_movie: default_arr_name_movie(),
            arr_name_tv: default_arr_name_tv(),
            arr_name_anime: default_arr_name_anime(),
        }
    }
}

/// Debrid Media Manager integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmmConfig {
    /// Public or self-hosted DMM URL (e.g. "https://debridmediamanager.com")
    #[serde(default)]
    pub url: String,
    /// Optional override for DMM's current auth salt if upstream changes its auth scheme
    #[serde(default)]
    pub auth_salt: Option<String>,
    /// Restrict torrent results to DMM's trusted cache set
    #[serde(default = "default_dmm_only_trusted")]
    pub only_trusted: bool,
    /// Max media candidates to inspect per DMM title lookup
    #[serde(default = "default_dmm_max_search_results")]
    pub max_search_results: usize,
    /// Max cached torrent candidates to inspect per media lookup
    #[serde(default = "default_dmm_max_torrent_results")]
    pub max_torrent_results: usize,
}

impl Default for DmmConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            auth_salt: None,
            only_trusted: default_dmm_only_trusted(),
            max_search_results: default_dmm_max_search_results(),
            max_torrent_results: default_dmm_max_torrent_results(),
        }
    }
}

/// Prowlarr indexer integration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProwlarrConfig {
    /// Prowlarr URL (e.g., "http://localhost:9696")
    #[serde(default)]
    pub url: String,
    /// Prowlarr API key
    #[serde(default)]
    pub api_key: String,
}

/// Bazarr subtitle integration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BazarrConfig {
    /// Bazarr URL (e.g., "http://localhost:6767")
    #[serde(default)]
    pub url: String,
    /// Bazarr API key
    #[serde(default)]
    pub api_key: String,
}

/// Tautulli Plex monitoring integration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TautulliConfig {
    /// Tautulli URL (e.g., "http://localhost:8383")
    #[serde(default)]
    pub url: String,
    /// Tautulli API key
    #[serde(default)]
    pub api_key: String,
}

/// Plex integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlexConfig {
    /// Plex URL (e.g. "http://localhost:32400")
    #[serde(default)]
    pub url: String,
    /// Plex token for library refresh requests
    #[serde(default)]
    pub token: String,
    /// Enable Symlinkarr-triggered Plex refreshes after linking
    #[serde(default = "default_plex_refresh_enabled")]
    pub refresh_enabled: bool,
    /// Delay between queued Plex refresh requests to avoid overloading Plex
    #[serde(default = "default_plex_refresh_delay_ms")]
    pub refresh_delay_ms: u64,
    /// Coalesce large per-library refresh groups to the library root after this many paths
    #[serde(default = "default_plex_refresh_coalesce_threshold")]
    pub refresh_coalesce_threshold: usize,
    /// Maximum Plex refresh batches queued per scan (0 = unlimited)
    #[serde(default = "default_plex_max_refresh_batches_per_run")]
    pub max_refresh_batches_per_run: usize,
    /// Abort the entire Plex refresh phase when the planned batch count exceeds the limit
    #[serde(default = "default_plex_abort_refresh_when_capped")]
    pub abort_refresh_when_capped: bool,
}

fn default_plex_refresh_enabled() -> bool {
    true
}

fn default_plex_refresh_delay_ms() -> u64 {
    250
}

fn default_plex_refresh_coalesce_threshold() -> usize {
    8
}

fn default_plex_max_refresh_batches_per_run() -> usize {
    12
}

fn default_plex_abort_refresh_when_capped() -> bool {
    true
}

impl Default for PlexConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            token: String::new(),
            refresh_enabled: default_plex_refresh_enabled(),
            refresh_delay_ms: default_plex_refresh_delay_ms(),
            refresh_coalesce_threshold: default_plex_refresh_coalesce_threshold(),
            max_refresh_batches_per_run: default_plex_max_refresh_batches_per_run(),
            abort_refresh_when_capped: default_plex_abort_refresh_when_capped(),
        }
    }
}

/// Emby/Jellyfin-style integration for targeted library invalidation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaBrowserConfig {
    /// Server URL (e.g. "http://localhost:8096")
    #[serde(default)]
    pub url: String,
    /// API key for media update and library refresh requests
    #[serde(default)]
    pub api_key: String,
    /// Enable Symlinkarr-triggered invalidation after linking or cleanup
    #[serde(default = "default_media_browser_refresh_enabled")]
    pub refresh_enabled: bool,
    /// Delay between queued invalidation requests
    #[serde(default = "default_media_browser_refresh_delay_ms")]
    pub refresh_delay_ms: u64,
    /// Maximum paths to include in one `/Library/Media/Updated` request
    #[serde(default = "default_media_browser_refresh_batch_size")]
    pub refresh_batch_size: usize,
    /// Maximum invalidation batches queued per run (0 = unlimited)
    #[serde(default = "default_media_browser_max_refresh_batches_per_run")]
    pub max_refresh_batches_per_run: usize,
    /// Abort the entire invalidation phase when the planned batch count exceeds the limit
    #[serde(default = "default_media_browser_abort_refresh_when_capped")]
    pub abort_refresh_when_capped: bool,
    /// Fall back to library-root invalidation when targeted paths would exceed the cap
    #[serde(default = "default_media_browser_fallback_to_library_roots_when_capped")]
    pub fallback_to_library_roots_when_capped: bool,
}

fn default_media_browser_refresh_enabled() -> bool {
    true
}

fn default_media_browser_refresh_delay_ms() -> u64 {
    250
}

fn default_media_browser_refresh_batch_size() -> usize {
    64
}

fn default_media_browser_max_refresh_batches_per_run() -> usize {
    12
}

fn default_media_browser_abort_refresh_when_capped() -> bool {
    true
}

fn default_media_browser_fallback_to_library_roots_when_capped() -> bool {
    true
}

impl Default for MediaBrowserConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            api_key: String::new(),
            refresh_enabled: default_media_browser_refresh_enabled(),
            refresh_delay_ms: default_media_browser_refresh_delay_ms(),
            refresh_batch_size: default_media_browser_refresh_batch_size(),
            max_refresh_batches_per_run: default_media_browser_max_refresh_batches_per_run(),
            abort_refresh_when_capped: default_media_browser_abort_refresh_when_capped(),
            fallback_to_library_roots_when_capped:
                default_media_browser_fallback_to_library_roots_when_capped(),
        }
    }
}

/// Radarr integration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RadarrConfig {
    /// Radarr URL (e.g., "http://localhost:7878")
    #[serde(default)]
    pub url: String,
    /// Radarr API key
    #[serde(default)]
    pub api_key: String,
}

/// Sonarr integration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SonarrConfig {
    /// Sonarr URL (e.g., "http://localhost:8989")
    #[serde(default)]
    pub url: String,
    /// Sonarr API key
    #[serde(default)]
    pub api_key: String,
}

/// Symlink backup settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    /// Enable automatic backups
    #[serde(default = "default_backup_enabled")]
    pub enabled: bool,
    /// Directory to store backup files
    #[serde(default = "default_backup_path")]
    pub path: PathBuf,
    /// Hours between scheduled full backups (0 = disabled)
    #[serde(default = "default_backup_interval")]
    pub interval_hours: u64,
    /// Maximum number of scheduled backups to keep
    #[serde(default = "default_max_backups")]
    pub max_backups: usize,
    /// Maximum number of safety snapshots to keep (0 = keep all, never rotate)
    #[serde(default = "default_max_safety_backups")]
    pub max_safety_backups: usize,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: default_backup_enabled(),
            path: default_backup_path(),
            interval_hours: default_backup_interval(),
            max_backups: default_max_backups(),
            max_safety_backups: default_max_safety_backups(),
        }
    }
}

impl BackupConfig {
    /// Create a BackupConfig suitable for standalone restore (no config.yaml needed).
    /// Uses the given directory as the backup directory with safe defaults.
    pub fn standalone(backup_dir: PathBuf) -> Self {
        Self {
            enabled: true,
            path: backup_dir,
            interval_hours: default_backup_interval(),
            max_backups: default_max_backups(),
            max_safety_backups: default_max_safety_backups(),
        }
    }
}

fn default_backup_enabled() -> bool {
    true
}

fn default_backup_path() -> PathBuf {
    PathBuf::from("backups")
}

fn default_backup_interval() -> u64 {
    24
}

fn default_max_backups() -> usize {
    10
}

fn default_max_safety_backups() -> usize {
    25
}

/// Feature flags controlling rollout behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    /// Enable anti-churn reconciliation path in scan/link lifecycle
    #[serde(default = "default_true")]
    pub reconcile_links: bool,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            reconcile_links: default_true(),
        }
    }
}

/// Security policy controls for destructive operations and secret handling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Enforce path allowlists for destructive operations
    #[serde(default = "default_true")]
    pub enforce_roots: bool,
    /// Require credentials to be loaded via env:/secretfile: indirection
    #[serde(default)]
    pub require_secret_provider: bool,
    /// Enforce secure file permissions for artifacts written by Symlinkarr
    #[serde(default = "default_true")]
    pub enforce_secure_permissions: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enforce_roots: default_true(),
            require_secret_provider: false,
            enforce_secure_permissions: default_true(),
        }
    }
}

/// Cleanup policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CleanupPolicyConfig {
    #[serde(default)]
    pub prune: PrunePolicyConfig,
}

/// Prune-policy settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrunePolicyConfig {
    /// Enforce report freshness/scope/version/confirmation policy before apply
    #[serde(default = "default_true")]
    pub enforce_policy: bool,
    /// Maximum report age (hours) when applying prune
    #[serde(default = "default_prune_max_report_age_hours")]
    pub max_report_age_hours: u64,
    /// Default max delete cap for apply when CLI does not specify --max-delete
    #[serde(default = "default_prune_default_max_delete")]
    pub default_max_delete: usize,
    /// Quarantine symlinks that are not actively tracked by Symlinkarr instead of deleting them.
    #[serde(default = "default_true")]
    pub quarantine_foreign: bool,
    /// Directory used to stash quarantined symlinks for later inspection/recovery.
    #[serde(default = "default_prune_quarantine_path")]
    pub quarantine_path: PathBuf,
}

impl Default for PrunePolicyConfig {
    fn default() -> Self {
        Self {
            enforce_policy: default_true(),
            max_report_age_hours: default_prune_max_report_age_hours(),
            default_max_delete: default_prune_default_max_delete(),
            quarantine_foreign: default_true(),
            quarantine_path: default_prune_quarantine_path(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_prune_max_report_age_hours() -> u64 {
    72
}

fn default_prune_default_max_delete() -> usize {
    5000
}

fn default_prune_quarantine_path() -> PathBuf {
    PathBuf::from("quarantine")
}

// --- Default value functions ---

fn default_db_path() -> String {
    "symlinkarr.db".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_media_type() -> MediaType {
    MediaType::Tv
}

fn default_source_media_type() -> String {
    "auto".to_string()
}

fn default_depth() -> usize {
    1
}

fn default_cache_ttl() -> u64 {
    87600 // ~10 years — metadata is intentionally sticky; freshness should come from targeted refresh signals
}

fn default_interval() -> u64 {
    30
}

fn default_vacuum_hour_local() -> u8 {
    4
}

fn default_realdebrid_torrents_page_limit() -> u32 {
    5000
}

fn default_realdebrid_pagination_delay_ms() -> u64 {
    200
}

fn default_realdebrid_max_pages() -> u32 {
    5000
}

fn default_naming_template() -> String {
    "{title} - S{season:02}E{episode:02} - {episode_title}".to_string()
}

fn default_source_probe_timeout_ms() -> u64 {
    2500
}

fn default_decypharr_url() -> String {
    "http://localhost:8282".to_string()
}

fn default_decypharr_poll_interval_seconds() -> u64 {
    30
}

fn default_decypharr_completion_timeout_minutes() -> u64 {
    180
}

fn default_decypharr_relink_timeout_minutes() -> u64 {
    15
}

fn default_decypharr_max_in_flight() -> usize {
    3
}

fn default_decypharr_max_requests_per_run() -> usize {
    10
}

fn default_decypharr_queue_page_size() -> usize {
    100
}

fn default_arr_name_movie() -> String {
    "radarr".to_string()
}

fn default_arr_name_tv() -> String {
    "sonarr".to_string()
}

fn default_arr_name_anime() -> String {
    "sonarr-anime".to_string()
}

fn default_dmm_only_trusted() -> bool {
    true
}

fn default_dmm_max_search_results() -> usize {
    3
}

fn default_dmm_max_torrent_results() -> usize {
    10
}

impl Config {
    /// Load configuration from a YAML file.
    /// If `path` is provided, it tries to load that specific file.
    /// Otherwise, checks default locations: config.yaml, /app/config/config.yaml.
    pub fn load(path: Option<String>) -> Result<Self> {
        let paths = candidate_config_paths(path);

        for path in &paths {
            if path.exists() {
                let dotenv_overlay = load_dotenv_chain(path)?;
                let config_str = std::fs::read_to_string(path)?;
                let mut value: serde_yml::Value = serde_yml::from_str(&config_str)?;
                apply_legacy_aliases(&mut value);
                warn_for_plaintext_secrets(&value);
                let secret_files = collect_secret_file_paths(&value, path.parent());
                let mut config: Config = serde_yml::from_value(value)?;
                config.resolve_secret_fields(path.parent(), &dotenv_overlay)?;
                config.loaded_from = Some(path.to_path_buf());
                config.secret_files = secret_files;
                tracing::info!("Configuration loaded from {:?}", path);
                return Ok(config);
            }
        }

        anyhow::bail!("No config.yaml found. Searched paths: {:?}", paths)
    }

    pub fn validate(&self) -> ValidationReport {
        let mut report = self.validate_runtime_settings();
        self.validate_paths(&mut report);
        report
    }

    pub fn validate_runtime_settings(&self) -> ValidationReport {
        let mut report = ValidationReport::default();

        if self.libraries.is_empty() {
            report.errors.push("No libraries configured".to_string());
        }
        if self.sources.is_empty() {
            report.errors.push("No sources configured".to_string());
        }

        if self.security.require_secret_provider {
            if self.realdebrid.api_token.is_empty() {
                report.warnings.push(
                    "security.require_secret_provider enabled but realdebrid.api_token is empty"
                        .to_string(),
                );
            }
            if cfg_has_url_without_key(&self.prowlarr.url, &self.prowlarr.api_key) {
                report
                    .errors
                    .push("Prowlarr configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.bazarr.url, &self.bazarr.api_key) {
                report
                    .errors
                    .push("Bazarr configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.tautulli.url, &self.tautulli.api_key) {
                report
                    .errors
                    .push("Tautulli configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.plex.url, &self.plex.token) {
                report
                    .errors
                    .push("Plex configured without token".to_string());
            }
            if cfg_has_url_without_key(&self.emby.url, &self.emby.api_key) {
                report
                    .errors
                    .push("Emby configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.jellyfin.url, &self.jellyfin.api_key) {
                report
                    .errors
                    .push("Jellyfin configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.radarr.url, &self.radarr.api_key) {
                report
                    .errors
                    .push("Radarr configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.sonarr.url, &self.sonarr.api_key) {
                report
                    .errors
                    .push("Sonarr configured without api_key".to_string());
            }
            if cfg_has_url_without_key(&self.sonarr_anime.url, &self.sonarr_anime.api_key) {
                report
                    .errors
                    .push("Sonarr-Anime configured without api_key".to_string());
            }
        }

        if self.backup.max_safety_backups == 0 {
            report.warnings.push(
                "backup.max_safety_backups=0 keeps unlimited safety snapshots; use a bounded value"
                    .to_string(),
            );
        }

        if self.api.cache_ttl_hours == 0 {
            report
                .errors
                .push("api.cache_ttl_hours must be greater than 0".to_string());
        }

        if self.daemon.interval_minutes == 0 {
            report
                .errors
                .push("daemon.interval_minutes must be greater than 0".to_string());
        }

        if self.daemon.vacuum_hour_local > 23 {
            report
                .errors
                .push("daemon.vacuum_hour_local must be between 0 and 23".to_string());
        }

        for source in &self.sources {
            let media_type = source.media_type.trim();
            if !is_valid_source_media_type(media_type) {
                report.errors.push(format!(
                    "Source '{}' media_type must be one of: auto, anime, tv, movie (got '{}')",
                    source.name, source.media_type
                ));
            }
        }

        if self.web.enabled {
            let bind_address = self.web.bind_address.trim();
            if bind_address.is_empty() {
                report
                    .errors
                    .push("web.bind_address must not be empty when web.enabled=true".to_string());
            } else if self.web.requires_remote_ack() {
                if self.web.allow_remote {
                    report.warnings.push(format!(
                        "web.bind_address={} exposes the web UI to the network because web.allow_remote=true",
                        bind_address
                    ));
                } else {
                    report.errors.push(format!(
                        "web.bind_address={} requires web.allow_remote=true before Symlinkarr will expose the web UI beyond loopback",
                        bind_address
                    ));
                }
            }

            if self.web.has_partial_basic_auth() {
                report.errors.push(
                    "web.username and web.password must either both be set or both be empty"
                        .to_string(),
                );
            }

            if self.web.requires_remote_ack() && self.web.allow_remote {
                if !self.web.has_basic_auth() {
                    report.errors.push(
                        "web.allow_remote=true requires web.username/web.password so the built-in HTML UI is not exposed unauthenticated"
                            .to_string(),
                    );
                } else if self.web.has_api_key_auth() && !self.web.has_basic_auth() {
                    report.warnings.push(
                        "web.api_key secures the JSON API, but the HTML UI remains unauthenticated without web.username/web.password"
                            .to_string(),
                    );
                }
            }
        }

        if self.has_decypharr() {
            if self.decypharr.poll_interval_seconds == 0 {
                report
                    .errors
                    .push("decypharr.poll_interval_seconds must be greater than 0".to_string());
            }
            if self.decypharr.completion_timeout_minutes == 0 {
                report.errors.push(
                    "decypharr.completion_timeout_minutes must be greater than 0".to_string(),
                );
            }
            if self.decypharr.relink_timeout_minutes == 0 {
                report
                    .errors
                    .push("decypharr.relink_timeout_minutes must be greater than 0".to_string());
            }
            if self.decypharr.max_in_flight == 0 {
                report
                    .errors
                    .push("decypharr.max_in_flight must be greater than 0".to_string());
            }
            if self.decypharr.max_requests_per_run == 0 {
                report
                    .errors
                    .push("decypharr.max_requests_per_run must be greater than 0".to_string());
            }
            if self.decypharr.queue_page_size == 0 {
                report
                    .errors
                    .push("decypharr.queue_page_size must be greater than 0".to_string());
            }
        }

        if self.has_dmm() {
            if self.dmm.max_search_results == 0 {
                report
                    .errors
                    .push("dmm.max_search_results must be greater than 0".to_string());
            }
            if self.dmm.max_torrent_results == 0 {
                report
                    .errors
                    .push("dmm.max_torrent_results must be greater than 0".to_string());
            }
        }

        if !self.realdebrid.api_token.is_empty() {
            if self.realdebrid.torrents_page_limit == 0 {
                report
                    .errors
                    .push("realdebrid.torrents_page_limit must be greater than 0".to_string());
            }
            if self.realdebrid.max_pages == 0 {
                report
                    .errors
                    .push("realdebrid.max_pages must be greater than 0".to_string());
            }
        }

        if self.security.enforce_secure_permissions {
            validate_secure_permissions(self, &mut report);
        }

        validate_naming_template(&self.symlink.naming_template, &mut report);
        if self.symlink.verify_source_readability && self.symlink.source_probe_timeout_ms == 0 {
            report.errors.push(
                "symlink.source_probe_timeout_ms must be greater than 0 when symlink.verify_source_readability is enabled"
                    .to_string(),
            );
        }

        report
    }

    fn validate_paths(&self, report: &mut ValidationReport) {
        for lib in &self.libraries {
            if !lib.path.is_absolute() {
                report.errors.push(format!(
                    "Library '{}' path must be absolute: {}",
                    lib.name,
                    lib.path.display()
                ));
                continue;
            }

            match fast_path_health(&lib.path) {
                PathHealth::Healthy => {}
                PathHealth::Missing => report.errors.push(format!(
                    "Library '{}' path does not exist: {}",
                    lib.name,
                    lib.path.display()
                )),
                PathHealth::TransportDisconnected => report.errors.push(format!(
                    "Library '{}' path is mounted but unhealthy: {} (transport endpoint is not connected)",
                    lib.name,
                    lib.path.display()
                )),
                PathHealth::Timeout => report.errors.push(format!(
                    "Library '{}' path probe timed out: {}",
                    lib.name,
                    lib.path.display()
                )),
                PathHealth::IoError(err) => report.errors.push(format!(
                    "Library '{}' path is not readable: {} ({})",
                    lib.name,
                    lib.path.display(),
                    err
                )),
            }
        }

        for source in &self.sources {
            if !source.path.is_absolute() {
                report.errors.push(format!(
                    "Source '{}' path must be absolute: {}",
                    source.name,
                    source.path.display()
                ));
                continue;
            }

            match fast_path_health(&source.path) {
                PathHealth::Healthy => {}
                PathHealth::Missing => report.errors.push(format!(
                    "Source '{}' path does not exist: {}",
                    source.name,
                    source.path.display()
                )),
                PathHealth::TransportDisconnected => report.errors.push(format!(
                    "Source '{}' path is mounted but unhealthy: {} (transport endpoint is not connected; restart/remount the source)",
                    source.name,
                    source.path.display()
                )),
                PathHealth::Timeout => report.errors.push(format!(
                    "Source '{}' path probe timed out: {}",
                    source.name,
                    source.path.display()
                )),
                PathHealth::IoError(err) => report.errors.push(format!(
                    "Source '{}' path is not readable: {} ({})",
                    source.name,
                    source.path.display(),
                    err
                )),
            }
        }
    }

    /// Check if TMDB API is configured
    pub fn has_tmdb(&self) -> bool {
        !self.api.tmdb_api_key.is_empty() || !self.api.tmdb_read_access_token.is_empty()
    }

    /// Check if TVDB API is configured
    pub fn has_tvdb(&self) -> bool {
        !self.api.tvdb_api_key.is_empty()
    }

    /// Check if Real-Debrid API is configured
    pub fn has_realdebrid(&self) -> bool {
        !self.realdebrid.api_token.is_empty()
    }

    /// Check if Decypharr is configured
    pub fn has_decypharr(&self) -> bool {
        !self.decypharr.url.is_empty()
    }

    /// Check if Debrid Media Manager is configured
    pub fn has_dmm(&self) -> bool {
        !self.dmm.url.is_empty()
    }

    /// Check if Prowlarr is configured
    pub fn has_prowlarr(&self) -> bool {
        !self.prowlarr.url.is_empty() && !self.prowlarr.api_key.is_empty()
    }

    /// Check if Bazarr is configured
    pub fn has_bazarr(&self) -> bool {
        !self.bazarr.url.is_empty() && !self.bazarr.api_key.is_empty()
    }

    /// Check if Tautulli is configured
    pub fn has_tautulli(&self) -> bool {
        !self.tautulli.url.is_empty() && !self.tautulli.api_key.is_empty()
    }

    /// Check if Plex is configured
    pub fn has_plex(&self) -> bool {
        !self.plex.url.is_empty() && !self.plex.token.is_empty()
    }

    /// Check if targeted Plex refresh is configured and enabled
    pub fn has_plex_refresh(&self) -> bool {
        self.has_plex() && self.plex.refresh_enabled
    }

    /// Check if Emby is configured
    pub fn has_emby(&self) -> bool {
        !self.emby.url.is_empty() && !self.emby.api_key.is_empty()
    }

    /// Check if Emby invalidation is configured and enabled
    pub fn has_emby_refresh(&self) -> bool {
        self.has_emby() && self.emby.refresh_enabled
    }

    /// Check if Jellyfin is configured
    pub fn has_jellyfin(&self) -> bool {
        !self.jellyfin.url.is_empty() && !self.jellyfin.api_key.is_empty()
    }

    /// Check if Jellyfin invalidation is configured and enabled
    pub fn has_jellyfin_refresh(&self) -> bool {
        self.has_jellyfin() && self.jellyfin.refresh_enabled
    }

    /// Check if Radarr is configured
    pub fn has_radarr(&self) -> bool {
        !self.radarr.url.is_empty() && !self.radarr.api_key.is_empty()
    }

    /// Check if Sonarr is configured
    pub fn has_sonarr(&self) -> bool {
        !self.sonarr.url.is_empty() && !self.sonarr.api_key.is_empty()
    }

    /// Check if Sonarr Anime is configured
    pub fn has_sonarr_anime(&self) -> bool {
        !self.sonarr_anime.url.is_empty() && !self.sonarr_anime.api_key.is_empty()
    }

    /// Check if the web UI is enabled
    #[allow(dead_code)]
    pub fn has_web(&self) -> bool {
        self.web.enabled
    }

    fn resolve_secret_fields(
        &mut self,
        config_dir: Option<&Path>,
        dotenv_overlay: &DotenvOverlay,
    ) -> Result<()> {
        self.api.tmdb_api_key = resolve_secret(
            &self.api.tmdb_api_key,
            "api.tmdb_api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.api.tmdb_read_access_token = resolve_secret(
            &self.api.tmdb_read_access_token,
            "api.tmdb_read_access_token",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.api.tvdb_api_key = resolve_secret(
            &self.api.tvdb_api_key,
            "api.tvdb_api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.realdebrid.api_token = resolve_secret(
            &self.realdebrid.api_token,
            "realdebrid.api_token",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        if let Some(token) = &self.decypharr.api_token {
            let resolved = resolve_secret(
                token,
                "decypharr.api_token",
                self.security.require_secret_provider,
                config_dir,
                Some(dotenv_overlay),
            )?;
            self.decypharr.api_token = Some(resolved);
        }
        if let Some(auth_salt) = &self.dmm.auth_salt {
            let resolved = resolve_secret(
                auth_salt,
                "dmm.auth_salt",
                false,
                config_dir,
                Some(dotenv_overlay),
            )?;
            self.dmm.auth_salt = Some(resolved);
        }
        self.prowlarr.api_key = resolve_secret(
            &self.prowlarr.api_key,
            "prowlarr.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.bazarr.api_key = resolve_secret(
            &self.bazarr.api_key,
            "bazarr.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.tautulli.api_key = resolve_secret(
            &self.tautulli.api_key,
            "tautulli.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.plex.token = resolve_secret(
            &self.plex.token,
            "plex.token",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.emby.api_key = resolve_secret(
            &self.emby.api_key,
            "emby.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.jellyfin.api_key = resolve_secret(
            &self.jellyfin.api_key,
            "jellyfin.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.web.password = resolve_secret(
            &self.web.password,
            "web.password",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.web.api_key = resolve_secret(
            &self.web.api_key,
            "web.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.radarr.api_key = resolve_secret(
            &self.radarr.api_key,
            "radarr.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.sonarr.api_key = resolve_secret(
            &self.sonarr.api_key,
            "sonarr.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;
        self.sonarr_anime.api_key = resolve_secret(
            &self.sonarr_anime.api_key,
            "sonarr_anime.api_key",
            self.security.require_secret_provider,
            config_dir,
            Some(dotenv_overlay),
        )?;

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct RestoreConfigProbe {
    #[serde(default = "default_db_path")]
    db_path: String,
}

fn resolve_restore_target_path(path: &Path, base_dir: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path)
    } else {
        path.to_path_buf()
    }
}

pub fn inspect_restore_targets(config_path: &Path) -> Result<RestoreConfigTargets> {
    let config_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file {}", config_path.display()))?;
    let mut value: serde_yml::Value = serde_yml::from_str(&config_str)
        .with_context(|| format!("Failed to parse config file {}", config_path.display()))?;
    apply_legacy_aliases(&mut value);
    let secret_files = collect_secret_file_paths(&value, config_path.parent());
    let probe: RestoreConfigProbe = serde_yml::from_value(value).with_context(|| {
        format!(
            "Failed to inspect restore targets from config {}",
            config_path.display()
        )
    })?;

    Ok(RestoreConfigTargets {
        db_path: resolve_restore_target_path(Path::new(&probe.db_path), config_path.parent()),
        secret_files,
    })
}

fn validate_naming_template(template: &str, report: &mut ValidationReport) {
    let known: &[&str] = &[
        "{title}",
        "{season}",
        "{season:02}",
        "{episode}",
        "{episode:02}",
        "{episode_title}",
    ];

    // Extract all {…} placeholders using a simple scanner.
    let chars = template.char_indices();
    for (i, c) in chars {
        if c == '{' {
            let start = i;
            let mut end = None;
            for (j, ch) in template[start + 1..].char_indices() {
                if ch == '}' {
                    end = Some(start + 1 + j);
                    break;
                }
                if ch == '{' {
                    // Nested brace — not a placeholder, skip.
                    break;
                }
            }
            if let Some(end_idx) = end {
                let placeholder = &template[start..=end_idx];
                if !known.contains(&placeholder) {
                    report.errors.push(format!(
                        "Unknown naming_template placeholder: {}",
                        placeholder
                    ));
                }
            }
        }
    }

    let has_episode = template.contains("{episode}") || template.contains("{episode:02}");
    if !has_episode {
        report.errors.push(
            "naming_template must contain at least one episode placeholder: \
             {episode} or {episode:02}"
                .to_string(),
        );
    }
}

pub fn candidate_config_paths(path: Option<String>) -> Vec<PathBuf> {
    if let Some(p) = path {
        return vec![PathBuf::from(p)];
    }

    let mut paths = Vec::new();
    if let Ok(env_path) = std::env::var("SYMLINKARR_CONFIG") {
        let env_path = env_path.trim();
        if !env_path.is_empty() {
            paths.push(PathBuf::from(env_path));
        }
    }
    paths.push(PathBuf::from("config.yaml"));
    paths.push(PathBuf::from("/app/config/config.yaml"));
    paths
}

fn load_dotenv_chain(config_path: &Path) -> Result<DotenvOverlay> {
    let mut overlay = DotenvOverlay::new();
    for path in candidate_dotenv_paths(config_path) {
        if path.exists() {
            let loaded = load_dotenv_file(&path, &mut overlay)?;
            if loaded > 0 {
                tracing::info!("Loaded {} env var(s) from {:?}", loaded, path);
            }
        }
    }
    Ok(overlay)
}

fn candidate_dotenv_paths(config_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    let mut push_unique = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    if let Some(config_dir) = config_path.parent() {
        push_unique(config_dir.join(".env"));
        push_unique(config_dir.join(".env.local"));
    }
    push_unique(PathBuf::from(".env"));
    push_unique(PathBuf::from(".env.local"));
    paths
}

fn load_dotenv_file(path: &Path, overlay: &mut DotenvOverlay) -> Result<usize> {
    let content = std::fs::read_to_string(path)?;
    let mut loaded = 0usize;

    for (line_no, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            anyhow::bail!(
                "Invalid .env entry in {} at line {}",
                path.display(),
                line_no + 1
            );
        };

        let key = key.trim();
        if key.is_empty() {
            anyhow::bail!(
                "Invalid .env key in {} at line {}",
                path.display(),
                line_no + 1
            );
        }
        if std::env::var_os(key).is_some() || overlay.contains_key(key) {
            continue;
        }

        let value = parse_dotenv_value(value.trim());
        overlay.insert(key.to_string(), value);
        loaded += 1;
    }

    Ok(loaded)
}

fn parse_dotenv_value(raw: &str) -> String {
    if raw.len() >= 2 {
        let bytes = raw.as_bytes();
        let first = bytes[0];
        let last = bytes[raw.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return raw[1..raw.len() - 1].to_string();
        }
    }

    raw.to_string()
}

fn warn_for_plaintext_secrets(root: &serde_yml::Value) {
    let plaintext_fields = raw_plaintext_secret_fields(root);
    if plaintext_fields.is_empty() {
        return;
    }

    let require_provider =
        yaml_bool_at(root, &["security", "require_secret_provider"]).unwrap_or(false);
    if require_provider {
        return;
    }

    tracing::warn!(
        "Plaintext secrets found in config for: {}. Prefer env:VAR or secretfile:/path",
        plaintext_fields.join(", ")
    );
}

fn raw_plaintext_secret_fields(root: &serde_yml::Value) -> Vec<&'static str> {
    let mut fields = Vec::new();
    for (path, field_name) in secret_field_paths() {
        if let Some(value) = yaml_str_at(root, path) {
            if !value.is_empty() && !uses_secret_provider(value) {
                fields.push(field_name);
            }
        }
    }
    fields
}

fn yaml_value_at<'a>(root: &'a serde_yml::Value, path: &[&str]) -> Option<&'a serde_yml::Value> {
    let mut current = root;
    for segment in path {
        let mapping = current.as_mapping()?;
        current = mapping.get(serde_yml::Value::from(*segment))?;
    }
    Some(current)
}

fn yaml_str_at<'a>(root: &'a serde_yml::Value, path: &[&str]) -> Option<&'a str> {
    yaml_value_at(root, path)?.as_str()
}

fn yaml_bool_at(root: &serde_yml::Value, path: &[&str]) -> Option<bool> {
    yaml_value_at(root, path)?.as_bool()
}

fn collect_secret_file_paths(root: &serde_yml::Value, config_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for (path, _) in secret_field_paths() {
        let Some(value) = yaml_str_at(root, path) else {
            continue;
        };
        let Some(secret_file) = value.strip_prefix("secretfile:") else {
            continue;
        };

        let secret_path = PathBuf::from(secret_file);
        let resolved = if secret_path.is_relative() {
            if let Some(config_dir) = config_dir {
                config_dir.join(secret_path)
            } else {
                secret_path
            }
        } else {
            secret_path
        };

        paths.push(resolved);
    }

    paths.sort();
    paths.dedup();
    paths
}

fn secret_field_paths() -> [(&'static [&'static str], &'static str); 16] {
    [
        (&["api", "tmdb_api_key"], "api.tmdb_api_key"),
        (
            &["api", "tmdb_read_access_token"],
            "api.tmdb_read_access_token",
        ),
        (&["api", "tvdb_api_key"], "api.tvdb_api_key"),
        (&["realdebrid", "api_token"], "realdebrid.api_token"),
        (&["decypharr", "api_token"], "decypharr.api_token"),
        (&["prowlarr", "api_key"], "prowlarr.api_key"),
        (&["bazarr", "api_key"], "bazarr.api_key"),
        (&["tautulli", "api_key"], "tautulli.api_key"),
        (&["plex", "token"], "plex.token"),
        (&["emby", "api_key"], "emby.api_key"),
        (&["jellyfin", "api_key"], "jellyfin.api_key"),
        (&["web", "password"], "web.password"),
        (&["web", "api_key"], "web.api_key"),
        (&["radarr", "api_key"], "radarr.api_key"),
        (&["sonarr", "api_key"], "sonarr.api_key"),
        (&["sonarr_anime", "api_key"], "sonarr_anime.api_key"),
    ]
}

fn apply_legacy_aliases(root: &mut serde_yml::Value) {
    let Some(mapping) = root.as_mapping_mut() else {
        return;
    };

    let backup_key = serde_yml::Value::from("backup");
    let Some(backup_value) = mapping.get_mut(&backup_key) else {
        return;
    };
    let Some(backup_map) = backup_value.as_mapping_mut() else {
        return;
    };

    let path_key = serde_yml::Value::from("path");
    let dir_key = serde_yml::Value::from("dir");
    if !backup_map.contains_key(&path_key) {
        if let Some(dir_value) = backup_map.get(&dir_key).cloned() {
            backup_map.insert(path_key, dir_value);
            tracing::warn!(
                "Deprecated config key 'backup.dir' detected; please migrate to 'backup.path'"
            );
        }
    }
}

fn resolve_secret(
    raw: &str,
    field: &str,
    require_provider: bool,
    config_dir: Option<&Path>,
    dotenv_overlay: Option<&DotenvOverlay>,
) -> Result<String> {
    if raw.is_empty() {
        return Ok(String::new());
    }

    if let Some(var) = raw.strip_prefix("env:") {
        let value = std::env::var(var)
            .ok()
            .or_else(|| dotenv_overlay.and_then(|overlay| overlay.get(var).cloned()))
            .ok_or_else(|| {
                anyhow::anyhow!("Missing environment variable '{}' for {}", var, field)
            })?;
        return Ok(value.trim().to_string());
    }

    if let Some(file) = raw.strip_prefix("secretfile:") {
        let file_path = PathBuf::from(file);
        let resolved_path = if file_path.is_relative() {
            if let Some(config_dir) = config_dir {
                config_dir.join(file_path)
            } else {
                file_path
            }
        } else {
            file_path
        };
        let value = std::fs::read_to_string(&resolved_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed reading secret file '{}' for {}: {}",
                resolved_path.display(),
                field,
                e
            )
        })?;
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!(
                "Secret file '{}' is empty or contains only whitespace",
                resolved_path.display()
            );
        }
        return Ok(trimmed);
    }

    if require_provider {
        anyhow::bail!(
            "Plaintext secret is not allowed for {}. Use env:VAR or secretfile:/path/to/file",
            field
        );
    }

    Ok(raw.to_string())
}

fn uses_secret_provider(raw: &str) -> bool {
    raw.starts_with("env:") || raw.starts_with("secretfile:")
}

fn cfg_has_url_without_key(url: &str, api_key: &str) -> bool {
    !url.trim().is_empty() && api_key.trim().is_empty()
}

mod validation;

#[cfg(test)]
mod tests;
