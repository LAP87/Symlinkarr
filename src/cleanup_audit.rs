use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::anime_roots::collect_anime_root_duplicate_groups;
use crate::api::sonarr::SonarrClient;
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::backup::BackupManager;
use crate::commands::ensure_runtime_sources_healthy;
use crate::config::{Config, ContentType, LibraryConfig, MetadataMode};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::matcher::{best_alias_score, fetch_metadata_static};
use crate::models::{ContentMetadata, LibraryItem, LinkStatus, MediaId, MediaType};
use crate::source_scanner::SourceScanner;
use crate::utils::{normalize, ProgressLine};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CleanupScope {
    Anime,
    Tv,
    Movie,
    All,
}

impl CleanupScope {
    pub fn parse(input: &str) -> Result<Self> {
        match input.to_lowercase().as_str() {
            "anime" => Ok(Self::Anime),
            "tv" | "series" | "shows" => Ok(Self::Tv),
            "movie" | "movies" | "film" | "films" => Ok(Self::Movie),
            "all" => Ok(Self::All),
            _ => anyhow::bail!(
                "Unsupported scope '{}'. Supported: anime, tv, movie, all",
                input
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Critical,
    High,
    Warning,
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingSeverity::Critical => write!(f, "critical"),
            FindingSeverity::High => write!(f, "high"),
            FindingSeverity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CleanupOwnership {
    Managed,
    #[default]
    Foreign,
}

impl std::fmt::Display for CleanupOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CleanupOwnership::Managed => write!(f, "managed"),
            CleanupOwnership::Foreign => write!(f, "foreign"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FindingReason {
    BrokenSource,
    LegacyAnimeRootDuplicate,
    ParserTitleMismatch,
    AlternateLibraryMatch,
    MovieEpisodeSource,
    ArrUntracked,
    EpisodeOutOfRange,
    DuplicateEpisodeSlot,
    SeasonCountAnomaly,
    NonRdSourcePath,
}

impl std::fmt::Display for FindingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingReason::BrokenSource => write!(f, "broken_source"),
            FindingReason::LegacyAnimeRootDuplicate => write!(f, "legacy_anime_root_duplicate"),
            FindingReason::ParserTitleMismatch => write!(f, "parser_title_mismatch"),
            FindingReason::AlternateLibraryMatch => write!(f, "alternate_library_match"),
            FindingReason::MovieEpisodeSource => write!(f, "movie_episode_source"),
            FindingReason::ArrUntracked => write!(f, "arr_untracked"),
            FindingReason::EpisodeOutOfRange => write!(f, "episode_out_of_range"),
            FindingReason::DuplicateEpisodeSlot => write!(f, "duplicate_episode_slot"),
            FindingReason::SeasonCountAnomaly => write!(f, "season_count_anomaly"),
            FindingReason::NonRdSourcePath => write!(f, "non_rd_source_path"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParsedContext {
    pub library_title: String,
    pub parsed_title: String,
    #[serde(default)]
    pub year: Option<u32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlternateMatchContext {
    pub media_id: String,
    pub title: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyAnimeRootDetails {
    pub normalized_title: String,
    pub untagged_root: PathBuf,
    pub tagged_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupFinding {
    pub symlink_path: PathBuf,
    pub source_path: PathBuf,
    pub media_id: String,
    pub severity: FindingSeverity,
    pub confidence: f64,
    pub reasons: Vec<FindingReason>,
    pub parsed: ParsedContext,
    #[serde(default)]
    pub alternate_match: Option<AlternateMatchContext>,
    #[serde(default)]
    pub legacy_anime_root: Option<LegacyAnimeRootDetails>,
    #[serde(default)]
    pub db_tracked: bool,
    #[serde(default)]
    pub ownership: CleanupOwnership,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CleanupSummary {
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupReport {
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub scope: CleanupScope,
    pub findings: Vec<CleanupFinding>,
    pub summary: CleanupSummary,
    #[serde(default)]
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct PruneOutcome {
    pub candidates: usize,
    pub blocked_candidates: usize,
    pub high_or_critical_candidates: usize,
    pub safe_warning_duplicate_candidates: usize,
    pub legacy_anime_root_candidates: usize,
    pub legacy_anime_root_groups: Vec<LegacyAnimeRootGroupCount>,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub reason_counts: Vec<PruneReasonCount>,
    pub blocked_reason_summary: Vec<PruneBlockedReasonSummary>,
    pub removed: usize,
    pub quarantined: usize,
    pub skipped: usize,
    pub confirmation_token: String,
    pub affected_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PruneDisposition {
    Delete,
    Quarantine,
}

#[derive(Debug, Clone)]
pub(crate) struct PrunePlan {
    pub candidate_paths: Vec<PathBuf>,
    pub blocked_candidates: usize,
    pub high_or_critical_candidates: usize,
    pub safe_warning_duplicate_candidates: usize,
    pub legacy_anime_root_candidates: usize,
    pub legacy_anime_root_groups: Vec<LegacyAnimeRootGroupCount>,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub reason_counts: Vec<PruneReasonCount>,
    pub blocked_reason_summary: Vec<PruneBlockedReasonSummary>,
    pub confirmation_token: String,
    dispositions: HashMap<PathBuf, PruneDisposition>,
    blocked_by_path: HashMap<PathBuf, PruneBlockedReasonCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrunePathAction {
    Delete,
    Quarantine,
    Blocked(PruneBlockedReasonCode),
    ObserveOnly,
}

impl PrunePlan {
    pub(crate) fn action_for_path(&self, path: &Path) -> PrunePathAction {
        match self.dispositions.get(path).copied() {
            Some(PruneDisposition::Delete) => PrunePathAction::Delete,
            Some(PruneDisposition::Quarantine) => PrunePathAction::Quarantine,
            None => self
                .blocked_by_path
                .get(path)
                .copied()
                .map(PrunePathAction::Blocked)
                .unwrap_or(PrunePathAction::ObserveOnly),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PruneReasonCount {
    pub reason: FindingReason,
    pub total: usize,
    pub managed: usize,
    pub foreign: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PruneBlockedReasonCode {
    ForeignQuarantineDisabled,
    DuplicateSlotNeedsTrackedAnchor,
    DuplicateSlotTainted,
    DuplicateSlotMultipleTrackedAnchors,
    DuplicateSlotSourceMismatch,
    LegacyAnimeRootsExcludedByDefault,
}

impl PruneBlockedReasonCode {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::ForeignQuarantineDisabled => {
                "foreign candidates are blocked because quarantine is disabled"
            }
            Self::DuplicateSlotNeedsTrackedAnchor => {
                "duplicate slots without a tracked anchor are blocked"
            }
            Self::DuplicateSlotTainted => "duplicate slots with extra anomaly reasons are blocked",
            Self::DuplicateSlotMultipleTrackedAnchors => {
                "duplicate slots with multiple tracked anchors are blocked"
            }
            Self::DuplicateSlotSourceMismatch => {
                "duplicate slots with mismatched source files are blocked"
            }
            Self::LegacyAnimeRootsExcludedByDefault => {
                "legacy anime-root warnings are excluded by default"
            }
        }
    }

    pub(crate) fn recommended_action(&self) -> &'static str {
        match self {
            Self::ForeignQuarantineDisabled => {
                "Enable cleanup.prune.quarantine_foreign or review the foreign symlinks manually before applying prune."
            }
            Self::DuplicateSlotNeedsTrackedAnchor => {
                "Keep scanning until one canonical tracked link owns the slot before auto-pruning the duplicates."
            }
            Self::DuplicateSlotTainted => {
                "Do not auto-prune mixed anomaly slots; inspect the title manually and clear the extra mismatch first."
            }
            Self::DuplicateSlotMultipleTrackedAnchors => {
                "Resolve which tracked path should win before letting prune remove anything else in that slot."
            }
            Self::DuplicateSlotSourceMismatch => {
                "Do not auto-prune duplicates that point at different source files; inspect the release choice manually."
            }
            Self::LegacyAnimeRootsExcludedByDefault => {
                "Use the guarded anime remediation workflow or explicitly opt in to legacy anime-root cleanup after review."
            }
        }
    }
}

impl std::fmt::Display for PruneBlockedReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignQuarantineDisabled => write!(f, "foreign_quarantine_disabled"),
            Self::DuplicateSlotNeedsTrackedAnchor => {
                write!(f, "duplicate_slot_needs_tracked_anchor")
            }
            Self::DuplicateSlotTainted => write!(f, "duplicate_slot_tainted"),
            Self::DuplicateSlotMultipleTrackedAnchors => {
                write!(f, "duplicate_slot_multiple_tracked_anchors")
            }
            Self::DuplicateSlotSourceMismatch => write!(f, "duplicate_slot_source_mismatch"),
            Self::LegacyAnimeRootsExcludedByDefault => {
                write!(f, "legacy_anime_roots_excluded_by_default")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PruneBlockedReasonSummary {
    pub code: PruneBlockedReasonCode,
    pub label: String,
    pub candidates: usize,
    pub recommended_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyAnimeRootGroupCount {
    pub normalized_title: String,
    pub total: usize,
    pub tagged_roots: Vec<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SafeDuplicatePrunePlan {
    pub prune_paths: Vec<PathBuf>,
    pub managed_paths: HashSet<PathBuf>,
    pub blocked_reason_counts: HashMap<PruneBlockedReasonCode, usize>,
    pub blocked_by_path: HashMap<PathBuf, PruneBlockedReasonCode>,
}

#[derive(Debug, Default, Clone)]
struct ArrSeriesSnapshot {
    with_file: HashSet<(u32, u32)>,
    season_counts: HashMap<u32, usize>,
}

#[derive(Debug, Clone)]
struct LegacyAnimeRootContext {
    normalized_title: String,
    untagged_root: PathBuf,
    tagged_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct WorkingEntry {
    symlink_path: PathBuf,
    source_path: PathBuf,
    media_id: String,
    media_type: MediaType,
    content_type: ContentType,
    parsed_title: String,
    year: Option<u32>,
    season: Option<u32>,
    episode: Option<u32>,
    library_title: String,
    alternate_match: Option<AlternateMatchContext>,
    legacy_anime_root: Option<LegacyAnimeRootDetails>,
    reasons: BTreeSet<FindingReason>,
}

pub struct CleanupAuditor<'a> {
    cfg: &'a Config,
    db: &'a Database,
    source_scanner: SourceScanner,
    emit_progress: bool,
}

impl<'a> CleanupAuditor<'a> {
    pub fn new_with_progress(cfg: &'a Config, db: &'a Database, emit_progress: bool) -> Self {
        Self {
            cfg,
            db,
            source_scanner: SourceScanner::new(),
            emit_progress,
        }
    }

    pub async fn run_audit(
        &self,
        scope: CleanupScope,
        output_path: Option<&Path>,
    ) -> Result<PathBuf> {
        let report = self.build_report(scope).await?;
        let out_path = output_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| default_report_path(scope));

        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
        if self.cfg.security.enforce_secure_permissions {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = std::fs::Permissions::from_mode(0o600);
                std::fs::set_permissions(&out_path, perm)?;
            }
        }
        info!(
            "Cleanup audit written: {:?} ({} findings)",
            out_path, report.summary.total_findings
        );

        Ok(out_path)
    }

    pub async fn run_audit_filtered(
        &self,
        scope: CleanupScope,
        selected_libraries: Option<&[String]>,
        output_path: Option<&Path>,
    ) -> Result<PathBuf> {
        let report = self
            .build_report_filtered(scope, selected_libraries)
            .await?;
        let out_path = output_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| default_report_path(scope));

        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
        if self.cfg.security.enforce_secure_permissions {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = std::fs::Permissions::from_mode(0o600);
                std::fs::set_permissions(&out_path, perm)?;
            }
        }
        info!(
            "Cleanup audit written: {:?} ({} findings)",
            out_path, report.summary.total_findings
        );

        Ok(out_path)
    }

    pub async fn build_report(&self, scope: CleanupScope) -> Result<CleanupReport> {
        self.build_report_filtered(scope, None).await
    }

    pub async fn build_report_filtered(
        &self,
        scope: CleanupScope,
        selected_libraries: Option<&[String]>,
    ) -> Result<CleanupReport> {
        let libraries = self.libraries_for_scope_filtered(scope, selected_libraries)?;
        if libraries.is_empty() {
            anyhow::bail!("No libraries found for scope {:?}", scope);
        }

        let scanner = LibraryScanner::new();
        let mut library_items = Vec::new();
        for lib in &libraries {
            library_items.extend(scanner.scan_library(lib));
        }
        let library_indices_by_path = build_library_indices_by_path(&library_items);
        let library_indices_by_id = build_library_indices_by_id(&library_items);
        let legacy_anime_roots = build_legacy_anime_root_lookup(&libraries);

        if self.emit_progress {
            println!("   🔗 Cleanup audit: collecting symlink entries...");
        }
        let entries_started = Instant::now();
        let mut entries = self.collect_symlink_entries(
            &libraries,
            &library_items,
            &library_indices_by_path,
            &legacy_anime_roots,
        );
        info!(
            "Cleanup audit: collected {} symlink entries in {:.1}s",
            entries.len(),
            entries_started.elapsed().as_secs_f64()
        );
        if self.emit_progress {
            println!(
                "   ✅ Cleanup audit collected {} symlink entries in {:.1}s",
                entries.len(),
                entries_started.elapsed().as_secs_f64()
            );
        }

        let referenced_media_ids: HashSet<String> = entries
            .iter()
            .filter(|entry| !entry.media_id.is_empty())
            .map(|entry| entry.media_id.clone())
            .collect();
        let referenced_library_items: Vec<LibraryItem> = library_items
            .iter()
            .filter(|item| referenced_media_ids.contains(&item.id.to_string()))
            .cloned()
            .collect();

        if self.emit_progress {
            println!(
                "   🧠 Cleanup audit: loading metadata for {} referenced item(s) ({} library items in scope)...",
                referenced_library_items.len(),
                library_items.len()
            );
        }
        let metadata_started = Instant::now();
        let metadata_map = self.load_metadata(&referenced_library_items).await;
        let alias_map_by_index = build_aliases_by_index(&library_items, &metadata_map);
        let alias_token_index = build_alias_token_index(&alias_map_by_index);
        info!(
            "Cleanup audit: metadata loaded for {} referenced items ({} library items in scope) in {:.1}s",
            referenced_library_items.len(),
            library_items.len(),
            metadata_started.elapsed().as_secs_f64()
        );
        if self.emit_progress {
            println!(
                "   ✅ Cleanup audit metadata ready in {:.1}s",
                metadata_started.elapsed().as_secs_f64()
            );
            println!("   📚 Cleanup audit: loading Sonarr cross-check snapshots...");
        }
        let arr_started = Instant::now();
        let arr_map = self.load_sonarr_snapshots(&referenced_library_items).await;
        info!(
            "Cleanup audit: Arr snapshots loaded for {} referenced items in {:.1}s",
            arr_map.len(),
            arr_started.elapsed().as_secs_f64()
        );
        if self.emit_progress {
            println!(
                "   ✅ Cleanup audit Sonarr snapshots ready in {:.1}s",
                arr_started.elapsed().as_secs_f64()
            );
        }

        let evaluate_started = Instant::now();
        for entry in &mut entries {
            if !entry.source_path.exists() {
                entry.reasons.insert(FindingReason::BrokenSource);
            }

            if !self.is_under_rd_sources(&entry.source_path) {
                entry.reasons.insert(FindingReason::NonRdSourcePath);
            }

            if entry.media_type == MediaType::Movie
                && (entry.season.is_some() || entry.episode.is_some())
            {
                entry.reasons.insert(FindingReason::MovieEpisodeSource);
            }

            if entry.media_id.is_empty() {
                continue;
            }

            if library_indices_by_id
                .get(&entry.media_id)
                .and_then(|idx| library_items.get(*idx))
                .is_some()
            {
                let owner_idx = *library_indices_by_id
                    .get(&entry.media_id)
                    .expect("owner index should exist when item exists");
                let owner_item = library_items
                    .get(owner_idx)
                    .expect("owner item should exist when index exists");
                let owner_metadata = metadata_map
                    .get(&entry.media_id)
                    .and_then(|meta| meta.as_ref());
                let aliases = alias_map_by_index
                    .get(owner_idx)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let normalized_parsed = normalize(&entry.parsed_title);
                let owner_title_ok = entry.parsed_title.is_empty()
                    || owner_title_matches(entry.content_type, aliases, &normalized_parsed);
                let owner_metadata_ok =
                    candidate_metadata_compatible(owner_item, entry, owner_metadata);
                if !owner_title_ok || !owner_metadata_ok {
                    entry.reasons.insert(FindingReason::ParserTitleMismatch);
                    if let Some(alternate_match) = find_alternate_library_match(
                        owner_idx,
                        entry,
                        &normalized_parsed,
                        &library_items,
                        &alias_map_by_index,
                        &alias_token_index,
                        &metadata_map,
                    ) {
                        entry.reasons.insert(FindingReason::AlternateLibraryMatch);
                        entry.alternate_match = Some(alternate_match);
                    }
                }

                if let (Some(season), Some(episode)) = (entry.season, entry.episode) {
                    if let Some(meta) = owner_metadata {
                        if episode_out_of_range(meta, season, episode) {
                            entry.reasons.insert(FindingReason::EpisodeOutOfRange);
                        }
                    }

                    if let Some(arr) = arr_map.get(&entry.media_id) {
                        // Specials (S00) are often inconsistently represented across metadata providers/Arr.
                        // Avoid hard-failing on Arr tracking for season 0 to reduce false positives.
                        if season > 0 && !arr.with_file.contains(&(season, episode)) {
                            entry.reasons.insert(FindingReason::ArrUntracked);
                        }
                    }
                }
            }
        }
        info!(
            "Cleanup audit: evaluated {} entries in {:.1}s",
            entries.len(),
            evaluate_started.elapsed().as_secs_f64()
        );

        let duplicate_started = Instant::now();
        apply_duplicate_and_count_signals(&mut entries, &metadata_map, &arr_map);
        let suppressed_count = suppress_redundant_season_count_warnings(&mut entries);
        info!(
            "Cleanup audit: duplicate/count signals applied in {:.1}s",
            duplicate_started.elapsed().as_secs_f64()
        );
        if suppressed_count > 0 {
            info!(
                "Cleanup audit: suppressed {} season_count_anomaly warnings in seasons with stronger signals",
                suppressed_count
            );
        }

        let findings_started = Instant::now();
        let mut findings = Vec::new();
        let mut summary = CleanupSummary::default();

        for entry in entries {
            if entry.reasons.is_empty() {
                continue;
            }

            let severity = classify_severity(&entry.reasons);
            let confidence = classify_confidence(&entry.reasons);
            let reasons: Vec<_> = entry.reasons.iter().copied().collect();

            match severity {
                FindingSeverity::Critical => summary.critical += 1,
                FindingSeverity::High => summary.high += 1,
                FindingSeverity::Warning => summary.warning += 1,
            }

            findings.push(CleanupFinding {
                symlink_path: entry.symlink_path,
                source_path: entry.source_path,
                media_id: entry.media_id,
                severity,
                confidence,
                reasons,
                parsed: ParsedContext {
                    library_title: entry.library_title,
                    parsed_title: entry.parsed_title,
                    year: entry.year,
                    season: entry.season,
                    episode: entry.episode,
                },
                alternate_match: entry.alternate_match,
                legacy_anime_root: entry.legacy_anime_root,
                db_tracked: false,
                ownership: CleanupOwnership::Foreign,
            });
        }
        info!(
            "Cleanup audit: materialized {} findings in {:.1}s",
            findings.len(),
            findings_started.elapsed().as_secs_f64()
        );

        let tracked_started = Instant::now();
        let tracked_paths = hydrate_db_tracked_flags(self.db, &mut findings).await?;
        debug!(
            "Cleanup audit: marked {}/{} findings as DB-tracked",
            tracked_paths.len(),
            findings.len()
        );
        info!(
            "Cleanup audit: hydrated DB tracked flags for {} findings in {:.1}s",
            findings.len(),
            tracked_started.elapsed().as_secs_f64()
        );
        summary.total_findings = findings.len();

        Ok(CleanupReport {
            version: 1,
            created_at: Utc::now(),
            scope,
            findings,
            summary,
            applied_at: None,
        })
    }

    fn libraries_for_scope_filtered(
        &self,
        scope: CleanupScope,
        selected_libraries: Option<&[String]>,
    ) -> Result<Vec<&LibraryConfig>> {
        let selected_names = selected_libraries.and_then(|names| {
            let names = names
                .iter()
                .map(|name| name.trim())
                .filter(|name| !name.is_empty())
                .collect::<HashSet<_>>();
            (!names.is_empty()).then_some(names)
        });

        let libraries: Vec<&LibraryConfig> = self
            .cfg
            .libraries
            .iter()
            .filter(|lib| match scope {
                CleanupScope::Anime => effective_content_type(lib) == ContentType::Anime,
                CleanupScope::Tv => effective_content_type(lib) == ContentType::Tv,
                CleanupScope::Movie => effective_content_type(lib) == ContentType::Movie,
                CleanupScope::All => true,
            })
            .filter(|lib| {
                selected_names
                    .as_ref()
                    .map(|names| names.contains(lib.name.as_str()))
                    .unwrap_or(true)
            })
            .collect();

        if let Some(names) = selected_names {
            if libraries.is_empty() {
                anyhow::bail!(
                    "No libraries matched scope {:?} for selection: {}",
                    scope,
                    names.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
        }

        Ok(libraries)
    }

    fn collect_symlink_entries(
        &self,
        libraries: &[&LibraryConfig],
        library_items: &[LibraryItem],
        library_indices_by_path: &HashMap<PathBuf, usize>,
        legacy_anime_roots: &HashMap<PathBuf, LegacyAnimeRootContext>,
    ) -> Vec<WorkingEntry> {
        let mut entries = Vec::new();
        let mut symlink_count = 0usize;
        let mut last_progress = Instant::now();
        let mut progress = self
            .emit_progress
            .then(|| ProgressLine::new("Cleanup audit symlink scan:"));

        for lib in libraries {
            for entry in WalkDir::new(&lib.path).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_symlink() {
                    continue;
                }
                symlink_count += 1;
                if let Some(progress) = progress.as_mut() {
                    if last_progress.elapsed() >= Duration::from_secs(5) {
                        if !progress.is_tty() {
                            info!(
                                "Cleanup audit symlink collection progress: {} symlinks",
                                symlink_count
                            );
                        }
                        progress.update(format!("{} symlinks", symlink_count));
                        last_progress = Instant::now();
                    }
                }

                let symlink_path = entry.path().to_path_buf();
                let target = match std::fs::read_link(&symlink_path) {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("Could not read symlink {:?}: {}", symlink_path, e);
                        continue;
                    }
                };

                let source_path = resolve_link_target(&symlink_path, &target);

                let owner =
                    find_owner_library_item(&symlink_path, library_items, library_indices_by_path);
                let legacy_root_context = legacy_anime_root_context_for_path(
                    &symlink_path,
                    &lib.path,
                    legacy_anime_roots,
                );

                let owner_content_type = owner
                    .map(|o| o.content_type)
                    .unwrap_or_else(|| effective_content_type(lib));
                let owner_media_type = owner.map(|o| o.media_type).unwrap_or(lib.media_type);
                let parsed_source = self
                    .source_scanner
                    .parse_filename_with_type(&source_path, owner_content_type)
                    .or_else(|| {
                        self.source_scanner
                            .parse_filename_with_type(&symlink_path, owner_content_type)
                    });

                let (media_id, mut library_title) = owner
                    .map(|o| (o.id.to_string(), o.title.clone()))
                    .unwrap_or_else(|| (String::new(), String::new()));
                let mut reasons = BTreeSet::new();
                let mut legacy_anime_root = None;
                if owner.is_none() {
                    if let Some(context) = legacy_root_context {
                        library_title = context.normalized_title.clone();
                        reasons.insert(FindingReason::LegacyAnimeRootDuplicate);
                        legacy_anime_root = Some(LegacyAnimeRootDetails {
                            normalized_title: context.normalized_title.clone(),
                            untagged_root: context.untagged_root.clone(),
                            tagged_roots: context.tagged_roots.clone(),
                        });
                    }
                }

                entries.push(WorkingEntry {
                    symlink_path,
                    source_path,
                    media_id,
                    media_type: owner_media_type,
                    content_type: owner_content_type,
                    parsed_title: parsed_source
                        .as_ref()
                        .map(|s| s.parsed_title.clone())
                        .unwrap_or_default(),
                    year: parsed_source.as_ref().and_then(|s| s.year),
                    season: parsed_source.as_ref().and_then(|s| s.season),
                    episode: parsed_source.as_ref().and_then(|s| s.episode),
                    library_title,
                    alternate_match: None,
                    legacy_anime_root,
                    reasons,
                });
            }
        }

        info!("Cleanup audit scanned {} symlinks", entries.len());
        if let Some(progress) = progress.as_mut() {
            progress.finish(format!("{} symlinks collected", symlink_count));
        }
        entries
    }

    fn is_under_rd_sources(&self, source_path: &Path) -> bool {
        self.cfg
            .sources
            .iter()
            .any(|source| source_path.starts_with(&source.path))
    }

    async fn load_metadata(
        &self,
        library_items: &[LibraryItem],
    ) -> HashMap<String, Option<ContentMetadata>> {
        let mut map = HashMap::new();
        let metadata_mode = self.cfg.matching.metadata_mode;

        let tmdb = if self.cfg.has_tmdb() {
            Some(TmdbClient::new(
                &self.cfg.api.tmdb_api_key,
                Some(&self.cfg.api.tmdb_read_access_token),
                self.cfg.api.cache_ttl_hours,
            ))
        } else {
            None
        };

        let tvdb = if self.cfg.has_tvdb() {
            Some(Arc::new(Mutex::new(TvdbClient::new(
                &self.cfg.api.tvdb_api_key,
                self.cfg.api.cache_ttl_hours,
            ))))
        } else {
            None
        };

        if metadata_mode == MetadataMode::Off {
            info!("Cleanup audit: metadata lookups disabled (matching.metadata_mode=off)");
            return map;
        }

        let mut unique_items = Vec::new();
        let mut seen_ids = HashSet::new();
        for item in library_items {
            let key = item.id.to_string();
            if seen_ids.insert(key.clone()) {
                unique_items.push((key, item.clone()));
            }
        }

        let total = unique_items.len();
        let mut last_progress = Instant::now();
        let mut progress = self
            .emit_progress
            .then(|| ProgressLine::new("Cleanup metadata:"));
        let concurrency = self.cfg.matching.metadata_concurrency.max(1);
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut join_set: JoinSet<(String, Result<Option<ContentMetadata>>)> = JoinSet::new();

        for (key, item) in unique_items {
            let sem = Arc::clone(&semaphore);
            let tmdb = tmdb.clone();
            let tvdb = tvdb.clone();
            let db = self.db.clone();

            join_set.spawn(async move {
                let permit = match sem.acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        return (
                            key,
                            Err(anyhow::anyhow!(
                                "cleanup metadata semaphore unexpectedly closed"
                            )),
                        );
                    }
                };
                let result =
                    fetch_metadata_static(&tmdb, tvdb.as_ref(), metadata_mode, &item, &db).await;
                drop(permit);
                (key, result)
            });
        }

        let mut completed = 0usize;
        while let Some(join_result) = join_set.join_next().await {
            completed += 1;
            if let Some(progress) = progress.as_mut() {
                if completed > 0 && last_progress.elapsed() >= Duration::from_secs(5) {
                    let pct = (completed as f64 / total.max(1) as f64) * 100.0;
                    if !progress.is_tty() {
                        info!(
                            "Cleanup audit metadata progress: {}/{} ({:.1}%)",
                            completed, total, pct
                        );
                    }
                    progress.update(format!("{}/{} ({:.1}%)", completed, total, pct));
                    last_progress = Instant::now();
                }
            }

            match join_result {
                Ok((key, Ok(metadata))) => {
                    map.insert(key, metadata);
                }
                Ok((key, Err(err))) => {
                    warn!("Cleanup audit metadata fetch failed for {}: {}", key, err);
                    map.insert(key, None);
                }
                Err(err) => {
                    warn!("Cleanup audit metadata task panicked: {}", err);
                }
            }
        }

        if let Some(progress) = progress.as_mut() {
            progress.finish(format!("{}/{} (100.0%)", total, total));
        }
        map
    }

    async fn load_sonarr_snapshots(
        &self,
        library_items: &[LibraryItem],
    ) -> HashMap<String, ArrSeriesSnapshot> {
        let maybe_client = if self.cfg.has_sonarr_anime() {
            Some(SonarrClient::new(
                &self.cfg.sonarr_anime.url,
                &self.cfg.sonarr_anime.api_key,
            ))
        } else if self.cfg.has_sonarr() {
            Some(SonarrClient::new(
                &self.cfg.sonarr.url,
                &self.cfg.sonarr.api_key,
            ))
        } else {
            None
        };

        let Some(client) = maybe_client else {
            info!("Cleanup audit: Sonarr not configured, skipping Arr cross-check");
            return HashMap::new();
        };

        let series = match client.get_series().await {
            Ok(series) => series,
            Err(e) => {
                warn!("Cleanup audit: could not fetch Sonarr series list: {}", e);
                return HashMap::new();
            }
        };

        let mut by_tvdb: HashMap<u64, i64> = HashMap::new();
        let mut by_tmdb: HashMap<u64, i64> = HashMap::new();
        for s in &series {
            if s.tvdb_id > 0 {
                by_tvdb.insert(s.tvdb_id as u64, s.id);
            }
            if s.tmdb_id > 0 {
                by_tmdb.insert(s.tmdb_id as u64, s.id);
            }
        }

        let mut snapshots_by_series: HashMap<i64, ArrSeriesSnapshot> = HashMap::new();
        let mut by_media_id: HashMap<String, ArrSeriesSnapshot> = HashMap::new();
        let total = library_items.len();
        let mut fetched_series = 0usize;
        let mut last_progress = Instant::now();
        let mut progress = self
            .emit_progress
            .then(|| ProgressLine::new("Cleanup Sonarr map:"));

        for (idx, item) in library_items.iter().enumerate() {
            if let Some(progress) = progress.as_mut() {
                if idx > 0 && last_progress.elapsed() >= Duration::from_secs(5) {
                    let pct = (idx as f64 / total.max(1) as f64) * 100.0;
                    if !progress.is_tty() {
                        info!(
                            "Cleanup audit Sonarr snapshot progress: {}/{} items ({:.1}%), {} series fetched",
                            idx, total, pct, fetched_series
                        );
                    }
                    progress.update(format!(
                        "{}/{} items ({:.1}%), {} series fetched",
                        idx, total, pct, fetched_series
                    ));
                    last_progress = Instant::now();
                }
            }
            let series_id = match item.id {
                MediaId::Tvdb(id) => by_tvdb.get(&id).copied(),
                MediaId::Tmdb(id) => by_tmdb.get(&id).copied(),
            };

            let Some(series_id) = series_id else {
                continue;
            };

            if let std::collections::hash_map::Entry::Vacant(entry) =
                snapshots_by_series.entry(series_id)
            {
                let episodes = match client.get_episodes_for_series(series_id).await {
                    Ok(episodes) => episodes,
                    Err(e) => {
                        warn!(
                            "Cleanup audit: could not fetch Sonarr episodes for series {}: {}",
                            series_id, e
                        );
                        continue;
                    }
                };

                let mut snapshot = ArrSeriesSnapshot::default();
                for ep in &episodes {
                    // Keep season 0 (specials) in snapshot for better audit context.
                    if ep.episode_number == 0 {
                        continue;
                    }

                    *snapshot.season_counts.entry(ep.season_number).or_insert(0) += 1;

                    if ep.has_file || ep.episode_file_id.unwrap_or(0) > 0 {
                        snapshot
                            .with_file
                            .insert((ep.season_number, ep.episode_number));
                    }
                }

                entry.insert(snapshot);
                fetched_series += 1;
            }

            if let Some(snapshot) = snapshots_by_series.get(&series_id) {
                by_media_id.insert(item.id.to_string(), snapshot.clone());
            }
        }

        info!(
            "Cleanup audit: Sonarr snapshots mapped for {} library items",
            by_media_id.len()
        );
        if let Some(progress) = progress.as_mut() {
            progress.finish(format!(
                "{}/{} items, {} series fetched",
                library_items.len(),
                library_items.len(),
                fetched_series
            ));
        }
        by_media_id
    }
}

pub async fn run_prune(
    cfg: &Config,
    db: &Database,
    report_path: &Path,
    apply: bool,
    include_legacy_anime_roots: bool,
    max_delete: Option<usize>,
    confirmation_token: Option<&str>,
) -> Result<PruneOutcome> {
    if apply {
        ensure_runtime_sources_healthy(&cfg.sources, "cleanup prune apply").await?;
    }

    let json = std::fs::read_to_string(report_path)?;
    let mut report: CleanupReport = serde_json::from_str(&json)?;
    hydrate_report_db_tracked_flags(db, &mut report).await?;
    let tracked_paths: HashSet<_> = report
        .findings
        .iter()
        .filter(|finding| finding.db_tracked)
        .map(|finding| finding.symlink_path.clone())
        .collect();
    let plan = build_prune_plan(
        &report,
        cfg.cleanup.prune.quarantine_foreign,
        include_legacy_anime_roots,
    );

    info!(
        "Cleanup prune: {} high/critical + {} safe duplicate candidates ({} total unique; {} managed delete / {} foreign quarantine)",
        plan.high_or_critical_candidates,
        plan.safe_warning_duplicate_candidates,
        plan.candidate_paths.len(),
        plan.managed_candidates,
        plan.foreign_candidates,
    );

    if !apply {
        return Ok(PruneOutcome {
            candidates: plan.candidate_paths.len(),
            blocked_candidates: plan.blocked_candidates,
            high_or_critical_candidates: plan.high_or_critical_candidates,
            safe_warning_duplicate_candidates: plan.safe_warning_duplicate_candidates,
            legacy_anime_root_candidates: plan.legacy_anime_root_candidates,
            legacy_anime_root_groups: plan.legacy_anime_root_groups.clone(),
            managed_candidates: plan.managed_candidates,
            foreign_candidates: plan.foreign_candidates,
            reason_counts: plan.reason_counts.clone(),
            blocked_reason_summary: plan.blocked_reason_summary.clone(),
            removed: 0,
            quarantined: 0,
            skipped: 0,
            confirmation_token: plan.confirmation_token,
            affected_paths: plan.candidate_paths,
        });
    }

    if let Some(applied_at) = report.applied_at {
        anyhow::bail!(
            "Refusing prune apply: this report was already applied at {}",
            applied_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    if plan.candidate_paths.is_empty() {
        if let Some(blocked) = plan.blocked_reason_summary.first() {
            anyhow::bail!(
                "Refusing prune apply: no actionable candidates remain; {} path(s) are blocked by current policy ({})",
                blocked.candidates,
                blocked.code
            );
        }
        anyhow::bail!("Refusing prune apply: report contains no actionable prune candidates");
    }

    let delete_cap = max_delete.unwrap_or(cfg.cleanup.prune.default_max_delete);
    if cfg.cleanup.prune.enforce_policy {
        if report.version != 1 {
            anyhow::bail!(
                "Refusing prune apply: unsupported report version {} (expected 1)",
                report.version
            );
        }

        let age = Utc::now().signed_duration_since(report.created_at);
        if age.num_hours() > cfg.cleanup.prune.max_report_age_hours as i64 {
            anyhow::bail!(
                "Refusing prune apply: report is too old ({}h > max {}h)",
                age.num_hours(),
                cfg.cleanup.prune.max_report_age_hours
            );
        }

        let provided = confirmation_token.unwrap_or("");
        if provided.is_empty() || provided != plan.confirmation_token {
            anyhow::bail!(
                "Refusing prune apply: invalid or missing confirmation token. Re-run preview and pass --confirm-token {}",
                plan.confirmation_token
            );
        }
    }

    if plan.candidate_paths.len() > delete_cap {
        anyhow::bail!(
            "Refusing prune apply: {} candidates exceeds delete cap {} (use --max-delete to override)",
            plan.candidate_paths.len(),
            delete_cap
        );
    }

    if cfg.backup.enabled {
        let backup = BackupManager::new(&cfg.backup);
        let extra_snapshot_paths: Vec<_> = plan
            .candidate_paths
            .iter()
            .filter(|path| !tracked_paths.contains(*path))
            .cloned()
            .collect();
        backup
            .create_safety_snapshot_with_extras(db, "cleanup-prune", &extra_snapshot_paths)
            .await?;
    }

    let mut removed = 0usize;
    let mut quarantined = 0usize;
    let mut skipped = 0usize;

    for symlink_path in &plan.candidate_paths {
        if cfg.security.enforce_roots && !path_is_within_roots(symlink_path, &library_roots(cfg)) {
            warn!(
                "Cleanup prune: skipping {:?} (outside configured library roots)",
                symlink_path
            );
            let _ = db
                .record_link_event_fields(
                    "prune_skipped",
                    symlink_path,
                    None,
                    None,
                    Some("outside_library_roots"),
                )
                .await;
            skipped += 1;
            continue;
        }

        match std::fs::symlink_metadata(symlink_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    match plan.dispositions.get(symlink_path).copied() {
                        Some(PruneDisposition::Delete) => {
                            if let Err(e) = std::fs::remove_file(symlink_path) {
                                warn!("Cleanup prune: failed removing {:?}: {}", symlink_path, e);
                                let _ = db
                                    .record_link_event_fields(
                                        "prune_skipped",
                                        symlink_path,
                                        None,
                                        None,
                                        Some("delete_failed"),
                                    )
                                    .await;
                                skipped += 1;
                            } else {
                                removed += 1;
                                if let Err(e) = db.mark_removed_path(symlink_path).await {
                                    warn!(
                                        "Cleanup prune: removed {:?} but failed DB mark_removed: {}",
                                        symlink_path, e
                                    );
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_removed",
                                            symlink_path,
                                            None,
                                            None,
                                            Some("db_mark_removed_failed"),
                                        )
                                        .await;
                                } else {
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_removed",
                                            symlink_path,
                                            None,
                                            None,
                                            None,
                                        )
                                        .await;
                                }
                            }
                        }
                        Some(PruneDisposition::Quarantine) => {
                            if !cfg.cleanup.prune.quarantine_foreign {
                                let _ = db
                                    .record_link_event_fields(
                                        "prune_skipped",
                                        symlink_path,
                                        None,
                                        None,
                                        Some("foreign_quarantine_disabled"),
                                    )
                                    .await;
                                skipped += 1;
                                continue;
                            }

                            match quarantine_symlink_for_cleanup(cfg, symlink_path) {
                                Ok(destination) => {
                                    let note = format!("quarantined_to={}", destination.display());
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_quarantined",
                                            symlink_path,
                                            None,
                                            None,
                                            Some(&note),
                                        )
                                        .await;
                                    quarantined += 1;
                                }
                                Err(e) => {
                                    warn!(
                                        "Cleanup prune: failed quarantining {:?}: {}",
                                        symlink_path, e
                                    );
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_skipped",
                                            symlink_path,
                                            None,
                                            None,
                                            Some("quarantine_failed"),
                                        )
                                        .await;
                                    skipped += 1;
                                }
                            }
                        }
                        None => {
                            let _ = db
                                .record_link_event_fields(
                                    "prune_skipped",
                                    symlink_path,
                                    None,
                                    None,
                                    Some("missing_prune_disposition"),
                                )
                                .await;
                            skipped += 1;
                        }
                    }
                } else {
                    warn!("Cleanup prune: skipping {:?} (not a symlink)", symlink_path);
                    let _ = db
                        .record_link_event_fields(
                            "prune_skipped",
                            symlink_path,
                            None,
                            None,
                            Some("not_symlink"),
                        )
                        .await;
                    skipped += 1;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _ = db
                    .record_link_event_fields(
                        "prune_skipped",
                        symlink_path,
                        None,
                        None,
                        Some("not_found"),
                    )
                    .await;
                skipped += 1;
            }
            Err(e) => {
                warn!("Cleanup prune: could not inspect {:?}: {}", symlink_path, e);
                let _ = db
                    .record_link_event_fields(
                        "prune_skipped",
                        symlink_path,
                        None,
                        None,
                        Some("metadata_failed"),
                    )
                    .await;
                skipped += 1;
            }
        }
    }

    // Stamp applied_at and write back so this report cannot be reused
    report.applied_at = Some(Utc::now());
    if let Ok(updated_json) = serde_json::to_string_pretty(&report) {
        if let Err(e) = std::fs::write(report_path, &updated_json) {
            warn!(
                "Cleanup prune: applied successfully but failed to stamp report: {}",
                e
            );
        }
    }

    Ok(PruneOutcome {
        candidates: plan.candidate_paths.len(),
        blocked_candidates: plan.blocked_candidates,
        high_or_critical_candidates: plan.high_or_critical_candidates,
        safe_warning_duplicate_candidates: plan.safe_warning_duplicate_candidates,
        legacy_anime_root_candidates: plan.legacy_anime_root_candidates,
        legacy_anime_root_groups: plan.legacy_anime_root_groups,
        managed_candidates: plan.managed_candidates,
        foreign_candidates: plan.foreign_candidates,
        reason_counts: plan.reason_counts,
        blocked_reason_summary: plan.blocked_reason_summary,
        removed,
        quarantined,
        skipped,
        confirmation_token: plan.confirmation_token,
        affected_paths: plan.candidate_paths,
    })
}

pub(crate) fn quarantine_symlink_for_cleanup(cfg: &Config, symlink_path: &Path) -> Result<PathBuf> {
    let target = std::fs::read_link(symlink_path)?;
    let resolved_target = resolve_link_target(symlink_path, &target);
    let destination = quarantine_destination(cfg, symlink_path);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if let Err(err) = std::fs::rename(symlink_path, &destination) {
        if err.raw_os_error() != Some(libc::EXDEV) {
            return Err(err.into());
        }
    } else {
        return Ok(destination);
    }

    let staging_path = quarantine_staging_path(symlink_path);
    std::fs::rename(symlink_path, &staging_path)?;

    if let Err(create_err) = create_symlink_like(&resolved_target, &destination) {
        let rollback_result = std::fs::rename(&staging_path, symlink_path);
        return match rollback_result {
            Ok(()) => Err(anyhow::anyhow!(
                "failed to stage quarantined symlink at {}: {}",
                destination.display(),
                create_err
            )),
            Err(rollback_err) => Err(anyhow::anyhow!(
                "failed to stage quarantined symlink at {}: {}; rollback also failed for {}: {}",
                destination.display(),
                create_err,
                symlink_path.display(),
                rollback_err
            )),
        };
    }

    if let Err(remove_err) = std::fs::remove_file(&staging_path) {
        return Err(anyhow::anyhow!(
            "quarantine staging cleanup failed for {}: {}",
            staging_path.display(),
            remove_err
        ));
    }

    Ok(destination)
}

fn quarantine_destination(cfg: &Config, symlink_path: &Path) -> PathBuf {
    let quarantine_root = if cfg.cleanup.prune.quarantine_path.is_absolute() {
        cfg.cleanup.prune.quarantine_path.clone()
    } else {
        cfg.backup.path.join(&cfg.cleanup.prune.quarantine_path)
    };

    let relative = library_roots(cfg)
        .into_iter()
        .find_map(|root| {
            symlink_path.strip_prefix(&root).ok().map(|rel| {
                let label = root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or("library");
                PathBuf::from(label).join(rel)
            })
        })
        .unwrap_or_else(|| {
            symlink_path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("unknown.symlink"))
        });

    unique_quarantine_path(quarantine_root.join(relative))
}

fn unique_quarantine_path(initial: PathBuf) -> PathBuf {
    if !initial.exists() {
        return initial;
    }

    let parent = initial
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = initial
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("quarantine");
    let ext = initial.extension().and_then(|e| e.to_str());

    for idx in 1.. {
        let filename = match ext {
            Some(ext) => format!("{stem}.quarantine-{idx}.{ext}"),
            None => format!("{stem}.quarantine-{idx}"),
        };
        let candidate = parent.join(filename);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("quarantine path search should always terminate")
}

fn quarantine_staging_path(original: &Path) -> PathBuf {
    let parent = original
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = original
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("quarantine-staging");

    for idx in 0.. {
        let candidate = parent.join(format!(".{name}.symlinkarr-quarantine-{idx}.tmp"));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("staging path search should always terminate")
}

#[cfg(unix)]
fn create_symlink_like(target: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, destination)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink_like(target: &Path, destination: &Path) -> Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

fn prune_confirmation_token(
    report: &CleanupReport,
    candidate_paths: &[PathBuf],
    dispositions: &HashMap<PathBuf, PruneDisposition>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    report.version.hash(&mut hasher);
    report.created_at.timestamp().hash(&mut hasher);
    report.scope.hash(&mut hasher);
    for path in candidate_paths {
        path.hash(&mut hasher);
        dispositions.get(path).hash(&mut hasher);
    }

    format!("{:016x}", hasher.finish())
}

fn library_roots(cfg: &Config) -> Vec<PathBuf> {
    cfg.libraries.iter().map(|l| l.path.clone()).collect()
}

fn path_is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    if roots.is_empty() {
        return false;
    }

    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };

    roots.iter().any(|root| {
        let normalized_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        abs.starts_with(normalized_root)
    })
}

fn resolve_link_target(symlink_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        symlink_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

fn build_legacy_anime_root_lookup(
    libraries: &[&LibraryConfig],
) -> HashMap<PathBuf, LegacyAnimeRootContext> {
    let anime_libraries: Vec<&LibraryConfig> = libraries
        .iter()
        .copied()
        .filter(|lib| effective_content_type(lib) == ContentType::Anime)
        .collect();

    let mut lookup = HashMap::new();
    for group in collect_anime_root_duplicate_groups(&anime_libraries) {
        for root in group.untagged_roots {
            lookup.insert(
                root.clone(),
                LegacyAnimeRootContext {
                    normalized_title: group.normalized_title.clone(),
                    untagged_root: root,
                    tagged_roots: group.tagged_roots.clone(),
                },
            );
        }
    }

    lookup
}

fn legacy_anime_root_context_for_path<'a>(
    symlink_path: &Path,
    library_root: &Path,
    legacy_anime_roots: &'a HashMap<PathBuf, LegacyAnimeRootContext>,
) -> Option<&'a LegacyAnimeRootContext> {
    let relative = symlink_path.strip_prefix(library_root).ok()?;
    let first_component = relative.components().next()?;
    let show_root = library_root.join(first_component.as_os_str());
    legacy_anime_roots.get(&show_root)
}

fn build_library_indices_by_path(library_items: &[LibraryItem]) -> HashMap<PathBuf, usize> {
    library_items
        .iter()
        .enumerate()
        .map(|(idx, item)| (item.path.clone(), idx))
        .collect()
}

fn build_library_indices_by_id(library_items: &[LibraryItem]) -> HashMap<String, usize> {
    library_items
        .iter()
        .enumerate()
        .map(|(idx, item)| (item.id.to_string(), idx))
        .collect()
}

fn find_owner_library_item<'a>(
    symlink_path: &Path,
    library_items: &'a [LibraryItem],
    library_indices_by_path: &HashMap<PathBuf, usize>,
) -> Option<&'a LibraryItem> {
    let mut current = symlink_path.parent();
    while let Some(path) = current {
        if let Some(idx) = library_indices_by_path.get(path) {
            return library_items.get(*idx);
        }
        current = path.parent();
    }

    None
}

fn build_aliases(item: &LibraryItem, metadata: Option<&Option<ContentMetadata>>) -> Vec<String> {
    let mut aliases = vec![normalize(&item.title)];

    if let Some(Some(meta)) = metadata {
        aliases.push(normalize(&meta.title));
        aliases.extend(meta.aliases.iter().map(|alias| normalize(alias)));
    }

    aliases.sort();
    aliases.dedup();
    aliases
}

fn build_aliases_by_index(
    library_items: &[LibraryItem],
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
) -> Vec<Vec<String>> {
    library_items
        .iter()
        .map(|item| build_aliases(item, metadata_map.get(&item.id.to_string())))
        .collect()
}

fn build_alias_token_index(alias_map_by_index: &[Vec<String>]) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, aliases) in alias_map_by_index.iter().enumerate() {
        let mut seen = HashSet::new();
        for alias in aliases {
            for token in title_lookup_tokens(alias) {
                if seen.insert(token.clone()) {
                    index.entry(token).or_default().push(idx);
                }
            }
        }
    }

    for indices in index.values_mut() {
        indices.sort_unstable();
        indices.dedup();
    }

    index
}

fn owner_title_matches(
    content_type: ContentType,
    aliases: &[String],
    normalized_parsed: &str,
) -> bool {
    if normalized_parsed.is_empty() {
        return true;
    }

    match content_type {
        ContentType::Anime => aliases
            .iter()
            .any(|alias| tokenized_title_match(alias, normalized_parsed)),
        ContentType::Tv | ContentType::Movie => {
            let parsed_variants = title_match_variants(normalized_parsed);
            aliases.iter().any(|alias| {
                let alias_variants = title_match_variants(alias);
                alias_variants.iter().any(|alias_variant| {
                    parsed_variants.iter().any(|parsed_variant| {
                        strict_owner_alias_match(alias_variant, parsed_variant)
                            || (content_type == ContentType::Tv
                                && tv_alias_with_embedded_episode_marker(
                                    alias_variant,
                                    parsed_variant,
                                ))
                    })
                })
            })
        }
    }
}

fn extract_title_year(title: &str) -> Option<u32> {
    fn parse_year(token: &str) -> Option<u32> {
        if token.len() != 4 {
            return None;
        }
        let year: u32 = token.parse().ok()?;
        (1900..=2099).contains(&year).then_some(year)
    }

    let mut digits = String::new();
    for ch in title.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if let Some(year) = parse_year(&digits) {
            return Some(year);
        }
        digits.clear();
    }

    parse_year(&digits)
}

fn candidate_release_year(item: &LibraryItem, metadata: Option<&ContentMetadata>) -> Option<u32> {
    metadata
        .and_then(|metadata| metadata.year)
        .or_else(|| extract_title_year(&item.title))
}

fn candidate_metadata_compatible(
    item: &LibraryItem,
    entry: &WorkingEntry,
    metadata: Option<&ContentMetadata>,
) -> bool {
    if let (Some(source_year), Some(candidate_year)) =
        (entry.year, candidate_release_year(item, metadata))
    {
        if source_year != candidate_year {
            return false;
        }
    }

    if item.media_type == MediaType::Tv && item.content_type == ContentType::Tv {
        if let (Some(source_season), Some(metadata)) = (entry.season, metadata) {
            let has_season_metadata = metadata
                .seasons
                .iter()
                .any(|season| !season.episodes.is_empty());
            if has_season_metadata
                && !metadata
                    .seasons
                    .iter()
                    .any(|season| season.season_number == source_season)
            {
                return false;
            }
        }
    }

    true
}

const MAX_ALTERNATE_MATCH_CANDIDATES: usize = 50;

fn title_lookup_tokens(title: &str) -> Vec<String> {
    title
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_string())
        .collect()
}

fn candidate_library_indices_for_title(
    normalized_title: &str,
    alias_token_index: &HashMap<String, Vec<usize>>,
) -> Vec<usize> {
    let tokens = title_lookup_tokens(normalized_title);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut overlap_counts: HashMap<usize, usize> = HashMap::new();
    for token in tokens {
        if let Some(indices) = alias_token_index.get(&token) {
            for idx in indices {
                *overlap_counts.entry(*idx).or_insert(0) += 1;
            }
        }
    }

    let mut ranked: Vec<(usize, usize)> = overlap_counts.into_iter().collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(MAX_ALTERNATE_MATCH_CANDIDATES);
    ranked.into_iter().map(|(idx, _)| idx).collect()
}

fn find_alternate_library_match(
    owner_idx: usize,
    entry: &WorkingEntry,
    normalized_parsed: &str,
    library_items: &[LibraryItem],
    alias_map_by_index: &[Vec<String>],
    alias_token_index: &HashMap<String, Vec<usize>>,
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
) -> Option<AlternateMatchContext> {
    if normalized_parsed.is_empty() {
        return None;
    }

    let parsed_variants = title_match_variants(normalized_parsed);
    let mut best: Option<(usize, f64)> = None;

    for idx in candidate_library_indices_for_title(normalized_parsed, alias_token_index) {
        if idx == owner_idx {
            continue;
        }

        let Some(candidate) = library_items.get(idx) else {
            continue;
        };
        if candidate.media_type != entry.media_type || candidate.content_type != entry.content_type
        {
            continue;
        }
        let candidate_metadata = metadata_map
            .get(&candidate.id.to_string())
            .and_then(|meta| meta.as_ref());
        if !candidate_metadata_compatible(candidate, entry, candidate_metadata) {
            continue;
        }

        let aliases = alias_map_by_index
            .get(idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let Some(score) = best_variant_alias_score(aliases, &parsed_variants) else {
            continue;
        };
        if score < 0.70 {
            continue;
        }

        match best {
            None => best = Some((idx, score)),
            Some((best_idx, best_score)) => {
                let replace = score > best_score
                    || (score == best_score
                        && candidate.title.len() > library_items[best_idx].title.len())
                    || (score == best_score
                        && candidate.title.len() == library_items[best_idx].title.len()
                        && candidate.title < library_items[best_idx].title);
                if replace {
                    best = Some((idx, score));
                }
            }
        }
    }

    best.and_then(|(idx, score)| {
        library_items.get(idx).map(|item| AlternateMatchContext {
            media_id: item.id.to_string(),
            title: item.title.clone(),
            score,
        })
    })
}

fn best_variant_alias_score(aliases: &[String], parsed_variants: &[String]) -> Option<f64> {
    let mut best: Option<f64> = None;

    for alias in aliases {
        for alias_variant in title_match_variants(alias) {
            for parsed_variant in parsed_variants {
                let Some((score, _)) = best_alias_score(
                    crate::config::MatchingMode::Strict,
                    std::slice::from_ref(&alias_variant),
                    parsed_variant,
                ) else {
                    continue;
                };
                if best.is_none_or(|current| score > current) {
                    best = Some(score);
                }
            }
        }
    }

    best
}

fn strict_owner_alias_match(alias: &str, normalized_parsed: &str) -> bool {
    if alias.is_empty() || normalized_parsed.is_empty() {
        return false;
    }

    let alias = alias.trim();
    let normalized_parsed = normalized_parsed.trim();
    if alias.is_empty() || normalized_parsed.is_empty() {
        return false;
    }

    best_alias_score(
        crate::config::MatchingMode::Strict,
        &[alias.to_string()],
        normalized_parsed,
    )
    .is_some()
}

fn tv_alias_with_embedded_episode_marker(alias: &str, normalized_parsed: &str) -> bool {
    if alias.is_empty() || normalized_parsed.is_empty() {
        return false;
    }

    let alias = alias.trim();
    let normalized_parsed = normalized_parsed.trim();
    let Some(rest) = normalized_parsed.strip_prefix(alias) else {
        return false;
    };
    if !rest.starts_with(' ') {
        return false;
    }

    let Some(marker) = rest.split_whitespace().next() else {
        return false;
    };

    is_episode_marker_token(marker)
}

fn is_episode_marker_token(token: &str) -> bool {
    let token = token.trim();
    if token.is_empty() {
        return false;
    }

    if let Some(rest) = token.strip_prefix('s') {
        return is_numeric_episode_pair(rest, 'e') || is_numeric_episode_pair(rest, 'x');
    }

    is_numeric_episode_pair(token, 'x')
}

fn is_numeric_episode_pair(value: &str, separator: char) -> bool {
    let Some((left, right)) = value.split_once(separator) else {
        return false;
    };

    !left.is_empty()
        && !right.is_empty()
        && left.chars().all(|c| c.is_ascii_digit())
        && right.chars().all(|c| c.is_ascii_digit())
}

fn strip_leading_article(value: &str) -> String {
    for article in ["the ", "a ", "an "] {
        if let Some(stripped) = value.strip_prefix(article) {
            return stripped.trim().to_string();
        }
    }

    value.trim().to_string()
}

fn strip_trailing_year(value: &str) -> String {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    if tokens.is_empty() {
        return String::new();
    }

    let mut end = tokens.len();
    while end > 0 {
        let token = tokens[end - 1];
        let Some(year) = token.parse::<u32>().ok() else {
            break;
        };
        if !(1900..=2099).contains(&year) {
            break;
        }
        end -= 1;
    }

    if end == 0 {
        return value.trim().to_string();
    }

    tokens[..end].join(" ")
}

fn title_match_variants(value: &str) -> Vec<String> {
    let base = value.trim();
    if base.is_empty() {
        return Vec::new();
    }

    let mut variants = vec![base.to_string()];
    let no_article = strip_leading_article(base);
    if !no_article.is_empty() {
        variants.push(no_article.clone());
    }

    let no_year = strip_trailing_year(base);
    if !no_year.is_empty() {
        variants.push(no_year.clone());
    }

    let no_article_no_year = strip_trailing_year(&no_article);
    if !no_article_no_year.is_empty() {
        variants.push(no_article_no_year);
    }

    variants.sort();
    variants.dedup();
    variants
}

fn apply_duplicate_and_count_signals(
    entries: &mut [WorkingEntry],
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
    arr_map: &HashMap<String, ArrSeriesSnapshot>,
) {
    let mut slot_indices: HashMap<(String, u32, u32), Vec<usize>> = HashMap::new();
    let mut season_indices: HashMap<(String, u32), Vec<usize>> = HashMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        if entry.media_id.is_empty() {
            continue;
        }

        if let (Some(season), Some(episode)) = (entry.season, entry.episode) {
            slot_indices
                .entry((entry.media_id.clone(), season, episode))
                .or_default()
                .push(idx);
            season_indices
                .entry((entry.media_id.clone(), season))
                .or_default()
                .push(idx);
        }
    }

    for idxs in slot_indices.values().filter(|v| v.len() > 1) {
        for idx in idxs {
            entries[*idx]
                .reasons
                .insert(FindingReason::DuplicateEpisodeSlot);
        }
    }

    for ((media_id, season), idxs) in &season_indices {
        // Season 0 (specials) is too provider-specific for robust count-anomaly checks.
        if *season == 0 {
            continue;
        }

        let actual = idxs.len();
        let expected = expected_count_for_season(media_id, *season, metadata_map, arr_map);

        let Some(expected) = expected else {
            continue;
        };

        if expected == 0 {
            continue;
        }

        if is_season_count_anomaly(actual, expected) {
            debug!(
                "Season count anomaly media={} season={} actual={} expected={}",
                media_id, season, actual, expected
            );
            for idx in idxs {
                entries[*idx]
                    .reasons
                    .insert(FindingReason::SeasonCountAnomaly);
            }
        }
    }
}

fn suppress_redundant_season_count_warnings(entries: &mut [WorkingEntry]) -> usize {
    let mut seasons_with_stronger_signals: HashSet<(String, u32)> = HashSet::new();

    for entry in entries.iter() {
        let Some(season) = entry.season else {
            continue;
        };
        if entry.media_id.is_empty() {
            continue;
        }

        if matches!(
            classify_severity(&entry.reasons),
            FindingSeverity::Critical | FindingSeverity::High
        ) {
            seasons_with_stronger_signals.insert((entry.media_id.clone(), season));
        }
    }

    let mut suppressed = 0usize;

    for entry in entries.iter_mut() {
        let Some(season) = entry.season else {
            continue;
        };
        if entry.media_id.is_empty() {
            continue;
        }

        if is_warning_only_season_count(&entry.reasons)
            && seasons_with_stronger_signals.contains(&(entry.media_id.clone(), season))
        {
            entry.reasons.remove(&FindingReason::SeasonCountAnomaly);
            suppressed += 1;
        }
    }

    suppressed
}

fn is_warning_only_season_count(reasons: &BTreeSet<FindingReason>) -> bool {
    reasons.len() == 1 && reasons.contains(&FindingReason::SeasonCountAnomaly)
}

fn is_safe_duplicate_candidate(reasons: &[FindingReason]) -> bool {
    reasons.contains(&FindingReason::DuplicateEpisodeSlot)
        && reasons.iter().all(|reason| {
            matches!(
                reason,
                FindingReason::DuplicateEpisodeSlot | FindingReason::SeasonCountAnomaly
            )
        })
}

fn finding_slot_key(finding: &CleanupFinding) -> Option<(String, u32, u32)> {
    let season = finding.parsed.season?;
    let episode = finding.parsed.episode?;
    Some((finding.media_id.clone(), season, episode))
}

async fn hydrate_db_tracked_flags(
    db: &Database,
    findings: &mut [CleanupFinding],
) -> Result<HashSet<PathBuf>> {
    let target_paths: Vec<_> = findings.iter().map(|f| f.symlink_path.clone()).collect();
    let tracked_paths: HashSet<_> = db
        .get_links_by_targets(&target_paths)
        .await?
        .into_iter()
        .filter(|link| link.status == LinkStatus::Active)
        .map(|link| link.target_path)
        .collect();

    for finding in findings {
        finding.db_tracked = tracked_paths.contains(&finding.symlink_path);
        finding.ownership = if finding.db_tracked {
            CleanupOwnership::Managed
        } else {
            CleanupOwnership::Foreign
        };
    }

    Ok(tracked_paths)
}

pub async fn hydrate_report_db_tracked_flags(
    db: &Database,
    report: &mut CleanupReport,
) -> Result<()> {
    let _ = hydrate_db_tracked_flags(db, &mut report.findings).await?;
    Ok(())
}

pub(crate) fn collect_safe_duplicate_prune_plan(
    findings: &[CleanupFinding],
) -> SafeDuplicatePrunePlan {
    let mut tainted_slots: HashSet<(String, u32, u32)> = HashSet::new();
    let mut managed_paths = HashSet::new();
    let mut by_slot: HashMap<(String, u32, u32), Vec<&CleanupFinding>> = HashMap::new();
    let mut blocked_reason_counts: HashMap<PruneBlockedReasonCode, usize> = HashMap::new();
    let mut blocked_by_path: HashMap<PathBuf, PruneBlockedReasonCode> = HashMap::new();

    for finding in findings {
        let Some(slot_key) = finding_slot_key(finding) else {
            continue;
        };

        by_slot.entry(slot_key.clone()).or_default().push(finding);

        if is_safe_duplicate_candidate(&finding.reasons) {
            managed_paths.insert(finding.symlink_path.clone());
        } else {
            tainted_slots.insert(slot_key);
        }
    }

    let mut prune_paths = Vec::new();
    for (slot_key, findings) in by_slot {
        let safe_findings: Vec<_> = findings
            .into_iter()
            .filter(|finding| is_safe_duplicate_candidate(&finding.reasons))
            .collect();
        if safe_findings.len() < 2 {
            continue;
        }

        let mut tracked_paths: Vec<_> = safe_findings
            .iter()
            .filter(|finding| finding.db_tracked)
            .map(|finding| finding.symlink_path.clone())
            .collect();
        tracked_paths.sort();
        tracked_paths.dedup();

        let mut untracked_paths: Vec<_> = safe_findings
            .iter()
            .filter(|finding| !finding.db_tracked)
            .map(|finding| finding.symlink_path.clone())
            .collect();
        untracked_paths.sort();
        untracked_paths.dedup();
        if untracked_paths.is_empty() {
            continue;
        }

        if tainted_slots.contains(&slot_key) {
            for path in &untracked_paths {
                blocked_by_path.insert(path.clone(), PruneBlockedReasonCode::DuplicateSlotTainted);
            }
            *blocked_reason_counts
                .entry(PruneBlockedReasonCode::DuplicateSlotTainted)
                .or_insert(0) += untracked_paths.len();
            continue;
        }

        let unique_sources: HashSet<_> = safe_findings
            .iter()
            .map(|finding| finding.source_path.clone())
            .collect();
        if unique_sources.len() > 1 {
            for path in &untracked_paths {
                blocked_by_path.insert(
                    path.clone(),
                    PruneBlockedReasonCode::DuplicateSlotSourceMismatch,
                );
            }
            *blocked_reason_counts
                .entry(PruneBlockedReasonCode::DuplicateSlotSourceMismatch)
                .or_insert(0) += untracked_paths.len();
            continue;
        }

        if tracked_paths.is_empty() {
            for path in &untracked_paths {
                blocked_by_path.insert(
                    path.clone(),
                    PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor,
                );
            }
            *blocked_reason_counts
                .entry(PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor)
                .or_insert(0) += untracked_paths.len();
            continue;
        }

        if tracked_paths.len() > 1 {
            for path in &untracked_paths {
                blocked_by_path.insert(
                    path.clone(),
                    PruneBlockedReasonCode::DuplicateSlotMultipleTrackedAnchors,
                );
            }
            *blocked_reason_counts
                .entry(PruneBlockedReasonCode::DuplicateSlotMultipleTrackedAnchors)
                .or_insert(0) += untracked_paths.len();
            continue;
        }

        prune_paths.extend(untracked_paths);
    }

    prune_paths.sort();
    prune_paths.dedup();

    SafeDuplicatePrunePlan {
        prune_paths,
        managed_paths,
        blocked_reason_counts,
        blocked_by_path,
    }
}

pub(crate) fn build_prune_plan(
    report: &CleanupReport,
    quarantine_foreign: bool,
    include_legacy_anime_roots: bool,
) -> PrunePlan {
    let safe_duplicate_plan = collect_safe_duplicate_prune_plan(&report.findings);
    let mut blocked_reason_counts = safe_duplicate_plan.blocked_reason_counts.clone();
    let mut blocked_by_path = safe_duplicate_plan.blocked_by_path.clone();
    let legacy_anime_root_candidates: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| !finding.db_tracked)
        .filter(|finding| {
            include_legacy_anime_roots
                && finding
                    .reasons
                    .contains(&FindingReason::LegacyAnimeRootDuplicate)
        })
        .collect();
    let high_or_critical_candidates: Vec<_> = report
        .findings
        .iter()
        .filter(|f| {
            matches!(
                f.severity,
                FindingSeverity::Critical | FindingSeverity::High
            )
        })
        .filter(|f| !safe_duplicate_plan.managed_paths.contains(&f.symlink_path))
        .collect();

