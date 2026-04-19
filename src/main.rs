#[cfg(not(unix))]
compile_error!("Symlinkarr requires a Unix platform (symlink support).");

mod anime_identity;
mod anime_roots;
mod anime_scanner;
mod api;
mod auto_acquire;
mod backup;
mod cache;
mod cleanup_audit;
mod commands;
mod config;
mod db;
mod discovery;
mod library_scanner;
mod linker;
mod matcher;
mod media_servers;
mod models;
mod repair;
mod source_scanner;
mod startup;
mod utils;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use crate::commands::print_final_summary;
use crate::db::AcquisitionJobStatus;

const ROOT_AFTER_HELP: &str = r#"Feature guide:
  scan      = look at your library and source mount, then create/update symlinks
  repair    = find dead symlinks and relink them to the best replacement
  cleanup   = inspect dead/legacy links first, then prune only when confirmed
  discover  = preview source-to-target placements for tagged folders that still need clean links
  queue     = inspect or retry persistent auto-acquire jobs
  backup    = snapshot state and restore only from the configured backup directory
  cache     = build the RD torrent cache or invalidate/clear sticky metadata entries
  doctor    = run preflight checks before a real scan or daemon run
  report    = export structured operator reports, including advanced legacy-anime cleanup inputs
  daemon    = run scheduled scans continuously
  web       = run the built-in operator UI and JSON API

Security modes:
  local-only       = trusted loopback mode, open by default
  remote operator  = remote bind plus Basic auth for the built-in UI
  scripted operator= optional API key for automation clients

Operator notes:
  status --health = shallow integration presence/activation summary
  doctor          = preflight checklist for DB schema, paths, and runtime safety

Known limit:
  anime specials without good anime-lists hints may still need manual terms

Docs:
  docs/CLI_MANUAL.md
  docs/API_SCHEMA.md
  docs/PRODUCT_SCOPE.md
  docs/GITHUB_WIKI_FEATURES.md
  docs/CHANGELOG.md
  docs/dev-notes/
"#;

