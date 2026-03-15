mod anime_scanner;
mod api;
mod auto_acquire;
mod backup;
mod cache;
mod cleanup_audit;
mod config;
mod db;
mod discovery;
mod library_scanner;
mod linker;
mod matcher;
mod models;
mod repair;
mod source_scanner;
mod utils;

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use tracing::info;

use crate::api::bazarr::BazarrClient;
use crate::api::plex::{find_section_for_path, PlexClient};
use crate::api::prowlarr::ProwlarrClient;
use crate::api::realdebrid::RealDebridClient;
use crate::api::tautulli::TautulliClient;
use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::auto_acquire::{process_auto_acquire_queue, AutoAcquireRequest, RelinkCheck};
use crate::cleanup_audit::{CleanupAuditor, CleanupScope};
use crate::config::{Config, ContentType};
use crate::db::{AcquisitionJobStatus, Database};
use crate::library_scanner::LibraryScanner;
use crate::linker::Linker;
use crate::matcher::Matcher;
use crate::models::{LibraryItem, MediaId, MediaType};
use crate::source_scanner::SourceScanner;
use crate::utils::{
    directory_path_health_with_timeout, fast_path_health, path_under_roots, stdout_text_guard,
    user_println, PathHealth,
};

const DIRECTORY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(
    name = "symlinkarr",
    about = "Symlinkarr — Intelligent symlink manager for Real-Debrid ↔ Plex",
    version
)]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum GateMode {
    Enforce,
    Relaxed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum QueueStatusFilter {
    Queued,
    Downloading,
    Relinking,
    Blocked,
    NoResult,
    Failed,
    CompletedUnlinked,
    CompletedLinked,
}

impl QueueStatusFilter {
    fn into_job_status(self) -> AcquisitionJobStatus {
        match self {
            Self::Queued => AcquisitionJobStatus::Queued,
            Self::Downloading => AcquisitionJobStatus::Downloading,
            Self::Relinking => AcquisitionJobStatus::Relinking,
            Self::Blocked => AcquisitionJobStatus::Blocked,
            Self::NoResult => AcquisitionJobStatus::NoResult,
            Self::Failed => AcquisitionJobStatus::Failed,
            Self::CompletedUnlinked => AcquisitionJobStatus::CompletedUnlinked,
            Self::CompletedLinked => AcquisitionJobStatus::CompletedLinked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum QueueRetryScope {
    All,
    Blocked,
    NoResult,
    Failed,
    CompletedUnlinked,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a full scan → match → link cycle
    Scan {
        /// Dry-run mode (scan + match + summary, no symlink creation)
        #[arg(long)]
        dry_run: bool,
        /// Search Prowlarr for library items with no match on the RD mount
        #[arg(long)]
        search_missing: bool,
        /// Only process one or more libraries (comma-separated names)
        #[arg(long)]
        library: Option<String>,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Run in dry-run mode (scan + match, no symlink creation)
    #[command(hide = true)]
    DryRun {
        /// Enable verbose progress output
        #[arg(short, long)]
        verbose: bool,
        /// Only process one or more libraries (comma-separated names)
        #[arg(long)]
        library: Option<String>,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Show database statistics
    Status {
        /// Check connection health with external services (Tautulli, etc)
        #[arg(long)]
        health: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Inspect and manage persistent auto-acquire jobs
    Queue {
        #[command(subcommand)]
        action: QueueAction,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Run continuously as a daemon
    Daemon,
    /// Cleanup workflows: dead links, audit, and prune
    Cleanup {
        #[command(subcommand)]
        action: Option<CleanupAction>,
        /// Only process one or more libraries (comma-separated names)
        #[arg(long)]
        library: Option<String>,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Repair dead symlinks by finding replacements
    Repair {
        #[command(subcommand)]
        action: RepairAction,
        /// Only process one or more libraries (comma-separated names)
        #[arg(long)]
        library: Option<String>,
    },
    /// Discover RD cache content not in your library
    Discover {
        #[command(subcommand)]
        action: DiscoverAction,
        /// Only process one or more libraries (comma-separated names)
        #[arg(long)]
        library: Option<String>,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Manage symlink backups
    Backup {
        #[command(subcommand)]
        action: BackupAction,
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Manage the Real-Debrid torrent cache
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Validate and inspect Symlinkarr configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Run a preflight doctor checklist
    Doctor {
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
enum CacheAction {
    /// Build the full RD torrent cache (fetches file info for all downloaded
    /// torrents — may take a long time for large accounts). Ideal as a
    /// nightly scheduled task.
    Build,
    /// Show cache statistics
    Status,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Validate config parsing, secrets indirection and referenced paths
    Validate {
        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
enum QueueAction {
    /// List queue jobs
    List {
        /// Filter by one status
        #[arg(long, value_enum)]
        status: Option<QueueStatusFilter>,
        /// Max jobs to show
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Reset retryable jobs back to queued
    Retry {
        /// Which retryable jobs to reset
        #[arg(long, value_enum, default_value_t = QueueRetryScope::All)]
        scope: QueueRetryScope,
    },
}

#[derive(Subcommand)]
enum RepairAction {
    /// Scan and report dead symlinks
    Scan,
    /// Auto-replace dead symlinks with best matches from RD mount
    Auto {
        /// Dry-run mode — report replacements without creating symlinks
        #[arg(long)]
        dry_run: bool,
        /// Search Prowlarr for replacements when local repair fails
        #[arg(long)]
        self_heal: bool,
    },
    /// Trigger Decypharr's repair for unrepairable items
    Trigger {
        /// Arr name to repair (e.g., "sonarr")
        #[arg(long)]
        arr: Option<String>,
    },
}

#[derive(Subcommand)]
enum DiscoverAction {
    /// List RD content not present in your library
    List,
    /// Add an RD torrent to Decypharr by its torrent ID
    Add {
        /// Real-Debrid torrent ID (shown in 'discover list' output)
        torrent_id: String,
        /// Arr name to route the download to (e.g., "sonarr", "radarr")
        #[arg(long, default_value = "sonarr")]
        arr: String,
    },
}

#[derive(Subcommand)]
enum BackupAction {
    /// Create a full backup of all active symlinks
    Create,
    /// List all available backups
    List,
    /// Restore symlinks from a backup file
    Restore {
        /// Path to the backup JSON file
        file: String,
        /// Dry-run mode — preview what would be restored
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum CleanupAction {
    /// Legacy behavior: check and clean up dead links
    Dead,
    /// Audit existing symlinks and emit a cleanup report JSON
    Audit {
        /// Audit scope (currently: anime)
        #[arg(long, default_value = "anime")]
        scope: String,
        /// Output report path (defaults to backups/cleanup-audit-*.json)
        #[arg(long)]
        out: Option<String>,
    },
    /// Prune symlinks flagged by an audit report
    Prune {
        /// Path to cleanup report JSON produced by `cleanup audit`
        #[arg(long)]
        report: String,
        /// Apply deletions. Without this flag, runs as preview only.
        #[arg(long)]
        apply: bool,
        /// Maximum deletions allowed in one apply run
        #[arg(long)]
        max_delete: Option<usize>,
        /// Confirmation token from prune preview output
        #[arg(long)]
        confirm_token: Option<String>,
        /// Temporarily relax policy enforcement for emergency fallback
        #[arg(long, value_enum, default_value_t = GateMode::Enforce)]
        gate_mode: GateMode,
    },
}

struct CleanupPruneArgs<'a> {
    report: &'a str,
    apply: bool,
    max_delete: Option<usize>,
    confirm_token: Option<&'a str>,
    gate_mode: GateMode,
    library_filter: Option<&'a str>,
    output: OutputFormat,
}

const PANEL_INNER_WIDTH: usize = 40;
const PANEL_VALUE_WIDTH: usize = 12;
const PANEL_TOTAL_WIDTH: usize = PANEL_INNER_WIDTH + 2;

fn panel_left_padding() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|columns| columns.saturating_sub(PANEL_TOTAL_WIDTH) / 2)
        .unwrap_or(0)
}

fn panel_print_line(line: &str) {
    let pad = " ".repeat(panel_left_padding());
    println!("{pad}{line}");
}

fn panel_border(left: char, fill: char, right: char) {
    panel_print_line(&format!(
        "{}{}{}",
        left,
        fill.to_string().repeat(PANEL_INNER_WIDTH),
        right
    ));
}

fn panel_title(title: &str) {
    panel_print_line(&format!("║{title:^width$}║", width = PANEL_INNER_WIDTH));
}

fn panel_kv_row(label: &str, value: impl std::fmt::Display) {
    let content_width = PANEL_INNER_WIDTH.saturating_sub(2);
    let raw = format!(
        "{label:<label_width$}{value:>value_width$}",
        label_width = content_width.saturating_sub(PANEL_VALUE_WIDTH),
        value_width = PANEL_VALUE_WIDTH,
    );
    let clipped: String = raw.chars().take(content_width).collect();
    panel_print_line(&format!("║ {clipped:<width$} ║", width = content_width));
}

/// Print final status summary at the end of symlinkarr execution.
async fn print_final_summary(
    db: &Database,
    added: Option<i64>,
    removed: Option<i64>,
) -> Result<()> {
    let (active, dead, total) = db.get_stats().await?;

    println!();
    panel_border('╔', '═', '╗');
    panel_title("Symlinkarr Summary");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Active symlinks:", active);
    panel_kv_row("  Dead links:", dead);
    panel_kv_row("  Total tracked:", total);

    if added.is_some() || removed.is_some() {
        panel_border('╠', '═', '╣');
        if let Some(count) = added {
            panel_kv_row("  Added:", count);
        }
        if let Some(count) = removed {
            panel_kv_row("  Removed:", count);
        }
    }

    panel_border('╚', '═', '╝');

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // Load configuration
    let cfg = Config::load(cli.config)?;

    // Initialize database
    let db = Database::new(&cfg.db_path).await?;

    match cli.command {
        Commands::Scan {
            dry_run,
            search_missing,
            library,
            output,
        } => {
            let (created, dead) = run_scan(
                &cfg,
                &db,
                dry_run,
                search_missing,
                false,
                output,
                library.as_deref(),
            )
            .await?;
            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "added": created,
                    "removed": dead,
                }));
            } else {
                print_final_summary(&db, Some(created), Some(dead)).await?
            }
        }
        Commands::DryRun {
            verbose,
            library,
            output,
        } => {
            tracing::warn!("'dry-run' command is deprecated; use 'scan --dry-run'");
            let (created, _dead) =
                run_scan(&cfg, &db, true, false, verbose, output, library.as_deref()).await?;
            if output == OutputFormat::Json {
                print_json(&serde_json::json!({ "added": created }));
            } else {
                print_final_summary(&db, Some(created), None).await?
            }
        }
        Commands::Status { health, output } => run_status(&cfg, &db, health, output).await?,
        Commands::Queue { action, output } => run_queue(&db, action, output).await?,
        Commands::Daemon => run_daemon(&cfg, &db).await?,
        Commands::Cleanup {
            action,
            library,
            output,
        } => {
            let removed = run_cleanup(&cfg, &db, action, library.as_deref(), output).await?;
            if output != OutputFormat::Json {
                print_final_summary(&db, None, removed).await?
            }
        }
        Commands::Repair { action, library } => {
            run_repair(&cfg, &db, action, library.as_deref()).await?
        }
        Commands::Discover {
            action,
            library,
            output,
        } => run_discover(&cfg, &db, action, library.as_deref(), output).await?,
        Commands::Backup { action, output } => run_backup(&cfg, &db, action, output).await?,
        Commands::Cache { action } => run_cache(&cfg, &db, action).await?,
        Commands::Config { action } => run_config(&cfg, action).await?,
        Commands::Doctor { output } => run_doctor(&cfg, &db, output).await?,
    }

    Ok(())
}

/// Run a single scan → match → link cycle.
async fn run_scan(
    cfg: &Config,
    db: &Database,
    dry_run: bool,
    search_missing: bool,
    _verbose: bool,
    output: OutputFormat,
    library_filter: Option<&str>,
) -> Result<(i64, i64)> {
    let _stdout_guard = stdout_text_guard(output != OutputFormat::Json);
    info!("=== Symlinkarr Scan ===");

    let selected_libraries = selected_libraries(cfg, library_filter)?;
    ensure_runtime_directories_healthy(&selected_libraries, &cfg.sources).await?;

    // Step 1: Scan libraries
    let lib_scanner = LibraryScanner::new();
    let mut library_items = Vec::new();
    for lib in &selected_libraries {
        library_items.extend(lib_scanner.scan_library(lib));
    }
    library_items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    info!("Step 1/4: {} library items identified", library_items.len());

    // Step 2: Scan sources
    let src_scanner = SourceScanner::new();
    let source_items = if !cfg.realdebrid.api_token.is_empty() {
        use crate::api::realdebrid::RealDebridClient;
        use crate::cache::TorrentCache;

        info!("Initializing Real-Debrid cache...");
        let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
        let cache = TorrentCache::new(db, &rd_client);

        // Sync cache with API
        match cache.sync().await {
            Ok(_) => info!("Real-Debrid cache synced successfully"),
            Err(e) => tracing::error!(
                "Failed to sync Real-Debrid cache: {}. Using existing cache if available.",
                e
            ),
        }

        // Check whether the cache has enough file-data coverage to be useful.
        // If less than 80% of downloaded torrents have file info, fall back to
        // a filesystem walk so we don't silently miss most of the library.
        const MIN_CACHE_COVERAGE: f64 = 0.80;
        let cache_available = match db.get_rd_torrent_counts().await {
            Ok((cached, total)) if total > 0 => {
                let coverage = cached as f64 / total as f64;
                if coverage >= MIN_CACHE_COVERAGE {
                    info!(
                        "Using cached source data ({}/{} downloaded torrents, {:.0}% coverage)",
                        cached,
                        total,
                        coverage * 100.0
                    );
                    true
                } else {
                    info!(
                        "Cache coverage too low ({}/{} = {:.0}%), walking filesystem instead",
                        cached,
                        total,
                        coverage * 100.0
                    );
                    false
                }
            }
            Ok(_) => {
                info!("Cache unavailable (no downloaded torrents), walking filesystem");
                false
            }
            Err(e) => {
                tracing::warn!(
                    "Could not query RD torrent count: {}. Walking filesystem.",
                    e
                );
                false
            }
        };

        let mut all_items = Vec::new();
        for source in &cfg.sources {
            if cache_available {
                match src_scanner.scan_source_with_cache(source, &cache).await {
                    Ok(items) => all_items.extend(items),
                    Err(e) => {
                        tracing::error!(
                            "Failed to read cache for source {}: {}. Falling back to filesystem scan.",
                            source.name,
                            e
                        );
                        all_items.extend(src_scanner.scan_source(source));
                    }
                }
            } else {
                all_items.extend(src_scanner.scan_source(source));
            }
        }
        all_items
    } else {
        src_scanner.scan_all(&cfg.sources)
    };
    info!("Step 2/4: {} source files found", source_items.len());

    // Step 3: Match
    let tmdb = if cfg.has_tmdb() {
        Some(TmdbClient::new(
            &cfg.api.tmdb_api_key,
            Some(&cfg.api.tmdb_read_access_token),
            cfg.api.cache_ttl_hours,
        ))
    } else {
        if cfg.matching.metadata_mode.allows_network() {
            tracing::warn!("No TMDB API key configured — matching limited to local titles");
        } else {
            tracing::info!(
                "TMDB API key not configured; metadata mode {:?} avoids network lookups",
                cfg.matching.metadata_mode
            );
        }
        None
    };

    let tvdb = if cfg.has_tvdb() {
        Some(TvdbClient::new(
            &cfg.api.tvdb_api_key,
            cfg.api.cache_ttl_hours,
        ))
    } else {
        None
    };

    let matcher = Matcher::new(
        tmdb.clone(),
        tvdb,
        cfg.matching.mode,
        cfg.matching.metadata_mode,
        cfg.matching.metadata_concurrency,
    );
    let mut matches = matcher
        .find_matches(&library_items, &source_items, db)
        .await?;
    info!("Step 3/4: {} matches confirmed", matches.len());

    // Step 4: Create symlinks
    let effective_dry_run = dry_run || cfg.symlink.dry_run;
    let linker = Linker::new_with_options(
        effective_dry_run,
        cfg.matching.mode.is_strict(),
        &cfg.symlink.naming_template,
        cfg.features.reconcile_links,
    );
    matcher.enrich_episode_titles(&mut matches, db).await?;
    let link_summary = linker.process_matches(&matches, db).await?;
    info!(
        "Step 4/4: symlinks created={}, updated={}, skipped={}",
        link_summary.created, link_summary.updated, link_summary.skipped
    );

    // Bazarr: trigger subtitle search for newly linked content
    let linked_total = link_summary.created + link_summary.updated;
    if linked_total > 0 && !effective_dry_run && cfg.has_bazarr() {
        let bazarr = BazarrClient::new(&cfg.bazarr);
        match bazarr.trigger_sync().await {
            Ok(_) => user_println("   📝 Bazarr: subtitle search triggered for new content"),
            Err(e) => user_println(format!("   ⚠️  Bazarr subtitle trigger failed: {}", e)),
        }
    }

    if linked_total > 0 && !effective_dry_run && cfg.has_plex() {
        if let Err(e) = trigger_plex_refresh(cfg, &link_summary.refresh_paths).await {
            user_println(format!("   ⚠️  Plex refresh failed: {}", e));
        }
    }

    // Record scan in history
    db.record_scan(
        library_items.len() as i64,
        source_items.len() as i64,
        matches.len() as i64,
        linked_total as i64,
    )
    .await?;

    // Full dead-link sweeps are expensive on the RD FUSE mount.
    // Keep normal scans fast; use repair/cleanup, or search-missing passes, for full reconciliation.
    let dead = if search_missing {
        let library_roots: Vec<_> = selected_libraries
            .iter()
            .map(|lib| lib.path.clone())
            .collect();
        let dead = linker
            .check_dead_links_scoped(db, Some(&library_roots))
            .await?;
        if dead.dead_marked > 0 {
            info!(
                "Dead links: marked={}, removed={}, skipped={}",
                dead.dead_marked, dead.removed, dead.skipped
            );
        }
        dead
    } else {
        info!("Skipping full dead-link sweep during scan for performance");
        crate::linker::DeadLinkSummary::default()
    };

    // External acquire providers: Prowlarr first, DMM fallback when configured.
    if search_missing && (cfg.has_prowlarr() || cfg.has_dmm()) && cfg.has_decypharr() {
        if !effective_dry_run {
            user_println(
                "\n   ⚠️  --search-missing triggers external grabs. Ensure you intended side effects.",
            );
        }
        use std::collections::HashSet;

        // Find library items that didn't get matched
        let matched_ids: HashSet<_> = matches.iter().map(|m| &m.library_item.id).collect();
        let matched_media_ids: HashSet<String> = matches
            .iter()
            .map(|m| m.library_item.id.to_string())
            .collect();
        let unmatched: Vec<_> = library_items
            .iter()
            .filter(|item| !matched_ids.contains(&item.id))
            .collect();
        let mut requests = Vec::new();
        let max_grabs = cfg.decypharr.max_requests_per_run;

        if !unmatched.is_empty() {
            user_println(format!(
                "\n   🔍 Auto-acquire: evaluating {} unmatched library items...",
                unmatched.len()
            ));
            let mut attempted_queries = HashSet::new();

            for item in &unmatched {
                if requests.len() >= max_grabs {
                    user_println(format!(
                        "\n   ⚠️  Auto-acquire: reached safety limit of {} requests. Stopping queue build.",
                        max_grabs
                    ));
                    break;
                }
                if item.media_type == MediaType::Tv {
                    continue;
                }

                let Some(query) = build_missing_search_query(item) else {
                    user_println(format!(
                        "      ⚠️  '{}' → skipped auto-grab (query too ambiguous)",
                        item.title
                    ));
                    continue;
                };
                if !attempted_queries.insert(query.clone()) {
                    continue;
                }
                let cats = prowlarr_categories(item.media_type, item.content_type);
                let request = AutoAcquireRequest {
                    label: item.title.clone(),
                    query: query.clone(),
                    imdb_id: lookup_item_imdb_id(tmdb.as_ref(), db, item).await,
                    categories: cats,
                    arr: decypharr_arr_name(cfg, item.media_type, item.content_type).to_string(),
                    library_filter: Some(item.library_name.clone()),
                    relink_check: RelinkCheck::MediaId(item.id.to_string()),
                };
                requests.push(request);
            }
        }

        if requests.len() < max_grabs {
            let remaining = max_grabs - requests.len();
            let cutoff_budget = if remaining >= 4 { remaining / 2 } else { 0 };
            let missing_budget = remaining.saturating_sub(cutoff_budget);

            let anime_missing_requests = match anime_scanner::build_anime_episode_requests(
                anime_scanner::AnimeEpisodeKind::Missing,
                cfg,
                db,
                tmdb.as_ref(),
                &library_items,
                &matched_media_ids,
                missing_budget,
            )
            .await
            {
                Ok(requests) => requests,
                Err(err) => {
                    user_println(format!(
                        "   ⚠️  Sonarr Anime missing lookup failed: {}. Continuing without episode-missing acquire.",
                        err
                    ));
                    Vec::new()
                }
            };
            if !anime_missing_requests.is_empty() {
                user_println(format!(
                    "   🎌 Sonarr Anime: queued {} episode-specific missing request(s)",
                    anime_missing_requests.len()
                ));
                requests.extend(anime_missing_requests);
            }

            let remaining = max_grabs - requests.len();
            if remaining > 0 {
                let anime_cutoff_requests = match anime_scanner::build_anime_episode_requests(
                    anime_scanner::AnimeEpisodeKind::CutoffUpgrade,
                    cfg,
                    db,
                    tmdb.as_ref(),
                    &library_items,
                    &matched_media_ids,
                    remaining,
                )
                .await
                {
                    Ok(requests) => requests,
                    Err(err) => {
                        user_println(format!(
                            "   ⚠️  Sonarr Anime cutoff lookup failed: {}. Continuing without cutoff upgrades.",
                            err
                        ));
                        Vec::new()
                    }
                };
                if !anime_cutoff_requests.is_empty() {
                    user_println(format!(
                        "   🎌 Sonarr Anime: queued {} cutoff-upgrade request(s)",
                        anime_cutoff_requests.len()
                    ));
                    requests.extend(anime_cutoff_requests);
                }
            }
        }

        match process_auto_acquire_queue(cfg, db, requests, effective_dry_run).await {
            Ok(summary) => {
                if summary.total > 0 {
                    user_println(format!(
                        "\n   📡 Auto-acquire summary: submitted={}, linked={}, completed_unlinked={}, no_result={}, blocked={}, failed={}",
                        summary.submitted,
                        summary.completed_linked,
                        summary.completed_unlinked,
                        summary.no_result,
                        summary.blocked,
                        summary.failed
                    ));
                }
            }
            Err(err) => {
                user_println(format!(
                    "\n   ⚠️  Auto-acquire failed: {}. Scan completed without external acquisition.",
                    err
                ));
            }
        }
    } else if search_missing && !cfg.has_prowlarr() && !cfg.has_dmm() {
        user_println(
            "\n   ⚠️  --search-missing specified but neither Prowlarr nor DMM is configured",
        );
    } else if search_missing && !cfg.has_decypharr() {
        user_println("\n   ⚠️  --search-missing specified but Decypharr not configured");
    }

    db.record_scan_run(&crate::db::ScanRunRecord {
        dry_run: effective_dry_run,
        library_items_found: library_items.len() as i64,
        source_items_found: source_items.len() as i64,
        matches_found: matches.len() as i64,
        links_created: link_summary.created as i64,
        links_updated: link_summary.updated as i64,
        dead_marked: dead.dead_marked as i64,
        links_removed: dead.removed as i64,
        links_skipped: (link_summary.skipped + dead.skipped) as i64,
        ambiguous_skipped: 0,
    })
    .await?;

    info!("=== Scan Complete ===");
    Ok((linked_total as i64, dead.removed as i64))
}

/// Show database statistics.
async fn run_status(cfg: &Config, db: &Database, health: bool, output: OutputFormat) -> Result<()> {
    let (active, dead, total) = db.get_stats().await?;
    let acquisition = db.get_acquisition_job_counts().await?;
    let acquisition_json = serde_json::json!({
        "active": acquisition.active_total(),
        "queued": acquisition.queued,
        "downloading": acquisition.downloading,
        "relinking": acquisition.relinking,
        "blocked": acquisition.blocked,
        "no_result": acquisition.no_result,
        "failed": acquisition.failed,
        "completed_unlinked": acquisition.completed_unlinked,
    });
    let emit_text = output != OutputFormat::Json;

    if !health && !emit_text {
        print_json(&serde_json::json!({
            "active": active,
            "dead": dead,
            "total": total,
            "acquisition": acquisition_json,
        }));
        return Ok(());
    }

    if emit_text {
        panel_border('╔', '═', '╗');
        panel_title("Symlinkarr Status");
        panel_border('╠', '═', '╣');
        panel_kv_row("  Active symlinks:", active);
        panel_kv_row("  Dead links:", dead);
        panel_kv_row("  Total:", total);
        panel_border('╠', '═', '╣');
        panel_kv_row("  Auto-acquire active:", acquisition.active_total());
        if acquisition.active_total() > 0 {
            panel_kv_row("  Queued:", acquisition.queued);
            panel_kv_row("  Downloading:", acquisition.downloading);
            panel_kv_row("  Relinking:", acquisition.relinking);
            panel_kv_row("  Blocked:", acquisition.blocked);
            panel_kv_row("  No result:", acquisition.no_result);
            panel_kv_row("  Failed:", acquisition.failed);
            panel_kv_row("  Unlinked:", acquisition.completed_unlinked);
        }
        panel_border('╚', '═', '╝');
    }

    if health {
        let mut health_json = Vec::new();
        if emit_text {
            println!("\n🏥 Health Check:");
        }

        // Tautulli check
        if cfg.has_tautulli() {
            let tautulli = TautulliClient::new(&cfg.tautulli);
            match tautulli.get_activity().await {
                Ok(response) => {
                    let stream_count = response.stream_count;
                    if emit_text {
                        println!(
                            "   ✅ Tautulli: Connected ({} active streams)",
                            stream_count
                        );
                    }
                    health_json.push(serde_json::json!({
                        "service": "tautulli",
                        "ok": true,
                        "streams": stream_count,
                    }));

                    if emit_text {
                        if let Ok(history) = tautulli.get_history(10, None).await {
                            println!(
                                "      Recent activity: {} entries fetched",
                                history.data.len()
                            );
                        }
                    }
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Tautulli: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "tautulli",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Tautulli: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "tautulli",
                "configured": false
            }));
        }

        if cfg.has_plex() {
            let plex = PlexClient::new(&cfg.plex.url, &cfg.plex.token);
            match plex.get_sections().await {
                Ok(sections) => {
                    if emit_text {
                        println!("   ✅ Plex: Connected ({} section(s))", sections.len());
                    }
                    health_json.push(serde_json::json!({
                        "service": "plex",
                        "ok": true,
                        "sections": sections.len(),
                    }));
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Plex: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "plex",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Plex: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "plex",
                "configured": false
            }));
        }

        // Prowlarr check
        if cfg.has_prowlarr() {
            let prowlarr = ProwlarrClient::new(&cfg.prowlarr);
            match prowlarr.get_system_status().await {
                Ok(_) => {
                    if emit_text {
                        println!("   ✅ Prowlarr: Connected (URL: {})", cfg.prowlarr.url);
                    }
                    health_json.push(serde_json::json!({
                        "service": "prowlarr",
                        "ok": true,
                    }));
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Prowlarr: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "prowlarr",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Prowlarr: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "prowlarr",
                "configured": false
            }));
        }

        // Bazarr check
        if cfg.has_bazarr() {
            let bazarr = BazarrClient::new(&cfg.bazarr);
            match bazarr.health_check().await {
                Ok(_) => {
                    if emit_text {
                        println!("   ✅ Bazarr: Connected (URL: {})", cfg.bazarr.url);
                    }
                    health_json.push(serde_json::json!({
                        "service": "bazarr",
                        "ok": true,
                    }));
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Bazarr: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "bazarr",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Bazarr: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "bazarr",
                "configured": false
            }));
        }

        // Radarr check
        if cfg.has_radarr() {
            let radarr =
                crate::api::radarr::RadarrClient::new(&cfg.radarr.url, &cfg.radarr.api_key);
            match radarr.get_system_status().await {
                Ok(_) => {
                    if emit_text {
                        println!("   ✅ Radarr: Connected (URL: {})", cfg.radarr.url);
                    }
                    health_json.push(serde_json::json!({
                        "service": "radarr",
                        "ok": true,
                    }));
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Radarr: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "radarr",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Radarr: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "radarr",
                "configured": false
            }));
        }

        // Sonarr check
        if cfg.has_sonarr() {
            let sonarr =
                crate::api::sonarr::SonarrClient::new(&cfg.sonarr.url, &cfg.sonarr.api_key);
            match sonarr.get_system_status().await {
                Ok(_) => {
                    if emit_text {
                        println!("   ✅ Sonarr: Connected (URL: {})", cfg.sonarr.url);
                    }
                    health_json.push(serde_json::json!({
                        "service": "sonarr",
                        "ok": true,
                    }));
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Sonarr: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "sonarr",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Sonarr: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "sonarr",
                "configured": false
            }));
        }

        // Sonarr Anime check
        if cfg.has_sonarr_anime() {
            let sonarr = crate::api::sonarr::SonarrClient::new(
                &cfg.sonarr_anime.url,
                &cfg.sonarr_anime.api_key,
            );
            match sonarr.get_system_status().await {
                Ok(_) => {
                    if emit_text {
                        println!(
                            "   ✅ Sonarr-Anime: Connected (URL: {})",
                            cfg.sonarr_anime.url
                        );
                    }
                    health_json.push(serde_json::json!({
                        "service": "sonarr_anime",
                        "ok": true,
                    }));
                }
                Err(e) => {
                    if emit_text {
                        println!("   ❌ Sonarr-Anime: Connection error ({})", e);
                    }
                    health_json.push(serde_json::json!({
                        "service": "sonarr_anime",
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        } else {
            if emit_text {
                println!("   ⚪ Sonarr-Anime: Not configured");
            }
            health_json.push(serde_json::json!({
                "service": "sonarr_anime",
                "configured": false
            }));
        }

        if !emit_text {
            print_json(&serde_json::json!({
                "active": active,
                "dead": dead,
                "total": total,
                "acquisition": acquisition_json,
                "health": health_json,
            }));
        }
    }

    Ok(())
}

async fn run_queue(db: &Database, action: QueueAction, output: OutputFormat) -> Result<()> {
    match action {
        QueueAction::List { status, limit } => {
            let statuses = status.map(|value| vec![value.into_job_status()]);
            let jobs = db
                .list_acquisition_jobs(statuses.as_deref(), limit.max(1))
                .await?;

            if output == OutputFormat::Json {
                let rows = jobs
                    .iter()
                    .map(|job| {
                        serde_json::json!({
                            "id": job.id,
                            "status": job.status.as_str(),
                            "label": job.label,
                            "query": job.query,
                            "arr": job.arr,
                            "attempts": job.attempts,
                            "error": job.error,
                            "next_retry_at": job.next_retry_at.map(|dt| dt.to_rfc3339()),
                            "release_title": job.release_title,
                        })
                    })
                    .collect::<Vec<_>>();
                print_json(&serde_json::json!({
                    "count": rows.len(),
                    "jobs": rows,
                }));
            } else if jobs.is_empty() {
                println!("No queue jobs found.");
            } else {
                println!("\n🧾 Auto-Acquire Queue ({})", jobs.len());
                for job in jobs {
                    println!(
                        "   #{} [{}] {}",
                        job.id,
                        format!("{:?}", job.status).to_lowercase(),
                        job.label
                    );
                    println!("      query: {}", job.query);
                    println!("      arr: {}, attempts: {}", job.arr, job.attempts);
                    if let Some(next_retry_at) = job.next_retry_at {
                        println!("      next retry: {}", next_retry_at.to_rfc3339());
                    }
                    if let Some(error) = &job.error {
                        println!("      error: {}", error);
                    }
                }
            }
        }
        QueueAction::Retry { scope } => {
            let statuses = match scope {
                QueueRetryScope::All => vec![
                    AcquisitionJobStatus::Blocked,
                    AcquisitionJobStatus::NoResult,
                    AcquisitionJobStatus::Failed,
                    AcquisitionJobStatus::CompletedUnlinked,
                ],
                QueueRetryScope::Blocked => vec![AcquisitionJobStatus::Blocked],
                QueueRetryScope::NoResult => vec![AcquisitionJobStatus::NoResult],
                QueueRetryScope::Failed => vec![AcquisitionJobStatus::Failed],
                QueueRetryScope::CompletedUnlinked => vec![AcquisitionJobStatus::CompletedUnlinked],
            };
            let reset = db.retry_acquisition_jobs(&statuses).await?;

            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "reset": reset,
                    "scope": format!("{:?}", scope).to_lowercase(),
                }));
            } else {
                println!(
                    "Reset {} queue job(s) to queued for scope '{}'.",
                    reset,
                    format!("{:?}", scope).to_lowercase()
                );
            }
        }
    }

    Ok(())
}

async fn trigger_plex_refresh(cfg: &Config, refresh_paths: &[std::path::PathBuf]) -> Result<()> {
    if refresh_paths.is_empty() || !cfg.has_plex() {
        return Ok(());
    }

    let plex = PlexClient::new(&cfg.plex.url, &cfg.plex.token);
    let sections = plex.get_sections().await?;
    let mut unique_paths = refresh_paths.to_vec();
    unique_paths.sort();
    unique_paths.dedup();

    let mut refreshed = 0usize;
    let mut skipped = 0usize;

    for path in unique_paths {
        let Some(section) = find_section_for_path(&sections, &path) else {
            println!(
                "   ⚠️  Plex: no matching library section found for {}",
                path.display()
            );
            skipped += 1;
            continue;
        };

        match plex.refresh_path(&section.key, &path).await {
            Ok(_) => refreshed += 1,
            Err(err) => {
                println!(
                    "   ⚠️  Plex: refresh failed for {} (section '{}'): {}",
                    path.display(),
                    section.title,
                    err
                );
                skipped += 1;
            }
        }
    }

    if refreshed > 0 {
        println!(
            "   📺 Plex: targeted refresh queued for {} path(s)",
            refreshed
        );
    }
    if skipped > 0 {
        println!("   ⚠️  Plex: {} path(s) were not refreshed", skipped);
    }

    Ok(())
}

/// Run continuously as a daemon.
async fn run_daemon(cfg: &Config, db: &Database) -> Result<()> {
    let interval = Duration::from_secs(cfg.daemon.interval_minutes * 60);
    info!(
        "Symlinkarr daemon starting (interval: {} minutes)",
        cfg.daemon.interval_minutes
    );

    // Housekeeping on startup: prune stale records that accumulate unboundedly.
    match db.housekeeping().await {
        Ok(stats) => {
            if stats.scan_runs_deleted + stats.link_events_deleted + stats.old_jobs_deleted > 0 {
                info!(
                    "Housekeeping: removed {} old scan_runs, {} link_events, {} completed jobs",
                    stats.scan_runs_deleted, stats.link_events_deleted, stats.old_jobs_deleted
                );
            }
        }
        Err(e) => tracing::warn!("Housekeeping failed (non-fatal): {}", e),
    }

    // C-06: Recover jobs stuck in Downloading after a crash.
    match db
        .recover_stale_downloading_jobs(cfg.decypharr.completion_timeout_minutes)
        .await
    {
        Ok(n) if n > 0 => info!("Recovered {} stale Downloading jobs after restart", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("Stale job recovery failed (non-fatal): {}", e),
    }

    loop {
        // Create safety snapshot before each scan cycle
        if cfg.backup.enabled {
            let bm = backup::BackupManager::new(&cfg.backup);
            if let Err(e) = bm.create_safety_snapshot(db, "daemon-scan").await {
                tracing::warn!("Pre-scan backup failed: {}", e);
            }
        }

        if let Err(e) = run_scan(
            cfg,
            db,
            false,
            cfg.daemon.search_missing,
            false,
            OutputFormat::Text,
            None,
        )
        .await
        {
            tracing::error!("Scan cycle failed: {}", e);
        }

        info!("Next scan in {} minutes...", cfg.daemon.interval_minutes);
        tokio::time::sleep(interval).await;
    }
}

/// Check and clean up dead links.
async fn run_cleanup(
    cfg: &Config,
    db: &Database,
    action: Option<CleanupAction>,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<Option<i64>> {
    match action {
        None | Some(CleanupAction::Dead) => Ok(Some(
            run_cleanup_dead(cfg, db, library_filter, output).await?,
        )),
        Some(CleanupAction::Audit { scope, out }) => {
            run_cleanup_audit(cfg, db, &scope, out, library_filter, output).await?;
            Ok(None)
        }
        Some(CleanupAction::Prune {
            report,
            apply,
            max_delete,
            confirm_token,
            gate_mode,
        }) => {
            let removed = run_cleanup_prune(
                cfg,
                db,
                CleanupPruneArgs {
                    report: &report,
                    apply,
                    max_delete,
                    confirm_token: confirm_token.as_deref(),
                    gate_mode,
                    library_filter,
                    output,
                },
            )
            .await?;
            Ok(Some(removed))
        }
    }
}

/// Legacy dead-link cleanup behavior.
async fn run_cleanup_dead(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<i64> {
    info!("=== Symlinkarr Cleanup ===");
    let selected = selected_libraries(cfg, library_filter)?;
    let library_roots: Vec<_> = selected.iter().map(|l| l.path.clone()).collect();

    // SAFETY: Create backup snapshot before any destructive cleanup
    if cfg.backup.enabled {
        let bm = backup::BackupManager::new(&cfg.backup);
        bm.create_safety_snapshot(db, "cleanup").await?;
    }

    let linker = Linker::new_with_options(
        cfg.symlink.dry_run,
        cfg.matching.mode.is_strict(),
        &cfg.symlink.naming_template,
        cfg.features.reconcile_links,
    );
    let dead = linker
        .check_dead_links_scoped(db, Some(&library_roots))
        .await?;
    info!(
        "Handled dead links: marked={}, removed={}, skipped={}",
        dead.dead_marked, dead.removed, dead.skipped
    );
    if output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "dead_marked": dead.dead_marked,
            "removed": dead.removed,
            "skipped": dead.skipped,
        }));
    }
    Ok(dead.removed as i64)
}

/// Audit symlinks and write a cleanup report.
async fn run_cleanup_audit(
    cfg: &Config,
    db: &Database,
    scope: &str,
    out: Option<String>,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<()> {
    info!("=== Symlinkarr Cleanup Audit ===");

    let scope = CleanupScope::parse(scope)?;
    let auditor = CleanupAuditor::new_with_progress(cfg, db, output != OutputFormat::Json);
    let out_path = auditor
        .run_audit(scope, out.as_deref().map(std::path::Path::new))
        .await?;

    let report_json = std::fs::read_to_string(&out_path)?;
    let mut report: cleanup_audit::CleanupReport = serde_json::from_str(&report_json)?;

    if library_filter.is_some() {
        let selected = selected_libraries(cfg, library_filter)?;
        let roots: Vec<_> = selected.iter().map(|lib| lib.path.clone()).collect();
        filter_cleanup_report_by_roots(&mut report, &roots);
        std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
    }

    if output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "file": out_path,
            "scope": format!("{:?}", report.scope).to_lowercase(),
            "findings": report.summary.total_findings,
            "critical": report.summary.critical,
            "high": report.summary.high,
            "warning": report.summary.warning,
        }));
    } else {
        println!("\n🧹 Cleanup Audit Report");
        println!("   File: {}", out_path.display());
        println!("   Findings: {}", report.summary.total_findings);
        println!("   Critical: {}", report.summary.critical);
        println!("   High: {}", report.summary.high);
        println!("   Warning: {}", report.summary.warning);
        println!(
            "   Next: symlinkarr cleanup prune --report {} --apply",
            out_path.display()
        );
    }

    Ok(())
}

/// Prune symlinks from a cleanup report.
async fn run_cleanup_prune(cfg: &Config, db: &Database, args: CleanupPruneArgs<'_>) -> Result<i64> {
    info!("=== Symlinkarr Cleanup Prune ===");
    let CleanupPruneArgs {
        report,
        apply,
        max_delete,
        confirm_token,
        gate_mode,
        library_filter,
        output,
    } = args;

    let report_path = std::path::Path::new(report);
    if !report_path.exists() {
        anyhow::bail!("Cleanup report not found: {}", report);
    }

    if matches!(gate_mode, GateMode::Relaxed) {
        tracing::warn!(
            "gate-mode=relaxed requested; policy gating is controlled by config cleanup.prune.enforce_policy"
        );
    }

    let mut effective_report_path = report_path.to_path_buf();
    let mut temporary_report: Option<std::path::PathBuf> = None;
    if library_filter.is_some() {
        let selected = selected_libraries(cfg, library_filter)?;
        let roots: Vec<_> = selected.iter().map(|lib| lib.path.clone()).collect();
        let report_json = std::fs::read_to_string(report_path)?;
        let mut report: cleanup_audit::CleanupReport = serde_json::from_str(&report_json)?;
        filter_cleanup_report_by_roots(&mut report, &roots);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let tmp = std::env::temp_dir().join(format!("symlinkarr-prune-filtered-{}.json", ts));
        std::fs::write(&tmp, serde_json::to_string_pretty(&report)?)?;
        effective_report_path = tmp.clone();
        temporary_report = Some(tmp);
    }

    let outcome = cleanup_audit::run_prune(
        cfg,
        db,
        &effective_report_path,
        apply,
        max_delete,
        confirm_token,
    )
    .await?;

    if let Some(tmp) = temporary_report {
        let _ = std::fs::remove_file(tmp);
    }

    if output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "apply": apply,
            "candidates": outcome.candidates,
            "high_or_critical_candidates": outcome.high_or_critical_candidates,
            "safe_warning_duplicate_candidates": outcome.safe_warning_duplicate_candidates,
            "removed": outcome.removed,
            "skipped": outcome.skipped,
            "confirmation_token": outcome.confirmation_token,
        }));
    } else {
        if apply {
            println!("\n🧹 Cleanup Prune Applied");
        } else {
            println!("\n🧹 Cleanup Prune Preview");
        }
        println!("   Candidates: {}", outcome.candidates);
        println!(
            "   High/Critical candidates: {}",
            outcome.high_or_critical_candidates
        );
        println!(
            "   Safe duplicate-warning candidates: {}",
            outcome.safe_warning_duplicate_candidates
        );
        println!("   Removed: {}", outcome.removed);
        println!("   Skipped: {}", outcome.skipped);
        if !apply {
            println!("   Confirmation token: {}", outcome.confirmation_token);
            println!(
                "   ℹ️  Re-run with --apply --confirm-token <token> to remove flagged symlinks"
            );
        }
    }

    Ok(outcome.removed as i64)
}

