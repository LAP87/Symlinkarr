use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CleanupScope {
    Anime,
    Tv,
    Movie,
    All,
}

impl CleanupScope {
    pub fn parse(input: &str) -> Result<Self> {
        match input.to_lowercase().as_str() {
            "anime" => Ok(Self::Anime),
            "tv" | "series" | "shows" => Ok(Self::Tv),
            "movie" | "movies" | "film" | "films" => Ok(Self::Movie),
            "all" => Ok(Self::All),
            _ => anyhow::bail!(
                "Unsupported scope '{}'. Supported: anime, tv, movie, all",
                input
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Critical,
    High,
    Warning,
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingSeverity::Critical => write!(f, "critical"),
            FindingSeverity::High => write!(f, "high"),
            FindingSeverity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CleanupOwnership {
    Managed,
    #[default]
    Foreign,
}

impl std::fmt::Display for CleanupOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CleanupOwnership::Managed => write!(f, "managed"),
            CleanupOwnership::Foreign => write!(f, "foreign"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FindingReason {
    BrokenSource,
    LegacyAnimeRootDuplicate,
    ParserTitleMismatch,
    AlternateLibraryMatch,
    MovieEpisodeSource,
    ArrUntracked,
    EpisodeOutOfRange,
    DuplicateEpisodeSlot,
    SeasonCountAnomaly,
    NonRdSourcePath,
}

impl std::fmt::Display for FindingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingReason::BrokenSource => write!(f, "broken_source"),
            FindingReason::LegacyAnimeRootDuplicate => write!(f, "legacy_anime_root_duplicate"),
            FindingReason::ParserTitleMismatch => write!(f, "parser_title_mismatch"),
            FindingReason::AlternateLibraryMatch => write!(f, "alternate_library_match"),
            FindingReason::MovieEpisodeSource => write!(f, "movie_episode_source"),
            FindingReason::ArrUntracked => write!(f, "arr_untracked"),
            FindingReason::EpisodeOutOfRange => write!(f, "episode_out_of_range"),
            FindingReason::DuplicateEpisodeSlot => write!(f, "duplicate_episode_slot"),
            FindingReason::SeasonCountAnomaly => write!(f, "season_count_anomaly"),
            FindingReason::NonRdSourcePath => write!(f, "non_rd_source_path"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParsedContext {
    pub library_title: String,
    pub parsed_title: String,
    #[serde(default)]
    pub year: Option<u32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlternateMatchContext {
    pub media_id: String,
    pub title: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyAnimeRootDetails {
    pub normalized_title: String,
    pub untagged_root: PathBuf,
    pub tagged_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupFinding {
    pub symlink_path: PathBuf,
    pub source_path: PathBuf,
    pub media_id: String,
    pub severity: FindingSeverity,
    pub confidence: f64,
    pub reasons: Vec<FindingReason>,
    pub parsed: ParsedContext,
    #[serde(default)]
    pub alternate_match: Option<AlternateMatchContext>,
    #[serde(default)]
    pub legacy_anime_root: Option<LegacyAnimeRootDetails>,
    #[serde(default)]
    pub db_tracked: bool,
    #[serde(default)]
    pub ownership: CleanupOwnership,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CleanupSummary {
    pub total_findings: usize,
    pub critical: usize,
    pub high: usize,
    pub warning: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupReport {
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub scope: CleanupScope,
    pub findings: Vec<CleanupFinding>,
    pub summary: CleanupSummary,
    #[serde(default)]
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct PruneOutcome {
    pub candidates: usize,
    pub blocked_candidates: usize,
    pub high_or_critical_candidates: usize,
    pub safe_warning_duplicate_candidates: usize,
    pub legacy_anime_root_candidates: usize,
    pub legacy_anime_root_groups: Vec<LegacyAnimeRootGroupCount>,
    pub managed_candidates: usize,
    pub foreign_candidates: usize,
    pub reason_counts: Vec<PruneReasonCount>,
    pub blocked_reason_summary: Vec<PruneBlockedReasonSummary>,
    pub removed: usize,
    pub quarantined: usize,
    pub skipped: usize,
    pub confirmation_token: String,
    pub affected_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PruneReasonCount {
    pub reason: FindingReason,
    pub total: usize,
    pub managed: usize,
    pub foreign: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PruneBlockedReasonCode {
    ForeignQuarantineDisabled,
    DuplicateSlotNeedsTrackedAnchor,
    DuplicateSlotTainted,
    DuplicateSlotMultipleTrackedAnchors,
    DuplicateSlotSourceMismatch,
    LegacyAnimeRootsExcludedByDefault,
}

impl PruneBlockedReasonCode {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::ForeignQuarantineDisabled => {
                "foreign candidates are blocked because quarantine is disabled"
            }
            Self::DuplicateSlotNeedsTrackedAnchor => {
                "duplicate slots without a tracked anchor are blocked"
            }
            Self::DuplicateSlotTainted => "duplicate slots with extra anomaly reasons are blocked",
            Self::DuplicateSlotMultipleTrackedAnchors => {
                "duplicate slots with multiple tracked anchors are blocked"
            }
            Self::DuplicateSlotSourceMismatch => {
                "duplicate slots with mismatched source files are blocked"
            }
            Self::LegacyAnimeRootsExcludedByDefault => {
                "legacy anime-root warnings are excluded by default"
            }
        }
    }

    pub(crate) fn recommended_action(&self) -> &'static str {
        match self {
            Self::ForeignQuarantineDisabled => {
                "Enable cleanup.prune.quarantine_foreign or review the foreign symlinks manually before applying prune."
            }
            Self::DuplicateSlotNeedsTrackedAnchor => {
                "Keep scanning until one canonical tracked link owns the slot before auto-pruning the duplicates."
            }
            Self::DuplicateSlotTainted => {
                "Do not auto-prune mixed anomaly slots; inspect the title manually and clear the extra mismatch first."
            }
            Self::DuplicateSlotMultipleTrackedAnchors => {
                "Resolve which tracked path should win before letting prune remove anything else in that slot."
            }
            Self::DuplicateSlotSourceMismatch => {
                "Do not auto-prune duplicates that point at different source files; inspect the release choice manually."
            }
            Self::LegacyAnimeRootsExcludedByDefault => {
                "Use the guarded anime remediation workflow or explicitly opt in to legacy anime-root cleanup after review."
            }
        }
    }
}

impl std::fmt::Display for PruneBlockedReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignQuarantineDisabled => write!(f, "foreign_quarantine_disabled"),
            Self::DuplicateSlotNeedsTrackedAnchor => {
                write!(f, "duplicate_slot_needs_tracked_anchor")
            }
            Self::DuplicateSlotTainted => write!(f, "duplicate_slot_tainted"),
            Self::DuplicateSlotMultipleTrackedAnchors => {
                write!(f, "duplicate_slot_multiple_tracked_anchors")
            }
            Self::DuplicateSlotSourceMismatch => write!(f, "duplicate_slot_source_mismatch"),
            Self::LegacyAnimeRootsExcludedByDefault => {
                write!(f, "legacy_anime_roots_excluded_by_default")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PruneBlockedReasonSummary {
    pub code: PruneBlockedReasonCode,
    pub label: String,
    pub candidates: usize,
    pub recommended_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyAnimeRootGroupCount {
    pub normalized_title: String,
    pub total: usize,
    pub tagged_roots: Vec<PathBuf>,
}
