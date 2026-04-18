use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use tracing::info;

use crate::cleanup_audit::{self, CleanupAuditor, CleanupScope};
use crate::commands::report::build_anime_remediation_report;
use crate::commands::{ensure_runtime_directories_healthy, print_json, selected_libraries};
use crate::config::Config;
use crate::db::Database;
use crate::linker::Linker;
use crate::media_servers::{
    configured_refresh_backends, display_server_list, invalidate_after_mutation,
    refresh_selected_library_roots, LibraryInvalidationOutcome,
};
use crate::utils::path_under_roots;
use crate::{CleanupAction, GateMode, OutputFormat};

pub(crate) use self::anime::{
    anime_remediation_block_reason_catalog, assess_anime_remediation_groups,
    filter_anime_remediation_groups, render_anime_remediation_groups_tsv,
    summarize_anime_remediation_blocked_reasons, AnimeRemediationBlockedReasonSummary,
    AnimeRemediationGroupFilters, AnimeRemediationPlanGroup,
};
use self::anime::{
    assess_anime_remediation_group, build_anime_remediation_cleanup_report,
    remediation_group_matches_title_filter, AnimeRemediationPlanReport,
    ANIME_REMEDIATION_REPORT_VERSION, ANIME_REMEDIATION_SAMPLE_LIMIT,
};
#[cfg(test)]
use self::anime::{
    make_anime_block_reason, AnimeRemediationBlockCode, AnimeRemediationVisibilityFilter,
};
use self::plan::{
    default_anime_remediation_report_path, load_anime_remediation_plan_report,
    resolve_plex_db_path, validate_anime_remediation_plan_report, write_temp_cleanup_report,
};

pub(crate) struct CleanupPruneArgs<'a> {
    pub report: &'a str,
    pub apply: bool,
    pub include_legacy_anime_roots: bool,
    pub max_delete: Option<usize>,
    pub confirm_token: Option<&'a str>,
    pub gate_mode: GateMode,
    pub library_filter: Option<&'a str>,
    pub output: OutputFormat,
}

pub(crate) struct CleanupPruneApplyArgs<'a> {
    pub libraries: &'a [&'a crate::config::LibraryConfig],
    pub report_path: &'a Path,
    pub include_legacy_anime_roots: bool,
    pub max_delete: Option<usize>,
    pub confirm_token: Option<&'a str>,
    pub emit_text: bool,
}