/// Handle repair subcommands.
async fn run_repair(
    cfg: &Config,
    db: &Database,
    action: RepairAction,
    library_filter: Option<&str>,
) -> Result<()> {
    let repairer = repair::Repairer::new();
    let selected = selected_libraries(cfg, library_filter)?;
    let selected_library_paths: Vec<_> = selected.iter().map(|l| l.path.clone()).collect();

    match action {
        RepairAction::Scan => {
            info!("=== Symlinkarr Repair Scan ===");
            let dead = repairer.scan_for_dead_symlinks(&selected_library_paths);

            if dead.is_empty() {
                println!("✅ No dead symlinks found!");
            } else {
                println!("\n⚠️  {} dead symlinks found:\n", dead.len());
                for d in &dead {
                    println!("  ✗ {:?} → {:?}", d.symlink_path, d.original_source);
                }
            }
        }
        RepairAction::Auto { dry_run, self_heal } => {
            info!("=== Symlinkarr Repair Auto ===");
            if self_heal && !dry_run {
                println!(
                    "   ⚠️  --self-heal may trigger external downloads via Prowlarr/Decypharr."
                );
            }

            // SAFETY: Create backup snapshot before repair
            if cfg.backup.enabled {
                if dry_run {
                    println!("   ℹ️  Skipping safety snapshot in --dry-run mode");
                } else {
                    println!("   🛡️ Creating safety snapshot before repair...");
                    let started = Instant::now();
                    let bm = backup::BackupManager::new(&cfg.backup);
                    bm.create_safety_snapshot(db, "repair").await?;
                    println!(
                        "   ✅ Safety snapshot created in {:.1}s",
                        started.elapsed().as_secs_f64()
                    );
                }
            }

            // Tautulli safe-repair guard: get currently streaming file paths
            let skip_paths = if cfg.has_tautulli() {
                let tautulli = TautulliClient::new(&cfg.tautulli);
                match tautulli.get_active_file_paths().await {
                    Ok(paths) => {
                        if !paths.is_empty() {
                            println!(
                                "   🎬 Tautulli: {} active streams detected — protecting those files",
                                paths.len()
                            );
                        }
                        paths
                    }
                    Err(e) => {
                        println!(
                            "   ⚠️  Tautulli query failed ({}), proceeding without guard",
                            e
                        );
                        vec![]
                    }
                }
            } else {
                vec![]
            };

            let source_paths: Vec<_> = cfg.sources.iter().map(|s| s.path.clone()).collect();

            // Use cache-backed catalog when coverage is sufficient
            let rd_client = if cfg.has_realdebrid() {
                Some(crate::api::realdebrid::RealDebridClient::from_config(
                    &cfg.realdebrid,
                ))
            } else {
                None
            };
            let torrent_cache = rd_client
                .as_ref()
                .map(|rd| crate::cache::TorrentCache::new(db, rd));
            const REPAIR_CACHE_COVERAGE: f64 = 0.80;
            let cache_ref = if let Some(ref tc) = torrent_cache {
                match db.get_rd_torrent_counts().await {
                    Ok((cached, total)) if total > 0 => {
                        let coverage = cached as f64 / total as f64;
                        if coverage >= REPAIR_CACHE_COVERAGE {
                            println!(
                                "   ⚡ Using RD cache for repair catalog ({:.0}% coverage)",
                                coverage * 100.0
                            );
                            Some(tc)
                        } else {
                            println!(
                                "   ℹ️  RD cache coverage {:.0}% < 80%, using filesystem walk",
                                coverage * 100.0
                            );
                            None
                        }
                    }
                    _ => None,
                }
            } else {
                None
            };

            let results = repairer
                .repair_all(
                    db,
                    &source_paths,
                    dry_run,
                    &skip_paths,
                    Some(&selected_library_paths),
                    cache_ref,
                )
                .await?;

            let repaired: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Repaired { .. }))
                .collect();
            let unrepairable: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Unrepairable { .. }))
                .collect();

            println!("\n📋 Repair Results:");
            println!("   ✅ Repaired: {}", repaired.len());
            for result in &repaired {
                if let repair::RepairResult::Repaired {
                    dead_link,
                    replacement,
                } = result
                {
                    println!(
                        "      ✓ {} → {:?}",
                        dead_link.meta.title,
                        replacement.file_name().unwrap_or_default()
                    );
                }
            }
            println!("   ❌ Unrepairable: {}", unrepairable.len());
            for result in &unrepairable {
                if let repair::RepairResult::Unrepairable { dead_link, reason } = result {
                    println!("      ✗ {} — {}", dead_link.meta.title, reason);
                }
            }

            let skipped: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Skipped { .. }))
                .collect();
            if !skipped.is_empty() {
                println!("   ⏸️  Skipped (streaming): {}", skipped.len());
                for result in &skipped {
                    if let repair::RepairResult::Skipped { dead_link, reason } = result {
                        println!("      ⏸ {} — {}", dead_link.meta.title, reason);
                    }
                }
            }

            let stale: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Stale { .. }))
                .collect();
            if !stale.is_empty() {
                println!("   🗃️  Stale DB records: {}", stale.len());
                if dry_run {
                    println!(
                        "      ℹ️ Re-run without --dry-run to mark stale records as removed in DB"
                    );
                }
            }

            // External self-heal: Prowlarr first, DMM fallback when configured.
            if self_heal
                && !unrepairable.is_empty()
                && (cfg.has_prowlarr() || cfg.has_dmm())
                && cfg.has_decypharr()
            {
                use std::collections::HashSet;

                let mut attempted_queries = HashSet::new();
                let mut requests = Vec::new();
                let max_requests = cfg.decypharr.max_requests_per_run;

                for result in &unrepairable {
                    if requests.len() >= max_requests {
                        println!(
                            "   ⚠️  Self-heal reached safety limit of {} requests. Stopping queue build.",
                            max_requests
                        );
                        break;
                    }
                    if let repair::RepairResult::Unrepairable { dead_link, .. } = result {
                        // Build search query from metadata
                        let Some(query) = build_repair_self_heal_query(dead_link) else {
                            println!(
                                "   ⚠️  Skipping self-heal for '{}' — query too ambiguous",
                                dead_link.meta.title
                            );
                            continue;
                        };

                        // Pick category based on media type
                        let cats =
                            prowlarr_categories(dead_link.media_type, dead_link.content_type);
                        let request = AutoAcquireRequest {
                            label: dead_link.meta.title.clone(),
                            query: query.clone(),
                            imdb_id: None,
                            categories: cats,
                            arr: decypharr_arr_name(
                                cfg,
                                dead_link.media_type,
                                dead_link.content_type,
                            )
                            .to_string(),
                            library_filter: library_name_for_path(cfg, &dead_link.symlink_path),
                            relink_check: RelinkCheck::SymlinkPath(dead_link.symlink_path.clone()),
                        };

                        // Avoid repeatedly trying the same title/season query.
                        if !attempted_queries.insert(query.clone()) {
                            continue;
                        }

                        requests.push(request);
                    }
                }

                let summary = process_auto_acquire_queue(cfg, db, requests, dry_run).await?;
                if summary.total > 0 {
                    println!(
                        "\n   📡 Self-heal summary: submitted={}, linked={}, completed_unlinked={}, no_result={}, blocked={}, failed={}",
                        summary.submitted,
                        summary.completed_linked,
                        summary.completed_unlinked,
                        summary.no_result,
                        summary.blocked,
                        summary.failed
                    );
                    if !dry_run {
                        println!("   ℹ️  Symlinkarr processed the queue with live progress and relink attempts");
                    }
                }
            } else if self_heal && !unrepairable.is_empty() && !cfg.has_prowlarr() && !cfg.has_dmm()
            {
                println!(
                    "\n   ⚠️  --self-heal specified but neither Prowlarr nor DMM is configured"
                );
            } else if self_heal && !unrepairable.is_empty() && !cfg.has_decypharr() {
                println!("\n   ⚠️  --self-heal specified but Decypharr not configured");
            }

            // Bazarr notification: notify for repaired items
            if !repaired.is_empty() && cfg.has_bazarr() && !dry_run {
                let bazarr = BazarrClient::new(&cfg.bazarr);
                println!(
                    "\n   📝 Notifying Bazarr about {} repaired files...",
                    repaired.len()
                );
                // Note: Full Bazarr integration requires Sonarr/Radarr IDs.
                // For now we log that Bazarr was configured but skip the actual call
                // since we don't track arr-specific IDs in Symlinkarr's DB yet.
                info!("Bazarr integration ready — needs Sonarr/Radarr IDs in future");
                let _ = bazarr; // suppress unused warning
            }
        }
        RepairAction::Trigger { arr } => {
            if !cfg.has_decypharr() {
                anyhow::bail!("Decypharr not configured in config.yaml");
            }

            let client = api::decypharr::DecypharrClient::from_config(&cfg.decypharr);

            let msg = client.trigger_repair(arr.as_deref(), vec![], true).await?;
            println!("✅ {}", msg);
        }
    }

    Ok(())
}

