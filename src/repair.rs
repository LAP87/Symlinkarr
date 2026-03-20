use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Result;
use regex::Regex;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::cache::TorrentCache;
use crate::config::ContentType;
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MediaType};
use crate::source_scanner::SourceScanner;
use crate::utils::{cached_source_exists, path_under_roots, ProgressLine, VIDEO_EXTENSIONS};

/// Minimum score threshold for TV replacements (title + season + episode required)
const TV_THRESHOLD: f64 = 0.75;
/// Minimum score threshold for movie replacements (title + year is enough)
const MOVIE_THRESHOLD: f64 = 0.55;
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
) -> Vec<DeadLink> {
    let mut source_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
    let mut parent_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
    let mut fresh = Vec::new();

    for link in links {
        if let Some(roots) = allowed_symlink_roots.as_ref() {
            if !path_under_roots(&link.target_path, roots) {
                processed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }

        let source_exists = cached_source_exists(
            &link.source_path,
            &mut source_exists_cache,
            &mut parent_exists_cache,
        );
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

    fresh
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

/// Parse a TRaSH Guide-style filename into structured metadata.
///
/// Handles both Sonarr and Radarr naming conventions:
///   Sonarr: "Title (Year) - S01E03 - Episode Title [Quality][Codec]-Group"
///   Radarr: "Title (Year) {imdb-ttXXX} [Quality][Codec]-Group"
pub fn parse_trash_filename(filename: &str) -> TrashMeta {
    // Strip extension
    let name = filename
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(filename);

    // Extract IMDB ID: {imdb-tt1234567}
    let imdb_id = trash_imdb_regex().captures(name).map(|c| c[1].to_string());

    // Extract season/episode: S01E03
    let (season, episode) = trash_season_episode_regex()
        .captures(name)
        .map(|c| (c[1].parse::<u32>().ok(), c[2].parse::<u32>().ok()))
        .unwrap_or((None, None));

    // Extract quality: [WEBDL-1080p], [Bluray-2160p], or standalone 1080p/2160p/720p
    let quality = trash_quality_regex().captures(name).map(|c| {
        let res = c.get(1).or(c.get(2)).unwrap().as_str();
        format!("{}p", res)
    });

    // Extract year: (2008) — first 4-digit number in parentheses
    let year = trash_year_regex()
        .captures(name)
        .and_then(|c| c[1].parse::<u32>().ok());

    // Extract title: everything before the first (year), S01E, or [quality] marker
    let title = if let Some(m) = trash_title_end_regex().find(name) {
        name[..m.start()]
            .trim()
            .trim_end_matches(" -")
            .trim()
            .to_string()
    } else {
        name.trim().to_string()
    };

    TrashMeta {
        title,
        year,
        season,
        episode,
        quality,
        imdb_id,
    }
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
        let mut source_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
        let mut parent_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
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
                        let chunk_fresh = result?;
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

                let source_exists = cached_source_exists(
                    &link.source_path,
                    &mut source_exists_cache,
                    &mut parent_exists_cache,
                );
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

        // SAFETY: Only remove symlinks, never directories or real files
        if symlink_path.exists() || std::fs::symlink_metadata(symlink_path).is_ok() {
            assert_symlink_only(symlink_path)?;
            std::fs::remove_file(symlink_path)?;
        }

        // SAFETY: Ensure we're creating a symlink, not overwriting a directory
        if let Ok(meta) = std::fs::symlink_metadata(symlink_path) {
            if meta.file_type().is_dir() {
                anyhow::bail!(
                    "SAFETY GUARD: Cannot create symlink, {:?} is a directory.",
                    symlink_path
                );
            }
        }

        // Create new symlink
        std::os::unix::fs::symlink(new_source, symlink_path)?;

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
    ) -> Result<()> {
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

        std::os::unix::fs::symlink(&dead_link.original_source, symlink_path)?;
        Ok(())
    }

    /// Full repair pipeline: find dead links, search replacements, repair.
    ///
    /// `skip_paths` — symlink file paths currently being streamed (from Tautulli/Plex).
    /// Dead links whose symlink path exactly matches any of these are skipped.
    pub async fn repair_all(
        &self,
        db: &Database,
        source_paths: &[PathBuf],
        dry_run: bool,
        skip_paths: &[String],
        allowed_symlink_roots: Option<&[PathBuf]>,
        cache: Option<&TorrentCache<'_>>,
    ) -> Result<Vec<RepairResult>> {
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
                if let Err(e) = self.repair_link(&dead_link, best, dry_run) {
                    warn!("Could not repair {:?}: {}", dead_link.symlink_path, e);
                    let reason = e.to_string();
                    results.push(RepairResult::Unrepairable {
                        dead_link,
                        reason: reason.clone(),
                    });
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
                } else {
                    // Update DB status back to Active
                    if !dry_run {
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
                        if let Err(e) = db.insert_link(&record).await {
                            let rollback_note = match self.rollback_repair_link(&dead_link, best) {
                                Ok(_) => {
                                    "filesystem rolled back to original dead symlink".to_string()
                                }
                                Err(rollback_err) => {
                                    format!("filesystem rollback failed: {}", rollback_err)
                                }
                            };
                            warn!(
                                "Repair partially failed for {:?}: DB update failed: {} ({})",
                                dead_link.symlink_path, e, rollback_note
                            );
                            results.push(RepairResult::Unrepairable {
                                dead_link,
                                reason: format!(
                                    "DB update failed after repair: {} ({})",
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

// ─── Scoring helpers ─────────────────────────────────────────────────

/// Normalize a title for comparison (lowercase, strip special chars, collapse spaces)
fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace(['.', '_', '-'], " ")
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_tokens(normalized_title: &str) -> Vec<String> {
    normalized_title
        .split_whitespace()
        .filter(|token| token.len() >= 2 && !token_is_lookup_noise(token))
        .map(|token| token.to_string())
        .collect()
}

fn token_is_lookup_noise(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();

    if let Some(num) = lower.strip_suffix('p') {
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }

    if let Some(rest) = lower.strip_prefix('s') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    if let Some(rest) = lower.strip_prefix('e') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }

    matches!(
        lower.as_str(),
        "x264" | "x265" | "hevc" | "webrip" | "webdl" | "bluray" | "bdrip" | "hdtv"
    )
}

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

fn trash_imdb_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{imdb-(tt\d+)\}").unwrap())
}

fn trash_season_episode_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[Ss](\d{1,2})[Ee](\d{1,3})").unwrap())
}

fn trash_quality_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:\[(?:[\w\s-]*?)?(2160|1080|720|480)p[^\]]*\]|(2160|1080|720|480)p)")
            .unwrap()
    })
}

fn trash_year_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\((\d{4})\)").unwrap())
}

fn trash_title_end_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(\(\d{4}\)|[Ss]\d{1,2}[Ee]|\[|\{imdb-)").unwrap())
}

fn quality_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(2160|1080|720|480)p").unwrap())
}

fn year_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(?((?:19|20)\d{2})\)?").unwrap())
}