pub(crate) struct CleanupAnimeRemediationArgs<'a> {
    pub report: Option<&'a str>,
    pub plex_db: Option<&'a str>,
    pub apply: bool,
    pub title: Option<&'a str>,
    pub out: Option<&'a str>,
    pub confirm_token: Option<&'a str>,
    pub max_delete: Option<usize>,
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
            include_legacy_anime_roots,
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
                    include_legacy_anime_roots,
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
        Some(CleanupAction::RemediateAnime {
            report,
            plex_db,
            apply,
            title,
            out,
            confirm_token,
            max_delete,
            gate_mode,
        }) => {
            run_cleanup_anime_remediation(
                cfg,
                db,
                CleanupAnimeRemediationArgs {
                    report: report.as_deref(),
                    plex_db: plex_db.as_deref(),
                    apply,
                    title: title.as_deref(),
                    out: out.as_deref(),
                    confirm_token: confirm_token.as_deref(),
                    max_delete,
                    gate_mode,
                    library_filter,
                    output,
                },
            )
            .await?;
            Ok(None)
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
        .check_dead_links_scoped(db, Some(&library_roots), None)
        .await?;
    let invalidation = if dead.removed > 0 {
        maybe_refresh_media_servers_after_cleanup(
            cfg,
            &selected,
            None,
            "dead-link cleanup",
            output != OutputFormat::Json,
        )
        .await
    } else {
        LibraryInvalidationOutcome::default()
    };
    info!(
        "Handled dead links: marked={}, removed={}, skipped={}",
        dead.dead_marked, dead.removed, dead.skipped
    );
    if output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "dead_marked": dead.dead_marked,
            "removed": dead.removed,
            "skipped": dead.skipped,
            "media_server_invalidation": invalidation,
        }));
    } else if let Some(summary) = invalidation.summary_suffix() {
        println!("   📺 Media-server refresh: {}", summary);
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
        include_legacy_anime_roots,
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
    let selected = selected_libraries(cfg, library_filter)?;
    if library_filter.is_some() {
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

    if apply {
        ensure_runtime_directories_healthy(&selected, &cfg.sources, "cleanup prune apply").await?;
    }

    let (outcome, invalidation) = if apply {
        apply_cleanup_prune_with_refresh(
            cfg,
            db,
            CleanupPruneApplyArgs {
                libraries: &selected,
                report_path: &effective_report_path,
                include_legacy_anime_roots,
                max_delete,
                confirm_token,
                emit_text: output != OutputFormat::Json,
            },
        )
        .await?
    } else {
        (
            cleanup_audit::run_prune(
                cfg,
                db,
                &effective_report_path,
                false,
                include_legacy_anime_roots,
                max_delete,
                confirm_token,
            )
            .await?,
            LibraryInvalidationOutcome::default(),
        )
    };

    if let Some(tmp) = temporary_report {
        let _ = std::fs::remove_file(tmp);
    }

    if output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "apply": apply,
            "candidates": outcome.candidates,
            "blocked_candidates": outcome.blocked_candidates,
            "high_or_critical_candidates": outcome.high_or_critical_candidates,
            "safe_warning_duplicate_candidates": outcome.safe_warning_duplicate_candidates,
            "legacy_anime_root_candidates": outcome.legacy_anime_root_candidates,
            "legacy_anime_root_groups": outcome.legacy_anime_root_groups,
            "managed_candidates": outcome.managed_candidates,
            "foreign_candidates": outcome.foreign_candidates,
            "reason_counts": outcome.reason_counts,
            "blocked_reason_summary": outcome.blocked_reason_summary,
            "removed": outcome.removed,
            "quarantined": outcome.quarantined,
            "skipped": outcome.skipped,
            "confirmation_token": outcome.confirmation_token,
            "media_server_invalidation": invalidation,
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
            "   Legacy anime-root warning candidates: {}",
            outcome.legacy_anime_root_candidates
        );
        println!(
            "   Managed delete candidates: {}",
            outcome.managed_candidates
        );
        println!(
            "   Foreign quarantine candidates: {}",
            outcome.foreign_candidates
        );
        println!(
            "   Blocked by policy or trust gates: {}",
            outcome.blocked_candidates
        );
        if !outcome.reason_counts.is_empty() {
            println!("   Top candidate reasons:");
            for bucket in outcome.reason_counts.iter().take(6) {
                println!(
                    "      - {}: {} (managed {}, foreign {})",
                    bucket.reason, bucket.total, bucket.managed, bucket.foreign
                );
            }
        }
        if !outcome.legacy_anime_root_groups.is_empty() {
            println!("   Top legacy anime-root groups:");
            for group in outcome.legacy_anime_root_groups.iter().take(6) {
                println!("      - {}: {}", group.normalized_title, group.total);
                for root in group.tagged_roots.iter().take(2) {
                    println!("          tagged root: {}", root.display());
                }
            }
        }
        if !outcome.blocked_reason_summary.is_empty() {
            println!("   Top blocked reasons:");
            for summary in outcome.blocked_reason_summary.iter().take(6) {
                println!(
                    "      - {} candidates: {} -> {}",
                    summary.candidates, summary.label, summary.recommended_action
                );
            }
        }
        println!("   Removed: {}", outcome.removed);
        println!("   Quarantined: {}", outcome.quarantined);
        println!("   Skipped: {}", outcome.skipped);
        if let Some(summary) = invalidation.summary_suffix() {
            println!("   📺 Media-server refresh: {}", summary);
        }
        if !apply {
            println!("   Confirmation token: {}", outcome.confirmation_token);
            println!(
                "   ℹ️  Re-run with --apply --confirm-token <token> to remove flagged symlinks"
            );
        }
    }

    Ok(outcome.removed as i64)
}