/// Handle discover subcommands.
async fn run_discover(
    cfg: &Config,
    _db: &Database,
    action: DiscoverAction,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<()> {
    match action {
        DiscoverAction::List => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }

            info!("=== Symlinkarr Discovery ===");

            let rd_client = RealDebridClient::from_config(&cfg.realdebrid);

            // Scan libraries to know what we already have
            let lib_scanner = LibraryScanner::new();
            let selected = selected_libraries(cfg, library_filter)?;
            let mut library_items = Vec::new();
            for lib in &selected {
                library_items.extend(lib_scanner.scan_library(lib));
            }

            // Sync cache before discovery so we have fresh data without
            // discovery itself making a redundant list_all_torrents() call.
            {
                use crate::cache::TorrentCache;
                let cache = TorrentCache::new(_db, &rd_client);
                if let Err(e) = cache.sync().await {
                    tracing::warn!("Failed to sync RD cache for discovery: {}", e);
                }
            }

            let disc = discovery::Discovery::new();
            let gaps = disc.find_gaps(_db, &library_items).await?;

            if output == OutputFormat::Json {
                #[derive(Serialize)]
                struct GapSummary<'a> {
                    rd_torrent_id: &'a str,
                    status: &'a str,
                    size: i64,
                    parsed_title: &'a str,
                }

                let out: Vec<GapSummary<'_>> = gaps
                    .iter()
                    .map(|g| GapSummary {
                        rd_torrent_id: &g.rd_torrent_id,
                        status: &g.status,
                        size: g.size,
                        parsed_title: &g.parsed_title,
                    })
                    .collect();
                print_json(&serde_json::json!({
                    "count": out.len(),
                    "items": out,
                }));
            } else {
                discovery::Discovery::print_summary(&gaps);
            }
        }
        DiscoverAction::Add { torrent_id, arr } => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }
            if !cfg.has_decypharr() {
                anyhow::bail!("Decypharr not configured in config.yaml");
            }

            info!("=== Symlinkarr Discover Add ===");

            // Get torrent info from RD to build magnet link
            let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
            let torrent_info = rd_client.get_torrent_info(&torrent_id).await?;

            let magnet = format!("magnet:?xt=urn:btih:{}", torrent_info.hash);
            println!(
                "\n🔗 Torrent: {} ({})",
                torrent_info.filename,
                bytesize(torrent_info.bytes)
            );
            println!("   Magnet hash: {}", torrent_info.hash);

            // Send to Decypharr
            let decypharr = api::decypharr::DecypharrClient::from_config(&cfg.decypharr);
            let arr = resolve_decypharr_arr_name(&decypharr, &arr).await?;
            let _ = decypharr.add_content(&[magnet], &arr, "none").await?;

            println!("   ✅ Sent to Decypharr ({} → {})", arr, cfg.decypharr.url);
            println!("   ℹ️  Run 'symlinkarr scan' once the download completes.");
        }
    }

    Ok(())
}

