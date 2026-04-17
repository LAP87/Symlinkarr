use super::*;

pub(super) fn default_metadata_concurrency() -> usize {
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

pub(super) fn default_web_bind_address() -> String {
    "127.0.0.1".to_string()
}

pub(super) fn default_web_port() -> u16 {
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

pub(super) fn default_plex_refresh_enabled() -> bool {
    true
}

pub(super) fn default_plex_refresh_delay_ms() -> u64 {
    250
}

pub(super) fn default_plex_refresh_coalesce_threshold() -> usize {
    8
}

pub(super) fn default_plex_max_refresh_batches_per_run() -> usize {
    12
}

pub(super) fn default_plex_abort_refresh_when_capped() -> bool {
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

pub(super) fn default_media_browser_refresh_enabled() -> bool {
    true
}

pub(super) fn default_media_browser_refresh_delay_ms() -> u64 {
    250
}

pub(super) fn default_media_browser_refresh_batch_size() -> usize {
    64
}

pub(super) fn default_media_browser_max_refresh_batches_per_run() -> usize {
    12
}

pub(super) fn default_media_browser_abort_refresh_when_capped() -> bool {
    true
}

pub(super) fn default_media_browser_fallback_to_library_roots_when_capped() -> bool {
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

pub(super) fn default_backup_enabled() -> bool {
    true
}

pub(super) fn default_backup_path() -> PathBuf {
    PathBuf::from("backups")
}

pub(super) fn default_backup_interval() -> u64 {
    24
}

pub(super) fn default_max_backups() -> usize {
    10
}

pub(super) fn default_max_safety_backups() -> usize {
    25
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            reconcile_links: default_true(),
        }
    }
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

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_prune_max_report_age_hours() -> u64 {
    72
}

pub(super) fn default_prune_default_max_delete() -> usize {
    5000
}

pub(super) fn default_prune_quarantine_path() -> PathBuf {
    PathBuf::from("quarantine")
}

pub(super) fn default_db_path() -> String {
    "symlinkarr.db".to_string()
}

pub(super) fn default_log_level() -> String {
    "info".to_string()
}

pub(super) fn default_media_type() -> MediaType {
    MediaType::Tv
}

pub(super) fn default_source_media_type() -> String {
    "auto".to_string()
}

pub(super) fn default_depth() -> usize {
    1
}

pub(super) fn default_cache_ttl() -> u64 {
    87600 // ~10 years — metadata is intentionally sticky; freshness should come from targeted refresh signals
}

pub(super) fn default_interval() -> u64 {
    30
}

pub(super) fn default_vacuum_hour_local() -> u8 {
    4
}

pub(super) fn default_realdebrid_torrents_page_limit() -> u32 {
    5000
}

pub(super) fn default_realdebrid_pagination_delay_ms() -> u64 {
    200
}

pub(super) fn default_realdebrid_max_pages() -> u32 {
    5000
}

pub(super) fn default_naming_template() -> String {
    "{title} - S{season:02}E{episode:02} - {episode_title}".to_string()
}

pub(super) fn default_source_probe_timeout_ms() -> u64 {
    2500
}

pub(super) fn default_decypharr_url() -> String {
    "http://localhost:8282".to_string()
}

pub(super) fn default_decypharr_poll_interval_seconds() -> u64 {
    30
}

pub(super) fn default_decypharr_completion_timeout_minutes() -> u64 {
    180
}

pub(super) fn default_decypharr_relink_timeout_minutes() -> u64 {
    15
}

pub(super) fn default_decypharr_max_in_flight() -> usize {
    3
}

pub(super) fn default_decypharr_max_requests_per_run() -> usize {
    10
}

pub(super) fn default_decypharr_queue_page_size() -> usize {
    100
}

pub(super) fn default_arr_name_movie() -> String {
    "radarr".to_string()
}

pub(super) fn default_arr_name_tv() -> String {
    "sonarr".to_string()
}

pub(super) fn default_arr_name_anime() -> String {
    "sonarr-anime".to_string()
}

pub(super) fn default_dmm_only_trusted() -> bool {
    true
}

pub(super) fn default_dmm_max_search_results() -> usize {
    3
}

pub(super) fn default_dmm_max_torrent_results() -> usize {
    10
}