// ─── CLI definitions ───────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "symlinkarr",
    about = "Symlinkarr — local-first symlink manager for Real-Debrid-backed Plex, Emby, and Jellyfin libraries",
    after_help = ROOT_AFTER_HELP,
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
pub(crate) enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum GateMode {
    Enforce,
    Relaxed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum QueueStatusFilter {
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
pub(crate) enum QueueRetryScope {
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
        /// Show what would change without touching symlinks or the DB
        #[arg(long)]
        dry_run: bool,
        /// Also build missing-content acquisition requests during the scan
        #[arg(long)]
        search_missing: bool,
        /// Restrict the run to one configured library name
        #[arg(long)]
        library: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Run in dry-run mode (scan + match, no symlink creation)
    #[command(hide = true)]
    DryRun {
        #[arg(short, long)]
        verbose: bool,
        #[arg(long)]
        library: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Show database statistics and optional integration health/presence summary
    Status {
        #[arg(long)]
        health: bool,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Inspect and manage persistent auto-acquire jobs
    Queue {
        #[command(subcommand)]
        action: QueueAction,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Run scheduled scans and maintenance continuously
    Daemon,
    /// Run the built-in operator UI and JSON API without the daemon loop
    Web {
        /// Override the configured web port for this run
        #[arg(long)]
        port: Option<u16>,
    },
    /// Cleanup workflows: dead links, audit, and prune
    Cleanup {
        #[command(subcommand)]
        action: Option<CleanupAction>,
        #[arg(long)]
        library: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Repair dead symlinks by finding replacements
    Repair {
        #[command(subcommand)]
        action: RepairAction,
        #[arg(long)]
        library: Option<String>,
    },
    /// Discover RD cache content not in your library
    Discover {
        #[command(subcommand)]
        action: DiscoverAction,
        #[arg(long)]
        library: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Manage symlink backups
    Backup {
        #[command(subcommand)]
        action: BackupAction,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Restore from a backup archive without needing a config file
    Restore {
        /// Path to the backup .json file
        file: String,
        /// Target directory for restored config and database
        #[arg(long)]
        dir: Option<String>,
        /// Preview what would be restored without writing files
        #[arg(long)]
        dry_run: bool,
        /// Show backup contents without restoring
        #[arg(long)]
        list: bool,
    },
    /// Create a starter config and required directories for a fresh install
    Bootstrap {
        /// Target directory
        #[arg(long)]
        dir: Option<String>,
        /// Show what is missing without creating anything
        #[arg(long)]
        list: bool,
    },
    /// Manage RD torrent cache and sticky metadata cache entries
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Validate and inspect Symlinkarr configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Run a preflight checklist for DB schema, paths, and runtime safety
    Doctor {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Generate operator reports for library state and drift
    Report {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
        /// Filter by media type (movie, series)
        #[arg(long)]
        filter: Option<String>,
        /// Filter to a specific configured library name
        #[arg(long)]
        library: Option<String>,
        /// Optional path to Plex's library database for path-set drift compare
        #[arg(long)]
        plex_db: Option<String>,
        /// Include all anime duplicate groups instead of the default sample-limited output
        #[arg(long)]
        full_anime_duplicates: bool,
        /// Optional TSV export path for the advanced legacy-anime cleanup queue
        #[arg(long)]
        anime_remediation_tsv: Option<String>,
        /// Pretty-print JSON output
        #[arg(long)]
        pretty: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum CacheAction {
    /// Build the full RD torrent cache
    Build,
    /// Show cache statistics
    Status,
    /// Invalidate cached metadata for a specific media ID or cache key
    Invalidate {
        /// Cache key prefix, exact key, or short-form ID (e.g., "tmdb:tv:", "tmdb:tv:12345", "tmdb:12345", "tvdb:67890", "anime-lists")
        key: String,
    },
    /// Clear all cached API metadata
    Clear,
}

#[derive(Subcommand)]
pub(crate) enum ConfigAction {
    /// Validate config parsing, secrets indirection and referenced paths
    Validate {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
pub(crate) enum QueueAction {
    /// List queue jobs
    List {
        #[arg(long, value_enum)]
        status: Option<QueueStatusFilter>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Reset retryable jobs back to queued
    Retry {
        #[arg(long, value_enum, default_value_t = QueueRetryScope::All)]
        scope: QueueRetryScope,
    },
}

#[derive(Subcommand)]
pub(crate) enum RepairAction {
    /// Scan and report dead symlinks
    Scan,
    /// Auto-replace dead symlinks with best matches from RD mount
    Auto {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        self_heal: bool,
    },
    /// Trigger Decypharr's repair for unrepairable items
    Trigger {
        #[arg(long)]
        arr: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum DiscoverAction {
    /// Preview source-to-target placements for folders that still need clean links
    List,
    /// Legacy/manual handoff: send an RD torrent to Decypharr by its torrent ID
    Add {
        torrent_id: String,
        #[arg(long, default_value = "sonarr")]
        arr: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum BackupAction {
    /// Create a full backup of all active symlinks
    Create,
    /// List all available backups
    List,
    /// Restore symlinks from a backup file
    Restore {
        file: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum CleanupAction {
    /// Legacy behavior: check and clean up dead links
    Dead,
    /// Audit existing symlinks and emit a cleanup report JSON
    Audit {
        #[arg(long, default_value = "anime")]
        scope: String,
        #[arg(long)]
        out: Option<String>,
    },
    /// Prune symlinks flagged by an audit report
    Prune {
        #[arg(long)]
        report: String,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        include_legacy_anime_roots: bool,
        #[arg(long)]
        max_delete: Option<usize>,
        #[arg(long)]
        confirm_token: Option<String>,
        #[arg(long, value_enum, default_value_t = GateMode::Enforce)]
        gate_mode: GateMode,
    },
    /// Preview or apply advanced legacy-anime cleanup for split roots / Plex Hama duplicates
    RemediateAnime {
        /// Existing remediation report generated by a prior preview run
        #[arg(long)]
        report: Option<String>,
        /// Optional path to Plex's library database
        #[arg(long)]
        plex_db: Option<String>,
        /// Apply the report instead of generating a new preview
        #[arg(long)]
        apply: bool,
        /// Restrict preview generation to titles whose normalized name contains this string
        #[arg(long)]
        title: Option<String>,
        /// Optional output path for the saved remediation report JSON
        #[arg(long)]
        out: Option<String>,
        /// Confirmation token from the remediation preview
        #[arg(long)]
        confirm_token: Option<String>,
        /// Maximum candidate symlinks allowed during apply before refusing
        #[arg(long)]
        max_delete: Option<usize>,
        #[arg(long, value_enum, default_value_t = GateMode::Enforce)]
        gate_mode: GateMode,
    },
}

// ─── Entry point ───────────────────────────────────────────────────

fn init_minimal_logger() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn missing_config_target_path(cli: &Cli) -> std::path::PathBuf {
    if let Some(path) = &cli.config {
        return std::path::PathBuf::from(path);
    }

    if std::path::Path::new("/app/config").is_dir() {
        std::path::PathBuf::from("/app/config/config.yaml")
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("config.yaml")
    }
}

fn resolved_config_path(cli: &Cli) -> Option<std::path::PathBuf> {
    config::candidate_config_paths(cli.config.clone())
        .into_iter()
        .find(|path| path.exists())
}

/// When config.yaml is missing, try to auto-restore from the latest backup.
/// Guard: only runs if config.yaml genuinely does not exist anywhere on the search path.
/// Returns Some(Config) if restore succeeded, None if no backup was found.
async fn try_auto_restore(cli: &Cli) -> Result<Option<config::Config>> {
    use std::path::Path;

    // Guard: if config.yaml exists at any candidate path, never auto-restore.
    for candidate in &config::candidate_config_paths(cli.config.clone()) {
        if candidate.exists() {
            return Ok(None);
        }
    }

    // Search for backups in default locations
    let backup_dirs = vec![
        std::path::PathBuf::from("backups"),
        std::path::PathBuf::from("/app/config/backups"),
        std::path::PathBuf::from("/app/backups"),
    ];

    for backup_dir in &backup_dirs {
        if !backup_dir.is_dir() {
            continue;
        }

        let bm = backup::BackupManager::new(&config::BackupConfig::standalone(backup_dir.clone()));
        let backups = match bm.list() {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Prefer the latest scheduled backup; fall back to latest overall
        let best = backups
            .iter()
            .filter(|b| matches!(b.backup_type, backup::BackupType::Scheduled))
            .max_by_key(|b| b.timestamp);

        let best = match best.or_else(|| backups.iter().max_by_key(|b| b.timestamp)) {
            Some(b) => b,
            None => continue,
        };

        println!(
            "No config.yaml found. Auto-restoring from: {}",
            best.filename
        );
        tracing::info!("Auto-restoring from backup: {}", best.filename);

        let config_path = missing_config_target_path(cli);
        let config_dir = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        // Parse manifest to restore app-state
        let json = match std::fs::read_to_string(&best.path) {
            Ok(j) => j,
            Err(_) => continue,
        };
        let manifest = match backup::parse_backup_manifest(&json, &best.path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Restore app state (config + secrets) without overwriting existing files
        commands::restore::restore_app_state_auto(&bm, &manifest, &config_path)?;

        let db_target = if config_path.exists() {
            match config::inspect_restore_targets(&config_path) {
                Ok(targets) => targets.db_path,
                Err(err) => {
                    tracing::warn!(
                        "Auto-restore: failed to inspect restored config {}: {}",
                        config_path.display(),
                        err
                    );
                    config_dir.join("symlinkarr.db")
                }
            }
        } else {
            config_dir.join("symlinkarr.db")
        };

        // Restore database snapshot if present and not already on disk
        if let Some(ref db_snap) = manifest.database_snapshot {
            let source = match bm.resolve_restore_path(Path::new(&db_snap.filename)) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !db_target.exists() {
                if let Some(parent) = db_target.parent() {
                    if !parent.as_os_str().is_empty() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            tracing::warn!(
                                "Auto-restore: failed to create database directory {}: {}",
                                parent.display(),
                                e
                            );
                            continue;
                        }
                    }
                }
                if let Err(e) = std::fs::copy(&source, &db_target) {
                    tracing::warn!(
                        "Auto-restore: failed to copy database snapshot to {}: {}",
                        db_target.display(),
                        e
                    );
                }
            }
        }

        // Try loading config again
        match config::Config::load(cli.config.clone()) {
            Ok(cfg) => {
                println!("Auto-restore succeeded. Starting normally.");
                tracing::info!("Auto-restore succeeded, config loaded from restored file");
                return Ok(Some(cfg));
            }
            Err(e) => {
                tracing::warn!(
                    "Auto-restore wrote files but config still fails to load: {}",
                    e
                );
                return Err(e);
            }
        }
    }

    Ok(None)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Restore and Bootstrap can run without a config file.
    // Handle them before loading config so a fresh install can use them.
    match &cli.command {
        Commands::Restore {
            file,
            dir,
            dry_run,
            list,
        } => {
            init_minimal_logger();
            return commands::restore::run_standalone_restore(
                std::path::Path::new(file),
                dir.as_deref().map(std::path::Path::new),
                *dry_run,
                *list,
            )
            .await;
        }
        Commands::Bootstrap { dir, list } => {
            init_minimal_logger();
            return commands::bootstrap::run_bootstrap(
                dir.as_deref().map(std::path::Path::new),
                *list,
            );
        }
        _ => {}
    }

    let cfg = match config::Config::load(cli.config.clone()) {
        Ok(cfg) => cfg,
        Err(e) => {
            init_minimal_logger();
            // Config missing: try auto-restore from latest backup before giving up.
            if let Some(restored_cfg) = try_auto_restore(&cli).await? {
                restored_cfg
            } else if let Commands::Web { port } = &cli.command {
                // No backup found + web command = serve the no-config setup page
                crate::web::serve_noconfig(port.unwrap_or(8726)).await?;
                return Ok(());
            } else {
                return Err(e);
            }
        }
    };
    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new(cfg.log_level.clone()))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(log_filter)
        .with_writer(std::io::stderr)
        .try_init();
    let db = db::Database::new(&cfg.db_path).await?;
    let config_path = resolved_config_path(&cli);

    match cli.command {
        Commands::Scan {
            dry_run,
            search_missing,
            library,
            output,
        } => {
            let (created, dead) = commands::scan::run_scan(
                &cfg,
                &db,
                dry_run,
                search_missing,
                output,
                library.as_deref(),
            )
            .await?;
            if output == OutputFormat::Json {
                commands::print_json(&serde_json::json!({
                    "added": created,
                    "removed": dead,
                }));
            } else {
                print_final_summary(&db, Some(created), Some(dead)).await?
            }
        }
        Commands::DryRun {
            verbose: _,
            library,
            output,
        } => {
            tracing::warn!("'dry-run' command is deprecated; use 'scan --dry-run'");
            let (created, _dead) =
                commands::scan::run_scan(&cfg, &db, true, false, output, library.as_deref())
                    .await?;
            if output == OutputFormat::Json {
                commands::print_json(&serde_json::json!({ "added": created }));
            } else {
                print_final_summary(&db, Some(created), None).await?
            }
        }
        Commands::Status { health, output } => {
            commands::status::run_status(&cfg, &db, health, output).await?
        }
        Commands::Queue { action, output } => {
            commands::queue::run_queue(&db, action, output).await?
        }
        Commands::Daemon => {
            startup::emit_runtime_banner(
                &cfg,
                startup::RuntimeMode::Daemon,
                config_path.as_deref(),
                None,
            );
            commands::daemon::run_daemon(&cfg, &db).await?
        }
        Commands::Web { port } => {
            let port = port.unwrap_or(cfg.web.port);
            startup::emit_runtime_banner(
                &cfg,
                startup::RuntimeMode::Web,
                config_path.as_deref(),
                Some(port),
            );
            crate::web::serve(cfg, db, port).await?
        }
        Commands::Cleanup {
            action,
            library,
            output,
        } => {
            let removed =
                commands::cleanup::run_cleanup(&cfg, &db, action, library.as_deref(), output)
                    .await?;
            if output != OutputFormat::Json {
                print_final_summary(&db, None, removed).await?
            }
        }
        Commands::Repair { action, library } => {
            commands::repair::run_repair(&cfg, &db, action, library.as_deref()).await?
        }
        Commands::Discover {
            action,
            library,
            output,
        } => {
            commands::discover::run_discover(&cfg, &db, action, library.as_deref(), output).await?
        }
        Commands::Backup { action, output } => {
            commands::backup::run_backup(&cfg, &db, action, output).await?
        }
        Commands::Cache { action } => commands::cache::run_cache(&cfg, &db, action).await?,
        Commands::Config { action } => commands::config::run_config(&cfg, action).await?,
        Commands::Doctor { output } => commands::doctor::run_doctor(&cfg, &db, output).await?,
        Commands::Report {
            output,
            filter,
            library,
            plex_db,
            full_anime_duplicates,
            anime_remediation_tsv,
            pretty,
        } => {
            let media_type_filter = match filter.as_deref() {
                Some("movie") => Some(crate::models::MediaType::Movie),
                Some("series") | Some("tv") => Some(crate::models::MediaType::Tv),
                Some(invalid) => {
                    anyhow::bail!("Invalid filter: {}. Must be 'movie' or 'series'.", invalid)
                }
                None => None,
            };

            commands::report::run_report(
                &cfg,
                &db,
                commands::report::ReportOptions {
                    output_format: output,
                    filter: media_type_filter,
                    library_filter: library.as_deref(),
                    plex_db_path: plex_db.as_deref().map(std::path::Path::new),
                    full_anime_duplicates,
                    anime_remediation_tsv_path: anime_remediation_tsv
                        .as_deref()
                        .map(std::path::Path::new),
                    pretty,
                },
            )
            .await?;
        }
        Commands::Restore { .. } | Commands::Bootstrap { .. } => unreachable!(),
    }

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use tokio::sync::Mutex;

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
    fn cli_accepts_web_subcommand() {
        let cli = Cli::try_parse_from(["symlinkarr", "web", "--port", "9999"]).unwrap();
        match cli.command {
            Commands::Web { port } => assert_eq!(port, Some(9999)),
            _ => panic!("expected web command"),
        }
    }

    #[test]
    fn cli_accepts_report_with_plex_db() {
        let cli = Cli::try_parse_from([
            "symlinkarr",
            "report",
            "--library",
            "Anime",
            "--plex-db",
            "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
            "--full-anime-duplicates",
            "--pretty",
        ])
        .unwrap();
        match cli.command {
            Commands::Report {
                library,
                plex_db,
                full_anime_duplicates,
                pretty,
                ..
            } => {
                assert_eq!(library.as_deref(), Some("Anime"));
                assert_eq!(
                    plex_db.as_deref(),
                    Some(
                        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db"
                    )
                );
                assert!(full_anime_duplicates);
                assert!(pretty);
            }
            _ => panic!("expected report command"),
        }
    }

    #[test]
    fn cli_accepts_report_with_anime_remediation_tsv() {
        let cli = Cli::try_parse_from([
            "symlinkarr",
            "report",
            "--library",
            "Anime",
            "--plex-db",
            "/tmp/plex.db",
            "--anime-remediation-tsv",
            "/tmp/anime-remediation.tsv",
        ])
        .unwrap();
        match cli.command {
            Commands::Report {
                library,
                plex_db,
                anime_remediation_tsv,
                ..
            } => {
                assert_eq!(library.as_deref(), Some("Anime"));
                assert_eq!(plex_db.as_deref(), Some("/tmp/plex.db"));
                assert_eq!(
                    anime_remediation_tsv.as_deref(),
                    Some("/tmp/anime-remediation.tsv")
                );
            }
            _ => panic!("expected report command"),
        }
    }

    #[test]
    fn cli_accepts_cleanup_prune_with_legacy_anime_opt_in() {
        let cli = Cli::try_parse_from([
            "symlinkarr",
            "cleanup",
            "prune",
            "--report",
            "/tmp/report.json",
            "--include-legacy-anime-roots",
        ])
        .unwrap();

        match cli.command {
            Commands::Cleanup {
                action:
                    Some(CleanupAction::Prune {
                        report,
                        include_legacy_anime_roots,
                        ..
                    }),
                ..
            } => {
                assert_eq!(report, "/tmp/report.json");
                assert!(include_legacy_anime_roots);
            }
            _ => panic!("expected cleanup prune command"),
        }
    }

    #[test]
    fn cli_accepts_cleanup_remediate_anime_preview() {
        let cli = Cli::try_parse_from([
            "symlinkarr",
            "cleanup",
            "remediate-anime",
            "--plex-db",
            "/tmp/plex.db",
            "--title",
            "Gundam",
            "--out",
            "/tmp/anime-remediation.json",
        ])
        .unwrap();

        match cli.command {
            Commands::Cleanup {
                action:
                    Some(CleanupAction::RemediateAnime {
                        plex_db,
                        title,
                        out,
                        apply,
                        ..
                    }),
                ..
            } => {
                assert_eq!(plex_db.as_deref(), Some("/tmp/plex.db"));
                assert_eq!(title.as_deref(), Some("Gundam"));
                assert_eq!(out.as_deref(), Some("/tmp/anime-remediation.json"));
                assert!(!apply);
            }
            _ => panic!("expected cleanup remediate-anime command"),
        }
    }

    #[test]
    fn cli_accepts_cleanup_remediate_anime_apply() {
        let cli = Cli::try_parse_from([
            "symlinkarr",
            "cleanup",
            "remediate-anime",
            "--apply",
            "--report",
            "/tmp/anime-remediation.json",
            "--confirm-token",
            "deadbeef",
        ])
        .unwrap();

        match cli.command {
            Commands::Cleanup {
                action:
                    Some(CleanupAction::RemediateAnime {
                        report,
                        confirm_token,
                        apply,
                        ..
                    }),
                ..
            } => {
                assert_eq!(report.as_deref(), Some("/tmp/anime-remediation.json"));
                assert_eq!(confirm_token.as_deref(), Some("deadbeef"));
                assert!(apply);
            }
            _ => panic!("expected cleanup remediate-anime apply command"),
        }
    }

    #[test]
    fn acquisition_job_status_json_values_are_canonical() {
        assert_eq!(AcquisitionJobStatus::NoResult.as_str(), "no_result");
        assert_eq!(
            AcquisitionJobStatus::CompletedUnlinked.as_str(),
            "completed_unlinked"
        );
    }

    fn startup_fs_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct CurrentDirGuard {
        original: PathBuf,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { original }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn restore_config_yaml(db_rel: &str, secret_rel: &str) -> String {
        format!(
            "libraries:\n  - name: Movies\n    path: \"/tmp/library\"\n    media_type: movie\nsources:\n  - name: RD\n    path: \"/tmp/source\"\n    media_type: auto\ndb_path: \"{db_rel}\"\nrealdebrid:\n  api_token: \"secretfile:{secret_rel}\"\n"
        )
    }

    fn write_managed_file_artifact(
        backup_dir: &Path,
        filename: &str,
        contents: &str,
        original_path: PathBuf,
    ) -> backup::BackupManagedFile {
        let path = backup_dir.join(filename);
        std::fs::write(&path, contents).unwrap();
        backup::BackupManagedFile {
            filename: filename.to_string(),
            sha256: "test-sha256".to_string(),
            size_bytes: contents.len() as u64,
            original_path,
        }
    }

    fn write_database_artifact(
        backup_dir: &Path,
        filename: &str,
        contents: &str,
    ) -> backup::BackupDatabaseSnapshot {
        let path = backup_dir.join(filename);
        std::fs::write(&path, contents).unwrap();
        backup::BackupDatabaseSnapshot {
            filename: filename.to_string(),
            sha256: "test-db-sha256".to_string(),
            size_bytes: contents.len() as u64,
        }
    }

    fn write_backup_fixture(
        backup_dir: &Path,
        name: &str,
        timestamp: chrono::DateTime<Utc>,
        backup_type: backup::BackupType,
        config_contents: &str,
        secret_contents: &str,
        db_contents: &str,
    ) {
        let config_snapshot = write_managed_file_artifact(
            backup_dir,
            &format!("{name}.config.yaml"),
            config_contents,
            backup_dir.join("legacy").join(format!("{name}.config.yaml")),
        );
        let secret_snapshot = write_managed_file_artifact(
            backup_dir,
            &format!("{name}.secret"),
            secret_contents,
            backup_dir.join("legacy").join(format!("{name}.secret")),
        );
        let database_snapshot = write_database_artifact(
            backup_dir,
            &format!("{name}.sqlite3"),
            db_contents,
        );
        let manifest = backup::BackupManifest {
            version: 1,
            timestamp,
            backup_type,
            label: name.to_string(),
            symlinks: Vec::new(),
            total_count: 0,
            database_snapshot: Some(database_snapshot),
            app_state: Some(backup::BackupAppState {
                config_snapshot: Some(config_snapshot),
                secret_snapshots: vec![secret_snapshot],
            }),
            content_sha256: None,
        };
        std::fs::write(
            backup_dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn auto_restore_prefers_latest_scheduled_backup_over_newer_safety_snapshot() {
        let _lock = startup_fs_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CurrentDirGuard::enter(dir.path());
        let backup_dir = dir.path().join("backups");
        let config_path = dir.path().join("install").join("config.yaml");
        std::fs::create_dir_all(&backup_dir).unwrap();

        write_backup_fixture(
            &backup_dir,
            "scheduled",
            Utc.with_ymd_and_hms(2026, 4, 16, 12, 0, 0).unwrap(),
            backup::BackupType::Scheduled,
            &restore_config_yaml("./data/scheduled.db", "./secrets/scheduled-token"),
            "scheduled-secret\n",
            "scheduled-db",
        );
        write_backup_fixture(
            &backup_dir,
            "newer-safety",
            Utc.with_ymd_and_hms(2026, 4, 17, 12, 0, 0).unwrap(),
            backup::BackupType::Safety {
                operation: "cleanup".to_string(),
            },
            &restore_config_yaml("./data/safety.db", "./secrets/safety-token"),
            "safety-secret\n",
            "safety-db",
        );

        let cli = Cli::try_parse_from([
            "symlinkarr",
            "--config",
            config_path.to_str().unwrap(),
            "doctor",
        ])
        .unwrap();
        let restored = try_auto_restore(&cli).await.unwrap().unwrap();

        assert_eq!(restored.db_path, "./data/scheduled.db");
        assert_eq!(restored.realdebrid.api_token, "scheduled-secret");
        assert_eq!(
            std::fs::read_to_string(config_path.parent().unwrap().join("data/scheduled.db"))
                .unwrap(),
            "scheduled-db"
        );
        assert!(!config_path.parent().unwrap().join("data/safety.db").exists());
    }

    #[tokio::test]
    async fn auto_restore_falls_back_to_latest_overall_backup_when_no_scheduled_exists() {
        let _lock = startup_fs_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CurrentDirGuard::enter(dir.path());
        let backup_dir = dir.path().join("backups");
        let config_path = dir.path().join("install").join("config.yaml");
        std::fs::create_dir_all(&backup_dir).unwrap();

        write_backup_fixture(
            &backup_dir,
            "older-safety",
            Utc.with_ymd_and_hms(2026, 4, 16, 12, 0, 0).unwrap(),
            backup::BackupType::Safety {
                operation: "repair".to_string(),
            },
            &restore_config_yaml("./data/older.db", "./secrets/older-token"),
            "older-secret\n",
            "older-db",
        );
        write_backup_fixture(
            &backup_dir,
            "newer-safety",
            Utc.with_ymd_and_hms(2026, 4, 17, 12, 0, 0).unwrap(),
            backup::BackupType::Safety {
                operation: "repair".to_string(),
            },
            &restore_config_yaml("./data/newer.db", "./secrets/newer-token"),
            "newer-secret\n",
            "newer-db",
        );

        let cli = Cli::try_parse_from([
            "symlinkarr",
            "--config",
            config_path.to_str().unwrap(),
            "doctor",
        ])
        .unwrap();
        let restored = try_auto_restore(&cli).await.unwrap().unwrap();

        assert_eq!(restored.db_path, "./data/newer.db");
        assert_eq!(restored.realdebrid.api_token, "newer-secret");
        assert_eq!(
            std::fs::read_to_string(config_path.parent().unwrap().join("data/newer.db")).unwrap(),
            "newer-db"
        );
        assert!(!config_path.parent().unwrap().join("data/older.db").exists());
    }

    #[tokio::test]
    async fn auto_restore_skips_when_explicit_config_already_exists() {
        let _lock = startup_fs_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CurrentDirGuard::enter(dir.path());
        let backup_dir = dir.path().join("backups");
        let config_path = dir.path().join("install").join("config.yaml");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "db_path: ./data/existing.db\n").unwrap();

        write_backup_fixture(
            &backup_dir,
            "scheduled",
            Utc.with_ymd_and_hms(2026, 4, 16, 12, 0, 0).unwrap(),
            backup::BackupType::Scheduled,
            &restore_config_yaml("./data/restored.db", "./secrets/restored-token"),
            "restored-secret\n",
            "restored-db",
        );

        let cli = Cli::try_parse_from([
            "symlinkarr",
            "--config",
            config_path.to_str().unwrap(),
            "doctor",
        ])
        .unwrap();
        let restored = try_auto_restore(&cli).await.unwrap();

        assert!(restored.is_none());
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "db_path: ./data/existing.db\n"
        );
        assert!(!config_path.parent().unwrap().join("data/restored.db").exists());
        assert!(!config_path.parent().unwrap().join("secrets/restored-token").exists());
    }
}