    if !include_legacy_anime_roots {
        let blocked_legacy_count = report
            .findings
            .iter()
            .filter(|finding| !finding.db_tracked)
            .filter(|finding| {
                finding
                    .reasons
                    .contains(&FindingReason::LegacyAnimeRootDuplicate)
            })
            .count();
        if blocked_legacy_count > 0 {
            for finding in report
                .findings
                .iter()
                .filter(|finding| !finding.db_tracked)
                .filter(|finding| {
                    finding
                        .reasons
                        .contains(&FindingReason::LegacyAnimeRootDuplicate)
                })
            {
                blocked_by_path.insert(
                    finding.symlink_path.clone(),
                    PruneBlockedReasonCode::LegacyAnimeRootsExcludedByDefault,
                );
            }
            *blocked_reason_counts
                .entry(PruneBlockedReasonCode::LegacyAnimeRootsExcludedByDefault)
                .or_insert(0) += blocked_legacy_count;
        }
    }

    let ownership_by_path: HashMap<_, _> = report
        .findings
        .iter()
        .map(|finding| (finding.symlink_path.clone(), finding.ownership))
        .collect();

    let mut candidate_paths: Vec<PathBuf> = high_or_critical_candidates
        .iter()
        .map(|f| f.symlink_path.clone())
        .collect();
    candidate_paths.extend(safe_duplicate_plan.prune_paths.iter().cloned());
    candidate_paths.extend(
        legacy_anime_root_candidates
            .iter()
            .map(|finding| finding.symlink_path.clone()),
    );
    candidate_paths.sort();
    candidate_paths.dedup();