/// Extract quality (e.g., "1080p") from a torrent-style filename
fn extract_quality(filename: &str) -> Option<String> {
    quality_tag_regex()
        .captures(filename)
        .map(|c| format!("{}p", &c[1]))
}

/// Extract year (e.g., "(2008)") from a filename string.
fn extract_year(filename: &str) -> Option<u32> {
    year_tag_regex()
        .captures(filename)
        .and_then(|c| c[1].parse::<u32>().ok())
}

/// Calculate match score between a dead link and a candidate replacement.
///
/// Scoring breakdown (max 1.0):
///   - Title match:    0.35 (exact) / 0.20 (containment)
///   - Season match:   0.25 (TV only, required)
///   - Episode match:  0.25 (TV only, required)
///   - Year match:     0.15 (movies only, bonus)
///   - Quality match:  0.10
///   - Size proximity: 0.05
struct MatchScoreInput<'a> {
    search_title: &'a str,
    candidate_title: &'a str,
    search_season: Option<u32>,
    search_episode: Option<u32>,
    candidate_season: Option<u32>,
    candidate_episode: Option<u32>,
    search_quality: &'a Option<String>,
    candidate_quality: &'a Option<String>,
    search_size: Option<u64>,
    candidate_size: Option<u64>,
    media_type: MediaType,
    search_year: Option<u32>,
    candidate_year: Option<u32>,
}