async fn run_cleanup_anime_remediation(
    cfg: &Config,
    db: &Database,
    args: CleanupAnimeRemediationArgs<'_>,
) -> Result<()> {
    info!("=== Symlinkarr Anime Remediation ===");

    if matches!(args.gate_mode, GateMode::Relaxed) {
        tracing::warn!(
            "gate-mode=relaxed requested; runtime safety remains enforced for anime remediation apply"
        );
    }

    if args.apply {
        if args.report.is_none() {
            anyhow::bail!("Anime remediation apply requires --report");
        }
        if args.plex_db.is_some() || args.title.is_some() || args.out.is_some() {
            anyhow::bail!(
                "Anime remediation apply only accepts --report, --confirm-token, --max-delete, and the shared --library filter"
            );
        }
        let report_path = Path::new(args.report.unwrap());
        let (plan, outcome, safety_snapshot, invalidation) =
            apply_anime_remediation_plan_with_refresh(
                cfg,
                db,
                args.library_filter,
                report_path,
                args.confirm_token,
                args.max_delete,
                args.output != OutputFormat::Json,
            )
            .await?;

        if args.output == OutputFormat::Json {
            print_json(&serde_json::json!({
                "apply": true,
                "report": report_path,
                "groups": plan.total_groups,
                "eligible_groups": plan.eligible_groups,
                "blocked_groups": plan.blocked_groups,
                "candidates": outcome.candidates,
                "quarantined": outcome.quarantined,
                "removed": outcome.removed,
                "skipped": outcome.skipped,
                "safety_snapshot": safety_snapshot,
                "media_server_invalidation": invalidation,
            }));
        } else {
            println!("\n🎌 Anime Remediation Applied");
            println!("   Report: {}", report_path.display());
            println!("   Eligible groups: {}", plan.eligible_groups);
            println!("   Blocked groups: {}", plan.blocked_groups);
            println!("   Candidates: {}", outcome.candidates);
            println!("   Quarantined: {}", outcome.quarantined);
            println!("   Removed: {}", outcome.removed);
            println!("   Skipped: {}", outcome.skipped);
            if let Some(summary) = invalidation.summary_suffix() {
                println!("   📺 Media-server refresh: {}", summary);
            }
            if let Some(snapshot) = &safety_snapshot {
                println!("   Safety snapshot: {}", snapshot.display());
            }
        }

        return Ok(());
    }

    if args.report.is_some() || args.confirm_token.is_some() {
        anyhow::bail!(
            "Anime remediation preview does not accept --report or --confirm-token; use --out to choose the saved remediation plan path"
        );
    }

    let Some(plex_db_path) = resolve_plex_db_path(args.plex_db) else {
        anyhow::bail!(
            "Plex DB path is required or must exist at a standard local path for anime remediation"
        );
    };
    let requested_out = args.out.map(PathBuf::from);
    let (plan, out_path) = preview_anime_remediation_plan(
        cfg,
        db,
        args.library_filter,
        &plex_db_path,
        args.title,
        requested_out.as_deref(),
    )
    .await?;

    if args.output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "apply": false,
            "file": out_path,
            "plex_db_path": plan.plex_db_path,
            "title_filter": plan.title_filter,
            "total_groups": plan.total_groups,
            "eligible_groups": plan.eligible_groups,
            "blocked_groups": plan.blocked_groups,
            "cleanup_candidates": plan.cleanup_candidates,
            "confirmation_token": plan.confirmation_token,
            "blocked_reason_summary": plan.blocked_reason_summary,
            "groups": plan.groups,
        }));
    } else {
        println!("\n🎌 Anime Remediation Preview");
        println!("   Report: {}", out_path.display());
        println!("   Plex DB: {}", plan.plex_db_path.display());
        println!("   Groups matched: {}", plan.total_groups);
        println!("   Eligible groups: {}", plan.eligible_groups);
        println!("   Blocked groups: {}", plan.blocked_groups);
        println!("   Candidate symlinks: {}", plan.cleanup_candidates);
        println!("   Confirmation token: {}", plan.confirmation_token);

        if !plan.blocked_reason_summary.is_empty() {
            println!("   Top block reasons:");
            for summary in plan
                .blocked_reason_summary
                .iter()
                .take(ANIME_REMEDIATION_SAMPLE_LIMIT)
            {
                println!(
                    "      - {} groups: {} -> {}",
                    summary.groups, summary.label, summary.recommended_action
                );
            }
        }

        if !plan.groups.is_empty() {
            println!("   Top groups:");
            for group in plan.groups.iter().take(ANIME_REMEDIATION_SAMPLE_LIMIT) {
                if group.eligible {
                    println!(
                        "      - {}: quarantine {} legacy symlinks -> keep {}",
                        group.normalized_title,
                        group.legacy_symlink_candidates,
                        group.recommended_tagged_root.path.display()
                    );
                } else {
                    let action = group
                        .block_reasons
                        .first()
                        .map(|reason| reason.recommended_action.as_str())
                        .unwrap_or("Review this title manually before attempting remediation.");
                    println!(
                        "      - {}: blocked ({}) -> {}",
                        group.normalized_title,
                        group
                            .block_reasons
                            .iter()
                            .map(|reason| reason.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                        action
                    );
                }
            }
        }

        println!(
            "   Next: symlinkarr cleanup remediate-anime --apply --report {} --confirm-token {}",
            out_path.display(),
            plan.confirmation_token
        );
    }

    Ok(())
}

