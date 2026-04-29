use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportModeReport {
    Preview,
    Safe,
    Aggressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportContentReport {
    Movie,
    Tv,
    Anime,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMetadataModeReport {
    Fast,
    Probe,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSourceShape {
    Missing,
    File,
    DirectItem,
    MultiItemFolder,
    BroadProviderRoot,
    EmptyFolder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportCandidateKind {
    File,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportResolutionSource {
    ExplicitId,
    CachedMetadata,
    TmdbLookup,
    TvdbLookup,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportConfidence {
    High,
    Medium,
    Low,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDecision {
    Preview,
    NeedsLookup,
    NeedsReview,
    Skipped,
    Created,
    Updated,
    WouldCreate,
    WouldUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportWriteAction {
    None,
    Create,
    Update,
    Skip,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportDestinations {
    pub destination: Option<PathBuf>,
    pub movie_destination: Option<PathBuf>,
    pub tv_destination: Option<PathBuf>,
    pub anime_destination: Option<PathBuf>,
    pub rules: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportCandidateReport {
    pub source_path: PathBuf,
    pub target_path: Option<PathBuf>,
    pub kind: ImportCandidateKind,
    pub title_hint: String,
    pub year_hint: Option<u32>,
    pub explicit_media_id: Option<String>,
    pub resolved_title: Option<String>,
    pub resolved_year: Option<u32>,
    pub probed_resolution: Option<String>,
    pub video_codec: Option<String>,
    pub hdr_formats: Vec<String>,
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub resolution_source: ImportResolutionSource,
    pub confidence: ImportConfidence,
    pub decision: ImportDecision,
    pub action: ImportWriteAction,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportSummary {
    pub candidates: usize,
    pub files: usize,
    pub folders: usize,
    pub movies: usize,
    pub tv: usize,
    pub anime: usize,
    pub unknown_content: usize,
    pub high_confidence: usize,
    pub medium_confidence: usize,
    pub low_confidence: usize,
    pub ambiguous_confidence: usize,
    pub explicit_ids: usize,
    pub needs_lookup: usize,
    pub skipped: usize,
    pub would_create: usize,
    pub would_update: usize,
    pub created: usize,
    pub updated: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportRulesSummary {
    pub loaded: bool,
    pub movie_default: Option<PathBuf>,
    pub tv_default: Option<PathBuf>,
    pub anime_default: Option<PathBuf>,
    pub movie_routes: usize,
    pub tv_routes: usize,
    pub anime_routes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportReport {
    pub version: u32,
    pub source: PathBuf,
    pub source_shape: ImportSourceShape,
    pub mode: ImportModeReport,
    pub content_type: ImportContentReport,
    pub metadata_mode: ImportMetadataModeReport,
    pub destinations: ImportDestinations,
    pub rules_summary: Option<ImportRulesSummary>,
    pub summary: ImportSummary,
    pub candidates: Vec<ImportCandidateReport>,
    pub warnings: Vec<String>,
    pub handoff: Vec<String>,
}