fn calculate_match_score(input: MatchScoreInput<'_>) -> f64 {
    let mut score = 0.0;
    let MatchScoreInput {
        search_title,
        candidate_title,
        search_season,
        search_episode,
        candidate_season,
        candidate_episode,
        search_quality,
        candidate_quality,
        search_size,
        candidate_size,
        media_type,
        search_year,
        candidate_year,
    } = input;

    // ── Title match (0.35 max) ──
    if search_title == candidate_title {
        score += 0.35;
    } else if search_title.contains(candidate_title) || candidate_title.contains(search_title) {
        let ratio = search_title.len().min(candidate_title.len()) as f64
            / search_title.len().max(candidate_title.len()) as f64;
        score += 0.20 * ratio;
    } else {
        return 0.0; // No title match → discard
    }

    // ── Season match (0.25 max, mandatory for TV) ──
    match (search_season, candidate_season) {
        (Some(a), Some(b)) if a == b => score += 0.25,
        (Some(_), Some(_)) => return 0.0, // Wrong season → discard
        (Some(_), None) if media_type == MediaType::Tv => return 0.0, // TV needs season
        _ => {}
    }

    // ── Episode match (0.25 max, mandatory for TV) ──
    match (search_episode, candidate_episode) {
        (Some(a), Some(b)) if a == b => score += 0.25,
        (Some(_), Some(_)) => return 0.0, // Wrong episode → discard
        (Some(_), None) if media_type == MediaType::Tv => return 0.0, // TV needs episode
        _ => {}
    }

    // ── Year match (0.15 bonus for movies) ──
    if media_type == MediaType::Movie {
        match (search_year, candidate_year) {
            (Some(y1), Some(y2)) if y1 == y2 => score += 0.15,
            (Some(_), Some(_)) => return 0.0, // Wrong year on a movie → discard
            _ => {}                           // Unknown year, no penalty
        }
    }

    // ── Quality match (0.10 bonus) ──
    match (search_quality, candidate_quality) {
        (Some(q1), Some(q2)) if q1.to_lowercase() == q2.to_lowercase() => {
            score += 0.10;
        }
        _ => {} // No penalty for unknown quality
    }

    // ── File size proximity (0.05 bonus) ──
    if let (Some(s1), Some(s2)) = (search_size, candidate_size) {
        if s1 > 0 && s2 > 0 {
            let ratio = s1.min(s2) as f64 / s1.max(s2) as f64;
            if ratio > (1.0 - SIZE_TOLERANCE) {
                score += 0.05;
            }
        }
    }

    score
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TRaSH parser tests ──

    #[test]
    fn test_parse_trash_sonarr_format() {
        let meta = parse_trash_filename(
            "Breaking Bad (2008) - S01E03 - ...And the Bag's in the River [WEBDL-1080p][x264]-GROUP.mkv",
        );
        assert_eq!(meta.title, "Breaking Bad");
        assert_eq!(meta.year, Some(2008));
        assert_eq!(meta.season, Some(1));
        assert_eq!(meta.episode, Some(3));
        assert_eq!(meta.quality, Some("1080p".to_string()));
        assert!(meta.imdb_id.is_none());
    }

    #[test]
    fn test_parse_trash_radarr_format() {
        let meta = parse_trash_filename(
            "The Matrix (1999) {imdb-tt0133093} [Bluray-2160p][DV HDR10][DTS-HD MA 5.1][x265]-GROUP.mkv",
        );
        assert_eq!(meta.title, "The Matrix");
        assert_eq!(meta.year, Some(1999));
        assert_eq!(meta.imdb_id, Some("tt0133093".to_string()));
        assert_eq!(meta.quality, Some("2160p".to_string()));
        assert!(meta.season.is_none());
    }

    #[test]
    fn test_parse_trash_minimal() {
        let meta = parse_trash_filename("Some Movie (2020).mkv");
        assert_eq!(meta.title, "Some Movie");
        assert_eq!(meta.year, Some(2020));
    }

    #[test]
    fn test_parse_trash_episode_only() {
        let meta = parse_trash_filename("My Show - S02E15 - Episode Title.mkv");
        assert_eq!(meta.title, "My Show");
        assert_eq!(meta.season, Some(2));
        assert_eq!(meta.episode, Some(15));
    }

    // ── Title normalization ──

    #[test]
    fn test_normalize_title() {
        assert_eq!(normalize_title("Breaking.Bad"), "breaking bad");
        assert_eq!(
            normalize_title("The_Big_Bang_Theory"),
            "the big bang theory"
        );
        assert_eq!(normalize_title("Game-of-Thrones"), "game of thrones");
    }

    #[test]
    fn test_title_tokens_filters_noise() {
        let tokens = title_tokens("breaking bad s01 1080p");
        assert!(tokens.contains(&"breaking".to_string()));
        assert!(tokens.contains(&"bad".to_string()));
        assert!(!tokens.contains(&"s01".to_string()));
        assert!(!tokens.contains(&"1080p".to_string()));
    }

    // ── Quality extraction ──

    #[test]
    fn test_extract_quality() {
        assert_eq!(
            extract_quality("Movie.2160p.BluRay"),
            Some("2160p".to_string())
        );
        assert_eq!(
            extract_quality("Show.S01E01.720p.HDTV"),
            Some("720p".to_string())
        );
        assert_eq!(extract_quality("no quality here"), None);
    }

    // ── Scoring tests ──

    #[test]
    fn test_score_perfect_tv_match() {
        let score = calculate_match_score(MatchScoreInput {
            search_title: "breaking bad",
            candidate_title: "breaking bad",
            search_season: Some(1),
            search_episode: Some(3),
            candidate_season: Some(1),
            candidate_episode: Some(3),
            search_quality: &Some("1080p".to_string()),
            candidate_quality: &Some("1080p".to_string()),
            search_size: Some(4_000_000_000),
            candidate_size: Some(4_200_000_000),
            media_type: MediaType::Tv,
            search_year: None,
            candidate_year: None,
        });
        assert_eq!(score, 1.0); // 0.35 + 0.25 + 0.25 + 0.10 + 0.05
    }

    #[test]
    fn test_score_tv_wrong_episode() {
        let score = calculate_match_score(MatchScoreInput {
            search_title: "breaking bad",
            candidate_title: "breaking bad",
            search_season: Some(1),
            search_episode: Some(3),
            candidate_season: Some(1),
            candidate_episode: Some(5),
            search_quality: &None,
            candidate_quality: &None,
            search_size: None,
            candidate_size: None,
            media_type: MediaType::Tv,
            search_year: None,
            candidate_year: None,
        });
        assert_eq!(score, 0.0); // Wrong episode = instant discard
    }

    #[test]
    fn test_score_tv_missing_season_info() {
        let score = calculate_match_score(MatchScoreInput {
            search_title: "breaking bad",
            candidate_title: "breaking bad",
            search_season: Some(1),
            search_episode: Some(3),
            candidate_season: None,
            candidate_episode: None,
            search_quality: &None,
            candidate_quality: &None,
            search_size: None,
            candidate_size: None,
            media_type: MediaType::Tv,
            search_year: None,
            candidate_year: None,
        });
        assert_eq!(score, 0.0); // TV without S/E info → discarded
    }

    #[test]
    fn test_score_movie_title_only() {
        let score = calculate_match_score(MatchScoreInput {
            search_title: "the matrix",
            candidate_title: "the matrix",
            search_season: None,
            search_episode: None,
            candidate_season: None,
            candidate_episode: None,
            search_quality: &None,
            candidate_quality: &None,
            search_size: None,
            candidate_size: None,
            media_type: MediaType::Movie,
            search_year: None,
            candidate_year: None,
        });
        assert_eq!(score, 0.35); // Title match only — below movie threshold (0.55)
    }

    #[test]
    fn test_score_movie_with_quality() {
        let score = calculate_match_score(MatchScoreInput {
            search_title: "the matrix",
            candidate_title: "the matrix",
            search_season: None,
            search_episode: None,
            candidate_season: None,
            candidate_episode: None,
            search_quality: &Some("2160p".to_string()),
            candidate_quality: &Some("2160p".to_string()),
            search_size: Some(50_000_000_000),
            candidate_size: Some(48_000_000_000),
            media_type: MediaType::Movie,
            search_year: Some(1999),
            candidate_year: Some(1999),
        });
        // 0.35 + 0.15 + 0.10 + 0.05 = 0.65, now above movie threshold with year match
        assert!(score >= MOVIE_THRESHOLD);
    }

    #[test]
    fn test_score_no_title_match() {
        let score = calculate_match_score(MatchScoreInput {
            search_title: "breaking bad",
            candidate_title: "game of thrones",
            search_season: Some(1),
            search_episode: Some(1),
            candidate_season: Some(1),
            candidate_episode: Some(1),
            search_quality: &None,
            candidate_quality: &None,
            search_size: None,
            candidate_size: None,
            media_type: MediaType::Tv,
            search_year: None,
            candidate_year: None,
        });
        assert_eq!(score, 0.0);
    }

    // ── Filesystem tests ──

    #[test]
    fn test_scan_empty_dir_for_dead_symlinks() {
        let repairer = Repairer::new();
        let dir = tempfile::TempDir::new().unwrap();
        let dead = repairer.scan_for_dead_symlinks(&[dir.path().to_path_buf()]);
        assert!(dead.is_empty());
    }

    #[test]
    fn test_scan_finds_dead_symlink_with_trash_name() {
        let repairer = Repairer::new();
        let dir = tempfile::TempDir::new().unwrap();

        // Create a dead symlink with TRaSH-format name
        let link_path = dir.path().join(
            "Breaking Bad (2008) - S01E03 - And the Bags in the River [WEBDL-1080p][x264].mkv",
        );
        std::os::unix::fs::symlink("/nonexistent/file.mkv", &link_path).unwrap();

        let dead = repairer.scan_for_dead_symlinks(&[dir.path().to_path_buf()]);
        assert_eq!(dead.len(), 1);

        // Verify TRaSH parsing enriched the dead link
        assert_eq!(dead[0].meta.title, "Breaking Bad");
        assert_eq!(dead[0].meta.year, Some(2008));
        assert_eq!(dead[0].meta.season, Some(1));
        assert_eq!(dead[0].meta.episode, Some(3));
        assert_eq!(dead[0].meta.quality, Some("1080p".to_string()));
        assert_eq!(dead[0].media_type, MediaType::Tv);
    }

    #[test]
    fn test_build_source_catalog_preserves_parsed_anime_quality() {
        let repairer = Repairer::new();
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir
            .path()
            .join("[Okay-Subs] Dan Da Dan - 01 (BD 1080p) [3F475D62].mkv");
        std::fs::write(&file, b"demo").unwrap();

        let catalog =
            repairer.build_source_catalog(&[dir.path().to_path_buf()], ContentType::Anime);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].quality.as_deref(), Some("1080p"));
    }

    #[test]
    fn test_streaming_guard_exact_path_match() {
        let path = PathBuf::from("/mnt/plex/anime/Show/Season 01/Show - S01E01.mkv");
        assert!(is_streaming_symlink_match(
            &path,
            "/mnt/plex/anime/Show/Season 01/Show - S01E01.mkv"
        ));
    }

    #[test]
    fn test_streaming_guard_does_not_match_substring() {
        let path = PathBuf::from("/mnt/plex/anime/Show/Season 01/Show - S01E01.mkv");
        assert!(!is_streaming_symlink_match(
            &path,
            "/mnt/plex/anime/Show/Season 01/Show - S01E0"
        ));
    }

    #[test]
    fn test_rollback_repair_restores_original_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repairer = Repairer::new();
        let symlink_path = tmp.path().join("Show - S01E01.mkv");
        let original_source = tmp.path().join("old-source.mkv");
        let replacement_source = tmp.path().join("new-source.mkv");

        std::os::unix::fs::symlink(&replacement_source, &symlink_path).unwrap();

        let dead_link = DeadLink {
            symlink_path: symlink_path.clone(),
            original_source: original_source.clone(),
            media_id: "tvdb-123".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
            meta: parse_trash_filename("Show - S01E01.mkv"),
            original_size: None,
        };
        let replacement = ReplacementCandidate {
            path: replacement_source.clone(),
            parsed_title: "show".to_string(),
            season: Some(1),
            episode: Some(1),
            quality: None,
            file_size: 0,
            score: 1.0,
        };

        repairer
            .rollback_repair_link(&dead_link, &replacement)
            .unwrap();

        assert_eq!(std::fs::read_link(&symlink_path).unwrap(), original_source);
    }

    #[tokio::test]
    async fn test_repair_all_detects_fresh_dead_links_without_prior_scan() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let library_root = tmp.path().join("library");
        let target = library_root.join("Show/Season 01/Show - S01E01.mkv");
        let missing_source = tmp.path().join("rd/missing-source.mkv");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&missing_source, &target).unwrap();

        let record = LinkRecord {
            id: None,
            source_path: missing_source,
            target_path: target.clone(),
            media_id: "tvdb-999".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };
        db.insert_link(&record).await.unwrap();

        let repairer = Repairer::new();
        let results = repairer
            .repair_all(&db, &[], false, &[], Some(&[library_root]), None)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RepairResult::Unrepairable { .. }));

        let dead_after = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
        assert_eq!(dead_after.len(), 1);
        assert_eq!(dead_after[0].target_path, target);
    }

    #[tokio::test]
    async fn test_repair_all_classifies_missing_symlink_as_stale_in_dry_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let target = tmp.path().join("library/Show/Season 01/S01E01.mkv");
        let source = tmp.path().join("rd/source.mkv");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();

        let record = LinkRecord {
            id: None,
            source_path: source.clone(),
            target_path: target.clone(),
            media_id: "tvdb-123".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };
        db.insert_link(&record).await.unwrap();
        db.mark_dead_path(&target).await.unwrap();

        let repairer = Repairer::new();
        let library_root = tmp.path().join("library");
        let results = repairer
            .repair_all(&db, &[], true, &[], Some(&[library_root]), None)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RepairResult::Stale { .. }));
        let dead_after = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
        assert_eq!(dead_after.len(), 1);
    }

    #[tokio::test]
    async fn test_repair_all_marks_stale_dead_links_as_removed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let target = tmp.path().join("library/Show/Season 01/S01E02.mkv");
        let source = tmp.path().join("rd/source2.mkv");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();

        let record = LinkRecord {
            id: None,
            source_path: source.clone(),
            target_path: target.clone(),
            media_id: "tvdb-456".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };
        db.insert_link(&record).await.unwrap();
        db.mark_dead_path(&target).await.unwrap();

        let repairer = Repairer::new();
        let library_root = tmp.path().join("library");
        let results = repairer
            .repair_all(&db, &[], false, &[], Some(&[library_root]), None)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], RepairResult::Stale { .. }));
        let removed = db.get_links_by_status(LinkStatus::Removed).await.unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].target_path, target);
    }
}
