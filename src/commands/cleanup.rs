use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "recommended_root_untracked_db" => Some(Self::RecommendedRootUntrackedDb),
            "legacy_roots_still_tracked" => Some(Self::LegacyRootsStillTracked),
            "legacy_roots_contain_real_media" => Some(Self::LegacyRootsContainRealMedia),
            "no_legacy_symlink_candidates" => Some(Self::NoLegacySymlinkCandidates),
            "manual_review_required" => Some(Self::ManualReviewRequired),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AnimeRemediationVisibilityFilter {
    #[default]
    All,
    Eligible,
    Blocked,
}

impl AnimeRemediationVisibilityFilter {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Eligible => "eligible",
            Self::Blocked => "blocked",
        }
    }

    pub(crate) fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("all") => Ok(Self::All),
            Some("eligible") => Ok(Self::Eligible),
            Some("blocked") => Ok(Self::Blocked),
            Some(other) => anyhow::bail!(
                "Invalid anime remediation state filter '{}' (expected all, eligible, or blocked)",
                other
            ),
        }
    }

    fn matches(self, eligible: bool) -> bool {
        match self {
            Self::All => true,
            Self::Eligible => eligible,
            Self::Blocked => !eligible,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AnimeRemediationGroupFilters {
    pub(crate) visibility: AnimeRemediationVisibilityFilter,
    pub(crate) block_code: Option<AnimeRemediationBlockCode>,
    pub(crate) title_contains: Option<String>,
}

impl AnimeRemediationGroupFilters {
    pub(crate) fn parse(
        visibility: Option<&str>,
        reason: Option<&str>,
        title: Option<&str>,
    ) -> Result<Self> {
        let visibility = AnimeRemediationVisibilityFilter::parse(visibility)?;
        let block_code = match reason.map(str::trim).filter(|value| !value.is_empty()) {
            Some(raw) => Some(AnimeRemediationBlockCode::from_str(raw).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid anime remediation reason filter '{}' (expected one of the documented block codes)",
                    raw
                )
            })?),
            None => None,
        };
        let title_contains = title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());

        Ok(Self {
            visibility,
            block_code,
            title_contains,
        })
    }
}

pub(crate) fn anime_remediation_block_reason_catalog() -> Vec<AnimeRemediationBlockedReasonSummary>
{
    [
        AnimeRemediationBlockCode::RecommendedRootUntrackedDb,
        AnimeRemediationBlockCode::LegacyRootsStillTracked,
        AnimeRemediationBlockCode::LegacyRootsContainRealMedia,
        AnimeRemediationBlockCode::NoLegacySymlinkCandidates,
        AnimeRemediationBlockCode::ManualReviewRequired,
    ]
    .into_iter()
    .map(|code| AnimeRemediationBlockedReasonSummary {
        code,
        label: code.label().to_string(),
        recommended_action: code.recommended_action().to_string(),
        groups: 0,
    })
    .collect()
}

pub(crate) fn assess_anime_remediation_groups(
    groups: &[AnimeRemediationSample],
) -> Result<Vec<AnimeRemediationPlanGroup>> {
    groups
        .iter()
        .map(assess_anime_remediation_group)
        .collect::<Result<Vec<_>>>()
}

pub(crate) fn filter_anime_remediation_groups(
    groups: Vec<AnimeRemediationPlanGroup>,
    filters: &AnimeRemediationGroupFilters,
) -> Vec<AnimeRemediationPlanGroup> {
    let title_contains = filters
        .title_contains
        .as_deref()
        .map(|value| value.to_ascii_lowercase());

    groups
        .into_iter()
        .filter(|group| filters.visibility.matches(group.eligible))
        .filter(|group| {
            filters
                .block_code
                .is_none_or(|code| group.block_reasons.iter().any(|reason| reason.code == code))
        })
        .filter(|group| {
            title_contains
                .as_ref()
                .is_none_or(|needle| group.normalized_title.to_ascii_lowercase().contains(needle))
        })
        .collect()
}

