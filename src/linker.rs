use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::api::decypharr::{DecypharrClient, WebDavProbeError};
use crate::config::Config;
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MatchResult, MediaType};
use crate::utils::{
    cached_source_exists, cached_source_health, path_under_roots, user_println, PathHealth,
    ProgressLine,
};

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
    pub skip_reasons: BTreeMap<String, u64>,
    pub refresh_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct DeadLinkSummary {
    pub dead_marked: u64,
    pub removed: u64,
    pub skipped: u64,
    pub skip_reasons: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkWriteResult {
    outcome: LinkWriteOutcome,
    skip_reason: Option<&'static str>,
    refresh_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum SourceReadiness {
    Ready,
    Unreadable(String),
}

impl SourceReadiness {
    fn into_result(self) -> std::result::Result<(), String> {
        match self {
            Self::Ready => Ok(()),
            Self::Unreadable(reason) => Err(reason),
        }
    }
}

struct SourceReadinessGate {
    decypharr: DecypharrClient,
    source_roots: Vec<PathBuf>,
    probe_timeout: Duration,
}

impl SourceReadinessGate {
    fn from_config(cfg: &Config) -> Option<Self> {
        if !cfg.symlink.verify_source_readability || !cfg.has_decypharr() || cfg.sources.is_empty()
        {
            return None;
        }

        Some(Self {
            decypharr: DecypharrClient::from_config(&cfg.decypharr),
            source_roots: cfg
                .sources
                .iter()
                .map(|source| source.path.clone())
                .collect(),
            probe_timeout: Duration::from_millis(cfg.symlink.source_probe_timeout_ms),
        })
    }

    fn matching_source_root<'a>(&'a self, source_path: &Path) -> Option<&'a PathBuf> {
        self.source_roots
            .iter()
            .filter(|root| source_path.starts_with(root))
            .max_by_key(|root| root.components().count())
    }

    fn candidate_relative_paths(source_root: &Path, source_path: &Path) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        let mut push_relative = |base: &Path| {
            if let Ok(relative) = source_path.strip_prefix(base) {
                let candidate = relative.to_path_buf();
                if !candidate.as_os_str().is_empty() && !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        };

        let root_name = source_root.file_name().and_then(|value| value.to_str());
        if root_name == Some("__all__") {
            if let Some(parent) = source_root.parent() {
                push_relative(parent);
            }
            push_relative(source_root);
        } else {
            push_relative(source_root);
            if let Some(parent) = source_root.parent() {
                push_relative(parent);
            }
        }

        candidates
    }

    async fn ensure_readable(
        &self,
        source_path: &Path,
        readiness_cache: &mut HashMap<PathBuf, SourceReadiness>,
    ) -> std::result::Result<(), String> {
        let cache_key = source_path.parent().unwrap_or(source_path).to_path_buf();
        if let Some(cached) = readiness_cache.get(&cache_key) {
            return cached.clone().into_result();
        }

        let outcome = if let Some(source_root) = self.matching_source_root(source_path) {
            let mut saw_not_found = false;
            let mut first_failure: Option<String> = None;

            for candidate in Self::candidate_relative_paths(source_root, source_path) {
                match self
                    .decypharr
                    .probe_webdav_path(&candidate, self.probe_timeout)
                    .await
                {
                    Ok(()) => {
                        first_failure = None;
                        break;
                    }
                    Err(WebDavProbeError::NotFound) => {
                        saw_not_found = true;
                    }
                    Err(WebDavProbeError::Unreadable(reason)) => {
                        first_failure = Some(reason);
                        break;
                    }
                }
            }

            if let Some(reason) = first_failure {
                SourceReadiness::Unreadable(reason)
            } else if saw_not_found {
                // The source path may belong to a non-Decypharr source tree or a mount layout
                // we cannot map safely; bypass rather than false-blocking the link.
                SourceReadiness::Ready
            } else {
                SourceReadiness::Ready
            }
        } else {
            SourceReadiness::Ready
        };

        readiness_cache.insert(cache_key, outcome.clone());
        outcome.into_result()
    }
}

fn increment_skip_reason(skip_reasons: &mut BTreeMap<String, u64>, reason: &str) {
    *skip_reasons.entry(reason.to_string()).or_insert(0) += 1;
}

