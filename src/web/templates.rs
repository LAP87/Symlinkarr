//! Askama templates for the web UI

use askama::Template;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::SystemTime;

#[allow(unused_imports)]
use super::filters;

use crate::cleanup_audit::CleanupReport;
use crate::config::Config;
use crate::db::ScanHistoryRecord;
use crate::models::LinkRecord;

// ─── Dashboard ──────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct DashboardStats {
    pub active_links: i64,
    pub dead_links: i64,
    pub total_scans: i64,
    pub last_scan: Option<DateTime<Utc>>,
}

#[derive(Template)]
#[template(path = "web/ui/dashboard.html")]
pub struct DashboardTemplate {
    pub stats: DashboardStats,
}

// ─── Status ─────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/status.html")]
pub struct StatusTemplate {
    pub stats: DashboardStats,
    pub recent_links: Vec<LinkRecord>,
}

// ─── Health ─────────────────────────────────────────────────────────

pub struct HealthCheck {
    pub service: String,
    pub status: String,
    pub message: String,
}

#[derive(Template)]
#[template(path = "web/ui/health.html")]
pub struct HealthTemplate {
    pub checks: std::collections::HashMap<String, HealthCheck>,
}

// ─── Scan ───────────────────────────────────────────────────────────

use crate::config::LibraryConfig;

#[derive(Template)]
#[template(path = "web/ui/scan.html")]
pub struct ScanTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub history: Vec<ScanHistoryRecord>,
}

#[derive(Template)]
#[template(path = "web/ui/scan_result.html")]
pub struct ScanResultTemplate {
    pub success: bool,
    pub message: String,
    pub matches: Vec<crate::models::MatchResult>,
    pub dry_run: bool,
}

#[derive(Template)]
#[template(path = "web/ui/scan_history.html")]
pub struct ScanHistoryTemplate {
    pub history: Vec<ScanHistoryRecord>,
}

// ─── Cleanup ────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/cleanup.html")]
pub struct CleanupTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub last_report: Option<PathBuf>,
}

#[derive(Template)]
#[template(path = "web/ui/cleanup_result.html")]
pub struct CleanupResultTemplate {
    pub success: bool,
    pub message: String,
    pub report_path: Option<PathBuf>,
}

#[derive(Template)]
#[template(path = "web/ui/prune_preview.html")]
pub struct PrunePreviewTemplate {
    pub findings: Vec<crate::cleanup_audit::CleanupFinding>,
    pub total: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
    pub confirmation_token: Option<String>,
}

// ─── Links ──────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "web/ui/links.html")]
pub struct LinksTemplate {
    pub links: Vec<LinkRecord>,
    pub filter: String,
}

#[derive(Template)]
#[template(path = "web/ui/dead_links.html")]
pub struct DeadLinksTemplate {
    pub links: Vec<LinkRecord>,
}

#[derive(Template)]
#[template(path = "web/ui/repair_result.html")]
pub struct RepairResultTemplate {
    pub success: bool,
    pub message: String,
    pub repaired: usize,
    pub failed: usize,
}

// ─── Config ─────────────────────────────────────────────────────────

pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Template)]
#[template(path = "web/ui/config.html")]
pub struct ConfigTemplate {
    pub config: Config,
    pub validation_result: Option<ValidationResult>,
}

// ─── Doctor ─────────────────────────────────────────────────────────

pub struct DoctorCheck {
    pub check: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Template)]
#[template(path = "web/ui/doctor.html")]
pub struct DoctorTemplate {
    pub checks: Vec<DoctorCheck>,
    pub all_passed: bool,
}

// ─── Discover ───────────────────────────────────────────────────────

use crate::discovery::DiscoveredItem;

#[derive(Template)]
#[template(path = "web/ui/discover.html")]
pub struct DiscoverTemplate {
    pub libraries: Vec<LibraryConfig>,
    pub discovered_items: Vec<DiscoveredItem>,
}

#[derive(Template)]
#[template(path = "web/ui/discover_result.html")]
pub struct DiscoverResultTemplate {
    pub success: bool,
    pub message: String,
}

// ─── Backup ─────────────────────────────────────────────────────────

pub struct BackupInfo {
    pub filename: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Template)]
#[template(path = "web/ui/backup.html")]
pub struct BackupTemplate {
    pub backups: Vec<BackupInfo>,
    pub backup_dir: PathBuf,
}

#[derive(Template)]
#[template(path = "web/ui/backup_result.html")]
pub struct BackupResultTemplate {
    pub success: bool,
    pub message: String,
    pub backup_path: Option<PathBuf>,
}