/// Format bytes to human-readable size
fn bytesize(bytes: i64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{:.1} GB", gb)
    } else {
        let mb = bytes as f64 / 1_048_576.0;
        format!("{:.1} MB", mb)
    }
}

/// Handle backup subcommands.
async fn run_backup(
    cfg: &Config,
    db: &Database,
    action: BackupAction,
    output: OutputFormat,
) -> Result<()> {
    let bm = backup::BackupManager::new(&cfg.backup);

    match action {
        BackupAction::Create => {
            info!("=== Symlinkarr Backup ===");
            let path = bm.create_backup(db, "Manual backup").await?;
            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "created": true,
                    "file": path,
                }));
            } else {
                println!("✅ Backup created: {}", path.display());
            }
        }
        BackupAction::List => {
            let backups = bm.list()?;
            if output == OutputFormat::Json {
                let items: Vec<_> = backups
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "filename": b.filename,
                            "timestamp": b.timestamp,
                            "type": match &b.backup_type {
                                crate::backup::BackupType::Scheduled => "scheduled",
                                crate::backup::BackupType::Safety { .. } => "safety",
                            },
                            "symlink_count": b.symlink_count,
                            "file_size": b.file_size,
                        })
                    })
                    .collect();
                print_json(&serde_json::json!({
                    "count": items.len(),
                    "items": items,
                }));
            } else if backups.is_empty() {
                println!("No backups found in {:?}", cfg.backup.path);
            } else {
                println!("\n📦 Available backups ({}):\n", backups.len());
                for b in &backups {
                    println!("  {}", b);
                }
                println!();
            }
        }
        BackupAction::Restore { file, dry_run } => {
            info!("=== Symlinkarr Restore ===");
            let path = std::path::Path::new(&file);
            if !path.exists() {
                anyhow::bail!("Backup file not found: {}", file);
            }

            let library_roots: Vec<_> = cfg.libraries.iter().map(|l| l.path.clone()).collect();
            let source_roots: Vec<_> = cfg.sources.iter().map(|s| s.path.clone()).collect();
            let (restored, skipped, errors) = bm
                .restore(
                    db,
                    path,
                    dry_run,
                    &library_roots,
                    &source_roots,
                    cfg.security.enforce_roots,
                )
                .await?;

            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "restored": restored,
                    "skipped": skipped,
                    "errors": errors,
                    "dry_run": dry_run,
                }));
            } else {
                println!("\n📋 Restore Results:");
                println!("   ✅ Restored: {}", restored);
                println!("   ⏭️  Skipped: {}", skipped);
                if errors > 0 {
                    println!("   ❌ Errors: {}", errors);
                }
            }
        }
    }

    Ok(())
}

