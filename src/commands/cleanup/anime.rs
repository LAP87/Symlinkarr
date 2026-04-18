use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use walkdir::WalkDir;

use crate::cleanup_audit::{self, CleanupScope};
use crate::commands::report::AnimeRemediationSample;

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

pub(crate) const ANIME_REMEDIATION_REPORT_VERSION: u32 = 1;
pub(crate) const ANIME_REMEDIATION_SAMPLE_LIMIT: usize = 6;

pub(crate) fn make_anime_block_reason(
    code: AnimeRemediationBlockCode,
    message: String,
) -> AnimeRemediationBlockReason {
    AnimeRemediationBlockReason {
        recommended_action: code.recommended_action().to_string(),
        message,
        code,
    }
}

pub(crate) fn remediation_group_matches_title_filter(
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

pub(crate) fn build_anime_remediation_cleanup_report(
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