fn destructive_source_exists(
    operation: &str,
    source_path: &std::path::Path,
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

fn resolve_link_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

fn verify_link_target(target_path: &Path, expected_source: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(target_path)?;
    if !meta.file_type().is_symlink() {
        anyhow::bail!(
            "post-write verification failed: {:?} is no longer a symlink",
            target_path
        );
    }

    let actual_target = std::fs::read_link(target_path)?;
    let resolved_target = resolve_link_target(target_path, &actual_target);
    if resolved_target != expected_source {
        anyhow::bail!(
            "post-write verification failed: {:?} points to {:?} instead of {:?}",
            target_path,
            resolved_target,
            expected_source
        );
    }

    Ok(())
}

/// Creates and manages symlinks from Real-Debrid sources to Plex library.
pub struct Linker {
    dry_run: bool,
    #[allow(dead_code)] // Reserved for strict-mode-specific safeguards
    strict_mode: bool,
    reconcile_links: bool,
    naming_template: String,
    source_readiness_gate: Option<SourceReadinessGate>,
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
            source_readiness_gate: None,
        }
    }

    pub fn with_source_readiness_from_config(mut self, cfg: &Config) -> Self {
        self.source_readiness_gate = SourceReadinessGate::from_config(cfg);
        self
    }

    /// Process a list of matches and create/update symlinks.
    pub async fn process_matches(
        &self,
        matches: &[MatchResult],
        db: &Database,
        run_token: Option<&str>,
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
        let mut source_readiness_cache: HashMap<PathBuf, SourceReadiness> = HashMap::new();
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
                    run_token,
                    "skipped",
                    &target_path,
                    Some(&m.source_item.path),
                    Some(media_id.as_str()),
                    Some("source_missing_before_link"),
                )
                .await;
                summary.skipped += 1;
                increment_skip_reason(&mut summary.skip_reasons, "source_missing_before_link");
                continue;
            }

            match self
                .create_link(
                    m,
                    &target_path,
                    db,
                    &mut existing_links,
                    run_token,
                    &mut source_readiness_cache,
                )
                .await
            {
                Ok(result) => {
                    match result.outcome {
                        LinkWriteOutcome::Created => summary.created += 1,
                        LinkWriteOutcome::Updated => summary.updated += 1,
                        LinkWriteOutcome::Skipped => {
                            summary.skipped += 1;
                            if let Some(reason) = result.skip_reason {
                                increment_skip_reason(&mut summary.skip_reasons, reason);
                            }
                        }
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
        run_token: Option<&str>,
        source_readiness_cache: &mut HashMap<PathBuf, SourceReadiness>,
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
                                    run_token,
                                    "skipped",
                                    target_path,
                                    Some(&m.source_item.path),
                                    Some(media_id.as_str()),
                                    Some("already_correct"),
                                )
                                .await;
                                return Ok(LinkWriteResult {
                                    outcome: LinkWriteOutcome::Skipped,
                                    skip_reason: Some("already_correct"),
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
                                run_token,
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
                                run_token,
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
                            skip_reason: Some("already_correct_disk"),
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
                    run_token,
                    "skipped",
                    target_path,
                    Some(&m.source_item.path),
                    Some(media_id.as_str()),
                    Some("regular_file_guard"),
                )
                .await;
                return Ok(LinkWriteResult {
                    outcome: LinkWriteOutcome::Skipped,
                    skip_reason: Some("regular_file_guard"),
                    refresh_path: None,
                });
            }
        }

        if !self.dry_run {
            if let Some(gate) = &self.source_readiness_gate {
                if let Err(reason) = gate
                    .ensure_readable(&m.source_item.path, source_readiness_cache)
                    .await
                {
                    warn!(
                        "Skipping link creation because source failed WebDAV readability probe: {:?} ({})",
                        m.source_item.path, reason
                    );
                    self.log_link_event(
                        db,
                        run_token,
                        "skipped",
                        target_path,
                        Some(&m.source_item.path),
                        Some(media_id.as_str()),
                        Some("source_unreadable_before_link"),
                    )
                    .await;
                    return Ok(LinkWriteResult {
                        outcome: LinkWriteOutcome::Skipped,
                        skip_reason: Some("source_unreadable_before_link"),
                        refresh_path: None,
                    });
                }
            }
        }

        if self.dry_run {
            debug!(
                "[DRY-RUN] Would create: {:?} → {:?}",
                target_path, m.source_item.path
            );
            self.log_link_event(
                db,
                run_token,
                if is_update {
                    "dry_run_updated"
                } else {
                    "dry_run_created"
                },
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
                skip_reason: None,
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
                verify_link_target(target_path, &m.source_item.path)?;
            }

            tx.commit().await?;

            info!(
                "Symlink created: {:?} → {:?}",
                target_path, m.source_item.path
            );

            existing_links.insert(target_path.clone(), record);
        }

        if is_update {
            self.log_link_event(
                db,
                run_token,
                "updated",
                target_path,
                Some(&m.source_item.path),
                Some(media_id.as_str()),
                None,
            )
            .await;
            Ok(LinkWriteResult {
                outcome: LinkWriteOutcome::Updated,
                skip_reason: None,
                refresh_path: Some(m.library_item.path.clone()),
            })
        } else {
            self.log_link_event(
                db,
                run_token,
                "created",
                target_path,
                Some(&m.source_item.path),
                Some(media_id.as_str()),
                None,
            )
            .await;
            Ok(LinkWriteResult {
                outcome: LinkWriteOutcome::Created,
                skip_reason: None,
                refresh_path: Some(m.library_item.path.clone()),
            })
        }
    }

    async fn preload_existing_links_for_matches(
        &self,
        db: &Database,
        matches: &[MatchResult],
    ) -> Result<HashMap<PathBuf, LinkRecord>> {
        let mut target_paths = Vec::with_capacity(matches.len());
        for m in matches {
            target_paths.push(self.build_target_path(m)?);
        }

        preload_existing_links(db, &target_paths).await
    }

    /// Build the target path for a symlink based on the naming template.
    pub(crate) fn build_target_path(&self, m: &MatchResult) -> Result<PathBuf> {
        let lib_path = &m.library_item.path;

        match m.library_item.media_type {
            MediaType::Tv => {
                let season = m
                    .source_item
                    .season
                    .context("TV match missing season while building target path")?;
                let episode = m
                    .source_item
                    .episode
                    .context("TV match missing episode while building target path")?;

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
        self.check_dead_links_scoped(db, None, None).await
    }

    /// Check all active links and mark dead ones, optionally scoped to library roots.
    pub async fn check_dead_links_scoped(
        &self,
        db: &Database,
        allowed_library_roots: Option<&[PathBuf]>,
        run_token: Option<&str>,
    ) -> Result<DeadLinkSummary> {
        let active_links = db.get_active_links_scoped(allowed_library_roots).await?;
        let mut summary = DeadLinkSummary::default();
        let total_links = active_links.len();
        let started = Instant::now();
        let mut source_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();
        let mut parent_health_cache: HashMap<PathBuf, PathHealth> = HashMap::new();

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
            let source_exists = destructive_source_exists(
                "dead-link sweep",
                &link.source_path,
                &mut source_health_cache,
                &mut parent_health_cache,
            )?;
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
                    run_token,
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
                                run_token,
                                "dead_skipped",
                                &link.target_path,
                                Some(&link.source_path),
                                Some(link.media_id.as_str()),
                                Some("directory_guard"),
                            )
                            .await;
                            summary.skipped += 1;
                            increment_skip_reason(&mut summary.skip_reasons, "directory_guard");
                            continue;
                        }
                    }
                    std::fs::remove_file(&link.target_path)?;
                    summary.removed += 1;
                    info!("Removed dead symlink: {:?}", link.target_path);
                    self.log_link_event(
                        db,
                        run_token,
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
                        run_token,
                        "dead_skipped",
                        &link.target_path,
                        Some(&link.source_path),
                        Some(link.media_id.as_str()),
                        Some("not_symlink"),
                    )
                    .await;
                    summary.skipped += 1;
                    increment_skip_reason(&mut summary.skip_reasons, "not_symlink");
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

    #[allow(clippy::too_many_arguments)]
    async fn log_link_event(
        &self,
        db: &Database,
        run_token: Option<&str>,
        action: &str,
        target_path: &std::path::Path,
        source_path: Option<&std::path::Path>,
        media_id: Option<&str>,
        note: Option<&str>,
    ) {
        if let Err(e) = db
            .record_link_event_fields_with_run_token(
                run_token,
                action,
                target_path,
                source_path,
                media_id,
                note,
            )
            .await
        {
            warn!(
                "Failed to record link event action='{}' target={:?}: {}",
                action, target_path, e
            );
        }
    }
}