pub(crate) async fn preview_anime_remediation_plan(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    plex_db_path: &Path,
    title_filter: Option<&str>,
    out_path: Option<&Path>,
) -> Result<(AnimeRemediationPlanReport, PathBuf)> {
    let selected = selected_libraries(cfg, library_filter)?;
    let anime_libraries: Vec<_> = selected
        .into_iter()
        .filter(|lib| lib.content_type == Some(crate::config::ContentType::Anime))
        .collect();

    if anime_libraries.is_empty() {
        anyhow::bail!("No anime libraries matched the current library filter");
    }

    let plan =
        build_anime_remediation_plan_report(cfg, db, &anime_libraries, plex_db_path, title_filter)
            .await?;
    let output_path = out_path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_anime_remediation_report_path(cfg));

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&output_path, serde_json::to_string_pretty(&plan)?)?;

    let canonical_output_path = output_path.canonicalize().unwrap_or(output_path);
    Ok((plan, canonical_output_path))
}

pub(crate) async fn apply_anime_remediation_plan(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    report_path: &Path,
    confirm_token: Option<&str>,
    max_delete: Option<usize>,
) -> Result<(
    AnimeRemediationPlanReport,
    cleanup_audit::PruneOutcome,
    Option<PathBuf>,
)> {
    let selected = selected_libraries(cfg, library_filter)?;
    let anime_libraries: Vec<_> = selected
        .into_iter()
        .filter(|lib| lib.content_type == Some(crate::config::ContentType::Anime))
        .collect();

    if anime_libraries.is_empty() {
        anyhow::bail!("No anime libraries matched the current library filter");
    }
    if !cfg.cleanup.prune.quarantine_foreign {
        anyhow::bail!(
            "Anime remediation apply requires cleanup.prune.quarantine_foreign=true because this workflow quarantines foreign legacy symlinks"
        );
    }

    ensure_runtime_directories_healthy(
        &anime_libraries,
        &cfg.sources,
        "cleanup anime remediation apply",
    )
    .await?;

    let plan = load_anime_remediation_plan_report(report_path)?;
    validate_anime_remediation_plan_report(&plan)?;

    let safety_snapshot = if cfg.backup.enabled {
        let extra_symlink_paths: Vec<_> = plan
            .cleanup_report
            .findings
            .iter()
            .map(|finding| finding.symlink_path.clone())
            .collect();
        Some(
            crate::backup::BackupManager::new(&cfg.backup)
                .create_safety_snapshot_with_extras(db, "anime-remediation", &extra_symlink_paths)
                .await?,
        )
    } else {
        None
    };

    let temp_cleanup_report_path =
        write_temp_cleanup_report(&cfg.backup.path, &plan.cleanup_report)?;
    let outcome = cleanup_audit::run_prune(
        cfg,
        db,
        &temp_cleanup_report_path,
        true,
        true,
        max_delete,
        confirm_token,
    )
    .await;
    let _ = std::fs::remove_file(&temp_cleanup_report_path);
    let outcome = outcome?;

    Ok((plan, outcome, safety_snapshot))
}

pub(crate) async fn apply_anime_remediation_plan_with_refresh(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    report_path: &Path,
    confirm_token: Option<&str>,
    max_delete: Option<usize>,
    emit_text: bool,
) -> Result<(
    AnimeRemediationPlanReport,
    cleanup_audit::PruneOutcome,
    Option<PathBuf>,
    LibraryInvalidationOutcome,
)> {
    let (plan, outcome, safety_snapshot) = apply_anime_remediation_plan(
        cfg,
        db,
        library_filter,
        report_path,
        confirm_token,
        max_delete,
    )
    .await?;
    let anime_libraries: Vec<_> = selected_libraries(cfg, library_filter)?
        .into_iter()
        .filter(|lib| lib.content_type == Some(crate::config::ContentType::Anime))
        .collect();
    let invalidation = if outcome.removed > 0 || outcome.quarantined > 0 {
        maybe_refresh_media_servers_after_cleanup(
            cfg,
            &anime_libraries,
            Some(&outcome.affected_paths),
            "anime remediation",
            emit_text,
        )
        .await
    } else {
        LibraryInvalidationOutcome::default()
    };

    Ok((plan, outcome, safety_snapshot, invalidation))
}

pub(crate) async fn apply_cleanup_prune_with_refresh(
    cfg: &Config,
    db: &Database,
    args: CleanupPruneApplyArgs<'_>,
) -> Result<(cleanup_audit::PruneOutcome, LibraryInvalidationOutcome)> {
    let CleanupPruneApplyArgs {
        libraries,
        report_path,
        include_legacy_anime_roots,
        max_delete,
        confirm_token,
        emit_text,
    } = args;
    let outcome = cleanup_audit::run_prune(
        cfg,
        db,
        report_path,
        true,
        include_legacy_anime_roots,
        max_delete,
        confirm_token,
    )
    .await?;
    let invalidation = if outcome.removed > 0 || outcome.quarantined > 0 {
        maybe_refresh_media_servers_after_cleanup(
            cfg,
            libraries,
            Some(&outcome.affected_paths),
            "cleanup prune",
            emit_text,
        )
        .await
    } else {
        LibraryInvalidationOutcome::default()
    };

    Ok((outcome, invalidation))
}

