use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use self::naming::{sanitize_filename, truncate_filename_to_limit, truncate_str_bytes};
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

mod naming;
#[cfg(test)]
mod tests;
