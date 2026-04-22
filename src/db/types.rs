use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::models::MediaType;

/// Result of a housekeeping run (H-09).
#[derive(Debug, Default)]
pub struct HousekeepingStats {
    pub scan_runs_deleted: u64,
    pub link_events_deleted: u64,
    pub old_jobs_deleted: u64,
    pub expired_api_cache_deleted: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ScanRunRecord {
    pub origin: ScanRunOrigin,
    pub dry_run: bool,
    pub library_filter: Option<String>,
    pub run_token: Option<String>,
    pub search_missing: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
    pub links_removed: i64,
    pub links_skipped: i64,
    pub ambiguous_skipped: i64,
    pub skip_reason_json: Option<String>,
    pub runtime_checks_ms: i64,
    pub library_scan_ms: i64,
    pub source_inventory_ms: i64,
    pub matching_ms: i64,
    pub title_enrichment_ms: i64,
    pub linking_ms: i64,
    pub plex_refresh_ms: i64,
    pub plex_refresh_requested_paths: i64,
    pub plex_refresh_unique_paths: i64,
    pub plex_refresh_planned_batches: i64,
    pub plex_refresh_coalesced_batches: i64,
    pub plex_refresh_coalesced_paths: i64,
    pub plex_refresh_refreshed_batches: i64,
    pub plex_refresh_refreshed_paths_covered: i64,
    pub plex_refresh_skipped_batches: i64,
    pub plex_refresh_unresolved_paths: i64,
    pub plex_refresh_capped_batches: i64,
    pub plex_refresh_aborted_due_to_cap: bool,
    pub plex_refresh_failed_batches: i64,
    pub media_server_refresh_json: Option<String>,
    pub dead_link_sweep_ms: i64,
    pub cache_hit_ratio: Option<f64>,
    pub candidate_slots: i64,
    pub scored_candidates: i64,
    pub exact_id_hits: i64,
    pub auto_acquire_requests: i64,
    pub auto_acquire_missing_requests: i64,
    pub auto_acquire_cutoff_requests: i64,
    pub auto_acquire_dry_run_hits: i64,
    pub auto_acquire_submitted: i64,
    pub auto_acquire_no_result: i64,
    pub auto_acquire_blocked: i64,
    pub auto_acquire_failed: i64,
    pub auto_acquire_completed_linked: i64,
    pub auto_acquire_completed_unlinked: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanRunOrigin {
    #[default]
    Unknown,
    Cli,
    Daemon,
    Web,
    AutoAcquire,
}

impl ScanRunOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Cli => "cli",
            Self::Daemon => "daemon",
            Self::Web => "web",
            Self::AutoAcquire => "auto_acquire",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "unknown" => Ok(Self::Unknown),
            "cli" => Ok(Self::Cli),
            "daemon" => Ok(Self::Daemon),
            "web" => Ok(Self::Web),
            "auto_acquire" => Ok(Self::AutoAcquire),
            _ => anyhow::bail!(
                "Unsupported scan run origin '{}' in the database. Expected one of: unknown, cli, daemon, web, auto_acquire",
                value
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnimeSearchOverrideSeed {
    pub media_id: String,
    pub preferred_title: Option<String>,
    pub extra_hints: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnimeSearchOverrideRecord {
    pub media_id: String,
    pub preferred_title: Option<String>,
    pub extra_hints: Vec<String>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct DeadLinkSeed {
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub media_id: String,
    pub media_type: MediaType,
}

#[derive(Debug, Clone, Default)]
pub struct LinkEventRecord {
    pub run_id: Option<i64>,
    pub run_token: Option<String>,
    pub action: String,
    pub target_path: PathBuf,
    pub source_path: Option<PathBuf>,
    pub media_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEventHistoryRecord {
    pub event_at: String,
    pub action: String,
    pub target_path: PathBuf,
    pub source_path: Option<PathBuf>,
    pub media_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionRelinkKind {
    MediaId,
    MediaEpisode,
    SymlinkPath,
}

impl AcquisitionRelinkKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MediaId => "media_id",
            Self::MediaEpisode => "media_episode",
            Self::SymlinkPath => "symlink_path",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "media_id" => Ok(Self::MediaId),
            "media_episode" => Ok(Self::MediaEpisode),
            "symlink_path" => Ok(Self::SymlinkPath),
            _ => anyhow::bail!(
                "Unsupported acquisition relink kind '{}' in the database. Expected one of: media_id, media_episode, symlink_path",
                value
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionJobStatus {
    Queued,
    Downloading,
    Relinking,
    NoResult,
    Blocked,
    CompletedLinked,
    CompletedUnlinked,
    Failed,
}

impl AcquisitionJobStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Relinking => "relinking",
            Self::NoResult => "no_result",
            Self::Blocked => "blocked",
            Self::CompletedLinked => "completed_linked",
            Self::CompletedUnlinked => "completed_unlinked",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "downloading" => Ok(Self::Downloading),
            "relinking" => Ok(Self::Relinking),
            "no_result" => Ok(Self::NoResult),
            "blocked" => Ok(Self::Blocked),
            "completed_linked" => Ok(Self::CompletedLinked),
            "completed_unlinked" => Ok(Self::CompletedUnlinked),
            "failed" => Ok(Self::Failed),
            _ => anyhow::bail!(
                "Unsupported acquisition job status '{}' in the database. Expected one of: queued, downloading, relinking, no_result, blocked, completed_linked, completed_unlinked, failed",
                value
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcquisitionJobSeed {
    pub request_key: String,
    pub label: String,
    pub query: String,
    pub query_hints: Vec<String>,
    pub imdb_id: Option<String>,
    pub categories: Vec<i32>,
    pub arr: String,
    pub library_filter: Option<String>,
    pub relink_kind: AcquisitionRelinkKind,
    pub relink_value: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AcquisitionJobRecord {
    pub id: i64,
    pub request_key: String,
    pub label: String,
    pub query: String,
    pub query_hints: Vec<String>,
    pub imdb_id: Option<String>,
    pub categories: Vec<i32>,
    pub arr: String,
    pub library_filter: Option<String>,
    pub relink_kind: AcquisitionRelinkKind,
    pub relink_value: String,
    pub status: AcquisitionJobStatus,
    pub release_title: Option<String>,
    pub info_hash: Option<String>,
    pub error: Option<String>,
    pub attempts: i64,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AcquisitionJobUpdate {
    pub status: AcquisitionJobStatus,
    pub release_title: Option<String>,
    pub info_hash: Option<String>,
    pub error: Option<String>,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub increment_attempts: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AcquisitionJobCounts {
    pub queued: i64,
    pub downloading: i64,
    pub relinking: i64,
    pub blocked: i64,
    pub no_result: i64,
    pub failed: i64,
    pub completed_unlinked: i64,
}

impl AcquisitionJobCounts {
    pub fn active_total(&self) -> i64 {
        self.queued
            + self.downloading
            + self.relinking
            + self.blocked
            + self.no_result
            + self.failed
            + self.completed_unlinked
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MediaTypeStats {
    pub media_type: String,
    pub library_items: i64,
    pub linked: i64,
    pub broken: i64,
}

/// Summary statistics for the web dashboard.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct WebStats {
    pub active_links: i64,
    pub dead_links: i64,
    pub total_scans: i64,
    pub last_scan: Option<String>,
}

/// A single scan run record for the web history view.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ScanHistoryRecord {
    pub id: i64,
    pub started_at: String,
    pub origin: ScanRunOrigin,
    pub dry_run: bool,
    pub library_filter: Option<String>,
    pub run_token: Option<String>,
    pub search_missing: bool,
    pub library_items_found: i64,
    pub source_items_found: i64,
    pub matches_found: i64,
    pub links_created: i64,
    pub links_updated: i64,
    pub dead_marked: i64,
    pub links_removed: i64,
    pub links_skipped: i64,
    pub ambiguous_skipped: i64,
    pub skip_reason_json: Option<String>,
    pub runtime_checks_ms: i64,
    pub library_scan_ms: i64,
    pub source_inventory_ms: i64,
    pub matching_ms: i64,
    pub title_enrichment_ms: i64,
    pub linking_ms: i64,
    pub plex_refresh_ms: i64,
    pub plex_refresh_requested_paths: i64,
    pub plex_refresh_unique_paths: i64,
    pub plex_refresh_planned_batches: i64,
    pub plex_refresh_coalesced_batches: i64,
    pub plex_refresh_coalesced_paths: i64,
    pub plex_refresh_refreshed_batches: i64,
    pub plex_refresh_refreshed_paths_covered: i64,
    pub plex_refresh_skipped_batches: i64,
    pub plex_refresh_unresolved_paths: i64,
    pub plex_refresh_capped_batches: i64,
    pub plex_refresh_aborted_due_to_cap: bool,
    pub plex_refresh_failed_batches: i64,
    pub media_server_refresh_json: Option<String>,
    pub dead_link_sweep_ms: i64,
    pub cache_hit_ratio: Option<f64>,
    pub candidate_slots: i64,
    pub scored_candidates: i64,
    pub exact_id_hits: i64,
    pub auto_acquire_requests: i64,
    pub auto_acquire_missing_requests: i64,
    pub auto_acquire_cutoff_requests: i64,
    pub auto_acquire_dry_run_hits: i64,
    pub auto_acquire_submitted: i64,
    pub auto_acquire_no_result: i64,
    pub auto_acquire_blocked: i64,
    pub auto_acquire_failed: i64,
    pub auto_acquire_completed_linked: i64,
    pub auto_acquire_completed_unlinked: i64,
}
