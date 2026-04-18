use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Result;
use regex::Regex;
#[cfg(test)]
use scoring::token_is_lookup_noise;
use scoring::{
    calculate_match_score, extract_quality, extract_year, normalize_title, title_tokens,
    MatchScoreInput,
};
use tracing::{debug, info, warn};
use trash::{extract_media_id_from_tagged_ancestors, parse_trash_filename};
#[cfg(test)]
use trash::{trash_quality_regex, trash_season_episode_regex};
use walkdir::WalkDir;

use crate::cache::TorrentCache;
use crate::commands::ensure_runtime_source_paths_healthy;
use crate::config::{ContentType, LibraryConfig};
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MediaType};
use crate::source_scanner::SourceScanner;
use crate::utils::{
    cached_source_health, path_under_roots, PathHealth, ProgressLine, VIDEO_EXTENSIONS,
};

/// Minimum score threshold for TV replacements (title + season + episode required)
const TV_THRESHOLD: f64 = 0.75;
/// Minimum score threshold for movie replacements (exact title + year is enough)
const MOVIE_THRESHOLD: f64 = 0.50;
/// Max allowed file size difference ratio (±40%)
const SIZE_TOLERANCE: f64 = 0.40;
const PARALLEL_DRIFT_MIN_LINKS: usize = 1000;

// ─── Safety guard ────────────────────────────────────────────────────

/// SAFETY: Verify a path is a symlink (not a regular file or directory)
/// before performing destructive operations. This prevents accidental
/// deletion of *arr-managed directories or real media files.
fn assert_symlink_only(path: &std::path::Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        anyhow::bail!(
            "SAFETY GUARD: Attempted to operate on a directory instead of a symlink: {:?}. \
             Symlinkarr never touches directories — they are managed by *arr apps.",
            path
        );
    }
    if !meta.file_type().is_symlink() {
        anyhow::bail!(
            "SAFETY GUARD: {:?} is a regular file, not a symlink. \
             Symlinkarr only operates on symlinks.",
            path
        );
    }
    Ok(())
}

fn normalized_compare_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_streaming_symlink_match(dead_symlink_path: &Path, active_path: &str) -> bool {
    if dead_symlink_path.to_string_lossy() == active_path {
        return true;
    }

    let active = PathBuf::from(active_path);
    normalized_compare_path(dead_symlink_path) == normalized_compare_path(&active)
}

