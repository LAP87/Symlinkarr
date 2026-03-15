use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::api::sonarr::SonarrClient;
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::backup::BackupManager;
use crate::config::{Config, ContentType, LibraryConfig, MetadataMode};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::models::{ContentMetadata, LibraryItem, MediaId, MediaType};
use crate::source_scanner::SourceScanner;
use crate::utils::{normalize, ProgressLine};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CleanupScope {
    Anime,
}

impl CleanupScope {
    pub fn parse(input: &str) -> Result<Self> {
        match input.to_lowercase().as_str() {
            "anime" => Ok(Self::Anime),
            _ => anyhow::bail!("Unsupported scope '{}'. Supported: anime", input),
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FindingReason {
    BrokenSource,
    ParserTitleMismatch,
    ArrUntracked,
    EpisodeOutOfRange,
    DuplicateEpisodeSlot,
    SeasonCountAnomaly,
    NonRdSourcePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParsedContext {
    pub library_title: String,
    pub parsed_title: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
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
}

#[derive(Debug, Clone)]
pub struct PruneOutcome {
    pub candidates: usize,
    pub high_or_critical_candidates: usize,
    pub safe_warning_duplicate_candidates: usize,
    pub removed: usize,
    pub skipped: usize,
    pub confirmation_token: String,
}

#[derive(Debug, Default, Clone)]
struct ArrSeriesSnapshot {
    with_file: HashSet<(u32, u32)>,
    season_counts: HashMap<u32, usize>,
}

#[derive(Debug, Clone)]
struct WorkingEntry {
    symlink_path: PathBuf,
    source_path: PathBuf,
    media_id: String,
    parsed_title: String,
    season: Option<u32>,
    episode: Option<u32>,
    library_title: String,
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

    pub async fn build_report(&self, scope: CleanupScope) -> Result<CleanupReport> {
        let libraries = self.libraries_for_scope(scope);
        if libraries.is_empty() {
            anyhow::bail!("No libraries found for scope {:?}", scope);
        }

        let scanner = LibraryScanner::new();
        let mut library_items = Vec::new();
        for lib in &libraries {
            library_items.extend(scanner.scan_library(lib));
        }

        if self.emit_progress {
            println!(
                "   🧠 Cleanup audit: loading metadata for {} library item(s)...",
                library_items.len()
            );
        }
        let metadata_started = Instant::now();
        let metadata_map = self.load_metadata(&library_items).await;
        if self.emit_progress {
            println!(
                "   ✅ Cleanup audit metadata ready in {:.1}s",
                metadata_started.elapsed().as_secs_f64()
            );
            println!("   📚 Cleanup audit: loading Sonarr cross-check snapshots...");
        }
        let arr_started = Instant::now();
        let arr_map = self.load_sonarr_snapshots(&library_items).await;
        if self.emit_progress {
            println!(
                "   ✅ Cleanup audit Sonarr snapshots ready in {:.1}s",
                arr_started.elapsed().as_secs_f64()
            );
        }

        if self.emit_progress {
            println!("   🔗 Cleanup audit: collecting symlink entries...");
        }
        let entries_started = Instant::now();
        let mut entries = self.collect_symlink_entries(&libraries, &library_items);
        if self.emit_progress {
            println!(
                "   ✅ Cleanup audit collected {} symlink entries in {:.1}s",
                entries.len(),
                entries_started.elapsed().as_secs_f64()
            );
        }

        for entry in &mut entries {
            if !entry.source_path.exists() {
                entry.reasons.insert(FindingReason::BrokenSource);
            }

            if !self.is_under_rd_sources(&entry.source_path) {
                entry.reasons.insert(FindingReason::NonRdSourcePath);
            }

            if entry.media_id.is_empty() {
                continue;
            }

            if let Some(item) = library_items
                .iter()
                .find(|li| li.id.to_string() == entry.media_id)
            {
                let aliases = build_aliases(item, metadata_map.get(&entry.media_id));
                if !entry.parsed_title.is_empty() {
                    let normalized_parsed = normalize(&entry.parsed_title);
                    if !aliases
                        .iter()
                        .any(|alias| tokenized_title_match(alias, &normalized_parsed))
                    {
                        entry.reasons.insert(FindingReason::ParserTitleMismatch);
                    }
                }

                if let (Some(season), Some(episode)) = (entry.season, entry.episode) {
                    if let Some(Some(meta)) = metadata_map.get(&entry.media_id) {
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

        apply_duplicate_and_count_signals(&mut entries, &metadata_map, &arr_map);
        let suppressed_count = suppress_redundant_season_count_warnings(&mut entries);
        if suppressed_count > 0 {
            info!(
                "Cleanup audit: suppressed {} season_count_anomaly warnings in seasons with stronger signals",
                suppressed_count
            );
        }

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
                    season: entry.season,
                    episode: entry.episode,
                },
            });
        }

        summary.total_findings = findings.len();

        Ok(CleanupReport {
            version: 1,
            created_at: Utc::now(),
            scope,
            findings,
            summary,
        })
    }

    fn libraries_for_scope(&self, scope: CleanupScope) -> Vec<&LibraryConfig> {
        self.cfg
            .libraries
            .iter()
            .filter(|lib| match scope {
                CleanupScope::Anime => effective_content_type(lib) == ContentType::Anime,
            })
            .collect()
    }

    fn collect_symlink_entries(
        &self,
        libraries: &[&LibraryConfig],
        library_items: &[LibraryItem],
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

                let parsed_source = self
                    .source_scanner
                    .parse_filename_with_type(&source_path, ContentType::Anime)
                    .or_else(|| {
                        self.source_scanner
                            .parse_filename_with_type(&symlink_path, ContentType::Anime)
                    });

                let owner = find_owner_library_item(&symlink_path, library_items);

                let (media_id, library_title) = owner
                    .map(|o| (o.id.to_string(), o.title.clone()))
                    .unwrap_or_else(|| (String::new(), String::new()));

                entries.push(WorkingEntry {
                    symlink_path,
                    source_path,
                    media_id,
                    parsed_title: parsed_source
                        .as_ref()
                        .map(|s| s.parsed_title.clone())
                        .unwrap_or_default(),
                    season: parsed_source.as_ref().and_then(|s| s.season),
                    episode: parsed_source.as_ref().and_then(|s| s.episode),
                    library_title,
                    reasons: BTreeSet::new(),
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

        let tmdb = if self.cfg.has_tmdb() && metadata_mode.allows_network() {
            Some(TmdbClient::new(
                &self.cfg.api.tmdb_api_key,
                Some(&self.cfg.api.tmdb_read_access_token),
                self.cfg.api.cache_ttl_hours,
            ))
        } else {
            None
        };

        let mut tvdb = if self.cfg.has_tvdb() && metadata_mode.allows_network() {
            Some(TvdbClient::new(
                &self.cfg.api.tvdb_api_key,
                self.cfg.api.cache_ttl_hours,
            ))
        } else {
            None
        };

        if metadata_mode == MetadataMode::Off {
            info!("Cleanup audit: metadata lookups disabled (matching.metadata_mode=off)");
        }

        let total = library_items.len();
        let mut last_progress = Instant::now();
        let mut progress = self
            .emit_progress
            .then(|| ProgressLine::new("Cleanup metadata:"));
        for (idx, item) in library_items.iter().enumerate() {
            if let Some(progress) = progress.as_mut() {
                if idx > 0 && last_progress.elapsed() >= Duration::from_secs(5) {
                    let pct = (idx as f64 / total.max(1) as f64) * 100.0;
                    if !progress.is_tty() {
                        info!(
                            "Cleanup audit metadata progress: {}/{} ({:.1}%)",
                            idx, total, pct
                        );
                    }
                    progress.update(format!("{}/{} ({:.1}%)", idx, total, pct));
                    last_progress = Instant::now();
                }
            }
            let key = item.id.to_string();
            if map.contains_key(&key) {
                continue;
            }

            let metadata = match metadata_mode {
                MetadataMode::Off => None,
                MetadataMode::CacheOnly => self.load_cached_metadata(item).await,
                MetadataMode::Full => match item.id {
                    MediaId::Tmdb(id) => {
                        if let Some(client) = &tmdb {
                            match client.get_tv_metadata(id, self.db).await {
                                Ok(meta) => Some(meta),
                                Err(e) => {
                                    warn!("TMDB metadata fetch failed for {}: {}", key, e);
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    }
                    MediaId::Tvdb(id) => {
                        if let Some(client) = tvdb.as_mut() {
                            match client.get_series_metadata(id, self.db).await {
                                Ok(meta) => Some(meta),
                                Err(e) => {
                                    warn!("TVDB metadata fetch failed for {}: {}", key, e);
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    }
                },
            };

            map.insert(key, metadata);
        }

        if let Some(progress) = progress.as_mut() {
            progress.finish(format!(
                "{}/{} (100.0%)",
                library_items.len(),
                library_items.len()
            ));
        }
        map
    }

    async fn load_cached_metadata(&self, item: &LibraryItem) -> Option<ContentMetadata> {
        let cache_key = match item.id {
            MediaId::Tmdb(id) => {
                if item.media_type == MediaType::Movie {
                    format!("tmdb:movie:{}", id)
                } else {
                    format!("tmdb:tv:{}", id)
                }
            }
            MediaId::Tvdb(id) => format!("tvdb:series:{}", id),
        };

        let cached = match self.db.get_cached(&cache_key).await {
            Ok(v) => v,
            Err(e) => {
                warn!("Cleanup audit: cache read failed for {}: {}", cache_key, e);
                return None;
            }
        }?;

        match serde_json::from_str::<ContentMetadata>(&cached) {
            Ok(meta) => Some(meta),
            Err(e) => {
                warn!(
                    "Cleanup audit: cache decode failed for {}: {}",
                    cache_key, e
                );
                None
            }
        }
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
    max_delete: Option<usize>,
    confirmation_token: Option<&str>,
) -> Result<PruneOutcome> {
    let json = std::fs::read_to_string(report_path)?;
    let report: CleanupReport = serde_json::from_str(&json)?;

    let high_or_critical_candidates: Vec<_> = report
        .findings
        .iter()
        .filter(|f| {
            matches!(
                f.severity,
                FindingSeverity::Critical | FindingSeverity::High
            )
        })
        .collect();

    let safe_warning_prunes = collect_safe_warning_duplicate_prunes(&report.findings);

    let mut candidate_paths: Vec<PathBuf> = high_or_critical_candidates
        .iter()
        .map(|f| f.symlink_path.clone())
        .collect();
    candidate_paths.extend(safe_warning_prunes.iter().cloned());
    candidate_paths.sort();
    candidate_paths.dedup();
    let token = prune_confirmation_token(&report, &candidate_paths);

    info!(
        "Cleanup prune: {} high/critical + {} safe-warning duplicate candidates ({} total unique)",
        high_or_critical_candidates.len(),
        safe_warning_prunes.len(),
        candidate_paths.len()
    );

    if !apply {
        return Ok(PruneOutcome {
            candidates: candidate_paths.len(),
            high_or_critical_candidates: high_or_critical_candidates.len(),
            safe_warning_duplicate_candidates: safe_warning_prunes.len(),
            removed: 0,
            skipped: 0,
            confirmation_token: token,
        });
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
        if provided.is_empty() || provided != token {
            anyhow::bail!(
                "Refusing prune apply: invalid or missing confirmation token. Re-run preview and pass --confirm-token {}",
                token
            );
        }
    }

    if candidate_paths.len() > delete_cap {
        anyhow::bail!(
            "Refusing prune apply: {} candidates exceeds delete cap {} (use --max-delete to override)",
            candidate_paths.len(),
            delete_cap
        );
    }

    if cfg.backup.enabled {
        let backup = BackupManager::new(&cfg.backup);
        backup.create_safety_snapshot(db, "cleanup-prune").await?;
    }

    let mut removed = 0usize;
    let mut skipped = 0usize;

    for symlink_path in &candidate_paths {
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
                        if let Err(e) = db.mark_removed_path(symlink_path).await {
                            warn!(
                                "Cleanup prune: removed {:?} but failed DB mark_removed: {}",
                                symlink_path, e
                            );
                            let _ = db
                                .record_link_event_fields(
                                    "prune_skipped",
                                    symlink_path,
                                    None,
                                    None,
                                    Some("db_mark_removed_failed"),
                                )
                                .await;
                            skipped += 1;
                            continue;
                        }
                        let _ = db
                            .record_link_event_fields(
                                "prune_removed",
                                symlink_path,
                                None,
                                None,
                                None,
                            )
                            .await;
                        removed += 1;
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

    Ok(PruneOutcome {
        candidates: candidate_paths.len(),
        high_or_critical_candidates: high_or_critical_candidates.len(),
        safe_warning_duplicate_candidates: safe_warning_prunes.len(),
        removed,
        skipped,
        confirmation_token: token,
    })
}

fn prune_confirmation_token(report: &CleanupReport, candidate_paths: &[PathBuf]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    report.version.hash(&mut hasher);
    report.created_at.timestamp().hash(&mut hasher);
    report.scope.hash(&mut hasher);
    for path in candidate_paths {
        path.hash(&mut hasher);
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

fn find_owner_library_item<'a>(
    symlink_path: &Path,
    library_items: &'a [LibraryItem],
) -> Option<&'a LibraryItem> {
    library_items
        .iter()
        .filter(|item| symlink_path.starts_with(&item.path))
        .max_by_key(|item| item.path.components().count())
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

fn is_warning_only_duplicate_slot(reasons: &[FindingReason], severity: FindingSeverity) -> bool {
    severity == FindingSeverity::Warning
        && reasons.len() == 1
        && reasons[0] == FindingReason::DuplicateEpisodeSlot
}

fn finding_slot_key(finding: &CleanupFinding) -> Option<(String, u32, u32)> {
    let season = finding.parsed.season?;
    let episode = finding.parsed.episode?;
    Some((finding.media_id.clone(), season, episode))
}

fn collect_safe_warning_duplicate_prunes(findings: &[CleanupFinding]) -> Vec<PathBuf> {
    let mut tainted_slots: HashSet<(String, u32, u32)> = HashSet::new();
    for finding in findings {
        let Some(slot_key) = finding_slot_key(finding) else {
            continue;
        };

        if !is_warning_only_duplicate_slot(&finding.reasons, finding.severity) {
            tainted_slots.insert(slot_key);
        }
    }

    let mut by_slot_source: HashMap<(String, u32, u32, PathBuf), Vec<PathBuf>> = HashMap::new();
    for finding in findings {
        if !is_warning_only_duplicate_slot(&finding.reasons, finding.severity) {
            continue;
        }

        let Some((media_id, season, episode)) = finding_slot_key(finding) else {
            continue;
        };

        if tainted_slots.contains(&(media_id.clone(), season, episode)) {
            continue;
        }

        by_slot_source
            .entry((media_id, season, episode, finding.source_path.clone()))
            .or_default()
            .push(finding.symlink_path.clone());
    }

    let mut prune_paths = Vec::new();
    for (_key, mut symlink_paths) in by_slot_source {
        if symlink_paths.len() < 2 {
            continue;
        }

        symlink_paths.sort();
        prune_paths.extend(symlink_paths.into_iter().skip(1));
    }

    prune_paths.sort();
    prune_paths.dedup();
    prune_paths
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
        || reasons.contains(&FindingReason::EpisodeOutOfRange)
        || (reasons.contains(&FindingReason::ArrUntracked)
            && reasons.contains(&FindingReason::ParserTitleMismatch))
    {
        return FindingSeverity::Critical;
    }

    if reasons.contains(&FindingReason::NonRdSourcePath)
        || reasons.contains(&FindingReason::ArrUntracked)
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
    };

    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    PathBuf::from(format!("backups/cleanup-audit-{}-{}.json", scope_name, ts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::Database;
    use crate::models::{LinkRecord, LinkStatus, MediaType};
    use chrono::Duration as ChronoDuration;

    fn test_working_entry(
        media_id: &str,
        season: Option<u32>,
        episode: Option<u32>,
        reasons: &[FindingReason],
    ) -> WorkingEntry {
        let mut reason_set = BTreeSet::new();
        for reason in reasons {
            reason_set.insert(*reason);
        }

        WorkingEntry {
            symlink_path: PathBuf::from("/lib/test.mkv"),
            source_path: PathBuf::from("/src/test.mkv"),
            media_id: media_id.to_string(),
            parsed_title: String::new(),
            season,
            episode,
            library_title: String::new(),
            reasons: reason_set,
        }
    }

    fn test_cleanup_finding(
        media_id: &str,
        season: u32,
        episode: u32,
        severity: FindingSeverity,
        reasons: Vec<FindingReason>,
        symlink_path: &str,
        source_path: &str,
    ) -> CleanupFinding {
        CleanupFinding {
            symlink_path: PathBuf::from(symlink_path),
            source_path: PathBuf::from(source_path),
            media_id: media_id.to_string(),
            severity,
            confidence: 0.5,
            reasons,
            parsed: ParsedContext {
                library_title: String::new(),
                parsed_title: String::new(),
                season: Some(season),
                episode: Some(episode),
            },
        }
    }

    #[test]
    fn test_token_match_rejects_group_substring_collision() {
        assert!(!tokenized_title_match("show", "showgroup fansub"));
        assert!(!tokenized_title_match("show group", "showgroup fansub"));
    }

    #[test]
    fn test_tokenized_title_match_exact_and_contiguous_tokens() {
        assert!(tokenized_title_match("jujutsu kaisen", "jujutsu kaisen 03"));
        assert!(tokenized_title_match("jujutsu kaisen 03", "jujutsu kaisen"));
        assert!(!tokenized_title_match("one piece", "piece one"));
    }

    #[test]
    fn test_classify_severity_critical_combo() {
        let mut reasons = BTreeSet::new();
        reasons.insert(FindingReason::ArrUntracked);
        reasons.insert(FindingReason::ParserTitleMismatch);
        assert_eq!(classify_severity(&reasons), FindingSeverity::Critical);
    }

    #[test]
    fn test_classify_severity_warning() {
        let mut reasons = BTreeSet::new();
        reasons.insert(FindingReason::SeasonCountAnomaly);
        assert_eq!(classify_severity(&reasons), FindingSeverity::Warning);
    }

    #[test]
    fn test_classify_confidence_uses_strongest_reason() {
        let mut reasons = BTreeSet::new();
        reasons.insert(FindingReason::SeasonCountAnomaly);
        reasons.insert(FindingReason::BrokenSource);
        assert_eq!(classify_confidence(&reasons), 1.0);
    }

    #[test]
    fn test_season_count_anomaly_ignores_missing_or_equal_counts() {
        assert!(!is_season_count_anomaly(18, 20));
        assert!(!is_season_count_anomaly(20, 20));
    }

    #[test]
    fn test_season_count_anomaly_small_season_requires_at_least_two_excess() {
        assert!(!is_season_count_anomaly(11, 10));
        assert!(is_season_count_anomaly(12, 10));
    }

    #[test]
    fn test_season_count_anomaly_medium_season_requires_ratio_and_excess() {
        assert!(!is_season_count_anomaly(23, 20));
        assert!(is_season_count_anomaly(24, 20));
    }

    #[test]
    fn test_season_count_anomaly_large_season_scales_with_expected_count() {
        assert!(!is_season_count_anomaly(59, 50));
        assert!(is_season_count_anomaly(60, 50));
    }

    #[test]
    fn test_episode_out_of_range_allows_unknown_specials() {
        let meta = ContentMetadata {
            title: "Test Show".to_string(),
            aliases: vec![],
            year: None,
            seasons: vec![crate::models::SeasonInfo {
                season_number: 1,
                episodes: vec![crate::models::EpisodeInfo {
                    episode_number: 1,
                    title: "Ep1".to_string(),
                }],
            }],
        };

        assert!(!episode_out_of_range(&meta, 0, 1));
    }

    #[test]
    fn test_episode_out_of_range_keeps_regular_unknown_season_strict() {
        let meta = ContentMetadata {
            title: "Test Show".to_string(),
            aliases: vec![],
            year: None,
            seasons: vec![crate::models::SeasonInfo {
                season_number: 1,
                episodes: vec![crate::models::EpisodeInfo {
                    episode_number: 1,
                    title: "Ep1".to_string(),
                }],
            }],
        };

        assert!(episode_out_of_range(&meta, 9, 1));
    }

    #[test]
    fn test_suppress_redundant_season_count_warning_when_season_has_high_signal() {
        let mut entries = vec![
            test_working_entry(
                "tvdb-1",
                Some(1),
                Some(1),
                &[FindingReason::SeasonCountAnomaly],
            ),
            test_working_entry(
                "tvdb-1",
                Some(1),
                Some(2),
                &[FindingReason::ParserTitleMismatch],
            ),
            test_working_entry(
                "tvdb-1",
                Some(2),
                Some(1),
                &[FindingReason::SeasonCountAnomaly],
            ),
        ];

        let suppressed = suppress_redundant_season_count_warnings(&mut entries);
        assert_eq!(suppressed, 1);
        assert!(entries[0].reasons.is_empty());
        assert!(entries[2]
            .reasons
            .contains(&FindingReason::SeasonCountAnomaly));
    }

    #[test]
    fn test_collect_safe_warning_duplicate_prunes_keeps_one_identical_source() {
        let findings = vec![
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::Warning,
                vec![FindingReason::DuplicateEpisodeSlot],
                "/lib/Show - S01E03 a.mkv",
                "/src/show-s01e03.mkv",
            ),
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::Warning,
                vec![FindingReason::DuplicateEpisodeSlot],
                "/lib/Show - S01E03 b.mkv",
                "/src/show-s01e03.mkv",
            ),
        ];

        let prunes = collect_safe_warning_duplicate_prunes(&findings);
        assert_eq!(prunes, vec![PathBuf::from("/lib/Show - S01E03 b.mkv")]);
    }

    #[test]
    fn test_collect_safe_warning_duplicate_prunes_skips_different_sources() {
        let findings = vec![
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::Warning,
                vec![FindingReason::DuplicateEpisodeSlot],
                "/lib/Show - S01E03 a.mkv",
                "/src/show-s01e03-source-a.mkv",
            ),
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::Warning,
                vec![FindingReason::DuplicateEpisodeSlot],
                "/lib/Show - S01E03 b.mkv",
                "/src/show-s01e03-source-b.mkv",
            ),
        ];

        let prunes = collect_safe_warning_duplicate_prunes(&findings);
        assert!(prunes.is_empty());
    }

    #[test]
    fn test_collect_safe_warning_duplicate_prunes_skips_tainted_slot() {
        let findings = vec![
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::Warning,
                vec![FindingReason::DuplicateEpisodeSlot],
                "/lib/Show - S01E03 a.mkv",
                "/src/show-s01e03.mkv",
            ),
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::Warning,
                vec![FindingReason::DuplicateEpisodeSlot],
                "/lib/Show - S01E03 b.mkv",
                "/src/show-s01e03.mkv",
            ),
            test_cleanup_finding(
                "tvdb-1",
                1,
                3,
                FindingSeverity::High,
                vec![
                    FindingReason::DuplicateEpisodeSlot,
                    FindingReason::ParserTitleMismatch,
                ],
                "/lib/Show - S01E03 suspicious.mkv",
                "/src/show-s01e03-alt.mkv",
            ),
        ];

        let prunes = collect_safe_warning_duplicate_prunes(&findings);
        assert!(prunes.is_empty());
    }

    fn test_config(library_root: &Path, source_root: &Path) -> Config {
        let yaml = format!(
            r#"
libraries:
  - name: Anime
    path: "{}"
    media_type: tv
    content_type: anime
sources:
  - name: RD
    path: "{}"
    media_type: auto
backup:
  enabled: false
"#,
            library_root.display(),
            source_root.display()
        );
        serde_yaml::from_str(&yaml).unwrap()
    }

    fn high_finding(path: &Path, source: &Path) -> CleanupFinding {
        CleanupFinding {
            symlink_path: path.to_path_buf(),
            source_path: source.to_path_buf(),
            media_id: "tvdb-1".to_string(),
            severity: FindingSeverity::High,
            confidence: 1.0,
            reasons: vec![FindingReason::BrokenSource],
            parsed: ParsedContext {
                library_title: "Show".to_string(),
                parsed_title: "Show".to_string(),
                season: Some(1),
                episode: Some(1),
            },
        }
    }

    fn report_with_findings(
        created_at: DateTime<Utc>,
        findings: Vec<CleanupFinding>,
    ) -> CleanupReport {
        let summary = CleanupSummary {
            total_findings: findings.len(),
            high: findings
                .iter()
                .filter(|f| matches!(f.severity, FindingSeverity::High))
                .count(),
            ..CleanupSummary::default()
        };
        CleanupReport {
            version: 1,
            created_at,
            scope: CleanupScope::Anime,
            findings,
            summary,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_prune_apply_marks_db_removed_and_deletes_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let library_root = dir.path().join("library");
        let source_root = dir.path().join("rd");
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();

        let source_file = source_root.join("source.mkv");
        std::fs::write(&source_file, "video").unwrap();
        let symlink_path = library_root.join("Show - S01E01.mkv");
        std::os::unix::fs::symlink(&source_file, &symlink_path).unwrap();

        let cfg = test_config(&library_root, &source_root);
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source_file.clone(),
            target_path: symlink_path.clone(),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let report =
            report_with_findings(Utc::now(), vec![high_finding(&symlink_path, &source_file)]);
        let report_path = dir.path().join("report.json");
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview = run_prune(&cfg, &db, &report_path, false, None, None)
            .await
            .unwrap();
        let outcome = run_prune(
            &cfg,
            &db,
            &report_path,
            true,
            None,
            Some(&preview.confirmation_token),
        )
        .await
        .unwrap();

        assert_eq!(outcome.removed, 1);
        assert!(!symlink_path.exists() && !symlink_path.is_symlink());

        let updated = db
            .get_link_by_target_path(&symlink_path)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, LinkStatus::Removed);
    }

    #[tokio::test]
    async fn test_prune_apply_rejects_stale_report() {
        let dir = tempfile::TempDir::new().unwrap();
        let library_root = dir.path().join("library");
        let source_root = dir.path().join("rd");
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();

        let cfg = test_config(&library_root, &source_root);
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let symlink_path = library_root.join("stale.mkv");
        let source_file = source_root.join("source.mkv");
        let report = report_with_findings(
            Utc::now() - ChronoDuration::hours(cfg.cleanup.prune.max_report_age_hours as i64 + 1),
            vec![high_finding(&symlink_path, &source_file)],
        );
        let report_path = dir.path().join("stale-report.json");
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview = run_prune(&cfg, &db, &report_path, false, None, None)
            .await
            .unwrap();
        let err = run_prune(
            &cfg,
            &db,
            &report_path,
            true,
            None,
            Some(&preview.confirmation_token),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("too old"));
    }

    #[tokio::test]
    async fn test_prune_apply_rejects_tampered_report_token() {
        let dir = tempfile::TempDir::new().unwrap();
        let library_root = dir.path().join("library");
        let source_root = dir.path().join("rd");
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();

        let cfg = test_config(&library_root, &source_root);
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let source_file = source_root.join("source.mkv");
        let report_path = dir.path().join("tampered-report.json");
        let mut report = report_with_findings(
            Utc::now(),
            vec![high_finding(&library_root.join("a.mkv"), &source_file)],
        );
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview = run_prune(&cfg, &db, &report_path, false, None, None)
            .await
            .unwrap();

        report
            .findings
            .push(high_finding(&library_root.join("b.mkv"), &source_file));
        report.summary.total_findings = report.findings.len();
        report.summary.high = report.findings.len();
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let err = run_prune(
            &cfg,
            &db,
            &report_path,
            true,
            None,
            Some(&preview.confirmation_token),
        )
        .await
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("invalid or missing confirmation token"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_prune_apply_blocks_path_escape() {
        let dir = tempfile::TempDir::new().unwrap();
        let library_root = dir.path().join("library");
        let source_root = dir.path().join("rd");
        let outside_root = dir.path().join("outside");
        std::fs::create_dir_all(&library_root).unwrap();
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&outside_root).unwrap();

        let source_file = source_root.join("source.mkv");
        std::fs::write(&source_file, "video").unwrap();
        let escaped_symlink = outside_root.join("escaped.mkv");
        std::os::unix::fs::symlink(&source_file, &escaped_symlink).unwrap();

        let cfg = test_config(&library_root, &source_root);
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let report = report_with_findings(
            Utc::now(),
            vec![high_finding(&escaped_symlink, &source_file)],
        );
        let report_path = dir.path().join("escaped-report.json");
        std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

        let preview = run_prune(&cfg, &db, &report_path, false, None, None)
            .await
            .unwrap();
        let outcome = run_prune(
            &cfg,
            &db,
            &report_path,
            true,
            None,
            Some(&preview.confirmation_token),
        )
        .await
        .unwrap();

        assert_eq!(outcome.removed, 0);
        assert_eq!(outcome.skipped, 1);
        assert!(escaped_symlink.is_symlink());
    }
}
