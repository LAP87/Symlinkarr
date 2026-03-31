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
mod utils;
#[allow(dead_code, unused_imports, unused_variables)]
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use crate::commands::print_final_summary;
use crate::db::AcquisitionJobStatus;

// ─── CLI definitions ───────────────────────────────────────────────

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
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        search_missing: bool,
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
    /// Show database statistics
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
    /// Run continuously as a daemon
    Daemon,
    /// Run only the web UI
    Web {
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
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// Generate a library report
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
        /// Optional TSV export path for the anime remediation queue
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
    /// List RD content not present in your library
    List,
    /// Add an RD torrent to Decypharr by its torrent ID
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
    /// Preview or apply quarantine-first remediation for mixed anime roots
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let cfg = config::Config::load(cli.config)?;
    let db = db::Database::new(&cfg.db_path).await?;

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
        Commands::Daemon => commands::daemon::run_daemon(&cfg, &db).await?,
        Commands::Web { port } => {
            let port = port.unwrap_or(cfg.web.port);
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
            .await?
        }
    }

    Ok(())
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
}