    let mut dispositions = HashMap::new();
    let mut managed_candidates = 0usize;
    let mut foreign_candidates = 0usize;
    for path in &candidate_paths {
        let ownership = ownership_by_path
            .get(path)
            .copied()
            .unwrap_or(CleanupOwnership::Foreign);
        let disposition = match ownership {
            CleanupOwnership::Managed => {
                managed_candidates += 1;
                Some(PruneDisposition::Delete)
            }
            CleanupOwnership::Foreign if quarantine_foreign => {
                foreign_candidates += 1;
                Some(PruneDisposition::Quarantine)
            }
            CleanupOwnership::Foreign => {
                blocked_by_path.insert(
                    path.clone(),
                    PruneBlockedReasonCode::ForeignQuarantineDisabled,
                );
                *blocked_reason_counts
                    .entry(PruneBlockedReasonCode::ForeignQuarantineDisabled)
                    .or_insert(0) += 1;
                None
            }
        };
        if let Some(disposition) = disposition {
            dispositions.insert(path.clone(), disposition);
        }
    }

    candidate_paths.retain(|path| dispositions.contains_key(path));

    let candidate_set: HashSet<_> = candidate_paths.iter().cloned().collect();
    let mut reason_counts: HashMap<FindingReason, PruneReasonCount> = HashMap::new();
    let mut legacy_anime_root_groups: HashMap<String, LegacyAnimeRootGroupCount> = HashMap::new();
    for finding in &report.findings {
        if !candidate_set.contains(&finding.symlink_path) {
            continue;
        }

        let ownership = ownership_by_path
            .get(&finding.symlink_path)
            .copied()
            .unwrap_or(CleanupOwnership::Foreign);
        for reason in &finding.reasons {
            let entry = reason_counts.entry(*reason).or_insert(PruneReasonCount {
                reason: *reason,
                total: 0,
                managed: 0,
                foreign: 0,
            });
            entry.total += 1;
            match ownership {
                CleanupOwnership::Managed => entry.managed += 1,
                CleanupOwnership::Foreign => entry.foreign += 1,
            }
        }

        if let Some(legacy) = &finding.legacy_anime_root {
            let entry = legacy_anime_root_groups
                .entry(legacy.normalized_title.clone())
                .or_insert(LegacyAnimeRootGroupCount {
                    normalized_title: legacy.normalized_title.clone(),
                    total: 0,
                    tagged_roots: legacy.tagged_roots.clone(),
                });
            entry.total += 1;
        }
    }

