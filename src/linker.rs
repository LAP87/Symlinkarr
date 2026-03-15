use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MatchResult, MediaType};
use crate::utils::{cached_source_exists, path_under_roots, user_println, ProgressLine};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkWriteOutcome {
    Created,
    Updated,
    Skipped,
}

#[derive(Debug, Clone, Default)]
pub struct LinkProcessSummary {
    pub created: u64,
    pub updated: u64,
    pub skipped: u64,
    pub refresh_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DeadLinkSummary {
    pub dead_marked: u64,
    pub removed: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkWriteResult {
    outcome: LinkWriteOutcome,
    refresh_path: Option<PathBuf>,
}

/// Creates and manages symlinks from Real-Debrid sources to Plex library.
pub struct Linker {
    dry_run: bool,
    #[allow(dead_code)] // Reserved for strict-mode-specific safeguards
    strict_mode: bool,
    reconcile_links: bool,
    naming_template: String,
}

impl Linker {
    #[allow(dead_code)] // Kept for backward compatibility with older call sites
    pub fn new(dry_run: bool, strict_mode: bool, naming_template: &str) -> Self {
        Self::new_with_options(dry_run, strict_mode, naming_template, true)
    }

    pub fn new_with_options(
        dry_run: bool,
        strict_mode: bool,
        naming_template: &str,
        reconcile_links: bool,
    ) -> Self {
        Self {
            dry_run,
            strict_mode,
            reconcile_links,
            naming_template: naming_template.to_string(),
        }
    }

    /// Process a list of matches and create/update symlinks.
    pub async fn process_matches(
        &self,
        matches: &[MatchResult],
        db: &Database,
    ) -> Result<LinkProcessSummary> {
        info!("Processing {} matched items...", matches.len());
        let mut summary = LinkProcessSummary::default();
        let total = matches.len();
        let mut progress = ProgressLine::new(if self.dry_run {
            "Dry-run link progress:"
        } else {
            "Link progress:"
        });
        let mut source_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
        let mut parent_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
        let mut existing_links = self.preload_existing_links_for_matches(db, matches).await?;

        for (idx, m) in matches.iter().enumerate() {
            if idx > 0 && idx % 500 == 0 {
                progress.update(format!(
                    "{}/{} (created={}, updated={}, skipped={})",
                    idx, total, summary.created, summary.updated, summary.skipped
                ));
            }

            let target_path = self.build_target_path(m)?;

            if !cached_source_exists(
                &m.source_item.path,
                &mut source_exists_cache,
                &mut parent_exists_cache,
            ) {
                let media_id = m.library_item.id.to_string();
                debug!(
                    "Skipping link creation because source is missing before write: {:?}",
                    m.source_item.path
                );
                self.log_link_event(
                    db,
                    "skipped",
                    &target_path,
                    Some(&m.source_item.path),
                    Some(media_id.as_str()),
                    Some("source_missing_before_link"),
                )
                .await;
                summary.skipped += 1;
                continue;
            }

            match self
                .create_link(m, &target_path, db, &mut existing_links)
                .await
            {
                Ok(result) => {
                    match result.outcome {
                        LinkWriteOutcome::Created => summary.created += 1,
                        LinkWriteOutcome::Updated => summary.updated += 1,
                        LinkWriteOutcome::Skipped => summary.skipped += 1,
                    }
                    if let Some(path) = result.refresh_path {
                        summary.refresh_paths.push(path);
                    }
                }
                Err(e) => warn!("Failed to create link for {:?}: {}", m.source_item.path, e),
            }
        }
        progress.finish(format!(
            "{}/{} (created={}, updated={}, skipped={})",
            total, total, summary.created, summary.updated, summary.skipped
        ));

        info!(
            "Done: {} created, {} updated, {} skipped{}",
            summary.created,
            summary.updated,
            summary.skipped,
            if self.dry_run { " (dry-run)" } else { "" }
        );
        Ok(summary)
    }

    /// Create a single symlink for a match result.
    async fn create_link(
        &self,
        m: &MatchResult,
        target_path: &PathBuf,
        db: &Database,
        existing_links: &mut HashMap<PathBuf, LinkRecord>,
    ) -> Result<LinkWriteResult> {
        let media_id = m.library_item.id.to_string();

        // Check if link already exists in DB with the SAME source path
        let existing_link = existing_links.get(target_path).cloned();
        let mut is_update = false;

        if let Some(link) = existing_link.as_ref() {
            if link.status == LinkStatus::Active && link.source_path == m.source_item.path {
                // Verify on-disk state too; DB can drift.
                if let Ok(meta) = std::fs::symlink_metadata(target_path) {
                    if meta.file_type().is_symlink() {
                        if let Ok(current_source) = std::fs::read_link(target_path) {
                            if current_source == m.source_item.path {
                                debug!("Symlink already exists and is correct: {:?}", target_path);
                                self.log_link_event(
                                    db,
                                    "skipped",
                                    target_path,
                                    Some(&m.source_item.path),
                                    Some(media_id.as_str()),
                                    Some("already_correct"),
                                )
                                .await;
                                return Ok(LinkWriteResult {
                                    outcome: LinkWriteOutcome::Skipped,
                                    refresh_path: None,
                                });
                            }
                        }
                    }
                }
                if self.dry_run {
                    debug!(
                        "Would recreate symlink: {:?} (db active, disk drifted)",
                        target_path
                    );
                } else {
                    info!(
                        "DB link was active but on-disk target was missing/incorrect; recreating {:?}",
                        target_path
                    );
                }
                is_update = true;
            }
            // If it exists but points elsewhere (or is dead), we continue and overwrite it
            if link.source_path != m.source_item.path {
                if self.reconcile_links {
                    if self.dry_run {
                        debug!("Would update symlink: {:?} (source changed)", target_path);
                    } else {
                        info!("Updating symlink: {:?} (source changed)", target_path);
                    }
                    is_update = true;
                } else {
                    debug!(
                        "Reconciliation disabled; treating changed source as recreated link: {:?}",
                        target_path
                    );
                }
            }
        }

        // SAFETY: Inspect existing target and only overwrite symlinks.
        if let Ok(meta) = std::fs::symlink_metadata(target_path) {
            if meta.file_type().is_dir() {
                anyhow::bail!(
                    "SAFETY GUARD: {:?} is a directory. Symlinkarr does not create symlinks \
                     over directories — they are managed by *arr apps.",
                    target_path
                );
            }

            if meta.file_type().is_symlink() {
                if let Ok(current_source) = std::fs::read_link(target_path) {
                    if current_source == m.source_item.path {
                        let has_active_db_record = existing_link
                            .as_ref()
                            .map(|link| {
                                link.status == LinkStatus::Active
                                    && link.source_path == m.source_item.path
                            })
                            .unwrap_or(false);
                        if !has_active_db_record {
                            debug!(
                                "Backfilling DB record for correct on-disk symlink: {:?}",
                                target_path
                            );
                            let record = LinkRecord {
                                id: None,
                                source_path: m.source_item.path.clone(),
                                target_path: target_path.clone(),
                                media_id: media_id.clone(),
                                media_type: m.library_item.media_type,
                                status: LinkStatus::Active,
                                created_at: None,
                                updated_at: None,
                            };
                            db.insert_link(&record).await?;
                            existing_links.insert(target_path.clone(), record);
                            self.log_link_event(
                                db,
                                "backfilled",
                                target_path,
                                Some(&m.source_item.path),
                                Some(media_id.as_str()),
                                Some("already_correct_disk"),
                            )
                            .await;
                        } else {
                            debug!("Symlink on disk is already correct: {:?}", target_path);
                            self.log_link_event(
                                db,
                                "skipped",
                                target_path,
                                Some(&m.source_item.path),
                                Some(media_id.as_str()),
                                Some("already_correct_disk"),
                            )
                            .await;
                        }
                        return Ok(LinkWriteResult {
                            outcome: LinkWriteOutcome::Skipped,
                            refresh_path: None,
                        });
                    }
                }
            } else {
                warn!(
                    "Refusing to overwrite regular file at {:?}. Symlinkarr only mutates symlinks.",
                    target_path
                );
                self.log_link_event(
                    db,
                    "skipped",
                    target_path,
                    Some(&m.source_item.path),
                    Some(media_id.as_str()),
                    Some("regular_file_guard"),
                )
                .await;
                return Ok(LinkWriteResult {
                    outcome: LinkWriteOutcome::Skipped,
                    refresh_path: None,
                });
            }
        }

        if self.dry_run {
            debug!(
                "[DRY-RUN] Would create: {:?} → {:?}",
                target_path, m.source_item.path
            );
            self.log_link_event(
                db,
                "skipped",
                target_path,
                Some(&m.source_item.path),
                Some(media_id.as_str()),
                Some("dry_run"),
            )
            .await;
            return Ok(LinkWriteResult {
                outcome: if is_update {
                    LinkWriteOutcome::Updated
                } else {
                    LinkWriteOutcome::Created
                },
                refresh_path: None,
            });
        } else {
            // Create parent directories
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Check for non-symlink targets we must never overwrite
            if let Ok(meta) = std::fs::symlink_metadata(target_path) {
                if !meta.file_type().is_symlink() {
                    anyhow::bail!(
                        "Refusing to overwrite non-symlink target: {:?}",
                        target_path
                    );
                }
            }

            // C-01: Open a DB transaction BEFORE touching the filesystem.
            // On success we commit; on FS failure the transaction auto-rolls back.
            let mut tx = db.begin().await?;
            let record = LinkRecord {
                id: None,
                source_path: m.source_item.path.clone(),
                target_path: target_path.clone(),
                media_id: m.library_item.id.to_string(),
                media_type: m.library_item.media_type,
                status: LinkStatus::Active,
                created_at: None,
                updated_at: None,
            };
            db.insert_link_in_tx(&record, &mut tx).await?;

            // M-11: Atomic symlink replacement — create at a temp path, then
            // rename() over the target so readers never see a missing file.
            #[cfg(unix)]
            {
                // Use short extension to stay under NAME_MAX (255) after truncation caps at 250.
                let temp_path = target_path.with_extension("glt");
                // Clean up any stale temp from a previous crash
                let _ = std::fs::remove_file(&temp_path);
                std::os::unix::fs::symlink(&m.source_item.path, &temp_path)?;
                std::fs::rename(&temp_path, target_path)?;
            }

            tx.commit().await?;

            info!(
                "Symlink created: {:?} → {:?}",
                target_path, m.source_item.path
            );

            existing_links.insert(target_path.clone(), record);
        }

        // (record variable moved into the tx block above for live path;
        //  dry-run path doesn't need DB recording)

        if is_update {
            self.log_link_event(
                db,
                "updated",
                target_path,
                Some(&m.source_item.path),
                Some(media_id.as_str()),
                None,
            )
            .await;
            Ok(LinkWriteResult {
                outcome: LinkWriteOutcome::Updated,
                refresh_path: Some(m.library_item.path.clone()),
            })
        } else {
            self.log_link_event(
                db,
                "created",
                target_path,
                Some(&m.source_item.path),
                Some(media_id.as_str()),
                None,
            )
            .await;
            Ok(LinkWriteResult {
                outcome: LinkWriteOutcome::Created,
                refresh_path: Some(m.library_item.path.clone()),
            })
        }
    }

    async fn preload_existing_links_for_matches(
        &self,
        db: &Database,
        matches: &[MatchResult],
    ) -> Result<HashMap<PathBuf, LinkRecord>> {
        preload_existing_links(db, &unique_library_roots(matches)).await
    }

    /// Build the target path for a symlink based on the naming template.
    fn build_target_path(&self, m: &MatchResult) -> Result<PathBuf> {
        let lib_path = &m.library_item.path;

        match m.library_item.media_type {
            MediaType::Tv => {
                let season = m.source_item.season.unwrap_or(1);
                let episode = m.source_item.episode.unwrap_or(1);

                let episode_title = m.episode_title.clone().unwrap_or_default();

                // Build path: Library/Show {id}/Season XX/Show - SxxExx - Title.ext
                let season_dir = lib_path.join(format!("Season {:02}", season));
                let filename = self.format_episode_name(
                    &m.library_item.title,
                    season,
                    episode,
                    &episode_title,
                    &m.source_item.extension,
                );

                Ok(season_dir.join(filename))
            }
            MediaType::Movie => {
                // Movies: Library/Movie {id}/Movie (Year).ext
                let year_str = m
                    .source_item
                    .year
                    .map(|y| format!(" ({})", y))
                    .unwrap_or_default();
                let san_title = sanitize_filename(&m.library_item.title);
                let mut filename = format!("{}{}.{}", san_title, year_str, m.source_item.extension);
                if filename.len() > 250 {
                    let excess = filename.len() - 250;
                    let truncated_title =
                        truncate_str_bytes(&san_title, san_title.len().saturating_sub(excess));
                    warn!(
                        "Movie filename exceeds 250 bytes; truncating title from {:?} to {:?}",
                        san_title, truncated_title
                    );
                    filename = format!(
                        "{}{}.{}",
                        truncated_title, year_str, m.source_item.extension
                    );
                }

                Ok(lib_path.join(filename))
            }
        }
    }

    /// Format an episode filename using the naming template.
    fn format_episode_name(
        &self,
        title: &str,
        season: u32,
        episode: u32,
        episode_title: &str,
        extension: &str,
    ) -> String {
        let san_title = sanitize_filename(title);
        let san_ep_title = sanitize_filename(episode_title);

        let mut result = self.naming_template.clone();
        // Substitute padded variants before bare variants to avoid partial replacement.
        result = result.replace("{season:02}", &format!("{:02}", season));
        result = result.replace("{season}", &season.to_string());
        result = result.replace("{episode:02}", &format!("{:02}", episode));
        result = result.replace("{episode}", &episode.to_string());
        result = result.replace("{title}", &san_title);

        if san_ep_title.is_empty() {
            result = result.replace(" - {episode_title}", "");
            result = result.replace("{episode_title} - ", "");
            result = result.replace("{episode_title}", "");
        } else {
            result = result.replace("{episode_title}", &san_ep_title);
        }

        let filename = format!("{}.{}", result, extension);

        truncate_filename_to_limit(
            filename,
            &san_title,
            &san_ep_title,
            season,
            episode,
            extension,
        )
    }

    /// Check all active links and mark dead ones.
    #[allow(dead_code)] // Compatibility wrapper around scoped variant
    pub async fn check_dead_links(&self, db: &Database) -> Result<DeadLinkSummary> {
        self.check_dead_links_scoped(db, None).await
    }

    /// Check all active links and mark dead ones, optionally scoped to library roots.
    pub async fn check_dead_links_scoped(
        &self,
        db: &Database,
        allowed_library_roots: Option<&[PathBuf]>,
    ) -> Result<DeadLinkSummary> {
        let active_links = db.get_active_links_scoped(allowed_library_roots).await?;
        let mut summary = DeadLinkSummary::default();
        let total_links = active_links.len();
        let started = Instant::now();
        let mut source_exists_cache: HashMap<PathBuf, bool> = HashMap::new();
        let mut parent_exists_cache: HashMap<PathBuf, bool> = HashMap::new();

        if total_links > 0 {
            user_println(format!(
                "   🔎 Validating {} active link(s) for dead source/target drift...",
                total_links
            ));
        }
        let mut progress = ProgressLine::new("Dead-link scan progress:");

        for (idx, link) in active_links.iter().enumerate() {
            if idx > 0 && idx % 2000 == 0 {
                let pct = (idx as f64 / total_links.max(1) as f64) * 100.0;
                progress.update(format!("{}/{} ({:.1}%)", idx, total_links, pct));
            }

            if let Some(roots) = allowed_library_roots {
                if !path_under_roots(&link.target_path, roots) {
                    continue;
                }
            }
            let source_exists = cached_source_exists(
                &link.source_path,
                &mut source_exists_cache,
                &mut parent_exists_cache,
            );
            let target_meta = std::fs::symlink_metadata(&link.target_path);

            let target_ok = match &target_meta {
                Ok(meta) if meta.file_type().is_symlink() => std::fs::read_link(&link.target_path)
                    .map(|resolved| resolved == link.source_path)
                    .unwrap_or(false),
                _ => false,
            };

            if !source_exists || !target_ok {
                warn!(
                    "Dead link: {:?} (source_exists={}, target_ok={})",
                    link.target_path, source_exists, target_ok
                );
                db.mark_dead_path(&link.target_path).await?;
                self.log_link_event(
                    db,
                    "dead_marked",
                    &link.target_path,
                    Some(&link.source_path),
                    Some(link.media_id.as_str()),
                    Some("source_or_target_invalid"),
                )
                .await;
                summary.dead_marked += 1;

                // Only remove if it's actually a symlink (SAFETY: never remove dirs)
                if !self.dry_run && link.target_path.is_symlink() {
                    if let Ok(meta) = std::fs::symlink_metadata(&link.target_path) {
                        if meta.file_type().is_dir() {
                            warn!(
                                "SAFETY GUARD: Skipping {:?} — it's a directory, not a symlink.",
                                link.target_path
                            );
                            self.log_link_event(
                                db,
                                "dead_skipped",
                                &link.target_path,
                                Some(&link.source_path),
                                Some(link.media_id.as_str()),
                                Some("directory_guard"),
                            )
                            .await;
                            summary.skipped += 1;
                            continue;
                        }
                    }
                    std::fs::remove_file(&link.target_path)?;
                    summary.removed += 1;
                    info!("Removed dead symlink: {:?}", link.target_path);
                    self.log_link_event(
                        db,
                        "dead_removed",
                        &link.target_path,
                        Some(&link.source_path),
                        Some(link.media_id.as_str()),
                        None,
                    )
                    .await;
                } else if !self.dry_run && !link.target_path.is_symlink() {
                    self.log_link_event(
                        db,
                        "dead_skipped",
                        &link.target_path,
                        Some(&link.source_path),
                        Some(link.media_id.as_str()),
                        Some("not_symlink"),
                    )
                    .await;
                    summary.skipped += 1;
                }
            }
        }

        if total_links > 0 {
            progress.finish(format!(
                "{}/{} (100.0%) in {:.1}s",
                total_links,
                total_links,
                started.elapsed().as_secs_f64()
            ));
        }

        if summary.dead_marked > 0 {
            info!(
                "Found {} dead links (removed={}, skipped={})",
                summary.dead_marked, summary.removed, summary.skipped
            );
        }

        Ok(summary)
    }

    async fn log_link_event(
        &self,
        db: &Database,
        action: &str,
        target_path: &std::path::Path,
        source_path: Option<&std::path::Path>,
        media_id: Option<&str>,
        note: Option<&str>,
    ) {
        if let Err(e) = db
            .record_link_event_fields(action, target_path, source_path, media_id, note)
            .await
        {
            warn!(
                "Failed to record link event action='{}' target={:?}: {}",
                action, target_path, e
            );
        }
    }
}

fn unique_library_roots(matches: &[MatchResult]) -> Vec<PathBuf> {
    let mut roots = matches
        .iter()
        .map(|m| m.library_item.path.clone())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

async fn preload_existing_links(
    db: &Database,
    library_roots: &[PathBuf],
) -> Result<HashMap<PathBuf, LinkRecord>> {
    let mut by_target = HashMap::new();
    // Chunk to avoid SQLite's "Expression tree too large" (max depth 1000)
    for chunk in library_roots.chunks(200) {
        for link in db.get_links_scoped(Some(chunk)).await? {
            by_target.insert(link.target_path.clone(), link);
        }
    }
    Ok(by_target)
}

/// Truncate `s` to at most `max_bytes` bytes, respecting UTF-8 char boundaries.
fn truncate_str_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk back from max_bytes until we land on a char boundary.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Ensure a constructed episode filename is ≤ 250 bytes.
///
/// Strategy:
/// 1. Truncate `episode_title` first (most variable).
/// 2. If still over, truncate `title` as a last resort.
fn truncate_filename_to_limit(
    filename: String,
    title: &str,
    episode_title: &str,
    season: u32,
    episode: u32,
    extension: &str,
) -> String {
    const LIMIT: usize = 250;

    if filename.len() <= LIMIT {
        return filename;
    }

    // Step 1: truncate episode_title
    let excess = filename.len() - LIMIT;
    let ep_title_bytes = episode_title.len();
    let new_ep_len = ep_title_bytes.saturating_sub(excess);
    let truncated_ep = truncate_str_bytes(episode_title, new_ep_len).trim_end();

    let candidate = if truncated_ep.is_empty() {
        format!("{} - S{:02}E{:02}.{}", title, season, episode, extension)
    } else {
        format!(
            "{} - S{:02}E{:02} - {}.{}",
            title, season, episode, truncated_ep, extension
        )
    };

    if candidate.len() <= LIMIT {
        warn!(
            "Episode filename exceeded 250 bytes; truncated episode title to {:?}",
            truncated_ep
        );
        return candidate;
    }

    // Step 2: truncate title as last resort
    let excess2 = candidate.len() - LIMIT;
    let truncated_title = truncate_str_bytes(title, title.len().saturating_sub(excess2)).trim_end();
    let final_name = if truncated_ep.is_empty() {
        format!(
            "{} - S{:02}E{:02}.{}",
            truncated_title, season, episode, extension
        )
    } else {
        format!(
            "{} - S{:02}E{:02} - {}.{}",
            truncated_title, season, episode, truncated_ep, extension
        )
    };
    warn!(
        "Episode filename still exceeded 250 bytes after episode title truncation; \
         also truncated show title from {:?} to {:?}",
        title, truncated_title
    );
    final_name
}

/// Remove invalid filename characters (Windows-compatible for safety).
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    use crate::config::ContentType;
    use crate::db::Database;
    use crate::models::{
        LibraryItem, LinkRecord, LinkStatus, MatchResult, MediaId, MediaType, SourceItem,
    };

    #[test]
    fn test_truncate_str_bytes_ascii() {
        assert_eq!(truncate_str_bytes("hello", 3), "hel");
        assert_eq!(truncate_str_bytes("hello", 10), "hello");
        assert_eq!(truncate_str_bytes("hello", 0), "");
    }

    #[test]
    fn test_truncate_str_bytes_unicode() {
        // "é" is 2 bytes (0xC3 0xA9); cutting at byte 1 must back up to 0.
        let s = "aé";
        assert_eq!(truncate_str_bytes(s, 2), "a");
        assert_eq!(truncate_str_bytes(s, 3), "aé");
    }

    const DEFAULT_TEMPLATE: &str = "{title} - S{season:02}E{episode:02} - {episode_title}";

    #[test]
    fn test_format_episode_name_long_ep_title_truncated() {
        let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
        // Build an episode title that will push the filename over 250 bytes.
        let long_ep_title = "A".repeat(240);
        let name = linker.format_episode_name("Show", 1, 1, &long_ep_title, "mkv");
        assert!(
            name.len() <= 250,
            "filename is {} bytes, expected ≤ 250",
            name.len()
        );
        assert!(name.ends_with(".mkv"));
        assert!(name.contains("S01E01"));
    }

    #[test]
    fn test_format_episode_name_long_title_truncated() {
        let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
        // Both title and episode title extremely long.
        let long_title = "B".repeat(200);
        let long_ep = "C".repeat(200);
        let name = linker.format_episode_name(&long_title, 1, 1, &long_ep, "mkv");
        assert!(
            name.len() <= 250,
            "filename is {} bytes, expected ≤ 250",
            name.len()
        );
        assert!(name.ends_with(".mkv"));
    }

    #[test]
    fn test_truncated_filename_safe_for_temp_extension() {
        // Regression: the atomic-swap temp path replaces the extension with ".glt".
        // Verify the worst case (short original ext → longer temp ext) stays under NAME_MAX=255.
        let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
        let long_ep_title = "あ".repeat(100); // 3 bytes each → 300 bytes of episode title
        let name = linker.format_episode_name("Show", 1, 1, &long_ep_title, "mkv");
        assert!(name.len() <= 250, "filename is {} bytes", name.len());

        // Simulate what the symlink code does: swap extension to ".glt"
        let p = std::path::Path::new(&name);
        let temp_name = p.with_extension("glt");
        let temp_len = temp_name.to_str().unwrap().len();
        assert!(
            temp_len <= 255,
            "temp filename is {} bytes, exceeds NAME_MAX",
            temp_len
        );
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("Normal Title"), "Normal Title");
        assert_eq!(sanitize_filename("Title: Subtitle"), "Title_ Subtitle");
        assert_eq!(
            sanitize_filename("Who Wants to be a Millionaire?"),
            "Who Wants to be a Millionaire_"
        );
    }

    #[test]
    fn test_format_episode_name() {
        let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
        let name = linker.format_episode_name("Breaking Bad", 1, 1, "Pilot", "mkv");
        assert_eq!(name, "Breaking Bad - S01E01 - Pilot.mkv");
    }

    #[test]
    fn test_format_episode_name_no_title() {
        let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
        let name = linker.format_episode_name("Breaking Bad", 2, 5, "", "mp4");
        assert_eq!(name, "Breaking Bad - S02E05.mp4");
    }

    #[test]
    fn test_custom_naming_template() {
        let linker =
            Linker::new_with_options(false, true, "{episode:02}x{season:02} - {title}", true);
        let name = linker.format_episode_name("My Show", 1, 5, "Episode Title", "mkv");
        assert_eq!(name, "05x01 - My Show.mkv");
    }

    #[test]
    fn test_cached_source_exists_short_circuits_missing_parent() {
        let root = tempfile::TempDir::new().unwrap();
        let missing = root.path().join("missing-parent").join("missing-file.mkv");
        let mut source_cache = HashMap::new();
        let mut parent_cache = HashMap::new();

        let exists = cached_source_exists(&missing, &mut source_cache, &mut parent_cache);
        assert!(!exists);

        let parent = missing.parent().unwrap().to_path_buf();
        assert_eq!(parent_cache.get(&parent), Some(&false));
        assert_eq!(source_cache.get(&missing), Some(&false));
    }

    #[test]
    fn test_cached_source_exists_true_for_existing_file() {
        let root = tempfile::TempDir::new().unwrap();
        let file = root.path().join("source.mkv");
        fs::write(&file, "data").unwrap();
        let mut source_cache = HashMap::new();
        let mut parent_cache = HashMap::new();

        let exists = cached_source_exists(&file, &mut source_cache, &mut parent_cache);
        assert!(exists);
        assert_eq!(source_cache.get(&file), Some(&true));
    }

    fn sample_movie_match(
        lib_path: &std::path::Path,
        source_path: &std::path::Path,
    ) -> MatchResult {
        MatchResult {
            library_item: LibraryItem {
                id: MediaId::Tmdb(550),
                path: lib_path.to_path_buf(),
                title: "Sample Movie".to_string(),
                library_name: "Movies".to_string(),
                media_type: MediaType::Movie,
                content_type: ContentType::Movie,
            },
            source_item: SourceItem {
                path: source_path.to_path_buf(),
                parsed_title: "Sample Movie".to_string(),
                season: None,
                episode: None,
                episode_end: None,
                quality: None,
                extension: "mkv".to_string(),
                year: None,
            },
            confidence: 1.0,
            matched_alias: "sample movie".to_string(),
            episode_title: None,
        }
    }

    #[tokio::test]
    async fn test_strict_mode_skips_regular_file_overwrite() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let source_path = dir.path().join("rd").join("sample_movie.mkv");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "video").unwrap();