async fn preload_existing_links(
    db: &Database,
    target_paths: &[PathBuf],
) -> Result<HashMap<PathBuf, LinkRecord>> {
    let mut by_target = HashMap::new();
    // Chunk to stay comfortably below SQLite's bind limit on large scans.
    for chunk in target_paths.chunks(500) {
        for link in db.get_links_by_targets(chunk).await? {
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

    use crate::api::test_helpers::spawn_sequence_http_server;
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
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
    fn test_sanitize_filename_all_special_chars() {
        // All Windows-incompatible filename characters replaced with underscore
        assert_eq!(
            sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"),
            "a_b_c_d_e_f_g_h_i_j"
        );
        // Unicode characters preserved
        assert_eq!(sanitize_filename("日本語タイトル"), "日本語タイトル");
    }

    #[test]
    fn test_sanitize_filename_trims_whitespace() {
        assert_eq!(sanitize_filename("  Title  "), "Title");
        assert_eq!(sanitize_filename("  Movie (2024)  "), "Movie (2024)");
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

    #[test]
    fn test_destructive_source_exists_rejects_unhealthy_parent() {
        let path = PathBuf::from("/mnt/rd/file.mkv");
        let parent = path.parent().unwrap().to_path_buf();
        let mut source_cache = HashMap::new();
        let mut parent_cache = HashMap::new();
        parent_cache.insert(parent, PathHealth::TransportDisconnected);

        let err = destructive_source_exists(
            "dead-link sweep",
            &path,
            &mut source_cache,
            &mut parent_cache,
        )
        .unwrap_err();

        assert!(err.to_string().contains("Aborting dead-link sweep"));
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

    fn sample_tv_match(
        lib_path: &std::path::Path,
        source_path: &std::path::Path,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> MatchResult {
        MatchResult {
            library_item: LibraryItem {
                id: MediaId::Tvdb(81189),
                path: lib_path.to_path_buf(),
                title: "Sample Show".to_string(),
                library_name: "Series".to_string(),
                media_type: MediaType::Tv,
                content_type: ContentType::Tv,
            },
            source_item: SourceItem {
                path: source_path.to_path_buf(),
                parsed_title: "Sample Show".to_string(),
                season,
                episode,
                episode_end: None,
                quality: None,
                extension: "mkv".to_string(),
                year: None,
            },
            confidence: 1.0,
            matched_alias: "sample show".to_string(),
            episode_title: Some("Pilot".to_string()),
        }
    }

    fn test_config_with_decypharr(base_url: &str, source_root: PathBuf) -> Config {
        Config {
            libraries: vec![],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: source_root,
                media_type: "auto".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig {
                url: base_url.to_string(),
                ..DecypharrConfig::default()
            },
            dmm: DmmConfig::default(),
            backup: BackupConfig::default(),
            db_path: ":memory:".to_string(),
            log_level: "info".to_string(),
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
        let mut readiness_cache = HashMap::new();

        let outcome = linker
            .create_link(
                &m,
                &target_path,
                &db,
                &mut existing_links,
                None,
                &mut readiness_cache,
            )
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
        let mut readiness_cache = HashMap::new();

        let err = linker
            .create_link(
                &m,
                &target_path,
                &db,
                &mut existing_links,
                None,
                &mut readiness_cache,
            )
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
            .process_matches(&[sample_movie_match(&lib_path, &source_path)], &db, None)
            .await
            .unwrap();

        assert_eq!(summary.created, 0);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.skipped, 1);

        let target = lib_path.join("Sample Movie.mkv");
        assert!(db.get_link_by_target_path(&target).await.unwrap().is_none());
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn test_create_link_skips_unreadable_source_before_live_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Movie {tmdb-550}");
        fs::create_dir_all(&lib_path).unwrap();

        let source_root = dir.path().join("mnt").join("realdebrid").join("__all__");
        let source_path = source_root.join("broken-release").join("sample_movie.mkv");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "video").unwrap();

        let (base_url, _) =
            spawn_sequence_http_server(&[("HTTP/1.1 503 Service Unavailable", "bad object")])
                .unwrap();
        let cfg = test_config_with_decypharr(&base_url, source_root.clone());

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "").with_source_readiness_from_config(&cfg);
        let m = sample_movie_match(&lib_path, &source_path);
        let target_path = linker.build_target_path(&m).unwrap();
        let mut existing_links = linker
            .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
            .await
            .unwrap();
        let mut readiness_cache = HashMap::new();

        let outcome = linker
            .create_link(
                &m,
                &target_path,
                &db,
                &mut existing_links,
                None,
                &mut readiness_cache,
            )
            .await
            .unwrap();

        assert_eq!(outcome.outcome, LinkWriteOutcome::Skipped);
        assert_eq!(outcome.skip_reason, Some("source_unreadable_before_link"));
        assert!(!target_path.exists());
        assert!(db
            .get_link_by_target_path(&target_path)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_build_target_path_errors_when_tv_match_lacks_season_or_episode() {
        let dir = tempfile::TempDir::new().unwrap();
        let lib_path = dir.path().join("Sample Show {tvdb-81189}");
        let source_path = dir.path().join("rd").join("sample_show.mkv");
        let linker = Linker::new(false, true, "");

        let missing_season = sample_tv_match(&lib_path, &source_path, None, Some(1));
        let err = linker.build_target_path(&missing_season).unwrap_err();
        assert!(err.to_string().contains("missing season"));

        let missing_episode = sample_tv_match(&lib_path, &source_path, Some(1), None);
        let err = linker.build_target_path(&missing_episode).unwrap_err();
        assert!(err.to_string().contains("missing episode"));
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
        let mut readiness_cache = HashMap::new();

        let outcome = linker
            .create_link(
                &m,
                &target_path,
                &db,
                &mut existing_links,
                None,
                &mut readiness_cache,
            )
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
        let mut readiness_cache = HashMap::new();

        let outcome = linker
            .create_link(
                &m,
                &target_path,
                &db,
                &mut existing_links,
                None,
                &mut readiness_cache,
            )
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
        let mut readiness_cache = HashMap::new();

        let outcome = linker
            .create_link(
                &m,
                &target_path,
                &db,
                &mut existing_links,
                None,
                &mut readiness_cache,
            )
            .await
            .unwrap();

        assert_eq!(outcome.outcome, LinkWriteOutcome::Updated);
        assert_eq!(outcome.refresh_path, Some(lib_path.clone()));
        assert_eq!(fs::read_link(&target).unwrap(), PathBuf::from(&new_source));
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_link_target_accepts_matching_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("rd").join("video.mkv");
        let target = dir.path().join("library").join("video.mkv");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&source, "video").unwrap();
        std::os::unix::fs::symlink(&source, &target).unwrap();

        verify_link_target(&target, &source).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_verify_link_target_rejects_wrong_destination() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("rd").join("video.mkv");
        let other_source = dir.path().join("rd").join("other.mkv");
        let target = dir.path().join("library").join("video.mkv");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&source, "video").unwrap();
        fs::write(&other_source, "video").unwrap();
        std::os::unix::fs::symlink(&other_source, &target).unwrap();

        let err = verify_link_target(&target, &source).unwrap_err();
        assert!(err.to_string().contains("post-write verification failed"));
    }

    #[test]
    fn truncate_filename_to_limit_under_limit() {
        let filename = "Show - S01E01 - Episode Title.mkv";
        let result =
            truncate_filename_to_limit(filename.to_string(), "Show", "Episode Title", 1, 1, "mkv");
        assert_eq!(result, filename);
    }

    #[test]
    fn truncate_filename_to_limit_truncates_episode_title() {
        // Long episode title should be truncated first
        let long_title = "A".repeat(300);
        let filename = format!("Show - S01E01 - {}.mkv", long_title);
        let result = truncate_filename_to_limit(filename, "Show", &long_title, 1, 1, "mkv");
        assert!(
            result.len() <= 250,
            "result len {} should be <= 250",
            result.len()
        );
        assert!(result.contains("Show"));
        assert!(result.contains("S01E01"));
    }

    #[test]
    fn truncate_filename_to_limit_handles_empty_episode_title() {
        // Long filename with empty episode title — should use title-only format
        let long_title = "A".repeat(230);
        let filename = format!("Show - S01E01 - {}.mkv", long_title);
        let result = truncate_filename_to_limit(filename, "Show", "", 1, 1, "mkv");
        assert!(
            result.len() <= 250,
            "result len {} should be <= 250",
            result.len()
        );
        // Should not have double dash before extension
        assert!(!result.contains(" - .mkv"));
    }
}