    let mut reason_counts: Vec<_> = reason_counts.into_values().collect();
    reason_counts.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.reason.cmp(&b.reason)));
    let mut legacy_anime_root_groups: Vec<_> = legacy_anime_root_groups.into_values().collect();
    legacy_anime_root_groups.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| a.normalized_title.cmp(&b.normalized_title))
    });
    let mut blocked_reason_summary: Vec<_> = blocked_reason_counts
        .into_iter()
        .filter(|(_, candidates)| *candidates > 0)
        .map(|(code, candidates)| PruneBlockedReasonSummary {
            code,
            label: code.label().to_string(),
            candidates,
            recommended_action: code.recommended_action().to_string(),
        })
        .collect();
    blocked_reason_summary.sort_by(|a, b| {
        b.candidates
            .cmp(&a.candidates)
            .then_with(|| a.label.cmp(&b.label))
    });
    let blocked_candidates = blocked_reason_summary
        .iter()
        .map(|item| item.candidates)
        .sum();

    PrunePlan {
        confirmation_token: prune_confirmation_token(report, &candidate_paths, &dispositions),
        candidate_paths,
        blocked_candidates,
        high_or_critical_candidates: high_or_critical_candidates.len(),
        safe_warning_duplicate_candidates: safe_duplicate_plan.prune_paths.len(),
        legacy_anime_root_candidates: legacy_anime_root_candidates.len(),
        legacy_anime_root_groups,
        managed_candidates,
        foreign_candidates,
        reason_counts,
        blocked_reason_summary,
        dispositions,
        blocked_by_path,
    }
}