async fn run_cache(cfg: &Config, db: &Database, action: CacheAction) -> Result<()> {
    use crate::api::realdebrid::RealDebridClient;
    use crate::cache::TorrentCache;

    if !cfg.has_realdebrid() {
        anyhow::bail!("Real-Debrid API key not configured in config.yaml");
    }

    let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
    let cache = TorrentCache::new(db, &rd_client);

    match action {
        CacheAction::Build => {
            info!("=== Symlinkarr Cache Build (full, no fetch cap) ===");
            println!("Building full RD torrent cache — this may take a while for large accounts.");
            cache.sync_full().await?;

            let (cached, total) = db.get_rd_torrent_counts().await?;
            println!(
                "\nCache build complete: {}/{} downloaded torrents have file info ({:.0}%)",
                cached,
                total,
                if total > 0 {
                    cached as f64 / total as f64 * 100.0
                } else {
                    0.0
                }
            );
        }
        CacheAction::Status => {
            let (cached, total) = db.get_rd_torrent_counts().await?;
            let coverage = if total > 0 {
                cached as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            println!("RD cache status:");
            println!("  Downloaded torrents:    {}", total);
            println!("  With file info cached:  {} ({:.0}%)", cached, coverage);
            if coverage < 80.0 {
                println!("  Scanner mode:           filesystem walk (coverage < 80%)");
            } else {
                println!("  Scanner mode:           cache-based (coverage >= 80%)");
            }
        }
    }

    Ok(())
}

async fn run_config(cfg: &Config, action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Validate { output } => {
            let report = cfg.validate();
            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "ok": report.errors.is_empty(),
                    "errors": report.errors,
                    "warnings": report.warnings,
                }));
                if !report.errors.is_empty() {
                    anyhow::bail!("Configuration validation failed");
                }
                return Ok(());
            } else if report.errors.is_empty() {
                println!("✅ Configuration validation passed");
            } else {
                println!("❌ Configuration validation failed:");
                for err in &report.errors {
                    println!("   - {}", err);
                }
            }

            if output == OutputFormat::Text && !report.warnings.is_empty() {
                println!("⚠️  Warnings:");
                for w in &report.warnings {
                    println!("   - {}", w);
                }
            }

            if !report.errors.is_empty() {
                anyhow::bail!("Configuration validation failed");
            }
        }
    }
    Ok(())
}