        let target = lib_path.join("Sample Movie.mkv");
        fs::write(&target, "real-file").unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "");
        let m = sample_movie_match(&lib_path, &source_path);
        let target_path = linker.build_target_path(&m).unwrap();
        let mut existing_links = linker
            .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
            .await
            .unwrap();

        let outcome = linker
            .create_link(&m, &target_path, &db, &mut existing_links)
            .await
            .unwrap();

        assert_eq!(outcome.outcome, LinkWriteOutcome::Skipped);
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(fs::read_to_string(&target).unwrap(), "real-file");
    }

    #[tokio::test]
    async fn test_directory_target_bails() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let source_path = dir.path().join("rd").join("sample_movie.mkv");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "video").unwrap();

        let target = lib_path.join("Sample Movie.mkv");
        fs::create_dir_all(&target).unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "");
        let m = sample_movie_match(&lib_path, &source_path);
        let target_path = linker.build_target_path(&m).unwrap();
        let mut existing_links = linker
            .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
            .await
            .unwrap();

        let err = linker
            .create_link(&m, &target_path, &db, &mut existing_links)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("directory"));
    }

    #[tokio::test]
    async fn test_process_matches_skips_missing_source_before_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let source_path = dir.path().join("rd").join("missing_source.mkv");

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "");

        let summary = linker
            .process_matches(&[sample_movie_match(&lib_path, &source_path)], &db)
            .await
            .unwrap();

        assert_eq!(summary.created, 0);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 1);

        let target = lib_path.join("Sample Movie.mkv");
        assert!(db.get_link_by_target_path(&target).await.unwrap().is_none());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_symlink_target_can_be_replaced() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let rd_dir = dir.path().join("rd");
        fs::create_dir_all(&rd_dir).unwrap();
        let old_source = rd_dir.join("old.mkv");
        let new_source = rd_dir.join("new.mkv");
        fs::write(&old_source, "old").unwrap();
        fs::write(&new_source, "new").unwrap();

        let target = lib_path.join("Sample Movie.mkv");
        std::os::unix::fs::symlink(&old_source, &target).unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "");
        let m = sample_movie_match(&lib_path, &new_source);
        let target_path = linker.build_target_path(&m).unwrap();
        let mut existing_links = linker
            .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
            .await
            .unwrap();

        let outcome = linker
            .create_link(&m, &target_path, &db, &mut existing_links)
            .await
            .unwrap();

        assert_eq!(outcome.outcome, LinkWriteOutcome::Created);
        assert_eq!(outcome.refresh_path, Some(lib_path.clone()));
        assert_eq!(fs::read_link(&target).unwrap(), PathBuf::from(&new_source));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_correct_on_disk_symlink_backfills_missing_db_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let rd_dir = dir.path().join("rd");
        fs::create_dir_all(&rd_dir).unwrap();
        let source = rd_dir.join("sample.mkv");
        fs::write(&source, "video").unwrap();

        let target = lib_path.join("Sample Movie.mkv");
        std::os::unix::fs::symlink(&source, &target).unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "");
        let m = sample_movie_match(&lib_path, &source);
        let target_path = linker.build_target_path(&m).unwrap();
        let mut existing_links = linker
            .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
            .await
            .unwrap();

        let outcome = linker
            .create_link(&m, &target_path, &db, &mut existing_links)
            .await
            .unwrap();

        assert_eq!(outcome.outcome, LinkWriteOutcome::Skipped);
        let record = db.get_link_by_target_path(&target).await.unwrap().unwrap();
        assert_eq!(record.source_path, source);
        assert_eq!(record.status, LinkStatus::Active);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_same_destination_new_source_is_classified_as_updated() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let rd_dir = dir.path().join("rd");
        fs::create_dir_all(&rd_dir).unwrap();
        let old_source = rd_dir.join("old.mkv");
        let new_source = rd_dir.join("new.mkv");
        fs::write(&old_source, "old").unwrap();
        fs::write(&new_source, "new").unwrap();

        let target = lib_path.join("Sample Movie.mkv");
        std::os::unix::fs::symlink(&old_source, &target).unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: old_source.clone(),
            target_path: target.clone(),
            media_id: "tmdb-550".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let linker = Linker::new_with_options(false, true, "", true);
        let m = sample_movie_match(&lib_path, &new_source);
        let target_path = linker.build_target_path(&m).unwrap();
        let mut existing_links = linker
            .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
            .await
            .unwrap();

        let outcome = linker
            .create_link(&m, &target_path, &db, &mut existing_links)
            .await
            .unwrap();

        assert_eq!(outcome.outcome, LinkWriteOutcome::Updated);
        assert_eq!(outcome.refresh_path, Some(lib_path.clone()));
        assert_eq!(fs::read_link(&target).unwrap(), PathBuf::from(&new_source));
    }
}