const SEASON_COUNT_ANOMALY_RATIO_THRESHOLD: f64 = 1.2;
const SEASON_COUNT_ANOMALY_EXCESS_RATIO: f64 = 0.15;
const SEASON_COUNT_ANOMALY_MIN_EXCESS: usize = 2;

fn is_season_count_anomaly(actual: usize, expected: usize) -> bool {
    // Count anomalies are only relevant for excess links in this season slot.
    // Lower-than-expected counts are common for partial libraries and should not flag.
    if expected == 0 || actual <= expected {
        return false;
    }

    let ratio = actual as f64 / expected as f64;
    if ratio < SEASON_COUNT_ANOMALY_RATIO_THRESHOLD {
        return false;
    }

    let excess = actual - expected;
    let ratio_min_excess = (expected as f64 * SEASON_COUNT_ANOMALY_EXCESS_RATIO).ceil() as usize;
    let min_excess = ratio_min_excess.max(SEASON_COUNT_ANOMALY_MIN_EXCESS);

    excess >= min_excess
}

fn expected_count_for_season(
    media_id: &str,
    season: u32,
    metadata_map: &HashMap<String, Option<ContentMetadata>>,
    arr_map: &HashMap<String, ArrSeriesSnapshot>,
) -> Option<usize> {
    if let Some(arr) = arr_map.get(media_id) {
        if let Some(count) = arr.season_counts.get(&season) {
            return Some(*count);
        }
    }

    metadata_map
        .get(media_id)
        .and_then(|meta| meta.as_ref())
        .and_then(|meta| {
            meta.seasons
                .iter()
                .find(|s| s.season_number == season)
                .map(|s| s.episodes.len())
        })
}