async fn run_doctor(cfg: &Config, db: &Database, output: OutputFormat) -> Result<()> {
    #[derive(Serialize)]
    struct DoctorCheck {
        name: String,
        ok: bool,
        detail: String,
    }

    let mut checks = Vec::new();

    match db.get_stats().await {
        Ok((active, dead, total)) => checks.push(DoctorCheck {
            name: "database".to_string(),
            ok: true,
            detail: format!(
                "reachable (active={}, dead={}, total={})",
                active, dead, total
            ),
        }),
        Err(e) => checks.push(DoctorCheck {
            name: "database".to_string(),
            ok: false,
            detail: format!("unreachable: {}", e),
        }),
    }
    match db.schema_version().await {
        Ok(version) => checks.push(DoctorCheck {
            name: "db_schema_version".to_string(),
            ok: version >= 1,
            detail: version.to_string(),
        }),
        Err(e) => checks.push(DoctorCheck {
            name: "db_schema_version".to_string(),
            ok: false,
            detail: e.to_string(),
        }),
    }
    let db_parent = std::path::Path::new(&cfg.db_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    checks.push(DoctorCheck {
        name: "db_parent_dir".to_string(),
        ok: can_write_in_directory(db_parent),
        detail: db_parent.display().to_string(),
    });

    for lib in &cfg.libraries {
        let health =
            directory_path_health_with_timeout(lib.path.clone(), DIRECTORY_PROBE_TIMEOUT).await;
        checks.push(DoctorCheck {
            name: format!("library:{}", lib.name),
            ok: health.is_healthy(),
            detail: health.describe(&lib.path),
        });
    }

    for src in &cfg.sources {
        let probe_path = runtime_source_probe_path(&src.path);
        let health = runtime_source_health(&src.path, &probe_path).await;
        checks.push(DoctorCheck {
            name: format!("source:{}", src.name),
            ok: health.is_healthy(),
            detail: health.describe(&probe_path),
        });
    }

    checks.push(DoctorCheck {
        name: "backup_dir".to_string(),
        ok: can_write_in_directory(&cfg.backup.path),
        detail: cfg.backup.path.display().to_string(),
    });
    checks.push(DoctorCheck {
        name: "backup.max_safety_backups".to_string(),
        ok: cfg.backup.max_safety_backups > 0,
        detail: cfg.backup.max_safety_backups.to_string(),
    });

    checks.push(DoctorCheck {
        name: "security.enforce_roots".to_string(),
        ok: cfg.security.enforce_roots,
        detail: cfg.security.enforce_roots.to_string(),
    });
    checks.push(DoctorCheck {
        name: "security.require_secret_provider".to_string(),
        ok: cfg.security.require_secret_provider,
        detail: cfg.security.require_secret_provider.to_string(),
    });
    checks.push(DoctorCheck {
        name: "cleanup.prune.enforce_policy".to_string(),
        ok: cfg.cleanup.prune.enforce_policy,
        detail: cfg.cleanup.prune.enforce_policy.to_string(),
    });

    let validation = cfg.validate();
    checks.push(DoctorCheck {
        name: "config_validation".to_string(),
        ok: validation.errors.is_empty(),
        detail: format_validation_detail(&validation),
    });

    if output == OutputFormat::Json {
        let failed = checks.iter().filter(|c| !c.ok).count();
        print_json(&serde_json::json!({
            "checks": checks,
            "failed": failed,
        }));
    } else {
        println!("🩺 Symlinkarr Doctor");
        for c in &checks {
            let icon = if c.ok { "✅" } else { "❌" };
            println!("   {} {:<34} {}", icon, c.name, c.detail);
        }
    }

    Ok(())
}

async fn ensure_runtime_directories_healthy(
    libraries: &[&crate::config::LibraryConfig],
    sources: &[crate::config::SourceConfig],
) -> Result<()> {
    for lib in libraries {
        let health =
            directory_path_health_with_timeout(lib.path.clone(), DIRECTORY_PROBE_TIMEOUT).await;
        if !health.is_healthy() {
            anyhow::bail!(
                "Library '{}' is not healthy: {}",
                lib.name,
                health.describe(&lib.path)
            );
        }
    }

    for src in sources {
        let probe_path = runtime_source_probe_path(&src.path);
        let health = runtime_source_health(&src.path, &probe_path).await;
        if !health.is_healthy() {
            anyhow::bail!(
                "Source '{}' is not healthy: {}",
                src.name,
                health.describe(&probe_path)
            );
        }
    }

    Ok(())
}

fn runtime_source_probe_path(path: &std::path::Path) -> std::path::PathBuf {
    let file_name = path.file_name().and_then(|name| name.to_str());
    if file_name == Some("__all__") {
        return path.parent().unwrap_or(path).to_path_buf();
    }

    path.to_path_buf()
}

async fn runtime_source_health(path: &std::path::Path, probe_path: &std::path::Path) -> PathHealth {
    let fast_health = fast_path_health(path);
    if !fast_health.is_healthy() {
        return fast_health;
    }

    let probe_health =
        directory_path_health_with_timeout(probe_path.to_path_buf(), DIRECTORY_PROBE_TIMEOUT).await;
    probe_health
}

fn can_write_in_directory(path: &std::path::Path) -> bool {
    if std::fs::create_dir_all(path).is_err() {
        return false;
    }

    let probe = path.join(format!(
        ".symlinkarr-write-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));

    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn format_validation_detail(report: &crate::config::ValidationReport) -> String {
    let mut detail = format!(
        "errors={}, warnings={}",
        report.errors.len(),
        report.warnings.len()
    );

    if !report.errors.is_empty() {
        detail.push_str("; ");
        detail.push_str(&report.errors.join(" | "));
    } else if !report.warnings.is_empty() {
        detail.push_str("; ");
        detail.push_str(&report.warnings.join(" | "));
    }

    detail
}

fn prowlarr_categories(media_type: MediaType, content_type: ContentType) -> Vec<i32> {
    match (media_type, content_type) {
        (MediaType::Movie, _) => vec![api::prowlarr::categories::MOVIES],
        (MediaType::Tv, ContentType::Anime) => vec![api::prowlarr::categories::TV_ANIME],
        (MediaType::Tv, _) => vec![
            api::prowlarr::categories::TV,
            api::prowlarr::categories::TV_ANIME,
        ],
    }
}

fn decypharr_arr_name(
    cfg: &Config,
    media_type: MediaType,
    content_type: ContentType,
) -> &'static str {
    match (media_type, content_type) {
        (MediaType::Movie, _) => "radarr",
        (MediaType::Tv, ContentType::Anime) if cfg.has_sonarr_anime() => "sonarr-anime",
        (MediaType::Tv, _) => "sonarr",
    }
}

fn library_name_for_path(cfg: &Config, path: &std::path::Path) -> Option<String> {
    cfg.libraries
        .iter()
        .find(|lib| path.starts_with(&lib.path))
        .map(|lib| lib.name.clone())
}

async fn resolve_decypharr_arr_name(
    client: &api::decypharr::DecypharrClient,
    requested: &str,
) -> Result<String> {
    let arrs = client.get_arrs().await?;
    if let Some(arr) = arrs.iter().find(|arr| {
        normalize_decypharr_arr_name(&arr.name) == normalize_decypharr_arr_name(requested)
    }) {
        return Ok(arr.name.clone());
    }

    let available = arrs
        .iter()
        .map(|arr| arr.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "Decypharr Arr '{}' not found. Available: {}",
        requested,
        available
    );
}

fn normalize_decypharr_arr_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn build_missing_search_query(item: &LibraryItem) -> Option<String> {
    if item.media_type == MediaType::Tv {
        return None;
    }

    let query = item.title.trim().to_string();
    is_safe_auto_acquire_query(&query).then_some(query)
}

pub(crate) async fn lookup_item_imdb_id(
    tmdb: Option<&TmdbClient>,
    db: &Database,
    item: &LibraryItem,
) -> Option<String> {
    let tmdb = tmdb?;

    match (&item.id, item.media_type) {
        (MediaId::Tmdb(id), MediaType::Movie) => {
            tmdb.get_movie_imdb_id(*id, db).await.ok().flatten()
        }
        (MediaId::Tmdb(id), MediaType::Tv) => tmdb.get_tv_imdb_id(*id, db).await.ok().flatten(),
        _ => None,
    }
}

fn build_repair_self_heal_query(dead_link: &repair::DeadLink) -> Option<String> {
    let mut query = dead_link.meta.title.trim().to_string();
    if query.is_empty() {
        query = dead_link.media_id.clone();
    }

    match dead_link.media_type {
        MediaType::Tv => match (dead_link.meta.season, dead_link.meta.episode) {
            (Some(season), Some(episode)) => {
                query.push_str(&format!(" S{:02}E{:02}", season, episode));
            }
            (Some(season), None) => {
                query.push_str(&format!(" S{:02}", season));
            }
            _ => {}
        },
        MediaType::Movie => {
            if let Some(year) = dead_link.meta.year {
                query.push_str(&format!(" {}", year));
            }
        }
    }

    is_safe_auto_acquire_query(&query).then_some(query)
}

pub(crate) fn is_safe_auto_acquire_query(query: &str) -> bool {
    let normalized = crate::utils::normalize(query);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }

    let has_year = tokens
        .iter()
        .any(|token| token.len() == 4 && token.chars().all(|c| c.is_ascii_digit()));
    let has_episode = tokens.iter().any(|token| {
        let lower = token.to_ascii_lowercase();
        if let Some((season, episode)) = lower.split_once('e') {
            let season = season.strip_prefix('s').unwrap_or("");
            !season.is_empty()
                && !episode.is_empty()
                && season.chars().all(|c| c.is_ascii_digit())
                && episode.chars().all(|c| c.is_ascii_digit())
        } else {
            false
        }
    });
    let strong_words = tokens
        .iter()
        .filter(|token| token.chars().any(|c| c.is_ascii_alphabetic()) && token.len() >= 4)
        .count();
    let longest_word = tokens.iter().map(|token| token.len()).max().unwrap_or(0);

    has_year || has_episode || strong_words >= 2 || longest_word >= 7
}

fn selected_libraries<'a>(
    cfg: &'a Config,
    library_filter: Option<&str>,
) -> Result<Vec<&'a crate::config::LibraryConfig>> {
    let Some(filter) = library_filter else {
        return Ok(cfg.libraries.iter().collect());
    };

    let wanted: Vec<String> = filter
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(cfg.libraries.iter().collect());
    }

    let mut selected = Vec::new();
    for lib in &cfg.libraries {
        if wanted.iter().any(|w| lib.name.eq_ignore_ascii_case(w)) {
            selected.push(lib);
        }
    }

    let missing: Vec<_> = wanted
        .iter()
        .filter(|want| {
            !cfg.libraries
                .iter()
                .any(|lib| lib.name.eq_ignore_ascii_case(want))
        })
        .cloned()
        .collect();

    if !missing.is_empty() {
        let available = cfg
            .libraries
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "Unknown library filter(s): {}. Available: {}",
            missing.join(", "),
            available
        );
    }

    Ok(selected)
}