fn tsv_cell(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

pub(crate) fn render_anime_remediation_groups_tsv(groups: &[AnimeRemediationPlanGroup]) -> String {
    let mut lines = Vec::with_capacity(groups.len() + 1);
    lines.push(
        [
            "normalized_title",
            "eligible",
            "block_codes",
            "block_messages",
            "recommended_action",
            "recommended_tagged_root",
            "legacy_roots",
            "legacy_symlink_candidates",
            "broken_symlink_candidates",
            "legacy_media_files",
            "candidate_symlink_samples",
            "broken_symlink_samples",
            "legacy_media_file_samples",
            "plex_live_rows",
            "plex_deleted_rows",
            "plex_guid_kinds",
        ]
        .join("\t"),
    );

    for group in groups {
        let row = [
            tsv_cell(&group.normalized_title),
            group.eligible.to_string(),
            tsv_cell(
                &group
                    .block_reasons
                    .iter()
                    .map(|reason| reason.code.as_str())
                    .collect::<Vec<_>>()
                    .join("|"),
            ),
            tsv_cell(
                &group
                    .block_reasons
                    .iter()
                    .map(|reason| reason.message.as_str())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            tsv_cell(
                group
                    .block_reasons
                    .first()
                    .map(|reason| reason.recommended_action.as_str())
                    .unwrap_or(""),
            ),
            tsv_cell(&group.recommended_tagged_root.path.display().to_string()),
            tsv_cell(
                &group
                    .legacy_roots
                    .iter()
                    .map(|root| root.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            group.legacy_symlink_candidates.to_string(),
            group.broken_symlink_candidates.to_string(),
            group.legacy_media_files.to_string(),
            tsv_cell(
                &group
                    .candidate_symlink_samples
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            tsv_cell(
                &group
                    .broken_symlink_samples
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            tsv_cell(
                &group
                    .legacy_media_file_samples
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
            ),
            group.plex_live_rows.to_string(),
            group.plex_deleted_rows.to_string(),
            tsv_cell(&group.plex_guid_kinds.join("|")),
        ]
        .join("\t");
        lines.push(row);
    }

    lines.join("\n")
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
                    code,
                    message,
                    recommended_action: code.recommended_action().to_string(),
                }
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnimeRemediationPlanGroup {
    pub(crate) normalized_title: String,
    pub(crate) eligible: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_anime_remediation_block_reasons"
    )]
    pub(crate) block_reasons: Vec<AnimeRemediationBlockReason>,
    pub(crate) recommended_tagged_root: crate::commands::report::AnimeRootUsageSample,
    pub(crate) alternate_tagged_roots: Vec<crate::commands::report::AnimeRootUsageSample>,
    pub(crate) legacy_roots: Vec<crate::commands::report::AnimeRootUsageSample>,
    pub(crate) legacy_symlink_candidates: usize,
    pub(crate) broken_symlink_candidates: usize,
    pub(crate) legacy_media_files: usize,
    #[serde(default)]
    pub(crate) candidate_symlink_samples: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) broken_symlink_samples: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) legacy_media_file_samples: Vec<PathBuf>,
    pub(crate) plex_live_rows: usize,
    pub(crate) plex_deleted_rows: usize,
    pub(crate) plex_guid_kinds: Vec<String>,
    pub(crate) plex_guids: Vec<String>,
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
    broken_symlink_samples: Vec<PathBuf>,
    media_files: Vec<PathBuf>,
    media_file_samples: Vec<PathBuf>,
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

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

fn canonical_plex_db_path(path: PathBuf) -> Option<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    let canonical = path.canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }

    canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.eq_ignore_ascii_case("db"))?;

    Some(canonical)
}

fn resolve_plex_db_path(query_path: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return canonical_plex_db_path(PathBuf::from(requested));
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find_map(canonical_plex_db_path)
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

pub(crate) fn assess_anime_remediation_group(
    sample: &AnimeRemediationSample,
) -> Result<AnimeRemediationPlanGroup> {
    let mut candidate_paths = BTreeSet::new();
    let mut broken_symlink_sample_paths = BTreeSet::new();
    let mut legacy_media_file_sample_paths = BTreeSet::new();
    let mut broken_symlink_candidates = 0usize;
    let mut legacy_media_files = 0usize;

    for legacy_root in &sample.legacy_roots {
        let scan = scan_legacy_root(&legacy_root.path);
        broken_symlink_candidates += scan.broken_symlink_paths.len();
        legacy_media_files += scan.media_files.len();
        for path in scan.broken_symlink_samples {
            if broken_symlink_sample_paths.len() < ANIME_REMEDIATION_SAMPLE_LIMIT {
                broken_symlink_sample_paths.insert(path);
            }
        }
        for path in scan.media_file_samples {
            if legacy_media_file_sample_paths.len() < ANIME_REMEDIATION_SAMPLE_LIMIT {
                legacy_media_file_sample_paths.insert(path);
            }
        }
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
        broken_symlink_samples: broken_symlink_sample_paths.into_iter().collect(),
        legacy_media_file_samples: legacy_media_file_sample_paths.into_iter().collect(),
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
                if scan.broken_symlink_samples.len() < ANIME_REMEDIATION_SAMPLE_LIMIT {
                    scan.broken_symlink_samples.push(path.clone());
                }
            }
            scan.symlink_paths.push(path);
        } else if file_type.is_file() && is_media_file_path(entry.path()) {
            scan.media_files.push(entry.path().to_path_buf());
            if scan.media_file_samples.len() < ANIME_REMEDIATION_SAMPLE_LIMIT {
                scan.media_file_samples.push(entry.path().to_path_buf());
            }
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
                        year: None,
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
        applied_at: None,
    }
}

pub(crate) fn summarize_anime_remediation_blocked_reasons(
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
                    code: reason.code,
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
mod tests;