fn collect_fresh_dead_links_chunk(
    links: Vec<LinkRecord>,
    allowed_symlink_roots: Option<Vec<PathBuf>>,
    processed: &AtomicUsize,
) -> Result<Vec<DeadLink>> {
    let mut source_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
    let mut parent_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
    let mut fresh = Vec::new();

    for link in links {
        if let Some(roots) = allowed_symlink_roots.as_ref() {
            if !path_under_roots(&link.target_path, roots) {
                processed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }

        let source_exists = destructive_source_exists(
            "repair dead-link detection",
            &link.source_path,
            &mut source_health_cache,
            &mut parent_health_cache,
        )?;
        let target_ok = match std::fs::symlink_metadata(&link.target_path) {
            Ok(meta) if meta.file_type().is_symlink() => std::fs::read_link(&link.target_path)
                .map(|resolved| resolved == link.source_path)
                .unwrap_or(false),
            _ => false,
        };

        if source_exists && target_ok {
            processed.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let symlink_name = link
            .target_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let meta = parse_trash_filename(symlink_name);

        fresh.push(DeadLink {
            symlink_path: link.target_path.clone(),
            original_source: link.source_path.clone(),
            media_id: link.media_id.clone(),
            media_type: link.media_type,
            content_type: ContentType::from_media_type(link.media_type),
            meta,
            original_size: if source_exists {
                std::fs::metadata(&link.source_path).ok().map(|m| m.len())
            } else {
                None
            },
        });
        processed.fetch_add(1, Ordering::Relaxed);
    }

    Ok(fresh)
}

fn destructive_source_exists(
    operation: &str,
    source_path: &Path,
    source_health_cache: &mut HashMap<PathBuf, PathHealth>,
    parent_health_cache: &mut HashMap<PathBuf, PathHealth>,
) -> Result<bool> {
    let source_health = cached_source_health(source_path, source_health_cache, parent_health_cache);
    if source_health.blocks_destructive_ops() {
        anyhow::bail!(
            "Aborting {}: source path became unhealthy: {}",
            operation,
            source_health.describe(source_path)
        );
    }
    Ok(source_health.is_healthy())
}

fn recommended_drift_workers(total_active: usize) -> usize {
    if total_active == 0 {
        return 1;
    }

    let available = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let target = (available * 3).div_ceil(4);
    target.max(1).min(total_active)
}

// ─── TRaSH-format parser ─────────────────────────────────────────────

/// Metadata extracted from a TRaSH Guide naming convention filename.
///
/// Example inputs:
///   "Breaking Bad (2008) - S01E03 - ...And the Bag's in the River [WEBDL-1080p][x264]-GROUP.mkv"
///   "The Matrix (1999) {imdb-tt0133093} [Bluray-2160p][DV HDR10]-GROUP.mkv"
#[derive(Debug, Clone)]
pub struct TrashMeta {
    /// Cleaned title: "Breaking Bad"
    pub title: String,
    /// Year: Some(2008)
    #[allow(dead_code)] // Populated by parser for future scoring
    pub year: Option<u32>,
    /// Season: Some(1)
    pub season: Option<u32>,
    /// Episode: Some(3)
    pub episode: Option<u32>,
    /// Quality string: Some("1080p"), Some("2160p")
    pub quality: Option<String>,
    /// IMDB ID: Some("tt0133093")
    #[allow(dead_code)] // Populated by parser for future API lookups
    pub imdb_id: Option<String>,
}

// ─── Core types ──────────────────────────────────────────────────────

/// A dead symlink that needs repair, enriched with TRaSH metadata.
#[derive(Debug, Clone)]
pub struct DeadLink {
    /// Path to the broken symlink
    pub symlink_path: PathBuf,
    /// Original source path that no longer exists
    pub original_source: PathBuf,
    /// Media ID string (e.g., "tvdb-81189")
    pub media_id: String,
    /// Media type
    pub media_type: MediaType,
    /// Content type (controls which parser to use for replacements)
    pub content_type: ContentType,
    /// Metadata parsed from the symlink filename (TRaSH format)
    pub meta: TrashMeta,
    /// File size of the original source (if known from DB or stat)
    pub original_size: Option<u64>,
}

/// A candidate replacement file found on the RD mount
#[derive(Debug, Clone)]
pub struct ReplacementCandidate {
    /// Path to the candidate file
    pub path: PathBuf,
    /// Parsed title
    #[allow(dead_code)] // Diagnostic context
    pub parsed_title: String,
    /// Season (for TV)
    #[allow(dead_code)] // Diagnostic context
    pub season: Option<u32>,
    /// Episode (for TV)
    #[allow(dead_code)] // Diagnostic context
    pub episode: Option<u32>,
    /// Quality (e.g., "1080p")
    pub quality: Option<String>,
    /// File size in bytes
    #[allow(dead_code)] // Diagnostic context
    pub file_size: u64,
    /// Match confidence score (0.0 - 1.0)
    pub score: f64,
}

#[derive(Debug, Clone)]
struct SourceCandidate {
    path: PathBuf,
    parsed_title: String,
    normalized_title: String,
    season: Option<u32>,
    episode: Option<u32>,
    quality: Option<String>,
    year: Option<u32>,
    file_size: u64,
}

#[derive(Debug, Clone, Default)]
struct SourceCatalog {
    entries: Vec<SourceCandidate>,
    token_index: HashMap<String, Vec<usize>>,
}

/// Repair result for a single dead link
#[derive(Debug)]
pub enum RepairResult {
    /// Found a replacement and created new symlink
    Repaired {
        #[allow(dead_code)] // Context for repair report
        dead_link: DeadLink,
        #[allow(dead_code)] // Context for repair report
        replacement: PathBuf,
    },
    /// No suitable replacement found
    Unrepairable {
        #[allow(dead_code)] // Context for repair report
        dead_link: DeadLink,
        #[allow(dead_code)] // Context for repair report
        reason: String,
    },
    /// Skipped due to active streaming or other guard
    Skipped {
        #[allow(dead_code)] // Context for repair report
        dead_link: DeadLink,
        #[allow(dead_code)] // Context for repair report
        reason: String,
    },
    /// Stale DB record where target symlink path is no longer a symlink on disk
    Stale {
        #[allow(dead_code)] // Context for repair report
        dead_link: DeadLink,
        #[allow(dead_code)] // Context for repair report
        reason: String,
    },
}

// ─── Repairer ────────────────────────────────────────────────────────

/// Handles detection and repair of dead symlinks.
pub struct Repairer {
    source_scanner: SourceScanner,
}

impl Repairer {
    pub fn new() -> Self {
        Self {
            source_scanner: SourceScanner::new(),
        }
    }

    async fn reconcile_stale_dead_links(
        &self,
        db: &Database,
        dead_links: Vec<DeadLink>,
        dry_run: bool,
    ) -> (Vec<DeadLink>, Vec<RepairResult>) {
        let mut candidates = Vec::new();
        let mut stale = Vec::new();
        let total = dead_links.len();
        let mut progress = ProgressLine::new("Dead-link reconciliation:");

        for (idx, dead_link) in dead_links.into_iter().enumerate() {
            if idx > 0 && idx % 2000 == 0 {
                progress.update(format!("{}/{}", idx, total));
            }

            let stale_reason = match std::fs::symlink_metadata(&dead_link.symlink_path) {
                Ok(meta) if meta.file_type().is_symlink() => None,
                Ok(meta) if meta.file_type().is_dir() => {
                    Some("target path is a directory, not a symlink".to_string())
                }
                Ok(_) => Some("target path is a regular file, not a symlink".to_string()),
                Err(_) => Some("target symlink path no longer exists on disk".to_string()),
            };

            if let Some(base_reason) = stale_reason {
                let mut reason = base_reason;

                if !dry_run {
                    if let Err(e) = db.mark_removed_path(&dead_link.symlink_path).await {
                        warn!(
                            "Could not mark stale dead link as removed in DB {:?}: {}",
                            dead_link.symlink_path, e
                        );
                        reason.push_str(&format!("; DB status update failed: {}", e));
                    }
                }

                let _ = db
                    .record_link_event_fields(
                        if dry_run {
                            "repair_stale_preview"
                        } else {
                            "repair_stale_removed"
                        },
                        &dead_link.symlink_path,
                        Some(&dead_link.original_source),
                        Some(dead_link.media_id.as_str()),
                        Some(reason.as_str()),
                    )
                    .await;

                stale.push(RepairResult::Stale { dead_link, reason });
            } else {
                candidates.push(dead_link);
            }
        }

        progress.finish(format!("{}/{}", total, total));

        (candidates, stale)
    }

    /// Scan for dead symlinks in the library directories.
    /// A dead symlink is one where the target (source file) no longer exists.
    pub async fn find_dead_links(
        &self,
        db: &Database,
        allowed_symlink_roots: Option<&[PathBuf]>,
    ) -> Result<Vec<DeadLink>> {
        info!("Loading dead links from database");
        println!("   🔎 Loading dead links from database...");
        let load_started = Instant::now();
        let (ticker_stop, ticker) = spawn_activity_ticker("Loading dead links from DB");
        let dead_records = db.get_dead_link_seeds_scoped(allowed_symlink_roots).await;
        stop_activity_ticker(ticker_stop, ticker).await;
        let dead_records = dead_records?;
        let total_dead = dead_records.len();
        println!(
            "   ✅ Loaded {} dead-link record(s) in {:.1}s",
            total_dead,
            load_started.elapsed().as_secs_f64()
        );
        let mut dead_links = Vec::new();
        let mut parse_progress = ProgressLine::new("Parsed dead-link metadata:");

        for (idx, record) in dead_records.into_iter().enumerate() {
            if idx > 0 && idx % 2000 == 0 {
                if !parse_progress.is_tty() {
                    info!("Dead-link parse progress: {}/{}", idx, total_dead);
                }
                parse_progress.update(format!("{}/{}", idx, total_dead));
            }
            // Parse the symlink filename using TRaSH format
            let symlink_name = record
                .target_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let meta = parse_trash_filename(symlink_name);

            // Skip stat on sources whose parent directory is missing — avoids
            // hanging on disconnected FUSE mounts when many links are dead.
            let original_size = record
                .source_path
                .parent()
                .map(|p| p.exists())
                .unwrap_or(false)
                .then(|| std::fs::metadata(&record.source_path).ok().map(|m| m.len()))
                .flatten();

            dead_links.push(DeadLink {
                symlink_path: record.target_path.clone(),
                original_source: record.source_path.clone(),
                media_id: record.media_id.clone(),
                media_type: record.media_type,
                content_type: ContentType::from_media_type(record.media_type),
                meta,
                original_size,
            });
        }

        parse_progress.finish(format!("{}/{}", total_dead, total_dead));

        info!("Found {} dead symlinks in database", dead_links.len());
        Ok(dead_links)
    }

    async fn find_fresh_dead_links(
        &self,
        db: &Database,
        allowed_symlink_roots: Option<&[PathBuf]>,
        dry_run: bool,
    ) -> Result<Vec<DeadLink>> {
        info!("Scanning active links for fresh dead-link drift");
        let active_links = db.get_active_links_scoped(allowed_symlink_roots).await?;
        let total_active = active_links.len();
        let mut last_progress = Instant::now();
        let mut progress = ProgressLine::new("Fresh dead-link drift:");
        let mut source_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
        let mut parent_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
        println!(
            "   🔎 Checking {} active link(s) for fresh dead-link drift...",
            total_active
        );
        let mut fresh = Vec::new();

        let worker_count = recommended_drift_workers(total_active);

        if total_active >= PARALLEL_DRIFT_MIN_LINKS && worker_count > 1 {
            println!(
                "   ⚙️  Fresh dead-link drift using {} worker(s)",
                worker_count
            );

            let processed = Arc::new(AtomicUsize::new(0));
            let allowed_roots = allowed_symlink_roots.map(|roots| roots.to_vec());
            let mut buckets = vec![Vec::new(); worker_count];
            for (idx, link) in active_links.into_iter().enumerate() {
                buckets[idx % worker_count].push(link);
            }

            let mut workers = tokio::task::JoinSet::new();
            for bucket in buckets {
                let processed = Arc::clone(&processed);
                let allowed_roots = allowed_roots.clone();
                workers.spawn_blocking(move || {
                    collect_fresh_dead_links_chunk(bucket, allowed_roots, processed.as_ref())
                });
            }

            while !workers.is_empty() {
                match tokio::time::timeout(Duration::from_millis(500), workers.join_next()).await {
                    Ok(Some(result)) => {
                        let chunk_fresh = result??;
                        fresh.extend(chunk_fresh);
                    }
                    Ok(None) => break,
                    Err(_) => {}
                }

                if last_progress.elapsed() >= Duration::from_secs(1) {
                    let done = processed.load(Ordering::Relaxed);
                    let pct = (done as f64 / total_active.max(1) as f64) * 100.0;
                    if !progress.is_tty() {
                        info!(
                            "Fresh dead-link drift progress: {}/{} ({:.1}%)",
                            done, total_active, pct
                        );
                    }
                    progress.update(format!("{}/{} ({:.1}%)", done, total_active, pct));
                    last_progress = Instant::now();
                }
            }
        } else {
            for (idx, link) in active_links.into_iter().enumerate() {
                if idx > 0 && last_progress.elapsed() >= Duration::from_secs(5) {
                    let pct = (idx as f64 / total_active.max(1) as f64) * 100.0;
                    if !progress.is_tty() {
                        info!(
                            "Fresh dead-link drift progress: {}/{} ({:.1}%)",
                            idx, total_active, pct
                        );
                    }
                    progress.update(format!("{}/{} ({:.1}%)", idx, total_active, pct));
                    last_progress = Instant::now();
                }
                if let Some(roots) = allowed_symlink_roots {
                    if !path_under_roots(&link.target_path, roots) {
                        continue;
                    }
                }

                let source_exists = destructive_source_exists(
                    "repair dead-link detection",
                    &link.source_path,
                    &mut source_health_cache,
                    &mut parent_health_cache,
                )?;
                let target_ok = match std::fs::symlink_metadata(&link.target_path) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        std::fs::read_link(&link.target_path)
                            .map(|resolved| resolved == link.source_path)
                            .unwrap_or(false)
                    }
                    _ => false,
                };

                if source_exists && target_ok {
                    continue;
                }

                let symlink_name = link
                    .target_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                let meta = parse_trash_filename(symlink_name);

                fresh.push(DeadLink {
                    symlink_path: link.target_path.clone(),
                    original_source: link.source_path.clone(),
                    media_id: link.media_id.clone(),
                    media_type: link.media_type,
                    content_type: ContentType::from_media_type(link.media_type),
                    meta,
                    original_size: if source_exists {
                        std::fs::metadata(&link.source_path).ok().map(|m| m.len())
                    } else {
                        None
                    },
                });
            }
        }

        if !dry_run {
            for dead_link in &fresh {
                db.mark_dead_path(&dead_link.symlink_path).await?;
                let _ = db
                    .record_link_event_fields(
                        "repair_dead_detected",
                        &dead_link.symlink_path,
                        Some(&dead_link.original_source),
                        Some(dead_link.media_id.as_str()),
                        Some("source_or_target_invalid"),
                    )
                    .await;
            }
        }

        progress.finish(format!("{}/{} (100.0%)", total_active, total_active));

        Ok(fresh)
    }

    /// Scan a directory for dead symlinks (filesystem-based detection).
    pub fn scan_for_dead_symlinks(&self, library_paths: &[PathBuf]) -> Vec<DeadLink> {
        let mut dead = Vec::new();
        let mut visited = 0usize;
        let mut last_progress = Instant::now();
        let mut progress = ProgressLine::new("Filesystem dead-link scan:");

        for lib_path in library_paths {
            for entry in WalkDir::new(lib_path).into_iter().filter_map(|e| e.ok()) {
                visited += 1;
                if last_progress.elapsed() >= Duration::from_secs(5) {
                    if !progress.is_tty() {
                        info!(
                            "Filesystem dead-link scan progress: {} entries visited, {} dead found",
                            visited,
                            dead.len()
                        );
                    }
                    progress.update(format!(
                        "{} entries visited, {} dead found",
                        visited,
                        dead.len()
                    ));
                    last_progress = Instant::now();
                }
                let path = entry.path();

                // Check if it's a symlink
                if let Ok(metadata) = std::fs::symlink_metadata(path) {
                    if metadata.file_type().is_symlink() && !path.exists() {
                        let target = std::fs::read_link(path).unwrap_or_default();
                        let symlink_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        let meta = parse_trash_filename(symlink_name);

                        debug!(
                            "Dead symlink: {:?} → {:?} (parsed: {:?})",
                            path, target, meta.title
                        );
                        dead.push(DeadLink {
                            symlink_path: path.to_path_buf(),
                            original_source: target,
                            media_id: String::new(),
                            media_type: if meta.season.is_some() {
                                MediaType::Tv
                            } else {
                                MediaType::Movie
                            },
                            content_type: ContentType::Tv, // Default, override via CLI
                            meta,
                            original_size: None,
                        });
                    }
                }
            }
        }

        info!("Found {} dead symlinks via filesystem scan", dead.len());
        progress.finish(format!(
            "{} entries visited, {} dead found",
            visited,
            dead.len()
        ));
        dead
    }

    fn find_orphan_dead_links(
        &self,
        libraries: &[LibraryConfig],
        known_targets: &HashSet<PathBuf>,
    ) -> Vec<DeadLink> {
        let library_paths: Vec<_> = libraries.iter().map(|lib| lib.path.clone()).collect();
        let scanned_dead = self.scan_for_dead_symlinks(&library_paths);
        let mut orphaned = Vec::new();

        for mut dead_link in scanned_dead {
            if known_targets.contains(&dead_link.symlink_path) {
                continue;
            }

            let Some(library) = libraries
                .iter()
                .find(|lib| dead_link.symlink_path.starts_with(&lib.path))
            else {
                continue;
            };

            dead_link.media_type = library.media_type;
            dead_link.content_type = library
                .content_type
                .unwrap_or(ContentType::from_media_type(library.media_type));
            dead_link.media_id =
                extract_media_id_from_tagged_ancestors(&dead_link.symlink_path, &library.path)
                    .unwrap_or_default();

            orphaned.push(dead_link);
        }

        info!(
            "Found {} orphan dead symlink(s) on disk outside DB tracking",
            orphaned.len()
        );
        orphaned
    }

    async fn persist_orphan_dead_link(&self, db: &Database, dead_link: &DeadLink) -> Result<()> {
        let record = LinkRecord {
            id: None,
            source_path: dead_link.original_source.clone(),
            target_path: dead_link.symlink_path.clone(),
            media_id: dead_link.media_id.clone(),
            media_type: dead_link.media_type,
            status: LinkStatus::Dead,
            created_at: None,
            updated_at: None,
        };
        db.insert_link(&record).await?;
        let _ = db
            .record_link_event_fields(
                "repair_dead_detected",
                &dead_link.symlink_path,
                Some(&dead_link.original_source),
                Some(dead_link.media_id.as_str()),
                Some("orphan_filesystem_dead_symlink"),
            )
            .await;
        Ok(())
    }

    /// Search for replacement candidates in the given source directories.
    #[allow(dead_code)] // Kept for compatibility with direct/manual repair tooling
    pub fn find_replacements(
        &self,
        dead_link: &DeadLink,
        source_paths: &[PathBuf],
    ) -> Vec<ReplacementCandidate> {
        let catalog = self.build_source_catalog(source_paths, dead_link.content_type);
        self.find_replacements_in_catalog(dead_link, &catalog)
    }

    fn build_source_catalog(
        &self,
        source_paths: &[PathBuf],
        content_type: ContentType,
    ) -> SourceCatalog {
        info!(
            "Building repair source catalog for {:?} from {} source path(s)",
            content_type,
            source_paths.len()
        );

        let mut entries = Vec::new();
        let mut scanned_files = 0usize;

        for source_path in source_paths {
            for entry in WalkDir::new(source_path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
            {
                let path = entry.path();
                scanned_files += 1;
                #[allow(clippy::manual_is_multiple_of)]
                if scanned_files % 100_000 == 0 {
                    info!(
                        "Catalog progress ({:?}): {} filesystem entries scanned",
                        content_type, scanned_files
                    );
                }

                // Only consider video files
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if !VIDEO_EXTENSIONS.contains(&ext.as_str()) {
                    continue;
                }

                let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

                if let Some(item) = self
                    .source_scanner
                    .parse_filename_with_type(path, content_type)
                {
                    let normalized_title = normalize_title(&item.parsed_title);
                    if normalized_title.is_empty() {
                        continue;
                    }

                    entries.push(SourceCandidate {
                        path: path.to_path_buf(),
                        parsed_title: item.parsed_title.clone(),
                        normalized_title,
                        season: item.season,
                        episode: item.episode,
                        quality: item.quality.clone().or_else(|| extract_quality(file_stem)),
                        year: item.year.or_else(|| extract_year(file_stem)),
                        file_size,
                    });
                }
            }
        }

        let mut token_index: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            for token in title_tokens(&entry.normalized_title) {
                token_index.entry(token).or_default().push(idx);
            }
        }
        for idxs in token_index.values_mut() {
            idxs.sort_unstable();
            idxs.dedup();
        }

        info!(
            "Repair source catalog ({:?}) built: {} scanned files, {} parsed candidates, {} token buckets",
            content_type,
            scanned_files,
            entries.len(),
            token_index.len()
        );

        SourceCatalog {
            entries,
            token_index,
        }
    }

    /// Build source catalog from the RD cache database instead of walking the filesystem.
    async fn build_source_catalog_from_cache(
        &self,
        cache: &TorrentCache<'_>,
        source_paths: &[PathBuf],
        content_type: ContentType,
    ) -> SourceCatalog {
        info!(
            "Building repair source catalog ({:?}) from RD cache",
            content_type,
        );

        let mut entries = Vec::new();
        let mut total_files = 0usize;

        for source_path in source_paths {
            let cached_files = match cache.get_files(source_path).await {
                Ok(files) => files,
                Err(e) => {
                    warn!("Failed to read cache for {:?}: {}", source_path, e);
                    continue;
                }
            };

            for (path, file_size) in cached_files {
                total_files += 1;

                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if !VIDEO_EXTENSIONS.contains(&ext.as_str()) {
                    continue;
                }

                // Extract file_stem before parse borrows path, since path is moved into SourceCandidate.
                let file_stem_owned = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                if let Some(item) = self
                    .source_scanner
                    .parse_filename_with_type(&path, content_type)
                {
                    let normalized_title = normalize_title(&item.parsed_title);
                    if normalized_title.is_empty() {
                        continue;
                    }

                    entries.push(SourceCandidate {
                        path,
                        parsed_title: item.parsed_title.clone(),
                        normalized_title,
                        season: item.season,
                        episode: item.episode,
                        quality: item
                            .quality
                            .clone()
                            .or_else(|| extract_quality(&file_stem_owned)),
                        year: item.year.or_else(|| extract_year(&file_stem_owned)),
                        file_size,
                    });
                }
            }
        }

        let mut token_index: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            for token in title_tokens(&entry.normalized_title) {
                token_index.entry(token).or_default().push(idx);
            }
        }
        for idxs in token_index.values_mut() {
            idxs.sort_unstable();
            idxs.dedup();
        }

        info!(
            "Repair source catalog ({:?}) built from cache: {} cached files, {} parsed candidates, {} token buckets",
            content_type,
            total_files,
            entries.len(),
            token_index.len()
        );

        SourceCatalog {
            entries,
            token_index,
        }
    }

    fn find_replacements_in_catalog(
        &self,
        dead_link: &DeadLink,
        catalog: &SourceCatalog,
    ) -> Vec<ReplacementCandidate> {
        let mut candidates = Vec::new();
        let search_title = normalize_title(&dead_link.meta.title);
        let search_tokens = title_tokens(&search_title);

        let mut candidate_indices: HashSet<usize> = HashSet::new();
        for token in &search_tokens {
            if let Some(indices) = catalog.token_index.get(token) {
                for idx in indices {
                    candidate_indices.insert(*idx);
                }
            }
        }
        if candidate_indices.is_empty() && search_tokens.is_empty() {
            candidate_indices.extend(0..catalog.entries.len());
        }

        let threshold = match dead_link.media_type {
            MediaType::Tv => TV_THRESHOLD,
            MediaType::Movie => MOVIE_THRESHOLD,
        };

        for idx in candidate_indices {
            let entry = &catalog.entries[idx];
            let score = calculate_match_score(MatchScoreInput {
                search_title: &search_title,
                candidate_title: &entry.normalized_title,
                search_season: dead_link.meta.season,
                search_episode: dead_link.meta.episode,
                candidate_season: entry.season,
                candidate_episode: entry.episode,
                search_quality: &dead_link.meta.quality,
                candidate_quality: &entry.quality,
                search_size: dead_link.original_size,
                candidate_size: Some(entry.file_size),
                media_type: dead_link.media_type,
                search_year: dead_link.meta.year,
                candidate_year: entry.year,
            });

            if score >= threshold {
                candidates.push(ReplacementCandidate {
                    path: entry.path.clone(),
                    parsed_title: entry.parsed_title.clone(),
                    season: entry.season,
                    episode: entry.episode,
                    quality: entry.quality.clone(),
                    file_size: entry.file_size,
                    score,
                });
            }
        }

        // Sort by score descending
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

        if !candidates.is_empty() {
            let best = &candidates[0];
            debug!(
                "Found {} replacement candidates for '{}' (best: '{}' S{:?}E{:?}, score={:.2}, size={})",
                candidates.len(),
                dead_link.meta.title,
                best.parsed_title,
                best.season,
                best.episode,
                best.score,
                best.file_size,
            );
        }

        candidates
    }

    /// Attempt to repair a dead link by creating a new symlink to the best replacement.
    pub fn repair_link(
        &self,
        dead_link: &DeadLink,
        replacement: &ReplacementCandidate,
        dry_run: bool,
    ) -> Result<()> {
        let symlink_path = &dead_link.symlink_path;
        let new_source = &replacement.path;

        if dry_run {
            info!(
                "[DRY-RUN] Would replace: '{}' → {:?} (score: {:.2}, quality: {:?})",
                dead_link.meta.title, new_source, replacement.score, replacement.quality
            );
            return Ok(());
        }

        // SAFETY: Only ever replace symlinks, never directories or regular files.
        if symlink_path.exists() || std::fs::symlink_metadata(symlink_path).is_ok() {
            assert_symlink_only(symlink_path)?;
        }

        if let Some(parent) = symlink_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Mirror linker semantics: create a temp symlink first, then rename it
        // over the broken link so readers never observe a missing path.
        let temp_path = symlink_path.with_extension("grt");
        let _ = std::fs::remove_file(&temp_path);
        std::os::unix::fs::symlink(new_source, &temp_path)?;
        std::fs::rename(&temp_path, symlink_path)?;
        let repaired_target = std::fs::read_link(symlink_path)?;
        if repaired_target != *new_source {
            anyhow::bail!(
                "repair verification failed: {:?} points to {:?} instead of {:?}",
                symlink_path,
                repaired_target,
                new_source
            );
        }

        info!(
            "Repaired: '{}' → {:?} (score: {:.2})",
            dead_link.meta.title, new_source, replacement.score
        );

        Ok(())
    }

    fn rollback_repair_link(
        &self,
        dead_link: &DeadLink,
        replacement: &ReplacementCandidate,
    ) -> Result<&'static str> {
        let symlink_path = &dead_link.symlink_path;

        if let Ok(meta) = std::fs::symlink_metadata(symlink_path) {
            if !meta.file_type().is_symlink() {
                anyhow::bail!(
                    "rollback refused: {:?} is no longer a symlink",
                    symlink_path
                );
            }

            let current = std::fs::read_link(symlink_path)?;
            if current != replacement.path {
                anyhow::bail!(
                    "rollback refused: {:?} points to unexpected source {:?}",
                    symlink_path,
                    current
                );
            }

            std::fs::remove_file(symlink_path)?;
        }

        if dead_link.original_source.exists() {
            std::os::unix::fs::symlink(&dead_link.original_source, symlink_path)?;
            Ok("filesystem rolled back to original symlink target")
        } else {
            Ok("filesystem rolled back by removing repaired symlink because the original source is still missing")
        }
    }

    /// Full repair pipeline: find dead links, search replacements, repair.
    ///
    /// `skip_paths` — symlink file paths currently being streamed (from Tautulli/Plex).
    /// Dead links whose symlink path exactly matches any of these are skipped.
    #[allow(clippy::too_many_arguments)]
    pub async fn repair_all(
        &self,
        db: &Database,
        source_paths: &[PathBuf],
        dry_run: bool,
        skip_paths: &[String],
        allowed_symlink_roots: Option<&[PathBuf]>,
        libraries: Option<&[LibraryConfig]>,
        cache: Option<&TorrentCache<'_>>,
    ) -> Result<Vec<RepairResult>> {
        ensure_runtime_source_paths_healthy(source_paths, "repair auto").await?;

        let mut dead_links = self.find_dead_links(db, allowed_symlink_roots).await?;
        let existing_dead_targets: HashSet<PathBuf> = dead_links
            .iter()
            .map(|link| link.symlink_path.clone())
            .collect();
        let fresh_dead_links = self
            .find_fresh_dead_links(db, allowed_symlink_roots, dry_run)
            .await?;
        for dead_link in fresh_dead_links {
            if !existing_dead_targets.contains(&dead_link.symlink_path) {
                dead_links.push(dead_link);
            }
        }
        let mut known_dead_targets: HashSet<PathBuf> = dead_links
            .iter()
            .map(|link| link.symlink_path.clone())
            .collect();
        let mut orphan_persisted = 0usize;
        let mut orphan_persist_failures = 0usize;
        if let Some(libraries) = libraries {
            println!(
                "   🔎 Scanning library roots for orphaned dead symlinks not tracked in DB..."
            );
            let orphan_dead_links = self.find_orphan_dead_links(libraries, &known_dead_targets);
            for dead_link in orphan_dead_links {
                if known_dead_targets.insert(dead_link.symlink_path.clone()) {
                    if !dry_run {
                        match self.persist_orphan_dead_link(db, &dead_link).await {
                            Ok(_) => orphan_persisted += 1,
                            Err(err) => {
                                orphan_persist_failures += 1;
                                warn!(
                                    "Could not persist orphan dead link {:?} in DB: {}",
                                    dead_link.symlink_path, err
                                );
                            }
                        }
                    }
                    dead_links.push(dead_link);
                }
            }
        }
        if orphan_persisted > 0 {
            println!(
                "   🗂️ Recorded {} orphan dead symlink(s) in DB so scan/status keep surfacing them until repaired or pruned.",
                orphan_persisted
            );
        }
        if orphan_persist_failures > 0 {
            println!(
                "   ⚠️  Failed to persist {} orphan dead symlink(s) in DB; this repair pass can still act on them right now.",
                orphan_persist_failures
            );
        }
        let dead_count = dead_links.len();
        println!("   🧹 Reconciling dead-link records against filesystem before matching...");
        let reconcile_started = Instant::now();
        let (dead_links, mut results) = self
            .reconcile_stale_dead_links(db, dead_links, dry_run)
            .await;
        let stale_count = results.len();
        println!(
            "   ✅ Reconciled dead links in {:.1}s: {} stale, {} repair-candidates",
            reconcile_started.elapsed().as_secs_f64(),
            stale_count,
            dead_links.len()
        );
        if dead_count > 0 && stale_count > 0 {
            println!(
                "   ℹ️  {} / {} dead records were stale DB entries",
                stale_count, dead_count
            );
        }

        let needs_standard = dead_links
            .iter()
            .any(|d| matches!(d.content_type, ContentType::Tv | ContentType::Movie));
        let needs_anime = dead_links
            .iter()
            .any(|d| matches!(d.content_type, ContentType::Anime));

        let use_cache = cache.is_some();
        let standard_catalog = if needs_standard {
            let started = Instant::now();
            let catalog = if let Some(c) = cache {
                println!("   🧭 Building TV/Movie source catalog from cache...");
                self.build_source_catalog_from_cache(c, source_paths, ContentType::Tv)
                    .await
            } else {
                println!("   🧭 Building TV/Movie source catalog (filesystem walk)...");
                let (ticker_stop, ticker) =
                    spawn_activity_ticker("Building TV/Movie source catalog");
                let catalog = self.build_source_catalog(source_paths, ContentType::Tv);
                stop_activity_ticker(ticker_stop, ticker).await;
                catalog
            };
            println!(
                "   ✅ TV/Movie source catalog ready in {:.1}s ({} candidates{})",
                started.elapsed().as_secs_f64(),
                catalog.entries.len(),
                if use_cache { ", from cache" } else { "" }
            );
            Some(catalog)
        } else {
            None
        };
        let anime_catalog = if needs_anime {
            let started = Instant::now();
            let catalog = if let Some(c) = cache {
                println!("   🧭 Building Anime source catalog from cache...");
                self.build_source_catalog_from_cache(c, source_paths, ContentType::Anime)
                    .await
            } else {
                println!("   🧭 Building Anime source catalog (filesystem walk)...");
                let (ticker_stop, ticker) = spawn_activity_ticker("Building Anime source catalog");
                let catalog = self.build_source_catalog(source_paths, ContentType::Anime);
                stop_activity_ticker(ticker_stop, ticker).await;
                catalog
            };
            println!(
                "   ✅ Anime source catalog ready in {:.1}s ({} candidates{})",
                started.elapsed().as_secs_f64(),
                catalog.entries.len(),
                if use_cache { ", from cache" } else { "" }
            );
            Some(catalog)
        } else {
            None
        };

        let total_dead = dead_links.len();
        let mut progress = ProgressLine::new("Repair progress:");

        for (idx, dead_link) in dead_links.into_iter().enumerate() {
            if idx % 250 == 0 && idx > 0 {
                if !progress.is_tty() {
                    info!("Repair progress: processed {} dead links", idx);
                }
                progress.update(format!(
                    "{}/{} ({:.1}%)",
                    idx,
                    total_dead,
                    (idx as f64 / total_dead.max(1) as f64) * 100.0
                ));
            }
            // Tautulli safe-repair guard: skip files being actively streamed
            if !skip_paths.is_empty()
                && skip_paths
                    .iter()
                    .any(|p| is_streaming_symlink_match(&dead_link.symlink_path, p))
            {
                info!(
                    "Skipping repair of {:?} — currently being streamed",
                    dead_link.symlink_path
                );
                results.push(RepairResult::Skipped {
                    dead_link,
                    reason: "File is currently being streamed".to_string(),
                });
                if let Some(RepairResult::Skipped { dead_link, reason }) = results.last() {
                    let _ = db
                        .record_link_event_fields(
                            "repair_skipped",
                            &dead_link.symlink_path,
                            Some(&dead_link.original_source),
                            Some(dead_link.media_id.as_str()),
                            Some(reason.as_str()),
                        )
                        .await;
                }
                continue;
            }

            let candidates = match dead_link.content_type {
                ContentType::Anime => anime_catalog
                    .as_ref()
                    .map(|catalog| self.find_replacements_in_catalog(&dead_link, catalog))
                    .unwrap_or_default(),
                ContentType::Tv | ContentType::Movie => standard_catalog
                    .as_ref()
                    .map(|catalog| self.find_replacements_in_catalog(&dead_link, catalog))
                    .unwrap_or_default(),
            };

            if let Some(best) = candidates.first() {
                if dry_run {
                    if let Err(e) = self.repair_link(&dead_link, best, true) {
                        warn!("Could not repair {:?}: {}", dead_link.symlink_path, e);
                        let reason = e.to_string();
                        results.push(RepairResult::Unrepairable {
                            dead_link,
                            reason: reason.clone(),
                        });
                        if let Some(RepairResult::Unrepairable { dead_link, reason }) =
                            results.last()
                        {
                            let _ = db
                                .record_link_event_fields(
                                    "repair_failed",
                                    &dead_link.symlink_path,
                                    Some(&dead_link.original_source),
                                    Some(dead_link.media_id.as_str()),
                                    Some(reason.as_str()),
                                )
                                .await;
                        }
                        continue;
                    }
                } else {
                    let record = LinkRecord {
                        id: None,
                        source_path: best.path.clone(),
                        target_path: dead_link.symlink_path.clone(),
                        media_id: dead_link.media_id.clone(),
                        media_type: dead_link.media_type,
                        status: LinkStatus::Active,
                        created_at: None,
                        updated_at: None,
                    };
                    let mut tx = match db.begin().await {
                        Ok(tx) => tx,
                        Err(e) => {
                            warn!(
                                "Could not begin repair DB transaction for {:?}: {}",
                                dead_link.symlink_path, e
                            );
                            let reason = format!("Could not begin DB transaction: {}", e);
                            results.push(RepairResult::Unrepairable {
                                dead_link,
                                reason: reason.clone(),
                            });
                            if let Some(RepairResult::Unrepairable { dead_link, reason }) =
                                results.last()
                            {
                                let _ = db
                                    .record_link_event_fields(
                                        "repair_failed",
                                        &dead_link.symlink_path,
                                        Some(&dead_link.original_source),
                                        Some(dead_link.media_id.as_str()),
                                        Some(reason.as_str()),
                                    )
                                    .await;
                            }
                            continue;
                        }
                    };
                    if let Err(e) = db.insert_link_in_tx(&record, &mut tx).await {
                        warn!(
                            "Could not stage repaired DB record for {:?}: {}",
                            dead_link.symlink_path, e
                        );
                        let reason = format!("Could not stage DB update: {}", e);
                        results.push(RepairResult::Unrepairable {
                            dead_link,
                            reason: reason.clone(),
                        });
                        if let Some(RepairResult::Unrepairable { dead_link, reason }) =
                            results.last()
                        {
                            let _ = db
                                .record_link_event_fields(
                                    "repair_failed",
                                    &dead_link.symlink_path,
                                    Some(&dead_link.original_source),
                                    Some(dead_link.media_id.as_str()),
                                    Some(reason.as_str()),
                                )
                                .await;
                        }
                        continue;
                    }
                    if let Err(e) = self.repair_link(&dead_link, best, false) {
                        warn!("Could not repair {:?}: {}", dead_link.symlink_path, e);
                        let reason = e.to_string();
                        results.push(RepairResult::Unrepairable {
                            dead_link,
                            reason: reason.clone(),
                        });
                        if let Some(RepairResult::Unrepairable { dead_link, reason }) =
                            results.last()
                        {
                            let _ = db
                                .record_link_event_fields(
                                    "repair_failed",
                                    &dead_link.symlink_path,
                                    Some(&dead_link.original_source),
                                    Some(dead_link.media_id.as_str()),
                                    Some(reason.as_str()),
                                )
                                .await;
                        }
                        continue;
                    }
                    if let Err(e) = tx.commit().await {
                        let rollback_note = match self.rollback_repair_link(&dead_link, best) {
                            Ok(note) => note.to_string(),
                            Err(rollback_err) => {
                                format!("filesystem rollback failed: {}", rollback_err)
                            }
                        };
                        warn!(
                            "Repair partially failed for {:?}: DB commit failed: {} ({})",
                            dead_link.symlink_path, e, rollback_note
                        );
                        results.push(RepairResult::Unrepairable {
                            dead_link,
                            reason: format!(
                                "DB commit failed after repair: {} ({})",
                                e, rollback_note
                            ),
                        });
                        if let Some(RepairResult::Unrepairable { dead_link, reason }) =
                            results.last()
                        {
                            let _ = db
                                .record_link_event_fields(
                                    "repair_failed",
                                    &dead_link.symlink_path,
                                    Some(&dead_link.original_source),
                                    Some(dead_link.media_id.as_str()),
                                    Some(reason.as_str()),
                                )
                                .await;
                        }
                        continue;
                    }
                }

                results.push(RepairResult::Repaired {
                    replacement: best.path.clone(),
                    dead_link,
                });
                if let Some(RepairResult::Repaired {
                    dead_link,
                    replacement,
                }) = results.last()
                {
                    let _ = db
                        .record_link_event_fields(
                            if dry_run {
                                "repair_preview"
                            } else {
                                "repair_applied"
                            },
                            &dead_link.symlink_path,
                            Some(replacement),
                            Some(dead_link.media_id.as_str()),
                            None,
                        )
                        .await;
                }
            } else {
                let reason = format!(
                    "No replacement found (title='{}', year={:?}, imdb={:?})",
                    dead_link.meta.title, dead_link.meta.year, dead_link.meta.imdb_id
                );
                debug!("{}", reason);
                results.push(RepairResult::Unrepairable { dead_link, reason });
                if let Some(RepairResult::Unrepairable { dead_link, reason }) = results.last() {
                    let _ = db
                        .record_link_event_fields(
                            "repair_failed",
                            &dead_link.symlink_path,
                            Some(&dead_link.original_source),
                            Some(dead_link.media_id.as_str()),
                            Some(reason.as_str()),
                        )
                        .await;
                }
            }
        }

        progress.finish(format!("{}/{} (100.0%)", total_dead, total_dead));

        let repaired = results
            .iter()
            .filter(|r| matches!(r, RepairResult::Repaired { .. }))
            .count();
        let failed = results
            .iter()
            .filter(|r| matches!(r, RepairResult::Unrepairable { .. }))
            .count();
        let skipped = results
            .iter()
            .filter(|r| matches!(r, RepairResult::Skipped { .. }))
            .count();
        let stale = results
            .iter()
            .filter(|r| matches!(r, RepairResult::Stale { .. }))
            .count();
        info!(
            "Repair complete: {} repaired, {} unrepairable, {} skipped, {} stale",
            repaired, failed, skipped, stale
        );

        Ok(results)
    }
}

// ─── Activity ticker ─────────────────────────────────────────────────

fn spawn_activity_ticker(
    label: &'static str,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let started = Instant::now();
        let mut progress = ProgressLine::new(label);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    progress.update(format!("{}s elapsed", started.elapsed().as_secs()));
                }
                _ = &mut stop_rx => {
                    progress.finish(format!("completed in {:.1}s", started.elapsed().as_secs_f64()));
                    break;
                }
            }
        }
    });
    (stop_tx, handle)
}

async fn stop_activity_ticker(
    stop_tx: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
) {
    let _ = stop_tx.send(());
    let _ = handle.await;
}

// ─── Tests ───────────────────────────────────────────────────────────

mod scoring;
mod trash;

#[cfg(test)]
mod tests;
