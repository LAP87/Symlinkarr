use std::path::PathBuf;

use anyhow::Result;
use tracing::info;

use crate::cleanup_audit::{self, CleanupAuditor, CleanupScope};
use crate::commands::{ensure_runtime_directories_healthy, print_json, selected_libraries};
use crate::config::Config;
use crate::db::Database;
use crate::linker::Linker;
use crate::utils::path_under_roots;
use crate::{CleanupAction, GateMode, OutputFormat};

pub(crate) struct CleanupPruneArgs<'a> {
    pub report: &'a str,
    pub apply: bool,
    pub max_delete: Option<usize>,
    pub confirm_token: Option<&'a str>,
    pub gate_mode: GateMode,
    pub library_filter: Option<&'a str>,
    pub output: OutputFormat,
}

pub(crate) async fn run_cleanup(
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

async fn run_cleanup_dead(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<i64> {
    info!("=== Symlinkarr Cleanup ===");
    let selected = selected_libraries(cfg, library_filter)?;
    let library_roots: Vec<_> = selected.iter().map(|l| l.path.clone()).collect();

    ensure_runtime_directories_healthy(&selected, &cfg.sources, "cleanup dead-link removal")
        .await?;

    if cfg.backup.enabled {
        let bm = crate::backup::BackupManager::new(&cfg.backup);
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

pub(crate) async fn run_cleanup_prune(
    cfg: &Config,
    db: &Database,
    args: CleanupPruneArgs<'_>,
) -> Result<i64> {
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
            "managed_candidates": outcome.managed_candidates,
            "foreign_candidates": outcome.foreign_candidates,
            "removed": outcome.removed,
            "quarantined": outcome.quarantined,
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
        println!(
            "   Managed delete candidates: {}",
            outcome.managed_candidates
        );
        println!(
            "   Foreign quarantine candidates: {}",
            outcome.foreign_candidates
        );
        println!("   Removed: {}", outcome.removed);
        println!("   Quarantined: {}", outcome.quarantined);
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

fn filter_cleanup_report_by_roots(report: &mut cleanup_audit::CleanupReport, roots: &[PathBuf]) {
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