fn classify_severity(reasons: &BTreeSet<FindingReason>) -> FindingSeverity {
    if reasons.contains(&FindingReason::BrokenSource)
        || reasons.contains(&FindingReason::MovieEpisodeSource)
        || reasons.contains(&FindingReason::EpisodeOutOfRange)
        || (reasons.contains(&FindingReason::AlternateLibraryMatch)
            && reasons.contains(&FindingReason::ParserTitleMismatch))
        || (reasons.contains(&FindingReason::ArrUntracked)
            && reasons.contains(&FindingReason::ParserTitleMismatch))
    {
        return FindingSeverity::Critical;
    }

    if reasons.contains(&FindingReason::NonRdSourcePath)
        || reasons.contains(&FindingReason::ArrUntracked)
        || reasons.contains(&FindingReason::AlternateLibraryMatch)
        || reasons.contains(&FindingReason::ParserTitleMismatch)
        || (reasons.contains(&FindingReason::DuplicateEpisodeSlot) && reasons.len() > 1)
    {
        return FindingSeverity::High;
    }

    FindingSeverity::Warning
}

fn classify_confidence(reasons: &BTreeSet<FindingReason>) -> f64 {
    let mut score = 0.0;

    for reason in reasons {
        let weight = match reason {
            FindingReason::BrokenSource => 1.0,
            FindingReason::LegacyAnimeRootDuplicate => 0.55,
            FindingReason::AlternateLibraryMatch => 0.98,
            FindingReason::MovieEpisodeSource => 0.95,
            FindingReason::EpisodeOutOfRange => 0.9,
            FindingReason::NonRdSourcePath => 0.8,
            FindingReason::ArrUntracked => 0.7,
            FindingReason::DuplicateEpisodeSlot => 0.65,
            FindingReason::ParserTitleMismatch => 0.6,
            FindingReason::SeasonCountAnomaly => 0.4,
        };
        if weight > score {
            score = weight;
        }
    }

    score
}