async fn maybe_refresh_media_servers_after_cleanup(
    cfg: &Config,
    libraries: &[&crate::config::LibraryConfig],
    affected_paths: Option<&[PathBuf]>,
    operation: &str,
    emit_text: bool,
) -> LibraryInvalidationOutcome {
    if libraries.is_empty() {
        return LibraryInvalidationOutcome::default();
    }

    if emit_text {
        let servers = configured_refresh_backends(cfg);
        if !servers.is_empty() {
            println!(
                "   📺 Post-{}: refreshing affected library roots in {}...",
                operation,
                display_server_list(&servers)
            );
        } else {
            println!(
                "   📺 Post-{}: checking whether any configured media server should be refreshed...",
                operation
            );
        }
    }

    let refresh_result = match affected_paths.filter(|paths| !paths.is_empty()) {
        Some(paths) => invalidate_after_mutation(cfg, libraries, paths, emit_text).await,
        None => refresh_selected_library_roots(cfg, libraries, emit_text)
            .await
            .map(|refresh| LibraryInvalidationOutcome {
                server: None,
                requested_library_roots: libraries.len(),
                configured: !configured_refresh_backends(cfg).is_empty(),
                refresh: Some(refresh),
                servers: Vec::new(),
            }),
    };

    match refresh_result {
        Ok(outcome) => outcome,
        Err(err) => {
            if emit_text {
                println!(
                    "   ⚠️  Post-{} media-server refresh failed: {}",
                    operation, err
                );
            }
            tracing::warn!("Post-{} media-server refresh failed: {}", operation, err);
            LibraryInvalidationOutcome::default()
        }
    }
}

async fn build_anime_remediation_plan_report(
    cfg: &Config,
    db: &Database,
    anime_libraries: &[&crate::config::LibraryConfig],
    plex_db_path: &Path,
    title_filter: Option<&str>,
) -> Result<AnimeRemediationPlanReport> {
    let mut scoped_cfg = cfg.clone();
    scoped_cfg.libraries = anime_libraries.iter().map(|lib| (*lib).clone()).collect();

    let remediation = build_anime_remediation_report(&scoped_cfg, db, plex_db_path, true)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("No anime libraries are configured for remediation reporting")
        })?;

    let title_filter = title_filter
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let filtered_groups: Vec<_> = remediation
        .groups
        .into_iter()
        .filter(|group| remediation_group_matches_title_filter(group, title_filter.as_deref()))
        .collect();

    let plan_groups: Vec<_> = filtered_groups
        .iter()
        .map(assess_anime_remediation_group)
        .collect::<Result<Vec<_>>>()?;

    let eligible_groups: Vec<_> = plan_groups
        .iter()
        .filter(|group| group.eligible)
        .cloned()
        .collect();
    let blocked_groups = plan_groups.len().saturating_sub(eligible_groups.len());
    let blocked_reason_summary = summarize_anime_remediation_blocked_reasons(&plan_groups);
    let cleanup_report = build_anime_remediation_cleanup_report(&eligible_groups);
    let preview_outcome = build_anime_remediation_prune_preview(cfg, db, &cleanup_report).await?;

    Ok(AnimeRemediationPlanReport {
        version: ANIME_REMEDIATION_REPORT_VERSION,
        created_at: Utc::now(),
        plex_db_path: plex_db_path.to_path_buf(),
        title_filter,
        total_groups: plan_groups.len(),
        eligible_groups: eligible_groups.len(),
        blocked_groups,
        cleanup_candidates: preview_outcome.candidates,
        confirmation_token: preview_outcome.confirmation_token,
        blocked_reason_summary,
        groups: plan_groups,
        cleanup_report,
    })
}

async fn build_anime_remediation_prune_preview(
    cfg: &Config,
    db: &Database,
    report: &cleanup_audit::CleanupReport,
) -> Result<cleanup_audit::PruneOutcome> {
    let temp_path = write_temp_cleanup_report(&cfg.backup.path, report)?;
    let result = cleanup_audit::run_prune(cfg, db, &temp_path, false, true, None, None).await;
    let _ = std::fs::remove_file(&temp_path);
    result
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

mod anime;
mod plan;
#[cfg(test)]
mod tests;