fn filter_cleanup_report_by_roots(
    report: &mut cleanup_audit::CleanupReport,
    roots: &[std::path::PathBuf],
) {
    report
        .findings
        .retain(|f| path_under_roots(&f.symlink_path, roots));
    report.summary.total_findings = report.findings.len();
    report.summary.critical = report
        .findings
        .iter()
        .filter(|f| matches!(f.severity, cleanup_audit::FindingSeverity::Critical))
        .count();
    report.summary.high = report
        .findings
        .iter()
        .filter(|f| matches!(f.severity, cleanup_audit::FindingSeverity::High))
        .count();
    report.summary.warning = report
        .findings
        .iter()
        .filter(|f| matches!(f.severity, cleanup_audit::FindingSeverity::Warning))
        .count();
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{}", json),
        Err(e) => println!(r#"{{"error":"json_encode_failed","detail":"{}"}}"#, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_uses_config_search_paths_when_flag_is_omitted() {
        let cli = Cli::try_parse_from(["symlinkarr", "doctor"]).unwrap();
        assert_eq!(cli.config, None);
    }

    #[test]
    fn cli_keeps_explicit_config_path() {
        let cli =
            Cli::try_parse_from(["symlinkarr", "--config", "/tmp/config.yaml", "doctor"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("/tmp/config.yaml"));
    }

    #[test]
    fn cli_accepts_json_output_for_config_validate() {
        let cli = Cli::try_parse_from(["symlinkarr", "config", "validate", "--output", "json"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn can_write_in_directory_creates_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("doctor").join("probe");
        assert!(can_write_in_directory(&nested));
        assert!(nested.exists());
    }

    #[test]
    fn runtime_source_probe_uses_mount_root_for_all_directory() {
        let path = std::path::Path::new("/mnt/decypharr/realdebrid/__all__");
        assert_eq!(
            runtime_source_probe_path(path),
            std::path::PathBuf::from("/mnt/decypharr/realdebrid")
        );
    }

    #[test]
    fn runtime_source_probe_leaves_normal_paths_unchanged() {
        let path = std::path::Path::new("/srv/media/source");
        assert_eq!(runtime_source_probe_path(path), path);
    }

    #[test]
    fn acquisition_job_status_json_values_are_canonical() {
        assert_eq!(AcquisitionJobStatus::NoResult.as_str(), "no_result");
        assert_eq!(
            AcquisitionJobStatus::CompletedUnlinked.as_str(),
            "completed_unlinked"
        );
    }

    #[test]
    fn format_validation_detail_includes_counts_and_messages() {
        let report = crate::config::ValidationReport {
            errors: vec!["missing library".to_string()],
            warnings: vec!["plaintext secret".to_string()],
        };
        let detail = format_validation_detail(&report);
        assert!(detail.contains("errors=1"));
        assert!(detail.contains("warnings=1"));
        assert!(detail.contains("missing library"));
        assert!(!detail.contains("plaintext secret"));

        let warnings_only = crate::config::ValidationReport {
            errors: Vec::new(),
            warnings: vec!["backup.max_safety_backups=0".to_string()],
        };
        let warning_detail = format_validation_detail(&warnings_only);
        assert!(warning_detail.contains("errors=0"));
        assert!(warning_detail.contains("warnings=1"));
        assert!(warning_detail.contains("backup.max_safety_backups=0"));
    }

    #[test]
    fn safe_auto_acquire_queries_require_enough_signal() {
        assert!(!is_safe_auto_acquire_query("It"));
        assert!(!is_safe_auto_acquire_query("You"));
        assert!(!is_safe_auto_acquire_query("Arcane"));
        assert!(is_safe_auto_acquire_query("Severance"));
        assert!(is_safe_auto_acquire_query("The Matrix 1999"));
        assert!(is_safe_auto_acquire_query("Breaking Bad S01E01"));
    }

    #[test]
    fn missing_auto_acquire_skips_tv_libraries() {
        let tv = LibraryItem {
            id: crate::models::MediaId::Tvdb(81189),
            path: std::path::PathBuf::from("/tmp/Breaking Bad {tvdb-81189}"),
            title: "Breaking Bad".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        };
        let movie = LibraryItem {
            id: crate::models::MediaId::Tmdb(603),
            path: std::path::PathBuf::from("/tmp/The Matrix {tmdb-603}"),
            title: "The Matrix 1999".to_string(),
            library_name: "Movies".to_string(),
            media_type: MediaType::Movie,
            content_type: ContentType::Movie,
        };

        assert_eq!(build_missing_search_query(&tv), None);
        assert_eq!(
            build_missing_search_query(&movie),
            Some("The Matrix 1999".to_string())
        );
    }

    #[test]
    fn normalize_decypharr_arr_name_ignores_separators() {
        assert_eq!(normalize_decypharr_arr_name("sonarr_anime"), "sonarranime");
        assert_eq!(normalize_decypharr_arr_name("sonarr-anime"), "sonarranime");
    }
}