fn episode_out_of_range(meta: &ContentMetadata, season: u32, episode: u32) -> bool {
    let Some(season_info) = meta.seasons.iter().find(|s| s.season_number == season) else {
        // Many providers omit/reshape specials; treat unknown S00 as "unknown" instead of hard error.
        return season != 0;
    };

    if season_info.episodes.is_empty() {
        return false;
    }

    let max_episode = season_info
        .episodes
        .iter()
        .map(|e| e.episode_number)
        .max()
        .unwrap_or(0);

    episode == 0 || episode > max_episode
}

fn tokenized_title_match(alias: &str, parsed: &str) -> bool {
    if alias == parsed {
        return true;
    }

    token_window_contains(parsed, alias) || token_window_contains(alias, parsed)
}

fn token_window_contains(haystack: &str, needle: &str) -> bool {
    if haystack.is_empty() || needle.is_empty() {
        return false;
    }

    let hay_tokens: Vec<_> = haystack.split_whitespace().collect();
    let needle_tokens: Vec<_> = needle.split_whitespace().collect();

    if needle_tokens.len() > hay_tokens.len() {
        return false;
    }

    hay_tokens
        .windows(needle_tokens.len())
        .any(|window| window == needle_tokens)
}

fn effective_content_type(lib: &LibraryConfig) -> ContentType {
    lib.content_type
        .unwrap_or(ContentType::from_media_type(lib.media_type))
}

fn default_report_path(scope: CleanupScope) -> PathBuf {
    let scope_name = match scope {
        CleanupScope::Anime => "anime",
        CleanupScope::Tv => "tv",
        CleanupScope::Movie => "movie",
        CleanupScope::All => "all",
    };

    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    PathBuf::from(format!("backups/cleanup-audit-{}-{}.json", scope_name, ts))
}

#[cfg(test)]
mod tests;
