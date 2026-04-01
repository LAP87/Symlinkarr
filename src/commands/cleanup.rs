use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use tracing::info;
use walkdir::WalkDir;

use crate::cleanup_audit::{self, CleanupAuditor, CleanupScope};
use crate::commands::report::{build_anime_remediation_report, AnimeRemediationSample};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnimeRemediationBlockCode {
    RecommendedRootUntrackedDb,
    LegacyRootsStillTracked,
    LegacyRootsContainRealMedia,
    NoLegacySymlinkCandidates,
    ManualReviewRequired,
}

impl AnimeRemediationBlockCode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::RecommendedRootUntrackedDb => "recommended_root_untracked_db",
            Self::LegacyRootsStillTracked => "legacy_roots_still_tracked",
            Self::LegacyRootsContainRealMedia => "legacy_roots_contain_real_media",
            Self::NoLegacySymlinkCandidates => "no_legacy_symlink_candidates",
            Self::ManualReviewRequired => "manual_review_required",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::RecommendedRootUntrackedDb => "recommended tagged root is not DB-anchored yet",
            Self::LegacyRootsStillTracked => "legacy roots still contain tracked DB links",
            Self::LegacyRootsContainRealMedia => "legacy roots contain real media files",
            Self::NoLegacySymlinkCandidates => "no removable legacy symlink candidates were found",
            Self::ManualReviewRequired => "manual review required",
        }
    }

    fn recommended_action(&self) -> &'static str {
        match self {
            Self::RecommendedRootUntrackedDb => {
                "Run a normal scan until the tagged root owns tracked links before remediation."
            }
            Self::LegacyRootsStillTracked => {
                "Do not auto-remediate yet; first move or prune the DB-tracked legacy links."
            }
            Self::LegacyRootsContainRealMedia => {
                "Manual migration required; move or relink real media files before remediation."
            }
            Self::NoLegacySymlinkCandidates => {
                "Nothing safe to quarantine automatically; inspect the legacy roots manually."
            }
            Self::ManualReviewRequired => {
                "Review this title manually before attempting remediation."
            }
        }
    }

    fn from_legacy_message(message: &str) -> Self {
        if message == "recommended tagged root has no tracked DB links" {
            Self::RecommendedRootUntrackedDb
        } else if message.starts_with("legacy roots still contain ")
            && message.contains(" tracked DB links")
        {
            Self::LegacyRootsStillTracked
        } else if message.starts_with("legacy roots contain ")
            && message.contains(" non-symlink media files")
        {
            Self::LegacyRootsContainRealMedia
        } else if message == "no legacy symlink candidates found under legacy roots" {
            Self::NoLegacySymlinkCandidates
        } else {
            Self::ManualReviewRequired
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnimeRemediationBlockReason {
    pub(crate) code: AnimeRemediationBlockCode,
    pub(crate) message: String,
    pub(crate) recommended_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnimeRemediationBlockedReasonSummary {
    pub(crate) code: AnimeRemediationBlockCode,
    pub(crate) label: String,
    pub(crate) recommended_action: String,
    pub(crate) groups: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum AnimeRemediationBlockReasonCompat {
    Structured(AnimeRemediationBlockReason),
    Legacy(String),
}

fn deserialize_anime_remediation_block_reasons<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AnimeRemediationBlockReason>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Vec::<AnimeRemediationBlockReasonCompat>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|entry| match entry {
            AnimeRemediationBlockReasonCompat::Structured(reason) => reason,
            AnimeRemediationBlockReasonCompat::Legacy(message) => {
                let code = AnimeRemediationBlockCode::from_legacy_message(&message);
                AnimeRemediationBlockReason {
                    code: code.clone(),
                    message,
                    recommended_action: code.recommended_action().to_string(),
                }
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnimeRemediationPlanGroup {
    normalized_title: String,
    eligible: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_anime_remediation_block_reasons"
    )]
    block_reasons: Vec<AnimeRemediationBlockReason>,
    recommended_tagged_root: crate::commands::report::AnimeRootUsageSample,
    alternate_tagged_roots: Vec<crate::commands::report::AnimeRootUsageSample>,
    legacy_roots: Vec<crate::commands::report::AnimeRootUsageSample>,
    legacy_symlink_candidates: usize,
    broken_symlink_candidates: usize,
    legacy_media_files: usize,
    candidate_symlink_samples: Vec<PathBuf>,
    plex_live_rows: usize,
    plex_deleted_rows: usize,
    plex_guid_kinds: Vec<String>,
    plex_guids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnimeRemediationPlanReport {
    pub(crate) version: u32,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) plex_db_path: PathBuf,
    pub(crate) title_filter: Option<String>,
    pub(crate) total_groups: usize,
    pub(crate) eligible_groups: usize,
    pub(crate) blocked_groups: usize,
    pub(crate) cleanup_candidates: usize,
    pub(crate) confirmation_token: String,
    #[serde(default)]
    pub(crate) blocked_reason_summary: Vec<AnimeRemediationBlockedReasonSummary>,
    pub(crate) groups: Vec<AnimeRemediationPlanGroup>,
    pub(crate) cleanup_report: cleanup_audit::CleanupReport,
}

#[derive(Debug, Clone, Default)]
struct LegacyRootScan {
    symlink_paths: Vec<PathBuf>,
    broken_symlink_paths: Vec<PathBuf>,
    media_files: Vec<PathBuf>,
}

const ANIME_REMEDIATION_REPORT_VERSION: u32 = 1;
const ANIME_REMEDIATION_SAMPLE_LIMIT: usize = 6;

fn make_anime_block_reason(
    code: AnimeRemediationBlockCode,
    message: String,
) -> AnimeRemediationBlockReason {
    AnimeRemediationBlockReason {
        recommended_action: code.recommended_action().to_string(),
        message,
        code,
    }
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
            "high_or_critical_candidates": outcome.high_or_critical_candidates,
            "safe_warning_duplicate_candidates": outcome.safe_warning_duplicate_candidates,
            "legacy_anime_root_candidates": outcome.legacy_anime_root_candidates,
            "legacy_anime_root_groups": outcome.legacy_anime_root_groups,
            "managed_candidates": outcome.managed_candidates,
            "foreign_candidates": outcome.foreign_candidates,
            "reason_counts": outcome.reason_counts,
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

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(requested);
        return path.exists().then_some(path);
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

fn default_anime_remediation_report_path(cfg: &Config) -> PathBuf {
    cfg.backup.path.join(format!(
        "anime-remediation-{}.json",
        Utc::now().format("%Y%m%d-%H%M%S")
    ))
}

fn load_anime_remediation_plan_report(path: &Path) -> Result<AnimeRemediationPlanReport> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn validate_anime_remediation_plan_report(report: &AnimeRemediationPlanReport) -> Result<()> {
    if report.version != ANIME_REMEDIATION_REPORT_VERSION {
        anyhow::bail!(
            "Unsupported anime remediation report version {} (expected {})",
            report.version,
            ANIME_REMEDIATION_REPORT_VERSION
        );
    }

    if report.cleanup_report.scope != CleanupScope::Anime {
        anyhow::bail!("Anime remediation report contains a non-anime cleanup payload");
    }

    if report.cleanup_report.findings.iter().any(|finding| {
        !finding
            .reasons
            .contains(&cleanup_audit::FindingReason::LegacyAnimeRootDuplicate)
            || finding.legacy_anime_root.is_none()
    }) {
        anyhow::bail!("Anime remediation report contains non-remediation cleanup findings");
    }

    Ok(())
}

fn write_temp_cleanup_report(
    backup_root: &Path,
    report: &cleanup_audit::CleanupReport,
) -> Result<PathBuf> {
    let temp_path = backup_root.join(format!(
        "anime-remediation-apply-{}.tmp.json",
        Utc::now().format("%Y%m%d-%H%M%S-%3f")
    ));
    if let Some(parent) = temp_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&temp_path, serde_json::to_string_pretty(report)?)?;
    Ok(temp_path)
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
        .map(build_anime_remediation_plan_group)
        .collect::<Result<Vec<_>>>()?;

    let eligible_groups: Vec<_> = plan_groups
        .iter()
        .filter(|group| group.eligible)
        .cloned()
        .collect();
    let blocked_groups = plan_groups.len().saturating_sub(eligible_groups.len());
    let blocked_reason_summary = build_anime_remediation_blocked_reason_summary(&plan_groups);
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

fn remediation_group_matches_title_filter(
    group: &AnimeRemediationSample,
    title_filter: Option<&str>,
) -> bool {
    let Some(filter) = title_filter else {
        return true;
    };

    let haystack = crate::utils::normalize(&group.normalized_title);
    let needle = crate::utils::normalize(filter);
    haystack.contains(&needle)
}

fn build_anime_remediation_plan_group(
    sample: &AnimeRemediationSample,
) -> Result<AnimeRemediationPlanGroup> {
    let mut candidate_paths = BTreeSet::new();
    let mut broken_symlink_candidates = 0usize;
    let mut legacy_media_files = 0usize;

    for legacy_root in &sample.legacy_roots {
        let scan = scan_legacy_root(&legacy_root.path);
        broken_symlink_candidates += scan.broken_symlink_paths.len();
        legacy_media_files += scan.media_files.len();
        for path in scan.symlink_paths {
            candidate_paths.insert(path);
        }
    }

    let legacy_db_total: usize = sample
        .legacy_roots
        .iter()
        .map(|root| root.db_active_links)
        .sum();
    let mut block_reasons = Vec::new();
    if sample.recommended_tagged_root.db_active_links == 0 {
        block_reasons.push(make_anime_block_reason(
            AnimeRemediationBlockCode::RecommendedRootUntrackedDb,
            "recommended tagged root has no tracked DB links".to_string(),
        ));
    }
    if legacy_db_total > 0 {
        block_reasons.push(make_anime_block_reason(
            AnimeRemediationBlockCode::LegacyRootsStillTracked,
            format!(
                "legacy roots still contain {} tracked DB links",
                legacy_db_total
            ),
        ));
    }
    if legacy_media_files > 0 {
        block_reasons.push(make_anime_block_reason(
            AnimeRemediationBlockCode::LegacyRootsContainRealMedia,
            format!(
                "legacy roots contain {} non-symlink media files",
                legacy_media_files
            ),
        ));
    }
    if candidate_paths.is_empty() {
        block_reasons.push(make_anime_block_reason(
            AnimeRemediationBlockCode::NoLegacySymlinkCandidates,
            "no legacy symlink candidates found under legacy roots".to_string(),
        ));
    }

    let candidate_symlink_samples = candidate_paths
        .iter()
        .take(ANIME_REMEDIATION_SAMPLE_LIMIT)
        .cloned()
        .collect();

    Ok(AnimeRemediationPlanGroup {
        normalized_title: sample.normalized_title.clone(),
        eligible: block_reasons.is_empty(),
        block_reasons,
        recommended_tagged_root: sample.recommended_tagged_root.clone(),
        alternate_tagged_roots: sample.alternate_tagged_roots.clone(),
        legacy_roots: sample.legacy_roots.clone(),
        legacy_symlink_candidates: candidate_paths.len(),
        broken_symlink_candidates,
        legacy_media_files,
        candidate_symlink_samples,
        plex_live_rows: sample.plex_live_rows,
        plex_deleted_rows: sample.plex_deleted_rows,
        plex_guid_kinds: sample.plex_guid_kinds.clone(),
        plex_guids: sample.plex_guids.clone(),
    })
}

fn scan_legacy_root(root: &Path) -> LegacyRootScan {
    let mut scan = LegacyRootScan::default();
    for entry in WalkDir::new(root).follow_links(false) {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.path() == root {
            continue;
        }

        let file_type = entry.file_type();
        if file_type.is_symlink() {
            let path = entry.path().to_path_buf();
            if symlink_source_missing(&path) {
                scan.broken_symlink_paths.push(path.clone());
            }
            scan.symlink_paths.push(path);
        } else if file_type.is_file() && is_media_file_path(entry.path()) {
            scan.media_files.push(entry.path().to_path_buf());
        }
    }

    scan
}

fn is_media_file_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some(
            "mkv"
                | "mp4"
                | "avi"
                | "m4v"
                | "mov"
                | "wmv"
                | "flv"
                | "webm"
                | "mpg"
                | "mpeg"
                | "ts"
                | "m2ts"
                | "iso"
        )
    )
}

fn symlink_source_missing(path: &Path) -> bool {
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    let resolved = resolve_link_target(path, &target);
    !resolved.exists()
}

fn resolve_link_target(symlink_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        symlink_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

fn build_anime_remediation_cleanup_report(
    groups: &[AnimeRemediationPlanGroup],
) -> cleanup_audit::CleanupReport {
    let mut findings = Vec::new();

    for group in groups {
        let tagged_roots: Vec<_> = std::iter::once(group.recommended_tagged_root.path.clone())
            .chain(
                group
                    .alternate_tagged_roots
                    .iter()
                    .map(|root| root.path.clone()),
            )
            .collect();

        for legacy_root in &group.legacy_roots {
            let scan = scan_legacy_root(&legacy_root.path);
            for symlink_path in scan.symlink_paths {
                let source_path = std::fs::read_link(&symlink_path)
                    .map(|target| resolve_link_target(&symlink_path, &target))
                    .unwrap_or_else(|_| symlink_path.clone());
                findings.push(cleanup_audit::CleanupFinding {
                    symlink_path,
                    source_path,
                    media_id: String::new(),
                    severity: cleanup_audit::FindingSeverity::Warning,
                    confidence: 1.0,
                    reasons: vec![cleanup_audit::FindingReason::LegacyAnimeRootDuplicate],
                    parsed: cleanup_audit::ParsedContext {
                        library_title: group.normalized_title.clone(),
                        parsed_title: group.normalized_title.clone(),
                        season: None,
                        episode: None,
                    },
                    alternate_match: None,
                    legacy_anime_root: Some(cleanup_audit::LegacyAnimeRootDetails {
                        normalized_title: group.normalized_title.clone(),
                        untagged_root: legacy_root.path.clone(),
                        tagged_roots: tagged_roots.clone(),
                    }),
                    db_tracked: false,
                    ownership: cleanup_audit::CleanupOwnership::Foreign,
                });
            }
        }
    }

    cleanup_audit::CleanupReport {
        version: 1,
        created_at: Utc::now(),
        scope: CleanupScope::Anime,
        summary: cleanup_audit::CleanupSummary {
            total_findings: findings.len(),
            critical: 0,
            high: 0,
            warning: findings.len(),
        },
        findings,
    }
}

fn build_anime_remediation_blocked_reason_summary(
    groups: &[AnimeRemediationPlanGroup],
) -> Vec<AnimeRemediationBlockedReasonSummary> {
    let mut counts: std::collections::BTreeMap<String, AnimeRemediationBlockedReasonSummary> =
        std::collections::BTreeMap::new();

    for group in groups.iter().filter(|group| !group.eligible) {
        for reason in &group.block_reasons {
            let key = format!("{:?}", reason.code);
            let entry = counts
                .entry(key)
                .or_insert_with(|| AnimeRemediationBlockedReasonSummary {
                    code: reason.code.clone(),
                    label: reason.code.label().to_string(),
                    recommended_action: reason.recommended_action.clone(),
                    groups: 0,
                });
            entry.groups += 1;
        }
    }

    let mut summary = counts.into_values().collect::<Vec<_>>();
    summary.sort_by(|a, b| b.groups.cmp(&a.groups).then_with(|| a.label.cmp(&b.label)));
    summary
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

#[cfg(test)]
mod tests {
    use super::*;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Executor;
    use std::str::FromStr;

    use crate::commands::report::AnimeRootUsageSample;
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::Database;
    use crate::models::{LinkRecord, MediaType};

    fn test_config(root: &Path) -> Config {
        let library = root.join("anime");
        let source = root.join("rd");
        let backups = root.join("backups");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&backups).unwrap();

        Config {
            libraries: vec![LibraryConfig {
                name: "Anime".to_string(),
                path: library,
                media_type: MediaType::Tv,
                content_type: Some(ContentType::Anime),
                depth: 1,
            }],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: source,
                media_type: "auto".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            backup: BackupConfig {
                path: backups,
                ..BackupConfig::default()
            },
            db_path: root.join("test.db").display().to_string(),
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

    fn sample_group(root: &Path) -> AnimeRemediationSample {
        AnimeRemediationSample {
            normalized_title: "Show A".to_string(),
            recommended_tagged_root: AnimeRootUsageSample {
                path: root.join("anime/Show A (2024) {tvdb-1}"),
                filesystem_symlinks: 2,
                db_active_links: 2,
            },
            alternate_tagged_roots: vec![],
            legacy_roots: vec![AnimeRootUsageSample {
                path: root.join("anime/Show A"),
                filesystem_symlinks: 2,
                db_active_links: 0,
            }],
            plex_total_rows: 2,
            plex_live_rows: 2,
            plex_deleted_rows: 0,
            plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            plex_guids: vec![],
        }
    }

    async fn create_test_plex_duplicate_db(path: &Path) {
        let options = SqliteConnectOptions::from_str(path.to_str().unwrap())
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        pool.execute(
            "CREATE TABLE section_locations (
                id INTEGER PRIMARY KEY,
                library_section_id INTEGER,
                root_path TEXT,
                available BOOLEAN,
                scanned_at INTEGER,
                created_at INTEGER,
                updated_at INTEGER
            );",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE metadata_items (
                id INTEGER PRIMARY KEY,
                library_section_id INTEGER,
                metadata_type INTEGER,
                title TEXT,
                original_title TEXT,
                year INTEGER,
                guid TEXT,
                deleted_at INTEGER
            );",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE media_items (
                id INTEGER PRIMARY KEY,
                library_section_id INTEGER,
                section_location_id INTEGER,
                metadata_item_id INTEGER,
                deleted_at INTEGER
            );",
        )
        .await
        .unwrap();
        pool.execute(
            "CREATE TABLE media_parts (
                id INTEGER PRIMARY KEY,
                media_item_id INTEGER,
                file TEXT,
                deleted_at INTEGER
            );",
        )
        .await
        .unwrap();
    }

    #[test]
    fn build_anime_remediation_plan_group_marks_simple_legacy_root_as_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());

        let legacy_root = cfg.libraries[0].path.join("Show A");
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
        let source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
        std::fs::write(&source, b"video").unwrap();
        let legacy_symlink = legacy_root.join("Season 01/Show A - S01E01.mkv");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, &legacy_symlink).unwrap();

        let group = build_anime_remediation_plan_group(&sample_group(dir.path())).unwrap();
        assert!(group.eligible);
        assert_eq!(group.legacy_symlink_candidates, 1);
        assert!(group.block_reasons.is_empty());
    }

    #[test]
    fn build_anime_remediation_plan_group_blocks_non_symlink_media_files() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());

        let legacy_root = cfg.libraries[0].path.join("Show A");
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
        std::fs::write(legacy_root.join("Season 01/Show A - S01E01.mkv"), b"video").unwrap();

        let group = build_anime_remediation_plan_group(&sample_group(dir.path())).unwrap();
        assert!(!group.eligible);
        assert_eq!(group.legacy_media_files, 1);
        assert!(group
            .block_reasons
            .iter()
            .any(|reason| reason.message.contains("non-symlink media files")));
        assert!(group.block_reasons.iter().any(|reason| matches!(
            reason.code,
            AnimeRemediationBlockCode::LegacyRootsContainRealMedia
        )));
    }

    #[test]
    fn build_anime_remediation_blocked_reason_summary_counts_groups_per_reason() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());

        let legacy_root = cfg.libraries[0].path.join("Show A");
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
        std::fs::write(legacy_root.join("Season 01/Show A - S01E01.mkv"), b"video").unwrap();

        let blocked = build_anime_remediation_plan_group(&sample_group(dir.path())).unwrap();
        let summary = build_anime_remediation_blocked_reason_summary(&[blocked]);

        assert_eq!(summary.len(), 2);
        assert!(matches!(
            summary[0].code,
            AnimeRemediationBlockCode::LegacyRootsContainRealMedia
        ));
        assert_eq!(summary[0].groups, 1);
        assert_eq!(
            summary[0].recommended_action,
            "Manual migration required; move or relink real media files before remediation."
        );
    }

    #[tokio::test]
    async fn cleanup_remediate_anime_preview_then_apply_quarantines_legacy_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.cleanup.prune.quarantine_path = dir.path().join("quarantine");

        let anime_root = cfg.libraries[0].path.clone();
        let tagged_root = anime_root.join("Show A (2024) {tvdb-1}");
        let legacy_root = anime_root.join("Show A");
        std::fs::create_dir_all(tagged_root.join("Season 01")).unwrap();
        std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();

        let db = Database::new(&cfg.db_path).await.unwrap();
        let tracked_source = cfg.sources[0].path.join("Show.A.S01E01.mkv");
        let tracked_target = tagged_root.join("Season 01/Show A - S01E01.mkv");
        std::fs::write(&tracked_source, b"video").unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: tracked_source.clone(),
            target_path: tracked_target,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: crate::models::LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let legacy_symlink = legacy_root.join("Season 01/Show A - S01E01.mkv");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&tracked_source, &legacy_symlink).unwrap();

        let plex_db_path = dir.path().join("plex.db");
        create_test_plex_duplicate_db(&plex_db_path).await;
        let options = SqliteConnectOptions::from_str(plex_db_path.to_str().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?)",
        )
        .bind(anime_root.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at)
             VALUES (1, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://anidb-100', NULL),
                    (2, 1, 2, 'Show A', '', 2024, 'com.plexapp.agents.hama://tvdb-1', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let report_path = dir.path().join("anime-remediation-plan.json");
        run_cleanup_anime_remediation(
            &cfg,
            &db,
            CleanupAnimeRemediationArgs {
                report: None,
                plex_db: Some(plex_db_path.to_str().unwrap()),
                apply: false,
                title: None,
                out: Some(report_path.to_str().unwrap()),
                confirm_token: None,
                max_delete: None,
                gate_mode: GateMode::Enforce,
                library_filter: Some("Anime"),
                output: OutputFormat::Json,
            },
        )
        .await
        .unwrap();

        let plan: AnimeRemediationPlanReport =
            serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
        assert_eq!(plan.eligible_groups, 1);
        assert_eq!(plan.cleanup_candidates, 1);
        assert!(plan.blocked_reason_summary.is_empty());

        run_cleanup_anime_remediation(
            &cfg,
            &db,
            CleanupAnimeRemediationArgs {
                report: Some(report_path.to_str().unwrap()),
                plex_db: None,
                apply: true,
                title: None,
                out: None,
                confirm_token: Some(&plan.confirmation_token),
                max_delete: None,
                gate_mode: GateMode::Enforce,
                library_filter: Some("Anime"),
                output: OutputFormat::Json,
            },
        )
        .await
        .unwrap();

        assert!(!legacy_symlink.exists());
        let quarantined = cfg
            .cleanup
            .prune
            .quarantine_path
            .join("anime/Show A/Season 01/Show A - S01E01.mkv");
        assert!(quarantined.is_symlink());

        let backup_entries = std::fs::read_dir(&cfg.backup.path)
            .unwrap()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>();
        assert!(backup_entries
            .iter()
            .any(|name| name.starts_with("safety-anime-remediation-")));
    }

    #[tokio::test]
    async fn cleanup_remediate_anime_apply_requires_foreign_quarantine() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.cleanup.prune.quarantine_foreign = false;

        let db = Database::new(&cfg.db_path).await.unwrap();
        let report_path = dir.path().join("anime-remediation-plan.json");
        std::fs::write(
            &report_path,
            serde_json::to_string(&AnimeRemediationPlanReport {
                version: ANIME_REMEDIATION_REPORT_VERSION,
                created_at: Utc::now(),
                plex_db_path: dir.path().join("plex.db"),
                title_filter: None,
                total_groups: 0,
                eligible_groups: 0,
                blocked_groups: 0,
                cleanup_candidates: 0,
                confirmation_token: "token".to_string(),
                blocked_reason_summary: Vec::new(),
                groups: Vec::new(),
                cleanup_report: cleanup_audit::CleanupReport {
                    version: 1,
                    created_at: Utc::now(),
                    scope: CleanupScope::Anime,
                    summary: cleanup_audit::CleanupSummary::default(),
                    findings: Vec::new(),
                },
            })
            .unwrap(),
        )
        .unwrap();

        let err = run_cleanup_anime_remediation(
            &cfg,
            &db,
            CleanupAnimeRemediationArgs {
                report: Some(report_path.to_str().unwrap()),
                plex_db: None,
                apply: true,
                title: None,
                out: None,
                confirm_token: Some("token"),
                max_delete: None,
                gate_mode: GateMode::Enforce,
                library_filter: Some("Anime"),
                output: OutputFormat::Json,
            },
        )
        .await
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("cleanup.prune.quarantine_foreign=true"));
    }

    #[test]
    fn anime_remediation_plan_report_loads_legacy_string_block_reasons() {
        let report_json = serde_json::json!({
            "version": ANIME_REMEDIATION_REPORT_VERSION,
            "created_at": Utc::now(),
            "plex_db_path": "/tmp/plex.db",
            "title_filter": serde_json::Value::Null,
            "total_groups": 1,
            "eligible_groups": 0,
            "blocked_groups": 1,
            "cleanup_candidates": 0,
            "confirmation_token": "token",
            "groups": [{
                "normalized_title": "show a",
                "eligible": false,
                "block_reasons": [
                    "legacy roots still contain 3 tracked DB links",
                    "no legacy symlink candidates found under legacy roots"
                ],
                "recommended_tagged_root": {
                    "path": "/anime/Show A (2024) {tvdb-1}",
                    "filesystem_symlinks": 1,
                    "db_active_links": 0
                },
                "alternate_tagged_roots": [],
                "legacy_roots": [{
                    "path": "/anime/Show A",
                    "filesystem_symlinks": 3,
                    "db_active_links": 3
                }],
                "legacy_symlink_candidates": 0,
                "broken_symlink_candidates": 0,
                "legacy_media_files": 0,
                "candidate_symlink_samples": [],
                "plex_live_rows": 2,
                "plex_deleted_rows": 0,
                "plex_guid_kinds": ["anidb", "tvdb"],
                "plex_guids": ["anidb-100", "tvdb-1"]
            }],
            "cleanup_report": {
                "version": 1,
                "created_at": Utc::now(),
                "scope": "anime",
                "summary": {
                    "total_findings": 0,
                    "critical": 0,
                    "high": 0,
                    "warning": 0,
                    "quarantine_candidates": 0
                },
                "findings": []
            }
        });

        let report: AnimeRemediationPlanReport = serde_json::from_value(report_json).unwrap();

        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].block_reasons.len(), 2);
        assert!(matches!(
            report.groups[0].block_reasons[0].code,
            AnimeRemediationBlockCode::LegacyRootsStillTracked
        ));
        assert_eq!(
            report.groups[0].block_reasons[0].recommended_action,
            "Do not auto-remediate yet; first move or prune the DB-tracked legacy links."
        );
        assert!(matches!(
            report.groups[0].block_reasons[1].code,
            AnimeRemediationBlockCode::NoLegacySymlinkCandidates
        ));
    }
}
