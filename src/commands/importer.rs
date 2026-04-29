use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use std::sync::OnceLock;
use walkdir::WalkDir;

use crate::api::tmdb::{TmdbClient, TmdbSearchMatch};
use crate::api::tvdb::{TvdbClient, TvdbSearchMatch};
use crate::db::Database;
use crate::import_report::{
    ImportCandidateKind, ImportCandidateReport, ImportConfidence, ImportContentReport,
    ImportDecision, ImportDestinations, ImportMetadataModeReport, ImportModeReport, ImportReport,
    ImportResolutionSource, ImportRulesSummary, ImportSourceShape, ImportSummary,
    ImportWriteAction,
};
use crate::models::{ContentMetadata, LinkRecord, LinkStatus, MediaType};
use crate::utils::VIDEO_EXTENSIONS;
use crate::{
    ImportContentType, ImportLookupMode, ImportMetadataMode, ImportMode, ImportProbeTool,
    OutputFormat,
};

const IMPORT_REMOTE_CACHE_TTL_HOURS: u64 = 168;
const IMPORT_PROBE_CACHE_TTL_HOURS: u64 = 24 * 30;
const IMPORT_RESOLUTION_CACHE_TTL_HOURS: u64 = 24 * 30;

#[derive(Debug, Clone)]
pub(crate) struct ImportOptions {
    pub source: PathBuf,
    pub destination: Option<PathBuf>,
    pub movie_destination: Option<PathBuf>,
    pub tv_destination: Option<PathBuf>,
    pub anime_destination: Option<PathBuf>,
    pub rules: Option<PathBuf>,
    pub content_type: ImportContentType,
    pub mode: ImportMode,
    pub metadata_mode: ImportMetadataMode,
    pub probe_tool: ImportProbeTool,
    pub lookup_mode: ImportLookupMode,
    pub offline: bool,
    pub refresh_metadata: bool,
    pub max_lookups: usize,
    pub report_path: Option<PathBuf>,
    pub yes: bool,
    pub folders_only: bool,
    pub create_links: bool,
    pub output: OutputFormat,
}

pub(crate) async fn run_import(
    options: ImportOptions,
    db: Option<&Database>,
    tmdb: Option<&TmdbClient>,
    tvdb: Option<&mut TvdbClient>,
) -> Result<()> {
    validate_options(&options)?;

    let mut report = build_import_report(&options, db, tmdb, tvdb).await?;

    if import_requires_confirmation(options.mode, options.yes) {
        confirm_import(&report, options.mode)?;
    }

    if options.mode != ImportMode::Preview {
        apply_import_plan(&mut report, &options)?;
        if let Some(db) = db.filter(|_| !options.folders_only) {
            backfill_import_links(&mut report, db, options.content_type).await;
        }
    }

    let saved_report_path = write_import_report(&report, options.report_path.as_deref())?;

    match options.output {
        OutputFormat::Json => crate::commands::print_json(&report),
        OutputFormat::Text => print_text_report(&report, &saved_report_path),
    }

    Ok(())
}

pub(crate) fn write_import_report(
    report: &ImportReport,
    report_path: Option<&Path>,
) -> Result<PathBuf> {
    let path = report_path
        .map(PathBuf::from)
        .unwrap_or_else(default_import_report_path);

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create import report dir {}", parent.display()))?;
    }

    let json =
        serde_json::to_string_pretty(report).context("failed to serialize import report JSON")?;
    std::fs::write(&path, format!("{json}\n"))
        .with_context(|| format!("failed to write import report {}", path.display()))?;
    Ok(path)
}

fn default_import_report_path() -> PathBuf {
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let pid = std::process::id();
    PathBuf::from(format!("symlinkarr-import-report-{ts}-{pid}.json"))
}

pub(crate) fn validate_options(options: &ImportOptions) -> Result<()> {
    if options.offline && options.lookup_mode == ImportLookupMode::Remote {
        anyhow::bail!("import --offline cannot be combined with --lookup-mode remote");
    }
    if options.offline && options.refresh_metadata {
        anyhow::bail!("import --refresh-metadata cannot be combined with --offline");
    }
    if options.refresh_metadata && options.lookup_mode != ImportLookupMode::Remote {
        anyhow::bail!("import --refresh-metadata requires --lookup-mode remote");
    }
    if options.folders_only && options.create_links {
        anyhow::bail!("import --folders-only cannot be combined with --create-links");
    }

    if options.content_type == ImportContentType::Auto {
        if options.destination.is_some()
            && (options.movie_destination.is_some()
                || options.tv_destination.is_some()
                || options.anime_destination.is_some())
        {
            anyhow::bail!(
                "import --content-type auto accepts either --destination or per-type destinations, not both"
            );
        }
        if options.destination.is_none()
            && options.movie_destination.is_none()
            && options.tv_destination.is_none()
            && options.anime_destination.is_none()
            && options.rules.is_none()
        {
            anyhow::bail!(
                "import --content-type auto requires --destination, per-type destinations, or --rules"
            );
        }
    } else if options.destination.is_none()
        && options.rules.is_none()
        && destination_for_content_type(options).is_none()
    {
        anyhow::bail!(
            "import --content-type {:?} requires --destination, a matching per-type destination, or --rules",
            options.content_type
        );
    }

    Ok(())
}

pub(crate) async fn build_import_report(
    options: &ImportOptions,
    db: Option<&Database>,
    tmdb: Option<&TmdbClient>,
    tvdb: Option<&mut TvdbClient>,
) -> Result<ImportReport> {
    let source_shape = detect_source_shape(&options.source);
    let mut warnings = Vec::new();
    if matches!(source_shape, ImportSourceShape::BroadProviderRoot) {
        warnings.push("Broad provider-root source detected; aggressive mode can create many top-level links from this source.".to_string());
    }
    if options.metadata_mode != ImportMetadataMode::Fast {
        warnings.push(format!(
            "Metadata probing mode {:?} may read small stream headers from provider-backed files.",
            options.metadata_mode
        ));
    }
    let rules_plan = load_rules_plan(options.rules.as_deref(), &mut warnings)?;
    let rules_summary = rules_plan.as_ref().map(|plan| plan.summary.clone());
    warn_for_populated_destinations(options, rules_plan.as_ref(), &mut warnings);

    let mut candidates = collect_candidates(&options.source, source_shape).with_context(|| {
        format!(
            "failed to collect import candidates from {}",
            options.source.display()
        )
    })?;
    apply_probe_metadata(&mut candidates, options, db, &mut warnings).await;
    let candidates = resolve_candidates_from_cache(candidates, options, db).await;
    let candidates =
        resolve_candidates_from_remote(candidates, options, db, tmdb, tvdb, &mut warnings).await;
    let candidates = plan_candidates(candidates, options, rules_plan.as_ref());
    let destinations = ImportDestinations {
        destination: options.destination.clone(),
        movie_destination: options.movie_destination.clone(),
        tv_destination: options.tv_destination.clone(),
        anime_destination: options.anime_destination.clone(),
        rules: options.rules.clone(),
    };
    let content_type = content_report(options.content_type);
    let summary = summarize_candidates(&candidates, content_type, &destinations);

    Ok(ImportReport {
        version: 1,
        source: options.source.clone(),
        source_shape,
        mode: mode_report(options.mode),
        content_type,
        metadata_mode: metadata_mode_report(options.metadata_mode),
        destinations,
        rules_summary,
        summary,
        candidates,
        warnings,
        handoff: import_handoff_messages(options.mode),
    })
}

fn import_handoff_messages(mode: ImportMode) -> Vec<String> {
    match mode {
        ImportMode::Preview => vec![
            "Review this report, then rerun import with --mode safe or --mode aggressive when ready to write links.".to_string(),
        ],
        ImportMode::Safe | ImportMode::Aggressive => vec![
            "Next: run `symlinkarr scan` or leave the daemon running so Symlinkarr can check the imported links.".to_string(),
            "If you use deferred media refresh, run `symlinkarr refresh drain` when you want media servers notified.".to_string(),
            "Import does not remove provider/RD/Usenet content; cleanup remains a separate explicit workflow.".to_string(),
        ],
    }
}

fn warn_for_populated_destinations(
    options: &ImportOptions,
    rules_plan: Option<&ImportRulesPlan>,
    warnings: &mut Vec<String>,
) {
    for destination in import_destination_roots(options, rules_plan) {
        if directory_has_entries(&destination) {
            let write_label = if options.folders_only {
                "create missing ID-tagged folders and still refuses existing target conflicts"
            } else {
                "create/update symlinks and still refuses non-symlink overwrites"
            };
            warnings.push(format!(
                "Destination {} is already populated; import will only {}.",
                destination.display(),
                write_label
            ));
        }
    }
}

fn import_destination_roots(
    options: &ImportOptions,
    rules_plan: Option<&ImportRulesPlan>,
) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    for destination in [
        options.destination.as_ref(),
        options.movie_destination.as_ref(),
        options.tv_destination.as_ref(),
        options.anime_destination.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        roots.insert(destination.clone());
    }
    if let Some(plan) = rules_plan {
        collect_rules_bucket_destinations(&plan.rules.destinations.movies, &mut roots);
        collect_rules_bucket_destinations(&plan.rules.destinations.tv, &mut roots);
        collect_rules_bucket_destinations(&plan.rules.destinations.anime, &mut roots);
    }
    roots.into_iter().collect()
}

fn collect_rules_bucket_destinations(bucket: &ImportRulesBucketXml, roots: &mut BTreeSet<PathBuf>) {
    if let Some(destination) = &bucket.default_destination {
        roots.insert(destination.clone());
    }
    for route in &bucket.routes {
        if let Some(destination) = &route.to {
            roots.insert(destination.clone());
        }
    }
}

fn directory_has_entries(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn destination_for_content_type(options: &ImportOptions) -> Option<PathBuf> {
    match options.content_type {
        ImportContentType::Movie => options.movie_destination.clone(),
        ImportContentType::Tv => options.tv_destination.clone(),
        ImportContentType::Anime => options.anime_destination.clone(),
        ImportContentType::Auto => None,
    }
}

fn plan_candidates(
    candidates: Vec<ImportCandidateReport>,
    options: &ImportOptions,
    rules_plan: Option<&ImportRulesPlan>,
) -> Vec<ImportCandidateReport> {
    candidates
        .into_iter()
        .map(|mut candidate| {
            if candidate.action == ImportWriteAction::Skip {
                return candidate;
            }
            let destination = destination_for_candidate(&candidate, options, rules_plan);
            let Some(destination) = destination else {
                candidate.decision = ImportDecision::Skipped;
                candidate.action = ImportWriteAction::Skip;
                candidate.reason = Some("no_destination".to_string());
                return candidate;
            };

            let Some(source_name) = candidate
                .source_path
                .file_name()
                .and_then(|name| name.to_str())
            else {
                candidate.decision = ImportDecision::Skipped;
                candidate.action = ImportWriteAction::Skip;
                candidate.reason = Some("invalid_source_name".to_string());
                return candidate;
            };

            let target_name = if options.folders_only {
                target_folder_name_with_optional_id(&candidate, source_name)
            } else {
                target_name_with_optional_id(&candidate, source_name)
            };
            let target_path = destination.join(target_name);
            candidate.target_path = Some(target_path.clone());

            if options.mode == ImportMode::Safe && candidate.explicit_media_id.is_none() {
                candidate.decision = ImportDecision::NeedsLookup;
                candidate.action = ImportWriteAction::Skip;
                candidate.reason = Some("safe_mode_requires_explicit_id".to_string());
                return candidate;
            }

            match classify_import_target(&target_path, &candidate.source_path, options.folders_only)
            {
                TargetState::Missing => {
                    candidate.decision = if options.mode == ImportMode::Preview {
                        ImportDecision::WouldCreate
                    } else {
                        ImportDecision::Preview
                    };
                    candidate.action = ImportWriteAction::Create;
                }
                TargetState::Directory if options.folders_only => {
                    candidate.decision = ImportDecision::Skipped;
                    candidate.action = ImportWriteAction::Skip;
                    candidate.reason = Some("target_directory_already_exists".to_string());
                }
                TargetState::MatchingSymlink => {
                    candidate.decision = ImportDecision::Skipped;
                    candidate.action = ImportWriteAction::Skip;
                    candidate.reason = Some("target_symlink_already_correct".to_string());
                }
                TargetState::Symlink => {
                    if options.mode == ImportMode::Safe {
                        candidate.decision = ImportDecision::Skipped;
                        candidate.action = ImportWriteAction::Skip;
                        candidate.reason = Some("target_symlink_exists".to_string());
                    } else {
                        candidate.decision = if options.mode == ImportMode::Preview {
                            ImportDecision::WouldUpdate
                        } else {
                            ImportDecision::Preview
                        };
                        candidate.action = ImportWriteAction::Update;
                    }
                }
                TargetState::Directory | TargetState::NonSymlink => {
                    candidate.decision = ImportDecision::Skipped;
                    candidate.action = ImportWriteAction::Skip;
                    candidate.reason = Some("target_is_not_symlink".to_string());
                }
            }

            candidate
        })
        .collect()
}

async fn resolve_candidates_from_cache(
    candidates: Vec<ImportCandidateReport>,
    options: &ImportOptions,
    db: Option<&Database>,
) -> Vec<ImportCandidateReport> {
    if options.offline || options.refresh_metadata || options.lookup_mode == ImportLookupMode::Off {
        return candidates;
    }
    let Some(db) = db else {
        return candidates;
    };
    if candidates
        .iter()
        .all(|candidate| candidate.explicit_media_id.is_some())
    {
        return candidates;
    }

    let candidates = resolve_candidates_from_import_cache(candidates, options, db).await;
    if candidates
        .iter()
        .all(|candidate| candidate.explicit_media_id.is_some())
    {
        return candidates;
    }

    let Ok(entries) = db.get_metadata_cache_entries().await else {
        return candidates;
    };
    let index = build_metadata_cache_index(entries, options.content_type);
    if index.is_empty() {
        return candidates;
    }

    candidates
        .into_iter()
        .map(|mut candidate| {
            if candidate.explicit_media_id.is_some() {
                return candidate;
            }
            if let Some(resolution) = lookup_cached_resolution(&candidate, &index) {
                apply_cached_import_resolution(&mut candidate, resolution);
            }
            candidate
        })
        .collect()
}

async fn resolve_candidates_from_import_cache(
    candidates: Vec<ImportCandidateReport>,
    options: &ImportOptions,
    db: &Database,
) -> Vec<ImportCandidateReport> {
    let mut resolved = Vec::with_capacity(candidates.len());
    for mut candidate in candidates {
        if candidate.explicit_media_id.is_none() {
            match lookup_import_resolution_cache(db, &candidate, options.content_type).await {
                Ok(Some(resolution)) => apply_cached_import_resolution(&mut candidate, resolution),
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    "Import resolution cache lookup failed for {}: {}",
                    candidate.title_hint,
                    err
                ),
            }
        }
        resolved.push(candidate);
    }
    resolved
}

fn apply_cached_import_resolution(
    candidate: &mut ImportCandidateReport,
    resolution: CachedImportResolution,
) {
    candidate.explicit_media_id = Some(resolution.media_id);
    candidate.resolved_title = Some(resolution.title);
    candidate.resolved_year = resolution.year;
    candidate.resolution_source = ImportResolutionSource::CachedMetadata;
    candidate.confidence = ImportConfidence::High;
    candidate.decision = ImportDecision::Preview;
}

async fn resolve_candidates_from_remote(
    mut candidates: Vec<ImportCandidateReport>,
    options: &ImportOptions,
    db: Option<&Database>,
    tmdb: Option<&TmdbClient>,
    mut tvdb: Option<&mut TvdbClient>,
    warnings: &mut Vec<String>,
) -> Vec<ImportCandidateReport> {
    if options.offline || options.lookup_mode != ImportLookupMode::Remote {
        return candidates;
    }
    let has_usable_lookup = match options.content_type {
        ImportContentType::Movie | ImportContentType::Auto => tmdb.is_some(),
        ImportContentType::Tv | ImportContentType::Anime => tmdb.is_some() || tvdb.is_some(),
    };
    if !has_usable_lookup {
        warnings.push(
            "Remote lookup requested but matching TMDB/TVDB credentials/config were not available."
                .to_string(),
        );
        return candidates;
    }
    if options.max_lookups == 0 {
        warnings.push(
            "Remote lookup requested with --max-lookups 0; no remote lookups were made."
                .to_string(),
        );
        return candidates;
    }

    let mut used = 0usize;
    for candidate in &mut candidates {
        if candidate.explicit_media_id.is_some() || used >= options.max_lookups {
            continue;
        }
        used += 1;
        match remote_lookup_candidate(tmdb, tvdb.as_deref_mut(), candidate, options.content_type)
            .await
        {
            Ok(RemoteLookupOutcome::Resolved(resolution)) => {
                if let Some(db) = db {
                    if let Err(err) =
                        cache_remote_resolution(db, &resolution, candidate, options.content_type)
                            .await
                    {
                        warnings.push(format!(
                            "Remote lookup cache write failed for {}: {}",
                            candidate.title_hint, err
                        ));
                    }
                }
                candidate.explicit_media_id = Some(resolution.media_id.clone());
                candidate.resolved_title = Some(resolution.metadata.title.clone());
                candidate.resolved_year = resolution.metadata.year;
                candidate.resolution_source = resolution.resolution_source;
                candidate.confidence = ImportConfidence::High;
                candidate.decision = ImportDecision::Preview;
            }
            Ok(RemoteLookupOutcome::Ambiguous) => {
                candidate.confidence = ImportConfidence::Ambiguous;
                candidate.decision = ImportDecision::NeedsReview;
                candidate.action = ImportWriteAction::Skip;
                candidate.reason = Some("remote_lookup_ambiguous".to_string());
            }
            Ok(RemoteLookupOutcome::NoMatch) => {}
            Err(err) => {
                if warnings.len() < 20 {
                    warnings.push(format!(
                        "Remote lookup failed for {}: {}",
                        candidate.title_hint, err
                    ));
                }
            }
        }
    }

    let unresolved_after_cap = candidates
        .iter()
        .filter(|candidate| candidate.explicit_media_id.is_none())
        .count();
    if used >= options.max_lookups && unresolved_after_cap > 0 {
        warnings.push(format!(
            "Remote lookup cap reached at {} lookup(s); {} unresolved candidate(s) were left for cache/explicit IDs or a later run.",
            options.max_lookups, unresolved_after_cap
        ));
    }

    candidates
}

#[derive(Debug, Clone)]
struct RemoteImportResolution {
    media_id: String,
    resolution_source: ImportResolutionSource,
    cache_key: String,
    metadata: ContentMetadata,
}

#[derive(Debug, Clone)]
enum RemoteLookupOutcome {
    Resolved(RemoteImportResolution),
    Ambiguous,
    NoMatch,
}

async fn remote_lookup_candidate(
    tmdb: Option<&TmdbClient>,
    tvdb: Option<&mut TvdbClient>,
    candidate: &ImportCandidateReport,
    content_type: ImportContentType,
) -> Result<RemoteLookupOutcome> {
    let query = candidate.title_hint.trim();
    if query.is_empty() {
        return Ok(RemoteLookupOutcome::NoMatch);
    }

    match content_type {
        ImportContentType::Movie | ImportContentType::Auto => {
            let Some(tmdb) = tmdb else {
                return Ok(RemoteLookupOutcome::NoMatch);
            };
            let matches = tmdb.search_movie(query, candidate.year_hint).await?;
            match select_tmdb_match(candidate, &matches) {
                LookupMatchSelection::Unique(id) => Ok(matches
                    .iter()
                    .find(|m| m.id == id)
                    .map(|selected| {
                        RemoteLookupOutcome::Resolved(remote_resolution_from_tmdb_match(
                            "movie",
                            selected,
                            ImportResolutionSource::TmdbLookup,
                        ))
                    })
                    .unwrap_or(RemoteLookupOutcome::NoMatch)),
                LookupMatchSelection::Ambiguous => Ok(RemoteLookupOutcome::Ambiguous),
                LookupMatchSelection::None => Ok(RemoteLookupOutcome::NoMatch),
            }
        }
        ImportContentType::Tv | ImportContentType::Anime => {
            if let Some(tvdb) = tvdb {
                let matches = tvdb.search_series(query, candidate.year_hint).await?;
                match select_tvdb_match(candidate, &matches) {
                    LookupMatchSelection::Unique(id) => {
                        if let Some(selected) = matches.iter().find(|m| m.id == id) {
                            return Ok(RemoteLookupOutcome::Resolved(
                                remote_resolution_from_tvdb_match(selected),
                            ));
                        }
                    }
                    LookupMatchSelection::Ambiguous => return Ok(RemoteLookupOutcome::Ambiguous),
                    LookupMatchSelection::None => {}
                }
            }
            let Some(tmdb) = tmdb else {
                return Ok(RemoteLookupOutcome::NoMatch);
            };
            let matches = tmdb.search_tv(query, candidate.year_hint).await?;
            match select_tmdb_match(candidate, &matches) {
                LookupMatchSelection::Unique(id) => Ok(matches
                    .iter()
                    .find(|m| m.id == id)
                    .map(|selected| {
                        RemoteLookupOutcome::Resolved(remote_resolution_from_tmdb_match(
                            "tv",
                            selected,
                            ImportResolutionSource::TmdbLookup,
                        ))
                    })
                    .unwrap_or(RemoteLookupOutcome::NoMatch)),
                LookupMatchSelection::Ambiguous => Ok(RemoteLookupOutcome::Ambiguous),
                LookupMatchSelection::None => Ok(RemoteLookupOutcome::NoMatch),
            }
        }
    }
}

fn remote_resolution_from_tmdb_match(
    media_kind: &str,
    selected: &TmdbSearchMatch,
    resolution_source: ImportResolutionSource,
) -> RemoteImportResolution {
    RemoteImportResolution {
        media_id: format!("tmdb-{}", selected.id),
        resolution_source,
        cache_key: format!("tmdb:{media_kind}:{}", selected.id),
        metadata: ContentMetadata {
            title: selected.title.clone(),
            aliases: Vec::new(),
            year: selected.year,
            seasons: Vec::new(),
        },
    }
}

fn remote_resolution_from_tvdb_match(selected: &TvdbSearchMatch) -> RemoteImportResolution {
    RemoteImportResolution {
        media_id: format!("tvdb-{}", selected.id),
        resolution_source: ImportResolutionSource::TvdbLookup,
        cache_key: format!("tvdb:series:{}", selected.id),
        metadata: ContentMetadata {
            title: selected.title.clone(),
            aliases: Vec::new(),
            year: selected.year,
            seasons: Vec::new(),
        },
    }
}

async fn cache_remote_resolution(
    db: &Database,
    resolution: &RemoteImportResolution,
    candidate: &ImportCandidateReport,
    content_type: ImportContentType,
) -> Result<()> {
    let json = serde_json::to_string(&resolution.metadata)
        .context("failed to serialize import lookup cache metadata")?;
    db.set_cached(&resolution.cache_key, &json, IMPORT_REMOTE_CACHE_TTL_HOURS)
        .await?;
    cache_import_resolution(db, resolution, candidate, content_type).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LookupMatchSelection {
    Unique(u64),
    Ambiguous,
    None,
}

fn select_tmdb_match(
    candidate: &ImportCandidateReport,
    matches: &[TmdbSearchMatch],
) -> LookupMatchSelection {
    let mut exact = exact_tmdb_matches(candidate, matches);
    exact.sort_by_key(|m| m.id);
    exact.dedup_by_key(|m| m.id);
    match exact.len() {
        0 => LookupMatchSelection::None,
        1 => LookupMatchSelection::Unique(exact[0].id),
        _ => LookupMatchSelection::Ambiguous,
    }
}

fn exact_tmdb_matches<'a>(
    candidate: &ImportCandidateReport,
    matches: &'a [TmdbSearchMatch],
) -> Vec<&'a TmdbSearchMatch> {
    let title_key = normalize_lookup_key(&candidate.title_hint);
    matches
        .iter()
        .filter(|m| {
            normalize_lookup_key(&m.title) == title_key
                && match (candidate.year_hint, m.year) {
                    (Some(candidate_year), Some(match_year)) => candidate_year == match_year,
                    _ => true,
                }
        })
        .collect()
}

fn select_tvdb_match(
    candidate: &ImportCandidateReport,
    matches: &[TvdbSearchMatch],
) -> LookupMatchSelection {
    let mut exact = exact_tvdb_matches(candidate, matches);
    exact.sort_by_key(|m| m.id);
    exact.dedup_by_key(|m| m.id);
    match exact.len() {
        0 => LookupMatchSelection::None,
        1 => LookupMatchSelection::Unique(exact[0].id),
        _ => LookupMatchSelection::Ambiguous,
    }
}

fn exact_tvdb_matches<'a>(
    candidate: &ImportCandidateReport,
    matches: &'a [TvdbSearchMatch],
) -> Vec<&'a TvdbSearchMatch> {
    let title_key = normalize_lookup_key(&candidate.title_hint);
    matches
        .iter()
        .filter(|m| {
            normalize_lookup_key(&m.title) == title_key
                && match (candidate.year_hint, m.year) {
                    (Some(candidate_year), Some(match_year)) => candidate_year == match_year,
                    _ => true,
                }
        })
        .collect()
}

#[derive(Debug, Clone)]
struct CachedImportMetadata {
    media_id: String,
    title: String,
    title_keys: Vec<String>,
    year: Option<u32>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct CachedImportResolution {
    media_id: String,
    title: String,
    year: Option<u32>,
}

async fn lookup_import_resolution_cache(
    db: &Database,
    candidate: &ImportCandidateReport,
    content_type: ImportContentType,
) -> Result<Option<CachedImportResolution>> {
    let Some(key) =
        import_resolution_cache_key(content_type, &candidate.title_hint, candidate.year_hint)
    else {
        return Ok(None);
    };
    let Some(json) = db.get_cached(&key).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_str::<CachedImportResolution>(&json).ok())
}

async fn cache_import_resolution(
    db: &Database,
    resolution: &RemoteImportResolution,
    candidate: &ImportCandidateReport,
    content_type: ImportContentType,
) -> Result<()> {
    let cached = CachedImportResolution {
        media_id: resolution.media_id.clone(),
        title: resolution.metadata.title.clone(),
        year: resolution.metadata.year,
    };
    let json = serde_json::to_string(&cached)?;
    let mut keys = Vec::new();
    if let Some(key) =
        import_resolution_cache_key(content_type, &candidate.title_hint, candidate.year_hint)
    {
        keys.push(key);
    }
    if let Some(key) = import_resolution_cache_key(
        content_type,
        &resolution.metadata.title,
        resolution.metadata.year,
    ) {
        keys.push(key);
    }
    keys.sort();
    keys.dedup();
    for key in keys {
        db.set_cached(&key, &json, IMPORT_RESOLUTION_CACHE_TTL_HOURS)
            .await?;
    }
    Ok(())
}

fn import_resolution_cache_key(
    content_type: ImportContentType,
    title: &str,
    year: Option<u32>,
) -> Option<String> {
    let title_key = normalize_lookup_key(title);
    if title_key.is_empty() {
        return None;
    }
    let kind = match content_type {
        ImportContentType::Movie => "movie",
        ImportContentType::Tv => "tv",
        ImportContentType::Anime => "anime",
        ImportContentType::Auto => "auto",
    };
    let year = year
        .map(|year| year.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    Some(format!("import:resolve:{kind}:{title_key}:{year}"))
}

fn build_metadata_cache_index(
    entries: Vec<(String, String)>,
    content_type: ImportContentType,
) -> Vec<CachedImportMetadata> {
    entries
        .into_iter()
        .filter_map(|(cache_key, json)| {
            let media_id = media_id_from_cache_key(&cache_key, content_type)?;
            let metadata = serde_json::from_str::<ContentMetadata>(&json).ok()?;
            let mut title_keys = Vec::new();
            title_keys.push(normalize_lookup_key(&metadata.title));
            title_keys.extend(
                metadata
                    .aliases
                    .iter()
                    .map(|alias| normalize_lookup_key(alias)),
            );
            title_keys.retain(|key| !key.is_empty());
            title_keys.sort();
            title_keys.dedup();
            if title_keys.is_empty() {
                return None;
            }
            Some(CachedImportMetadata {
                media_id,
                title: metadata.title,
                title_keys,
                year: metadata.year,
            })
        })
        .collect()
}

fn media_id_from_cache_key(cache_key: &str, content_type: ImportContentType) -> Option<String> {
    if matches!(
        content_type,
        ImportContentType::Movie | ImportContentType::Auto
    ) && cache_key.starts_with("tmdb:movie:")
    {
        return cache_key
            .strip_prefix("tmdb:movie:")
            .filter(|id| id.chars().all(|c| c.is_ascii_digit()))
            .map(|id| format!("tmdb-{id}"));
    }
    if matches!(
        content_type,
        ImportContentType::Tv | ImportContentType::Anime | ImportContentType::Auto
    ) && cache_key.starts_with("tvdb:series:")
    {
        return cache_key
            .strip_prefix("tvdb:series:")
            .filter(|id| id.chars().all(|c| c.is_ascii_digit()))
            .map(|id| format!("tvdb-{id}"));
    }
    if matches!(
        content_type,
        ImportContentType::Tv | ImportContentType::Anime | ImportContentType::Auto
    ) && cache_key.starts_with("tmdb:tv:")
    {
        return cache_key
            .strip_prefix("tmdb:tv:")
            .filter(|id| id.chars().all(|c| c.is_ascii_digit()))
            .map(|id| format!("tmdb-{id}"));
    }
    None
}

fn lookup_cached_resolution(
    candidate: &ImportCandidateReport,
    index: &[CachedImportMetadata],
) -> Option<CachedImportResolution> {
    let title_key = normalize_lookup_key(&candidate.title_hint);
    if title_key.is_empty() {
        return None;
    }

    let mut matches = index
        .iter()
        .filter(|entry| {
            entry.title_keys.iter().any(|key| key == &title_key)
                && match (candidate.year_hint, entry.year) {
                    (Some(candidate_year), Some(entry_year)) => candidate_year == entry_year,
                    _ => true,
                }
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| a.media_id.cmp(&b.media_id));
    matches.dedup_by(|a, b| a.media_id == b.media_id);

    if matches.len() == 1 {
        Some(CachedImportResolution {
            media_id: matches[0].media_id.clone(),
            title: matches[0].title.clone(),
            year: matches[0].year,
        })
    } else {
        None
    }
}

fn normalize_lookup_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[derive(Debug, Default, serde::Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    color_transfer: Option<String>,
    #[serde(default)]
    side_data_list: Vec<FfprobeSideData>,
    tags: Option<FfprobeTags>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FfprobeSideData {
    side_data_type: Option<String>,
    dv_profile: Option<u32>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FfprobeTags {
    language: Option<String>,
}

async fn apply_probe_metadata(
    candidates: &mut [ImportCandidateReport],
    options: &ImportOptions,
    db: Option<&Database>,
    warnings: &mut Vec<String>,
) {
    if options.metadata_mode == ImportMetadataMode::Fast {
        return;
    }
    let mut run_cache: HashMap<String, ProbeMetadata> = HashMap::new();
    for candidate in candidates {
        let Some(video_path) = first_video_path_for_candidate(candidate) else {
            if options.metadata_mode == ImportMetadataMode::Strict {
                candidate.decision = ImportDecision::Skipped;
                candidate.action = ImportWriteAction::Skip;
                candidate.reason = Some("strict_probe_no_video_file".to_string());
            }
            continue;
        };

        match probe_media_file_cached(&video_path, options.probe_tool, db, &mut run_cache).await {
            Ok(metadata) => {
                candidate.probed_resolution = metadata.resolution;
                candidate.video_codec = metadata.video_codec;
                candidate.hdr_formats = metadata.hdr_formats;
                candidate.audio_languages = metadata.audio_languages;
                candidate.subtitle_languages = metadata.subtitle_languages;
            }
            Err(err) => {
                if warnings.len() < 20 {
                    warnings.push(format!("Could not probe {}: {}", video_path.display(), err));
                }
                if options.metadata_mode == ImportMetadataMode::Strict {
                    candidate.decision = ImportDecision::Skipped;
                    candidate.action = ImportWriteAction::Skip;
                    candidate.reason = Some("strict_probe_failed".to_string());
                }
            }
        }
    }
}

async fn probe_media_file_cached(
    path: &Path,
    probe_tool: ImportProbeTool,
    db: Option<&Database>,
    run_cache: &mut HashMap<String, ProbeMetadata>,
) -> Result<ProbeMetadata> {
    let cache_key = probe_cache_key(path, probe_tool)?;
    if let Some(metadata) = run_cache.get(&cache_key) {
        return Ok(metadata.clone());
    }
    if let Some(db) = db {
        if let Some(json) = db.get_cached(&cache_key).await? {
            if let Ok(metadata) = serde_json::from_str::<ProbeMetadata>(&json) {
                run_cache.insert(cache_key, metadata.clone());
                return Ok(metadata);
            }
        }
    }

    let metadata = probe_media_file(path, probe_tool)?;
    if let Some(db) = db {
        let json = serde_json::to_string(&metadata)?;
        if let Err(err) = db
            .set_cached(&cache_key, &json, IMPORT_PROBE_CACHE_TTL_HOURS)
            .await
        {
            tracing::warn!(
                "Import probe cache write failed for {}: {}",
                path.display(),
                err
            );
        }
    }
    run_cache.insert(cache_key, metadata.clone());
    Ok(metadata)
}

fn probe_cache_key(path: &Path, probe_tool: ImportProbeTool) -> Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("failed to stat probe source {}", path.display()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    let path_hash = hasher.finish();
    Ok(format!(
        "import:probe:{:?}:{:016x}:{}:{}",
        probe_tool,
        path_hash,
        metadata.len(),
        modified
    ))
}

fn probe_media_file(path: &Path, probe_tool: ImportProbeTool) -> Result<ProbeMetadata> {
    match probe_tool {
        ImportProbeTool::Ffprobe => probe_with_ffprobe(path),
        ImportProbeTool::Mediainfo => probe_with_mediainfo(path),
        ImportProbeTool::Auto => probe_with_ffprobe(path).or_else(|_| probe_with_mediainfo(path)),
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
struct ProbeMetadata {
    resolution: Option<String>,
    video_codec: Option<String>,
    hdr_formats: Vec<String>,
    audio_languages: Vec<String>,
    subtitle_languages: Vec<String>,
}

fn first_video_path_for_candidate(candidate: &ImportCandidateReport) -> Option<PathBuf> {
    match candidate.kind {
        ImportCandidateKind::File => Some(candidate.source_path.clone()),
        ImportCandidateKind::Folder => WalkDir::new(&candidate.source_path)
            .follow_links(false)
            .max_depth(4)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file() && is_video_file(entry.path()))
            .map(|entry| entry.path().to_path_buf())
            .min(),
    }
}

fn probe_with_ffprobe(path: &Path) -> Result<ProbeMetadata> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-probesize",
            "32768",
            "-analyzeduration",
            "1000000",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,color_transfer:stream_tags=language:stream_side_data=side_data_type,dv_profile",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .with_context(|| "failed to run ffprobe")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "ffprobe exited with {} ({})",
            output.status,
            stderr.lines().next().unwrap_or("no stderr")
        );
    }

    let parsed: FfprobeOutput =
        serde_json::from_slice(&output.stdout).context("failed to parse ffprobe JSON")?;
    let mut metadata = ProbeMetadata::default();
    for stream in parsed.streams {
        match stream.codec_type.as_deref() {
            Some("video") if metadata.resolution.is_none() => {
                metadata.resolution = resolution_label(stream.width, stream.height);
                metadata.video_codec = stream.codec_name.map(normalize_codec_name);
                if let Some(hdr) = hdr_label_from_ffprobe_transfer(stream.color_transfer.as_deref())
                {
                    push_unique(&mut metadata.hdr_formats, hdr);
                }
                for hdr in hdr_labels_from_ffprobe_side_data(&stream.side_data_list) {
                    push_unique(&mut metadata.hdr_formats, hdr);
                }
            }
            Some("audio") => push_language(&mut metadata.audio_languages, stream.tags),
            Some("subtitle") => push_language(&mut metadata.subtitle_languages, stream.tags),
            _ => {}
        }
    }
    Ok(metadata)
}

#[derive(Debug, Default, serde::Deserialize)]
struct MediainfoOutput {
    media: Option<MediainfoMedia>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct MediainfoMedia {
    #[serde(default)]
    track: Vec<MediainfoTrack>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct MediainfoTrack {
    #[serde(rename = "@type")]
    track_type: Option<String>,
    #[serde(rename = "Width")]
    width: Option<MediainfoNumber>,
    #[serde(rename = "Height")]
    height: Option<MediainfoNumber>,
    #[serde(rename = "Language")]
    language: Option<String>,
    #[serde(rename = "Format")]
    format: Option<String>,
    #[serde(rename = "HDR_Format")]
    hdr_format: Option<String>,
    #[serde(rename = "HDR_Format_Profile")]
    hdr_format_profile: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum MediainfoNumber {
    Number(u32),
    String(String),
}

impl MediainfoNumber {
    fn as_u32(&self) -> Option<u32> {
        match self {
            MediainfoNumber::Number(value) => Some(*value),
            MediainfoNumber::String(value) => value
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok(),
        }
    }
}

fn probe_with_mediainfo(path: &Path) -> Result<ProbeMetadata> {
    let output = std::process::Command::new("mediainfo")
        .arg("--Output=JSON")
        .arg(path)
        .output()
        .with_context(|| "failed to run mediainfo")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "mediainfo exited with {} ({})",
            output.status,
            stderr.lines().next().unwrap_or("no stderr")
        );
    }
    let parsed: MediainfoOutput =
        serde_json::from_slice(&output.stdout).context("failed to parse mediainfo JSON")?;
    Ok(probe_metadata_from_mediainfo(parsed))
}

fn probe_metadata_from_mediainfo(parsed: MediainfoOutput) -> ProbeMetadata {
    let mut metadata = ProbeMetadata::default();
    let tracks = parsed.media.map(|media| media.track).unwrap_or_default();
    for track in tracks {
        match track.track_type.as_deref() {
            Some("Video") if metadata.resolution.is_none() => {
                metadata.resolution = resolution_label(
                    track.width.as_ref().and_then(MediainfoNumber::as_u32),
                    track.height.as_ref().and_then(MediainfoNumber::as_u32),
                );
                metadata.video_codec = track.format.map(normalize_codec_name);
                if let Some(hdr) = hdr_label_from_mediainfo(track.hdr_format.as_deref()) {
                    push_unique(&mut metadata.hdr_formats, hdr);
                }
                for hdr in hdr_labels_from_mediainfo_profile(track.hdr_format_profile.as_deref()) {
                    push_unique(&mut metadata.hdr_formats, hdr);
                }
            }
            Some("Audio") => push_language_value(&mut metadata.audio_languages, track.language),
            Some("Text") | Some("Menu") => {
                push_language_value(&mut metadata.subtitle_languages, track.language)
            }
            _ => {}
        }
    }
    metadata
}

fn resolution_label(width: Option<u32>, height: Option<u32>) -> Option<String> {
    let h = height?;
    let label = if h >= 2000 {
        "2160p"
    } else if h >= 1000 {
        "1080p"
    } else if h >= 700 {
        "720p"
    } else if h >= 470 {
        "480p"
    } else {
        return width.map(|w| format!("{w}x{h}"));
    };
    Some(label.to_string())
}

fn push_language(languages: &mut Vec<String>, tags: Option<FfprobeTags>) {
    let Some(language) = tags.and_then(|tags| tags.language) else {
        return;
    };
    push_language_value(languages, Some(language));
}

fn push_language_value(languages: &mut Vec<String>, language: Option<String>) {
    let Some(language) = language else {
        return;
    };
    let language = language.trim().to_ascii_lowercase();
    push_unique(languages, language);
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

fn normalize_codec_name(value: String) -> String {
    normalize_rule_token(&value)
}

fn hdr_label_from_ffprobe_transfer(value: Option<&str>) -> Option<String> {
    match value.map(normalize_rule_token).as_deref() {
        Some("smpte2084") => Some("hdr10".to_string()),
        Some("aribstdb67") => Some("hlg".to_string()),
        _ => None,
    }
}

fn hdr_labels_from_ffprobe_side_data(side_data: &[FfprobeSideData]) -> Vec<String> {
    let mut labels = Vec::new();
    for item in side_data {
        let Some(side_data_type) = item.side_data_type.as_deref().map(normalize_rule_token) else {
            continue;
        };
        if !side_data_type.contains("dovi") && !side_data_type.contains("dolbyvision") {
            continue;
        }
        push_unique(&mut labels, "dv".to_string());
        if let Some(profile) = item.dv_profile {
            push_unique(&mut labels, format!("dv-p{profile}"));
        }
    }
    labels
}

fn hdr_label_from_mediainfo(value: Option<&str>) -> Option<String> {
    let value = normalize_rule_token(value?);
    if value.contains("dolbyvision") {
        Some("dv".to_string())
    } else if value.contains("hdr10") {
        Some("hdr10".to_string())
    } else if value.contains("hlg") {
        Some("hlg".to_string())
    } else if value.contains("smpte2084") {
        Some("hdr10".to_string())
    } else {
        None
    }
}

fn hdr_labels_from_mediainfo_profile(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let normalized = normalize_rule_token(value);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut labels = Vec::new();
    if normalized.contains("dvhe") || normalized.contains("dolbyvision") {
        push_unique(&mut labels, "dv".to_string());
    }
    if let Some(index) = normalized.find("dvhe") {
        let digits = normalized[index + 4..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        if digits.len() >= 2 {
            let profile = digits[..2].trim_start_matches('0');
            if !profile.is_empty() {
                push_unique(&mut labels, format!("dv-p{profile}"));
            }
        }
    }
    labels
}

fn destination_for_candidate(
    candidate: &ImportCandidateReport,
    options: &ImportOptions,
    rules_plan: Option<&ImportRulesPlan>,
) -> Option<PathBuf> {
    if let Some(destination) = destination_from_rules(options.content_type, candidate, rules_plan) {
        return Some(destination);
    }

    match options.content_type {
        ImportContentType::Movie => options
            .movie_destination
            .clone()
            .or_else(|| options.destination.clone()),
        ImportContentType::Tv => options
            .tv_destination
            .clone()
            .or_else(|| options.destination.clone()),
        ImportContentType::Anime => options
            .anime_destination
            .clone()
            .or_else(|| options.destination.clone()),
        ImportContentType::Auto => destination_for_auto_candidate(candidate, options, rules_plan)
            .or_else(|| options.destination.clone()),
    }
}

fn destination_from_rules(
    content_type: ImportContentType,
    candidate: &ImportCandidateReport,
    rules_plan: Option<&ImportRulesPlan>,
) -> Option<PathBuf> {
    let rules = rules_plan?;
    match content_type {
        ImportContentType::Movie => rules.rules.destinations.movies.destination_for(candidate),
        ImportContentType::Tv => rules.rules.destinations.tv.destination_for(candidate),
        ImportContentType::Anime => rules.rules.destinations.anime.destination_for(candidate),
        ImportContentType::Auto => None,
    }
}

fn destination_for_auto_candidate(
    candidate: &ImportCandidateReport,
    options: &ImportOptions,
    rules_plan: Option<&ImportRulesPlan>,
) -> Option<PathBuf> {
    let id = candidate.explicit_media_id.as_deref().unwrap_or_default();
    if id.starts_with("tvdb-") {
        return rules_plan
            .and_then(|rules| rules.rules.destinations.tv.destination_for(candidate))
            .or_else(|| options.tv_destination.clone());
    }
    if id.starts_with("tmdb-") || id.starts_with("imdb-") {
        return rules_plan
            .and_then(|rules| rules.rules.destinations.movies.destination_for(candidate))
            .or_else(|| options.movie_destination.clone());
    }

    let path_hint = candidate.source_path.to_string_lossy().to_ascii_lowercase();
    if path_hint.contains("anime") {
        return rules_plan
            .and_then(|rules| rules.rules.destinations.anime.destination_for(candidate))
            .or_else(|| options.anime_destination.clone());
    }

    None
}

fn target_name_with_optional_id(candidate: &ImportCandidateReport, source_name: &str) -> String {
    let mut name = sanitize_target_name(source_name);
    let Some(media_id) = candidate.explicit_media_id.as_deref() else {
        return name;
    };
    if let Some(canonical) = canonical_target_name(candidate, media_id, source_name) {
        return canonical;
    }
    if explicit_id_regex().is_match(&name) {
        return name;
    }

    if candidate.kind == ImportCandidateKind::File {
        let path = Path::new(&name);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(&name);
        let extension = path.extension().and_then(|ext| ext.to_str());
        name = match extension {
            Some(extension) => format!("{stem} {{{media_id}}}.{extension}"),
            None => format!("{stem} {{{media_id}}}"),
        };
    } else {
        name = format!("{name} {{{media_id}}}");
    }
    name
}

fn target_folder_name_with_optional_id(
    candidate: &ImportCandidateReport,
    source_name: &str,
) -> String {
    let folder_name = if candidate.kind == ImportCandidateKind::File {
        Path::new(source_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(source_name)
    } else {
        source_name
    };
    let mut folder_candidate = candidate.clone();
    folder_candidate.kind = ImportCandidateKind::Folder;
    target_name_with_optional_id(&folder_candidate, folder_name)
}

fn canonical_target_name(
    candidate: &ImportCandidateReport,
    media_id: &str,
    source_name: &str,
) -> Option<String> {
    let title = candidate.resolved_title.as_deref()?.trim();
    if title.is_empty() {
        return None;
    }
    let mut base = match candidate.resolved_year {
        Some(year) => format!("{title} ({year}) {{{media_id}}}"),
        None => format!("{title} {{{media_id}}}"),
    };
    base = sanitize_target_name(&base);
    if candidate.kind == ImportCandidateKind::File {
        let extension = Path::new(source_name)
            .extension()
            .and_then(|ext| ext.to_str());
        if let Some(extension) = extension {
            return Some(format!("{base}.{extension}"));
        }
    }
    Some(base)
}

fn sanitize_target_name(name: &str) -> String {
    let cleaned = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        "Imported item".to_string()
    } else {
        cleaned
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetState {
    Missing,
    MatchingSymlink,
    Symlink,
    Directory,
    NonSymlink,
}

fn classify_target(target_path: &Path) -> TargetState {
    match std::fs::symlink_metadata(target_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => TargetState::Symlink,
        Ok(metadata) if metadata.file_type().is_dir() => TargetState::Directory,
        Ok(_) => TargetState::NonSymlink,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => TargetState::Missing,
        Err(_) => TargetState::NonSymlink,
    }
}

fn classify_import_target(
    target_path: &Path,
    source_path: &Path,
    folders_only: bool,
) -> TargetState {
    if folders_only {
        return classify_target(target_path);
    }
    classify_target_for_source(target_path, source_path)
}

fn classify_target_for_source(target_path: &Path, source_path: &Path) -> TargetState {
    match classify_target(target_path) {
        TargetState::Symlink if symlink_points_to(target_path, source_path) => {
            TargetState::MatchingSymlink
        }
        state => state,
    }
}

fn symlink_points_to(target_path: &Path, source_path: &Path) -> bool {
    std::fs::read_link(target_path)
        .map(|target| resolve_symlink_target(target_path, &target) == source_path)
        .unwrap_or(false)
}

fn resolve_symlink_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

fn import_requires_confirmation(mode: ImportMode, yes: bool) -> bool {
    !yes && matches!(mode, ImportMode::Safe | ImportMode::Aggressive)
}

fn confirm_import(report: &ImportReport, mode: ImportMode) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "{:?} import requires --yes when stdin is not interactive",
            mode
        );
    }

    let planned = report
        .candidates
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.action,
                ImportWriteAction::Create | ImportWriteAction::Update
            )
        })
        .count();
    let label = match mode {
        ImportMode::Safe => "Safe import",
        ImportMode::Aggressive => "Aggressive import",
        ImportMode::Preview => return Ok(()),
    };
    eprintln!(
        "{label} will write {planned} target(s) from {}.",
        report.source.display()
    );
    if mode == ImportMode::Aggressive {
        eprintln!("This mode may create incorrect targets when provider filenames are ambiguous.");
    }
    eprintln!("Continue and apply these changes? [y/N]");
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("failed to read import confirmation")?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        anyhow::bail!("{:?} import aborted by operator", mode)
    }
}

pub(crate) fn apply_import_plan(report: &mut ImportReport, options: &ImportOptions) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = options;
        anyhow::bail!("live import symlink writes are only supported on Unix targets");
    }

    #[cfg(unix)]
    {
        for candidate in &mut report.candidates {
            match candidate.action {
                ImportWriteAction::Create => {
                    let Some(target_path) = candidate.target_path.clone() else {
                        candidate.decision = ImportDecision::Skipped;
                        candidate.reason = Some("missing_target_path".to_string());
                        continue;
                    };
                    let write_result = if options.folders_only {
                        create_import_folder(&target_path)
                    } else {
                        create_import_symlink(&candidate.source_path, &target_path)
                    };
                    if let Err(err) = write_result {
                        candidate.decision = ImportDecision::Skipped;
                        candidate.action = ImportWriteAction::Skip;
                        candidate.reason = Some(format!("write_failed: {err}"));
                    } else {
                        candidate.decision = ImportDecision::Created;
                        candidate.reason = None;
                    }
                }
                ImportWriteAction::Update if options.mode == ImportMode::Aggressive => {
                    let Some(target_path) = candidate.target_path.clone() else {
                        candidate.decision = ImportDecision::Skipped;
                        candidate.reason = Some("missing_target_path".to_string());
                        continue;
                    };
                    if let Err(err) = replace_import_symlink(&candidate.source_path, &target_path) {
                        candidate.decision = ImportDecision::Skipped;
                        candidate.action = ImportWriteAction::Skip;
                        candidate.reason = Some(format!("write_failed: {err}"));
                    } else {
                        candidate.decision = ImportDecision::Updated;
                        candidate.reason = None;
                    }
                }
                ImportWriteAction::Update => {
                    candidate.decision = ImportDecision::Skipped;
                    candidate.action = ImportWriteAction::Skip;
                    candidate.reason = Some("update_requires_aggressive_mode".to_string());
                }
                ImportWriteAction::None | ImportWriteAction::Skip => {}
            }
        }
        report.summary = summarize_candidates(
            &report.candidates,
            report.content_type,
            &report.destinations,
        );
        Ok(())
    }
}

pub(crate) async fn backfill_import_links(
    report: &mut ImportReport,
    db: &Database,
    content_type: ImportContentType,
) {
    for candidate in &report.candidates {
        if !matches!(
            candidate.decision,
            ImportDecision::Created | ImportDecision::Updated
        ) {
            continue;
        }
        let Some(target_path) = candidate.target_path.clone() else {
            continue;
        };
        let Some(media_id) = candidate.explicit_media_id.clone() else {
            continue;
        };
        let Some(media_type) = media_type_for_import_candidate(candidate, content_type) else {
            continue;
        };
        let record = LinkRecord {
            id: None,
            source_path: candidate.source_path.clone(),
            target_path: target_path.clone(),
            media_id,
            media_type,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };
        if let Err(err) = db.insert_link(&record).await {
            report.warnings.push(format!(
                "Failed to backfill DB link record for {}: {}",
                target_path.display(),
                err
            ));
        }
    }
}

fn media_type_for_import_candidate(
    candidate: &ImportCandidateReport,
    content_type: ImportContentType,
) -> Option<MediaType> {
    match content_type {
        ImportContentType::Movie => Some(MediaType::Movie),
        ImportContentType::Tv | ImportContentType::Anime => Some(MediaType::Tv),
        ImportContentType::Auto => {
            let id = candidate.explicit_media_id.as_deref()?;
            if id.starts_with("tvdb-") {
                Some(MediaType::Tv)
            } else if id.starts_with("tmdb-") || id.starts_with("imdb-") {
                Some(MediaType::Movie)
            } else {
                None
            }
        }
    }
}

#[cfg(unix)]
fn create_import_symlink(source_path: &Path, target_path: &Path) -> Result<()> {
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create target parent {}", parent.display()))?;
    }
    match classify_target(target_path) {
        TargetState::Missing => {}
        TargetState::MatchingSymlink => anyhow::bail!("target symlink already exists"),
        TargetState::Symlink => anyhow::bail!("target symlink already exists"),
        TargetState::Directory => anyhow::bail!("target exists and is a directory"),
        TargetState::NonSymlink => anyhow::bail!("target exists and is not a symlink"),
    }

    let temp_path = temp_symlink_path(target_path);
    let _ = std::fs::remove_file(&temp_path);
    std::os::unix::fs::symlink(source_path, &temp_path)
        .with_context(|| format!("failed to create temp symlink {}", temp_path.display()))?;
    std::fs::rename(&temp_path, target_path)
        .with_context(|| format!("failed to move temp symlink into {}", target_path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn create_import_folder(target_path: &Path) -> Result<()> {
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create target parent {}", parent.display()))?;
    }
    match classify_target(target_path) {
        TargetState::Missing => {
            std::fs::create_dir(target_path).with_context(|| {
                format!("failed to create import folder {}", target_path.display())
            })?;
            Ok(())
        }
        TargetState::Directory => anyhow::bail!("target directory already exists"),
        TargetState::MatchingSymlink | TargetState::Symlink => {
            anyhow::bail!("target exists and is a symlink")
        }
        TargetState::NonSymlink => anyhow::bail!("target exists and is not a directory"),
    }
}

#[cfg(unix)]
fn replace_import_symlink(source_path: &Path, target_path: &Path) -> Result<()> {
    match classify_target(target_path) {
        TargetState::MatchingSymlink => {}
        TargetState::Symlink => {}
        TargetState::Missing => return create_import_symlink(source_path, target_path),
        TargetState::Directory => anyhow::bail!("target exists and is a directory"),
        TargetState::NonSymlink => anyhow::bail!("target exists and is not a symlink"),
    }

    let temp_path = temp_symlink_path(target_path);
    let _ = std::fs::remove_file(&temp_path);
    std::os::unix::fs::symlink(source_path, &temp_path)
        .with_context(|| format!("failed to create temp symlink {}", temp_path.display()))?;
    std::fs::rename(&temp_path, target_path)
        .with_context(|| format!("failed to replace symlink {}", target_path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn temp_symlink_path(target_path: &Path) -> PathBuf {
    target_path.with_extension("sit")
}

fn load_rules_plan(
    rules_path: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Result<Option<ImportRulesPlan>> {
    let Some(rules_path) = rules_path else {
        return Ok(None);
    };
    if !rules_path.exists() {
        warnings.push(format!(
            "Rules file {} does not exist yet; routing rules were not loaded in this preview.",
            rules_path.display()
        ));
        return Ok(Some(ImportRulesPlan {
            summary: ImportRulesSummary::default(),
            rules: ImportRulesXml::default(),
        }));
    }

    let xml = std::fs::read_to_string(rules_path)
        .with_context(|| format!("failed to read import rules {}", rules_path.display()))?;
    let rules: ImportRulesXml = quick_xml::de::from_str(&xml)
        .with_context(|| format!("failed to parse import rules {}", rules_path.display()))?;
    Ok(Some(ImportRulesPlan {
        summary: rules.summary(),
        rules,
    }))
}

fn detect_source_shape(source: &Path) -> ImportSourceShape {
    if !source.exists() {
        return ImportSourceShape::Missing;
    }
    if source.is_file() {
        return ImportSourceShape::File;
    }

    let Ok(children) = std::fs::read_dir(source) else {
        return ImportSourceShape::EmptyFolder;
    };
    let mut child_dirs = 0usize;
    let mut child_video_files = 0usize;
    let mut nested_video_files = 0usize;
    let mut provider_category_dirs = 0usize;
    let mut only_child_dir_name: Option<String> = None;
    let mut video_file_names = Vec::new();

    for entry in children.flatten() {
        let path = entry.path();
        if path.is_dir() {
            child_dirs += 1;
            let child_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string());
            if child_name
                .as_deref()
                .is_some_and(is_provider_category_dir_name)
            {
                provider_category_dirs += 1;
            }
            only_child_dir_name = child_name;
            if directory_contains_video(&path, 3) {
                nested_video_files += 1;
            }
        } else if is_video_file(&path) {
            child_video_files += 1;
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                video_file_names.push(name.to_string());
            }
        }
    }

    match (child_dirs, child_video_files, nested_video_files) {
        (0, 0, _) => ImportSourceShape::EmptyFolder,
        (0, 1, _) => ImportSourceShape::DirectItem,
        (0, _, _) if video_files_look_like_single_episodic_item(&video_file_names) => {
            ImportSourceShape::DirectItem
        }
        (0, _, _) => ImportSourceShape::MultiItemFolder,
        (1, 0, 1)
            if only_child_dir_name
                .as_deref()
                .is_some_and(is_season_dir_name) =>
        {
            ImportSourceShape::DirectItem
        }
        (1, 0, 1) => ImportSourceShape::MultiItemFolder,
        (dirs, files, nested) if dirs >= 8 && nested >= 4 && files == 0 => {
            ImportSourceShape::BroadProviderRoot
        }
        (dirs, files, nested)
            if dirs >= 2 && nested >= 2 && files == 0 && provider_category_dirs > 0 =>
        {
            ImportSourceShape::BroadProviderRoot
        }
        (dirs, _, nested) if dirs >= 2 && nested >= 2 => ImportSourceShape::MultiItemFolder,
        _ => ImportSourceShape::DirectItem,
    }
}

fn video_files_look_like_single_episodic_item(names: &[String]) -> bool {
    if names.len() < 2 {
        return true;
    }

    let mut keys = names
        .iter()
        .filter_map(|name| episodic_group_key(name))
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys.len() == 1
}

fn episodic_group_key(name: &str) -> Option<String> {
    static EPISODE_RE: OnceLock<Regex> = OnceLock::new();
    let re = EPISODE_RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            (?:
                \bS\d{1,3}E\d{1,4}(?:E\d{1,4})*\b
                |\b\d{1,3}x\d{1,4}\b
                |\b(?:ep|episode)\s*\d{1,4}\b
            ).*$
            ",
        )
        .unwrap()
    });
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    if !re.is_match(stem) {
        return None;
    }
    let prefix = re.replace(stem, " ");
    let key = normalize_lookup_key(&title_hint(prefix.trim()));
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

fn is_provider_category_dir_name(name: &str) -> bool {
    static CATEGORY_RE: OnceLock<Regex> = OnceLock::new();
    let re = CATEGORY_RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(movies?|films?|tv|shows?|series|anime|kids|documentaries|docs|4k|uhd|downloads?)$",
        )
        .unwrap()
    });
    re.is_match(name.trim())
}

fn is_season_dir_name(name: &str) -> bool {
    static SEASON_RE: OnceLock<Regex> = OnceLock::new();
    let re = SEASON_RE
        .get_or_init(|| Regex::new(r"(?i)^(?:season\s*\d{1,3}|s\d{1,3}|specials?)$").unwrap());
    re.is_match(name.trim())
}

fn directory_contains_video(path: &Path, max_depth: usize) -> bool {
    WalkDir::new(path)
        .follow_links(false)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.file_type().is_file() && is_video_file(entry.path()))
}

fn collect_candidates(
    source: &Path,
    source_shape: ImportSourceShape,
) -> Result<Vec<ImportCandidateReport>> {
    if matches!(
        source_shape,
        ImportSourceShape::Missing | ImportSourceShape::EmptyFolder
    ) {
        return Ok(Vec::new());
    }

    if source_shape == ImportSourceShape::File {
        return Ok(vec![candidate_for_path(source, ImportCandidateKind::File)]);
    }

    if source_shape == ImportSourceShape::DirectItem {
        return Ok(vec![candidate_for_path(
            source,
            ImportCandidateKind::Folder,
        )]);
    }

    if source_shape == ImportSourceShape::BroadProviderRoot {
        return collect_broad_provider_candidates(source);
    }

    collect_child_candidates(source)
}

fn collect_child_candidates(source: &Path) -> Result<Vec<ImportCandidateReport>> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if directory_contains_video(&path, 4) {
                candidates.push(candidate_for_path(&path, ImportCandidateKind::Folder));
            }
        } else if is_video_file(&path) {
            candidates.push(candidate_for_path(&path, ImportCandidateKind::File));
        }
    }
    candidates.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    Ok(candidates)
}

fn collect_broad_provider_candidates(source: &Path) -> Result<Vec<ImportCandidateReport>> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_provider_category_dir_name)
        {
            candidates.extend(collect_child_candidates(&path)?);
        } else if path.is_dir() {
            if directory_contains_video(&path, 4) {
                candidates.push(candidate_for_path(&path, ImportCandidateKind::Folder));
            }
        } else if is_video_file(&path) {
            candidates.push(candidate_for_path(&path, ImportCandidateKind::File));
        }
    }
    candidates.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    candidates.dedup_by(|a, b| a.source_path == b.source_path);
    Ok(candidates)
}

fn candidate_for_path(path: &Path, kind: ImportCandidateKind) -> ImportCandidateReport {
    let raw_name = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let explicit_media_id = extract_explicit_media_id(path);
    let title_hint = title_hint(raw_name);
    let year_hint = extract_year(raw_name);
    let resolution_source = if explicit_media_id.is_some() {
        ImportResolutionSource::ExplicitId
    } else {
        ImportResolutionSource::Unresolved
    };
    let confidence = if explicit_media_id.is_some() {
        ImportConfidence::High
    } else {
        ImportConfidence::Low
    };
    let decision = if explicit_media_id.is_some() {
        ImportDecision::Preview
    } else {
        ImportDecision::NeedsLookup
    };

    ImportCandidateReport {
        source_path: path.to_path_buf(),
        target_path: None,
        kind,
        title_hint,
        year_hint,
        explicit_media_id,
        resolved_title: None,
        resolved_year: None,
        probed_resolution: None,
        video_codec: None,
        hdr_formats: Vec::new(),
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        resolution_source,
        confidence,
        decision,
        action: ImportWriteAction::None,
        reason: None,
    }
}

fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn explicit_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:\{|\[)?(?P<kind>tmdb|tvdb|imdb)(?:id)?[-_:\s]?(?P<id>tt\d+|\d+)(?:\}|\])?",
        )
        .unwrap()
    })
}

fn extract_explicit_media_id(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    let captures = explicit_id_regex().captures(text.as_ref())?;
    let kind = captures.name("kind")?.as_str().to_ascii_lowercase();
    let id = captures.name("id")?.as_str();
    Some(format!("{kind}-{id}"))
}

fn extract_year(name: &str) -> Option<u32> {
    static YEAR_RE: OnceLock<Regex> = OnceLock::new();
    let re = YEAR_RE.get_or_init(|| Regex::new(r"\b((?:19|20)\d{2})\b").unwrap());
    re.captures(name)
        .and_then(|captures| captures.get(1))
        .and_then(|m| m.as_str().parse::<u32>().ok())
}

fn title_hint(raw_name: &str) -> String {
    static CLEAN_RE: OnceLock<Regex> = OnceLock::new();
    static RELEASE_GROUP_RE: OnceLock<Regex> = OnceLock::new();
    let re = CLEAN_RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            (\{(?:tmdb|tvdb|imdb)-[^}]+\})
            |(\[(?:tmdb|tvdb|imdb)id?-[^\]]+\])
            |\b(?:19|20)\d{2}\b
            |\b(?:2160p|1080p|720p|480p|4k|remux|bluray|web[-_. ]?dl|webrip|hdtv|x264|x265|h\.?264|h\.?265|hevc)\b
            ",
        )
        .unwrap()
    });
    let cleaned = raw_name.replace(['.', '_'], " ");
    let cleaned = re.replace_all(&cleaned, " ");
    let release_group_re = RELEASE_GROUP_RE.get_or_init(|| Regex::new(r"\s+-\s*\S+\s*$").unwrap());
    let cleaned = release_group_re.replace(&cleaned, " ");
    let cleaned = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '-' || c.is_whitespace())
        .to_string();
    if cleaned.is_empty() {
        raw_name.to_string()
    } else {
        cleaned
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryContentKind {
    Movie,
    Tv,
    Anime,
    Unknown,
}

fn summarize_candidates(
    candidates: &[ImportCandidateReport],
    content_type: ImportContentReport,
    destinations: &ImportDestinations,
) -> ImportSummary {
    let mut summary = ImportSummary {
        candidates: candidates.len(),
        ..ImportSummary::default()
    };
    for candidate in candidates {
        match candidate.kind {
            ImportCandidateKind::File => summary.files += 1,
            ImportCandidateKind::Folder => summary.folders += 1,
        }
        if candidate.explicit_media_id.is_some() {
            summary.explicit_ids += 1;
        }
        match summary_content_kind(candidate, content_type, destinations) {
            SummaryContentKind::Movie => summary.movies += 1,
            SummaryContentKind::Tv => summary.tv += 1,
            SummaryContentKind::Anime => summary.anime += 1,
            SummaryContentKind::Unknown => summary.unknown_content += 1,
        }
        match candidate.confidence {
            ImportConfidence::High => summary.high_confidence += 1,
            ImportConfidence::Medium => summary.medium_confidence += 1,
            ImportConfidence::Low => summary.low_confidence += 1,
            ImportConfidence::Ambiguous => summary.ambiguous_confidence += 1,
        }
        if candidate.action == ImportWriteAction::Skip {
            summary.skipped += 1;
        }
        match candidate.decision {
            ImportDecision::NeedsLookup => summary.needs_lookup += 1,
            ImportDecision::Skipped if candidate.action != ImportWriteAction::Skip => {
                summary.skipped += 1;
            }
            ImportDecision::WouldCreate => summary.would_create += 1,
            ImportDecision::WouldUpdate => summary.would_update += 1,
            ImportDecision::Created => summary.created += 1,
            ImportDecision::Updated => summary.updated += 1,
            ImportDecision::Preview => match candidate.action {
                ImportWriteAction::Create => summary.would_create += 1,
                ImportWriteAction::Update => summary.would_update += 1,
                _ => {}
            },
            ImportDecision::NeedsReview | ImportDecision::Skipped => {}
        }
    }
    summary
}

fn summary_content_kind(
    candidate: &ImportCandidateReport,
    content_type: ImportContentReport,
    destinations: &ImportDestinations,
) -> SummaryContentKind {
    match content_type {
        ImportContentReport::Movie => return SummaryContentKind::Movie,
        ImportContentReport::Tv => return SummaryContentKind::Tv,
        ImportContentReport::Anime => return SummaryContentKind::Anime,
        ImportContentReport::Auto => {}
    }

    let Some(target_path) = candidate.target_path.as_deref() else {
        return SummaryContentKind::Unknown;
    };
    if path_is_under_optional_root(target_path, destinations.anime_destination.as_deref()) {
        return SummaryContentKind::Anime;
    }
    if path_is_under_optional_root(target_path, destinations.tv_destination.as_deref()) {
        return SummaryContentKind::Tv;
    }
    if path_is_under_optional_root(target_path, destinations.movie_destination.as_deref()) {
        return SummaryContentKind::Movie;
    }

    match candidate.explicit_media_id.as_deref() {
        Some(id) if id.starts_with("tvdb-") => SummaryContentKind::Tv,
        Some(id) if id.starts_with("tmdb-") => SummaryContentKind::Movie,
        _ => SummaryContentKind::Unknown,
    }
}

fn path_is_under_optional_root(path: &Path, root: Option<&Path>) -> bool {
    root.is_some_and(|root| path.starts_with(root))
}

fn print_text_report(report: &ImportReport, saved_report_path: &Path) {
    println!("Import {:?}", report.mode);
    println!("  Source:        {}", report.source.display());
    println!("  Shape:         {:?}", report.source_shape);
    println!("  Mode:          {:?}", report.mode);
    println!("  Content type:  {:?}", report.content_type);
    println!("  Metadata:      {:?}", report.metadata_mode);
    println!("  Report:        {}", saved_report_path.display());
    if let Some(destination) = &report.destinations.destination {
        println!("  Destination:   {}", destination.display());
    }
    if let Some(destination) = &report.destinations.movie_destination {
        println!("  Movies:        {}", destination.display());
    }
    if let Some(destination) = &report.destinations.tv_destination {
        println!("  TV:            {}", destination.display());
    }
    if let Some(destination) = &report.destinations.anime_destination {
        println!("  Anime:         {}", destination.display());
    }
    if let Some(rules) = &report.destinations.rules {
        println!("  Rules:         {}", rules.display());
    }
    if let Some(rules) = &report.rules_summary {
        println!(
            "  Rules loaded:  {} (movie={}, tv={}, anime={})",
            rules.loaded, rules.movie_routes, rules.tv_routes, rules.anime_routes
        );
    }

    println!();
    println!("Summary");
    println!("  Candidates:    {}", report.summary.candidates);
    println!("  Folders:       {}", report.summary.folders);
    println!("  Files:         {}", report.summary.files);
    println!("  Movies:        {}", report.summary.movies);
    println!("  TV:            {}", report.summary.tv);
    println!("  Anime:         {}", report.summary.anime);
    println!("  Unknown type:  {}", report.summary.unknown_content);
    println!("  High conf:     {}", report.summary.high_confidence);
    println!("  Medium conf:   {}", report.summary.medium_confidence);
    println!("  Low conf:      {}", report.summary.low_confidence);
    println!("  Ambiguous:     {}", report.summary.ambiguous_confidence);
    println!("  Explicit IDs:  {}", report.summary.explicit_ids);
    println!("  Need lookup:   {}", report.summary.needs_lookup);
    println!("  Would create:  {}", report.summary.would_create);
    println!("  Would update:  {}", report.summary.would_update);
    println!("  Created:       {}", report.summary.created);
    println!("  Updated:       {}", report.summary.updated);
    println!("  Skipped:       {}", report.summary.skipped);

    if !report.warnings.is_empty() {
        println!();
        println!("Warnings");
        for warning in &report.warnings {
            println!("  - {warning}");
        }
    }

    if !report.handoff.is_empty() {
        println!();
        println!("Next");
        for message in &report.handoff {
            println!("  - {message}");
        }
    }

    if !report.candidates.is_empty() {
        println!();
        println!("Candidates");
        for candidate in report.candidates.iter().take(25) {
            let id = candidate.explicit_media_id.as_deref().unwrap_or("-");
            let probe = text_probe_summary(candidate);
            println!(
                "  - {:?}: {} [{} {:?}] id={} year={} target={}{}{}",
                candidate.kind,
                candidate.title_hint,
                candidate.decision_label(),
                candidate.confidence,
                id,
                candidate
                    .year_hint
                    .map(|year| year.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                candidate
                    .target_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
                candidate
                    .reason
                    .as_ref()
                    .map(|reason| format!(" reason={reason}"))
                    .unwrap_or_default(),
                probe
            );
        }
        if report.candidates.len() > 25 {
            println!("  ... {} more", report.candidates.len() - 25);
        }
    }
}

fn text_probe_summary(candidate: &ImportCandidateReport) -> String {
    let mut parts = Vec::new();
    if let Some(resolution) = &candidate.probed_resolution {
        parts.push(format!("res={resolution}"));
    }
    if let Some(codec) = &candidate.video_codec {
        parts.push(format!("codec={codec}"));
    }
    if !candidate.hdr_formats.is_empty() {
        parts.push(format!("hdr={}", candidate.hdr_formats.join(",")));
    }
    if !candidate.audio_languages.is_empty() {
        parts.push(format!("audio={}", candidate.audio_languages.join(",")));
    }
    if !candidate.subtitle_languages.is_empty() {
        parts.push(format!("subs={}", candidate.subtitle_languages.join(",")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" probe=[{}]", parts.join(" "))
    }
}

#[derive(Debug, Clone)]
struct ImportRulesPlan {
    summary: ImportRulesSummary,
    rules: ImportRulesXml,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportRulesXml {
    #[serde(default)]
    destinations: ImportRulesDestinationsXml,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ImportRulesDestinationsXml {
    #[serde(default)]
    movies: ImportRulesBucketXml,
    #[serde(default)]
    tv: ImportRulesBucketXml,
    #[serde(default)]
    anime: ImportRulesBucketXml,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ImportRulesBucketXml {
    #[serde(rename = "@default")]
    default_destination: Option<PathBuf>,
    #[serde(rename = "route", default)]
    routes: Vec<ImportRouteXml>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct ImportRouteXml {
    #[serde(rename = "@to")]
    to: Option<PathBuf>,
    #[serde(rename = "@resolution")]
    resolution: Option<String>,
    #[serde(rename = "@quality")]
    quality: Option<String>,
    #[serde(rename = "@hdr")]
    hdr: Option<String>,
    #[serde(rename = "@codec")]
    codec: Option<String>,
    #[serde(rename = "@edition")]
    edition: Option<String>,
    #[serde(rename = "@audio")]
    audio: Option<String>,
    #[serde(rename = "@subtitles")]
    subtitles: Option<String>,
    #[serde(rename = "@sourcePathContains")]
    source_path_contains: Option<String>,
    #[serde(rename = "@releaseTitleContains")]
    release_title_contains: Option<String>,
}

impl ImportRulesXml {
    fn summary(&self) -> ImportRulesSummary {
        ImportRulesSummary {
            loaded: true,
            movie_default: self.destinations.movies.default_destination.clone(),
            tv_default: self.destinations.tv.default_destination.clone(),
            anime_default: self.destinations.anime.default_destination.clone(),
            movie_routes: self.destinations.movies.routes.len(),
            tv_routes: self.destinations.tv.routes.len(),
            anime_routes: self.destinations.anime.routes.len(),
        }
    }
}

impl ImportRulesBucketXml {
    fn destination_for(&self, candidate: &ImportCandidateReport) -> Option<PathBuf> {
        self.routes
            .iter()
            .find_map(|route| route.destination_for(candidate))
            .or_else(|| self.default_destination.clone())
    }
}

impl ImportRouteXml {
    fn destination_for(&self, candidate: &ImportCandidateReport) -> Option<PathBuf> {
        if self.matches(candidate) {
            self.to.clone()
        } else {
            None
        }
    }

    fn matches(&self, candidate: &ImportCandidateReport) -> bool {
        route_resolution_matches(&self.resolution, candidate)
            && route_attr_matches(&self.quality, candidate)
            && route_collection_matches(&self.hdr, &candidate.hdr_formats, candidate)
            && route_optional_value_matches(
                &self.codec,
                candidate.video_codec.as_deref(),
                candidate,
            )
            && route_attr_matches(&self.edition, candidate)
            && route_language_matches(&self.audio, &candidate.audio_languages, candidate)
            && route_language_matches(&self.subtitles, &candidate.subtitle_languages, candidate)
            && route_path_contains_matches(&self.source_path_contains, candidate)
            && route_title_contains_matches(&self.release_title_contains, candidate)
    }
}

fn route_attr_matches(expected: &Option<String>, candidate: &ImportCandidateReport) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    let needle = normalize_rule_token(expected);
    if needle.is_empty() {
        return true;
    }
    let haystack = normalize_rule_token(&candidate.source_path.to_string_lossy());
    haystack.contains(&needle)
}

fn route_resolution_matches(expected: &Option<String>, candidate: &ImportCandidateReport) -> bool {
    if let Some(probed) = candidate.probed_resolution.as_deref() {
        let Some(expected) = expected.as_deref() else {
            return true;
        };
        return normalize_rule_token(probed).contains(&normalize_rule_token(expected));
    }
    route_attr_matches(expected, candidate)
}

fn route_optional_value_matches(
    expected: &Option<String>,
    value: Option<&str>,
    candidate: &ImportCandidateReport,
) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    if let Some(value) = value {
        return normalize_rule_token(value).contains(&normalize_rule_token(expected));
    }
    route_attr_matches(&Some(expected.to_string()), candidate)
}

fn route_collection_matches(
    expected: &Option<String>,
    values: &[String],
    candidate: &ImportCandidateReport,
) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    let needle = normalize_rule_token(expected);
    if !values.is_empty() {
        return values
            .iter()
            .any(|value| normalize_rule_token(value).contains(&needle));
    }
    route_attr_matches(&Some(expected.to_string()), candidate)
}

fn route_language_matches(
    expected: &Option<String>,
    values: &[String],
    candidate: &ImportCandidateReport,
) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    let needle = normalize_rule_token(expected);
    if !values.is_empty() {
        return values
            .iter()
            .any(|value| normalize_rule_token(value).contains(&needle));
    }
    route_attr_matches(&Some(expected.to_string()), candidate)
}

fn route_path_contains_matches(
    expected: &Option<String>,
    candidate: &ImportCandidateReport,
) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    candidate
        .source_path
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains(&expected.to_ascii_lowercase())
}

fn route_title_contains_matches(
    expected: &Option<String>,
    candidate: &ImportCandidateReport,
) -> bool {
    let Some(expected) = expected.as_deref() else {
        return true;
    };
    candidate
        .title_hint
        .to_ascii_lowercase()
        .contains(&expected.to_ascii_lowercase())
        || candidate
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                name.to_ascii_lowercase()
                    .contains(&expected.to_ascii_lowercase())
            })
            .unwrap_or(false)
}

fn normalize_rule_token(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

impl ImportCandidateReport {
    fn decision_label(&self) -> &'static str {
        match self.decision {
            ImportDecision::Preview => "preview",
            ImportDecision::NeedsLookup => "needs_lookup",
            ImportDecision::NeedsReview => "needs_review",
            ImportDecision::Skipped => "skipped",
            ImportDecision::Created => "created",
            ImportDecision::Updated => "updated",
            ImportDecision::WouldCreate => "would_create",
            ImportDecision::WouldUpdate => "would_update",
        }
    }
}

fn mode_report(mode: ImportMode) -> ImportModeReport {
    match mode {
        ImportMode::Preview => ImportModeReport::Preview,
        ImportMode::Safe => ImportModeReport::Safe,
        ImportMode::Aggressive => ImportModeReport::Aggressive,
    }
}

fn content_report(content_type: ImportContentType) -> ImportContentReport {
    match content_type {
        ImportContentType::Movie => ImportContentReport::Movie,
        ImportContentType::Tv => ImportContentReport::Tv,
        ImportContentType::Anime => ImportContentReport::Anime,
        ImportContentType::Auto => ImportContentReport::Auto,
    }
}

fn metadata_mode_report(metadata_mode: ImportMetadataMode) -> ImportMetadataModeReport {
    match metadata_mode {
        ImportMetadataMode::Fast => ImportMetadataModeReport::Fast,
        ImportMetadataMode::Probe => ImportMetadataModeReport::Probe,
        ImportMetadataMode::Strict => ImportMetadataModeReport::Strict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_options(source: PathBuf, destination: PathBuf, mode: ImportMode) -> ImportOptions {
        ImportOptions {
            source,
            destination: Some(destination),
            movie_destination: None,
            tv_destination: None,
            anime_destination: None,
            rules: None,
            content_type: ImportContentType::Movie,
            mode,
            metadata_mode: ImportMetadataMode::Fast,
            probe_tool: ImportProbeTool::Auto,
            lookup_mode: ImportLookupMode::Cache,
            offline: false,
            refresh_metadata: false,
            max_lookups: 50,
            report_path: None,
            yes: true,
            folders_only: false,
            create_links: false,
            output: OutputFormat::Json,
        }
    }

    fn report_without_db(options: &ImportOptions) -> ImportReport {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(build_import_report(options, None, None, None))
            .unwrap()
    }

    fn assert_report_json_contract(report: &ImportReport) -> ImportReport {
        let json = serde_json::to_string_pretty(report).unwrap();
        let value = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        assert_eq!(value["version"], 1);
        assert!(value["source"].is_string());
        assert!(value["source_shape"].is_string());
        assert!(value["summary"].is_object());
        assert!(value["candidates"].is_array());
        if let Some(candidate) = value["candidates"]
            .as_array()
            .and_then(|items| items.first())
        {
            assert!(candidate["source_path"].is_string());
            assert!(candidate["title_hint"].is_string());
            assert!(candidate["confidence"].is_string());
            assert!(candidate["decision"].is_string());
        }
        serde_json::from_str::<ImportReport>(&json).unwrap()
    }

    #[test]
    fn explicit_id_supports_common_forms() {
        assert_eq!(
            extract_explicit_media_id(Path::new("/mnt/rd/Dune {tmdb-438631}/Dune.mkv")).as_deref(),
            Some("tmdb-438631")
        );
        assert_eq!(
            extract_explicit_media_id(Path::new("/mnt/rd/Show [tvdbid-12345]/S01E01.mkv"))
                .as_deref(),
            Some("tvdb-12345")
        );
        assert_eq!(
            extract_explicit_media_id(Path::new("/mnt/rd/Movie imdb-tt1234567.mkv")).as_deref(),
            Some("imdb-tt1234567")
        );
    }

    #[test]
    fn title_hint_strips_common_release_tokens() {
        assert_eq!(
            title_hint("Dune.Part.Two.2024.2160p.WEB-DL.x265-GROUP"),
            "Dune Part Two"
        );
    }

    #[test]
    fn missing_source_shape_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_source_shape(&dir.path().join("missing")),
            ImportSourceShape::Missing
        );
    }

    #[test]
    fn single_movie_child_is_multi_item_folder() {
        let dir = tempfile::tempdir().unwrap();
        let movie = dir.path().join("Dune.2021.2160p");
        std::fs::create_dir_all(&movie).unwrap();
        std::fs::write(movie.join("Dune.2021.2160p.mkv"), b"").unwrap();

        assert_eq!(
            detect_source_shape(dir.path()),
            ImportSourceShape::MultiItemFolder
        );
    }

    #[test]
    fn single_season_child_is_direct_item() {
        let dir = tempfile::tempdir().unwrap();
        let season = dir.path().join("Season 01");
        std::fs::create_dir_all(&season).unwrap();
        std::fs::write(season.join("Show.S01E01.mkv"), b"").unwrap();

        assert_eq!(
            detect_source_shape(dir.path()),
            ImportSourceShape::DirectItem
        );
    }

    #[test]
    fn loose_movie_files_are_multi_item_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Dune.2021.mkv"), b"").unwrap();
        std::fs::write(dir.path().join("Arrival.2016.mkv"), b"").unwrap();

        assert_eq!(
            detect_source_shape(dir.path()),
            ImportSourceShape::MultiItemFolder
        );

        let candidates = collect_candidates(dir.path(), ImportSourceShape::MultiItemFolder)
            .expect("collect candidates");
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .all(|candidate| candidate.kind == ImportCandidateKind::File));
    }

    #[test]
    fn sibling_episode_files_are_direct_item() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Example.Show.S01E01.mkv"), b"").unwrap();
        std::fs::write(dir.path().join("Example.Show.S01E02.mkv"), b"").unwrap();

        assert_eq!(
            detect_source_shape(dir.path()),
            ImportSourceShape::DirectItem
        );
    }

    #[test]
    fn provider_category_root_is_broad_provider_root() {
        let dir = tempfile::tempdir().unwrap();
        let movie = dir.path().join("Movies").join("Dune.2021");
        let show = dir.path().join("Shows").join("Example.Show.S01");
        std::fs::create_dir_all(&movie).unwrap();
        std::fs::create_dir_all(&show).unwrap();
        std::fs::write(movie.join("Dune.2021.mkv"), b"").unwrap();
        std::fs::write(show.join("Example.Show.S01E01.mkv"), b"").unwrap();

        assert_eq!(
            detect_source_shape(dir.path()),
            ImportSourceShape::BroadProviderRoot
        );
    }

    #[test]
    fn broad_provider_root_expands_category_dirs_to_items() {
        let dir = tempfile::tempdir().unwrap();
        let movie = dir.path().join("Movies").join("Dune.2021");
        let show = dir.path().join("Shows").join("Example.Show.S01");
        std::fs::create_dir_all(&movie).unwrap();
        std::fs::create_dir_all(&show).unwrap();
        std::fs::write(movie.join("Dune.2021.mkv"), b"").unwrap();
        std::fs::write(show.join("Example.Show.S01E01.mkv"), b"").unwrap();

        let candidates =
            collect_candidates(dir.path(), ImportSourceShape::BroadProviderRoot).unwrap();
        let paths = candidates
            .iter()
            .map(|candidate| candidate.source_path.strip_prefix(dir.path()).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                Path::new("Movies/Dune.2021"),
                Path::new("Shows/Example.Show.S01")
            ]
        );
    }

    #[test]
    fn import_report_fixture_direct_item_movie_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library").join("movies");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let report = report_without_db(&import_options(
            source.clone(),
            destination.clone(),
            ImportMode::Preview,
        ));
        let roundtrip = assert_report_json_contract(&report);

        assert_eq!(roundtrip.source_shape, ImportSourceShape::DirectItem);
        assert_eq!(roundtrip.summary.candidates, 1);
        assert_eq!(roundtrip.summary.movies, 1);
        assert_eq!(roundtrip.summary.high_confidence, 1);
        assert_eq!(
            roundtrip.candidates[0].target_path.as_deref(),
            Some(destination.join("Dune.2021.2160p {tmdb-438631}").as_path())
        );
    }

    #[test]
    fn import_report_fixture_tv_folder_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Example.Show [tvdbid-12345]");
        let destination = dir.path().join("library").join("tv");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Example.Show.S01E01.mkv"), b"").unwrap();
        let mut options = import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Preview,
        );
        options.content_type = ImportContentType::Tv;

        let report = report_without_db(&options);
        let roundtrip = assert_report_json_contract(&report);

        assert_eq!(roundtrip.source_shape, ImportSourceShape::MultiItemFolder);
        assert_eq!(roundtrip.summary.candidates, 1);
        assert_eq!(roundtrip.summary.tv, 1);
        assert_eq!(roundtrip.summary.high_confidence, 1);
        assert_eq!(
            roundtrip.candidates[0].target_path.as_deref(),
            Some(destination.join("Example.Show [tvdbid-12345]").as_path())
        );
    }

    #[test]
    fn import_report_fixture_anime_folder_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Frieren.2023 [tvdbid-424536]");
        let destination = dir.path().join("library").join("anime");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Frieren.S01E01.mkv"), b"").unwrap();
        let mut options = import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Preview,
        );
        options.content_type = ImportContentType::Anime;

        let report = report_without_db(&options);
        let roundtrip = assert_report_json_contract(&report);

        assert_eq!(roundtrip.source_shape, ImportSourceShape::MultiItemFolder);
        assert_eq!(roundtrip.summary.candidates, 1);
        assert_eq!(roundtrip.summary.anime, 1);
        assert_eq!(roundtrip.summary.high_confidence, 1);
        assert_eq!(
            roundtrip.candidates[0].target_path.as_deref(),
            Some(destination.join("Frieren.2023 [tvdbid-424536]").as_path())
        );
    }

    #[test]
    fn import_report_fixture_mixed_provider_root_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let rd = dir.path().join("rd");
        let movie = rd.join("Movies").join("Dune.2021 {tmdb-438631}");
        let show = rd.join("Shows").join("Example.Show [tvdbid-12345]");
        let anime = rd.join("Anime").join("Frieren.2023");
        std::fs::create_dir_all(&movie).unwrap();
        std::fs::create_dir_all(&show).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::write(movie.join("Dune.2021.mkv"), b"").unwrap();
        std::fs::write(show.join("Example.Show.S01E01.mkv"), b"").unwrap();
        std::fs::write(anime.join("Frieren.S01E01.mkv"), b"").unwrap();
        let movie_destination = dir.path().join("library").join("movies");
        let tv_destination = dir.path().join("library").join("tv");
        let anime_destination = dir.path().join("library").join("anime");
        let mut options = import_options(rd, dir.path().join("library"), ImportMode::Preview);
        options.content_type = ImportContentType::Auto;
        options.destination = None;
        options.movie_destination = Some(movie_destination);
        options.tv_destination = Some(tv_destination);
        options.anime_destination = Some(anime_destination);

        let report = report_without_db(&options);
        let roundtrip = assert_report_json_contract(&report);

        assert_eq!(roundtrip.source_shape, ImportSourceShape::BroadProviderRoot);
        assert_eq!(roundtrip.summary.candidates, 3);
        assert_eq!(roundtrip.summary.movies, 1);
        assert_eq!(roundtrip.summary.tv, 1);
        assert_eq!(roundtrip.summary.anime, 1);
        assert_eq!(roundtrip.summary.high_confidence, 2);
        assert_eq!(roundtrip.summary.low_confidence, 1);
        assert!(roundtrip
            .warnings
            .iter()
            .any(|warning| warning.contains("Broad provider-root source detected")));
    }

    #[test]
    fn import_rules_summary_counts_routes() {
        let dir = tempfile::tempdir().unwrap();
        let rules_path = dir.path().join("import-rules.xml");
        std::fs::write(
            &rules_path,
            r#"
<importRules>
  <destinations>
    <movies default="/library/movies">
      <route resolution="2160p" to="/library/movies-4k" />
      <route quality="remux" to="/library/movies-remux" />
    </movies>
    <tv default="/library/tv">
      <route sourcePathContains="/kids/" to="/library/kids-tv" />
    </tv>
    <anime default="/library/anime">
      <route audio="ja" subtitles="en" to="/library/anime-subbed" />
    </anime>
  </destinations>
</importRules>
"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let summary = load_rules_plan(Some(&rules_path), &mut warnings)
            .unwrap()
            .unwrap()
            .summary;

        assert!(warnings.is_empty());
        assert!(summary.loaded);
        assert_eq!(
            summary.movie_default,
            Some(PathBuf::from("/library/movies"))
        );
        assert_eq!(summary.movie_routes, 2);
        assert_eq!(summary.tv_routes, 1);
        assert_eq!(summary.anime_routes, 1);
    }

    #[test]
    fn import_rules_route_by_resolution_overrides_default_destination() {
        let dir = tempfile::tempdir().unwrap();
        let rules_path = dir.path().join("import-rules.xml");
        let movie_default = dir.path().join("movies");
        let movie_4k = dir.path().join("movies-4k");
        std::fs::write(
            &rules_path,
            format!(
                r#"
<importRules>
  <destinations>
    <movies default="{}">
      <route resolution="2160p" to="{}" />
    </movies>
  </destinations>
</importRules>
"#,
                movie_default.display(),
                movie_4k.display()
            ),
        )
        .unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let mut options = import_options(
            dir.path().join("rd"),
            dir.path().join("fallback"),
            ImportMode::Preview,
        );
        options.rules = Some(rules_path);

        let report = report_without_db(&options);

        assert_eq!(
            report.candidates[0].target_path.as_deref(),
            Some(movie_4k.join("Dune.2021.2160p {tmdb-438631}").as_path())
        );
    }

    #[test]
    fn preview_plans_target_path_and_id_tag() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let report = report_without_db(&import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Preview,
        ));

        assert_eq!(report.summary.would_create, 1);
        assert_eq!(report.summary.high_confidence, 1);
        assert_eq!(report.summary.low_confidence, 0);
        assert!(
            report.handoff[0].contains("--mode safe")
                && report.handoff[0].contains("--mode aggressive")
        );
        assert_eq!(report.candidates[0].decision, ImportDecision::WouldCreate);
        assert_eq!(report.candidates[0].confidence, ImportConfidence::High);
        assert_eq!(
            report.candidates[0].target_path.as_deref(),
            Some(destination.join("Dune.2021.2160p {tmdb-438631}").as_path())
        );
    }

    #[test]
    fn import_warns_when_destination_is_already_populated() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();
        std::fs::write(destination.join("existing.txt"), b"").unwrap();

        let report = report_without_db(&import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Preview,
        ));

        assert!(report.warnings.iter().any(|warning| {
            warning.contains(&destination.display().to_string())
                && warning.contains("already populated")
        }));
    }

    #[test]
    fn import_report_file_is_written_as_json_and_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library");
        let report_path = dir.path().join("reports").join("import-report.json");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let report = report_without_db(&import_options(
            dir.path().join("rd"),
            destination,
            ImportMode::Preview,
        ));
        let saved = write_import_report(&report, Some(&report_path)).unwrap();

        assert_eq!(saved, report_path);
        let json = std::fs::read_to_string(saved).unwrap();
        assert!(json.contains("\"source_shape\": \"multi_item_folder\""));
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn safe_mode_skips_unresolved_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let report = report_without_db(&import_options(
            dir.path().join("rd"),
            destination,
            ImportMode::Safe,
        ));

        assert_eq!(report.summary.needs_lookup, 1);
        assert_eq!(report.summary.low_confidence, 1);
        assert_eq!(report.candidates[0].action, ImportWriteAction::Skip);
        assert_eq!(report.candidates[0].confidence, ImportConfidence::Low);
        assert_eq!(
            report.candidates[0].reason.as_deref(),
            Some("safe_mode_requires_explicit_id")
        );
    }

    #[test]
    fn preview_mode_does_not_write_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let report = report_without_db(&import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Preview,
        ));

        assert_eq!(report.summary.would_create, 1);
        assert!(!destination.join("Dune.2021.2160p {tmdb-438631}").exists());
    }

    #[cfg(unix)]
    #[test]
    fn safe_mode_creates_explicit_id_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let options = import_options(dir.path().join("rd"), destination.clone(), ImportMode::Safe);
        let mut report = report_without_db(&options);
        apply_import_plan(&mut report, &options).unwrap();

        let target = destination.join("Dune.2021.2160p {tmdb-438631}");
        assert_eq!(report.summary.created, 1);
        assert_eq!(std::fs::read_link(target).unwrap(), source);
    }

    #[test]
    fn write_modes_require_confirmation_without_yes_flag() {
        assert!(!import_requires_confirmation(ImportMode::Preview, false));
        assert!(!import_requires_confirmation(ImportMode::Safe, true));
        assert!(!import_requires_confirmation(ImportMode::Aggressive, true));
        assert!(import_requires_confirmation(ImportMode::Safe, false));
        assert!(import_requires_confirmation(ImportMode::Aggressive, false));
    }

    #[test]
    fn offline_import_rejects_remote_lookup_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = import_options(
            dir.path().join("rd"),
            dir.path().join("library"),
            ImportMode::Preview,
        );
        options.offline = true;
        options.lookup_mode = ImportLookupMode::Remote;

        assert!(validate_options(&options).is_err());
    }

    #[test]
    fn refresh_metadata_requires_remote_lookup_and_online_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = import_options(
            dir.path().join("rd"),
            dir.path().join("library"),
            ImportMode::Preview,
        );
        options.refresh_metadata = true;

        let err = validate_options(&options).unwrap_err();
        assert!(err.to_string().contains("--lookup-mode remote"));

        options.lookup_mode = ImportLookupMode::Cache;
        options.offline = true;
        let err = validate_options(&options).unwrap_err();
        assert!(err.to_string().contains("--refresh-metadata"));
    }

    #[test]
    fn folders_only_rejects_explicit_create_links_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = import_options(
            dir.path().join("rd"),
            dir.path().join("library"),
            ImportMode::Preview,
        );
        options.folders_only = true;
        options.create_links = true;

        let err = validate_options(&options).unwrap_err();
        assert!(err
            .to_string()
            .contains("--folders-only cannot be combined with --create-links"));
    }

    #[test]
    fn auto_import_requires_destination_or_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = import_options(
            dir.path().join("rd"),
            dir.path().join("library"),
            ImportMode::Preview,
        );
        options.content_type = ImportContentType::Auto;
        options.destination = None;

        let err = validate_options(&options).unwrap_err();
        assert!(err.to_string().contains("--content-type auto requires"));
    }

    #[tokio::test]
    async fn cache_lookup_resolves_unresolved_candidate_id() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();
        db.set_cached(
            "tmdb:movie:438631",
            r#"{"title":"Dune","aliases":[],"year":2021,"seasons":[]}"#,
            24,
        )
        .await
        .unwrap();

        let report = build_import_report(
            &import_options(dir.path().join("rd"), destination.clone(), ImportMode::Safe),
            Some(&db),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.summary.explicit_ids, 1);
        assert_eq!(report.summary.would_create, 1);
        assert_eq!(report.summary.created, 0);
        assert_eq!(
            report.candidates[0].explicit_media_id.as_deref(),
            Some("tmdb-438631")
        );
        assert_eq!(
            report.candidates[0].resolution_source,
            ImportResolutionSource::CachedMetadata
        );
        assert_eq!(report.candidates[0].action, ImportWriteAction::Create);
        assert_eq!(report.candidates[0].resolved_title.as_deref(), Some("Dune"));
        assert_eq!(report.candidates[0].resolved_year, Some(2021));
        assert_eq!(
            report.candidates[0].target_path.as_deref(),
            Some(destination.join("Dune (2021) {tmdb-438631}").as_path())
        );
    }

    #[tokio::test]
    async fn offline_import_ignores_metadata_cache() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();
        db.set_cached(
            "tmdb:movie:438631",
            r#"{"title":"Dune","aliases":[],"year":2021,"seasons":[]}"#,
            24,
        )
        .await
        .unwrap();
        let mut options = import_options(dir.path().join("rd"), destination, ImportMode::Safe);
        options.offline = true;

        let report = build_import_report(&options, Some(&db), None, None)
            .await
            .unwrap();

        assert_eq!(report.summary.needs_lookup, 1);
        assert_eq!(report.summary.low_confidence, 1);
        assert_eq!(report.candidates[0].explicit_media_id, None);
    }

    #[tokio::test]
    async fn remote_resolution_cache_write_uses_metadata_cache_shape() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();
        let resolution = RemoteImportResolution {
            media_id: "tvdb-424536".to_string(),
            resolution_source: ImportResolutionSource::TvdbLookup,
            cache_key: "tvdb:series:424536".to_string(),
            metadata: ContentMetadata {
                title: "Frieren".to_string(),
                aliases: Vec::new(),
                year: Some(2023),
                seasons: Vec::new(),
            },
        };

        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Frieren.2023/Frieren.mkv"),
            ImportCandidateKind::Folder,
        );
        candidate.title_hint = "Frieren".to_string();
        candidate.year_hint = Some(2023);

        cache_remote_resolution(&db, &resolution, &candidate, ImportContentType::Tv)
            .await
            .unwrap();
        let cached = db.get_metadata_cache_entries().await.unwrap();
        let index = build_metadata_cache_index(cached, ImportContentType::Tv);

        let cached = lookup_cached_resolution(&candidate, &index).unwrap();
        assert_eq!(cached.media_id, "tvdb-424536");
        assert_eq!(cached.title, "Frieren");
        assert_eq!(cached.year, Some(2023));
        let direct_cached = lookup_import_resolution_cache(&db, &candidate, ImportContentType::Tv)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(direct_cached.media_id, "tvdb-424536");
    }

    #[tokio::test]
    async fn import_resolution_cache_resolves_later_report_without_metadata_index() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Frieren");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Frieren.S01E01.mkv"), b"").unwrap();
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();
        let resolution = RemoteImportResolution {
            media_id: "tvdb-424536".to_string(),
            resolution_source: ImportResolutionSource::TvdbLookup,
            cache_key: "tvdb:series:424536".to_string(),
            metadata: ContentMetadata {
                title: "Frieren".to_string(),
                aliases: Vec::new(),
                year: Some(2023),
                seasons: Vec::new(),
            },
        };

        let mut cached_candidate = candidate_for_path(&source, ImportCandidateKind::Folder);
        cached_candidate.title_hint = "Frieren".to_string();
        cached_candidate.year_hint = None;
        cache_import_resolution(&db, &resolution, &cached_candidate, ImportContentType::Tv)
            .await
            .unwrap();

        let mut options =
            import_options(dir.path().join("rd"), destination.clone(), ImportMode::Safe);
        options.content_type = ImportContentType::Tv;
        let report = build_import_report(&options, Some(&db), None, None)
            .await
            .unwrap();

        assert_eq!(report.summary.explicit_ids, 1);
        assert_eq!(
            report.candidates[0].explicit_media_id.as_deref(),
            Some("tvdb-424536")
        );
        assert_eq!(
            report.candidates[0].resolved_title.as_deref(),
            Some("Frieren")
        );
        assert_eq!(report.candidates[0].resolved_year, Some(2023));
        assert_eq!(
            report.candidates[0].target_path.as_deref(),
            Some(destination.join("Frieren (2023) {tvdb-424536}").as_path())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn import_apply_backfills_database_for_created_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p {tmdb-438631}");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();
        let db_path = dir.path().join("symlinkarr.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let options = import_options(dir.path().join("rd"), destination.clone(), ImportMode::Safe);
        let mut report = build_import_report(&options, None, None, None)
            .await
            .unwrap();
        apply_import_plan(&mut report, &options).unwrap();
        backfill_import_links(&mut report, &db, ImportContentType::Movie).await;

        assert!(report
            .handoff
            .iter()
            .any(|message| message.contains("symlinkarr scan")));
        let target = destination.join("Dune.2021.2160p {tmdb-438631}");
        let record = db.get_link_by_target_path(&target).await.unwrap().unwrap();
        assert_eq!(record.source_path, source);
        assert_eq!(record.media_id, "tmdb-438631");
        assert_eq!(record.media_type, MediaType::Movie);
        assert!(report.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn aggressive_mode_creates_folder_symlink_for_unresolved_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let options = import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Aggressive,
        );
        let mut report = report_without_db(&options);
        apply_import_plan(&mut report, &options).unwrap();

        let target = destination.join("Dune.2021.2160p");
        assert_eq!(report.summary.created, 1);
        assert_eq!(std::fs::read_link(target).unwrap(), source);
    }

    #[cfg(unix)]
    #[test]
    fn folders_only_creates_real_id_tagged_folder_without_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir
            .path()
            .join("rd")
            .join("Dune.2021.2160p {tmdb-438631}.mkv");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, b"").unwrap();

        let mut options =
            import_options(source_file.clone(), destination.clone(), ImportMode::Safe);
        options.folders_only = true;
        let mut report = report_without_db(&options);
        apply_import_plan(&mut report, &options).unwrap();

        let target = destination.join("Dune.2021.2160p {tmdb-438631}");
        assert_eq!(report.summary.created, 1);
        assert!(target.is_dir());
        assert!(!target.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn aggressive_mode_replaces_existing_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let previous = dir.path().join("old").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        let target = destination.join("Dune.2021.2160p");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();
        std::os::unix::fs::symlink(&previous, &target).unwrap();

        let options = import_options(dir.path().join("rd"), destination, ImportMode::Aggressive);
        let mut report = report_without_db(&options);
        apply_import_plan(&mut report, &options).unwrap();

        assert_eq!(report.summary.updated, 1);
        assert_eq!(std::fs::read_link(target).unwrap(), source);
    }

    #[cfg(unix)]
    #[test]
    fn aggressive_import_rerun_skips_already_correct_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();

        let options = import_options(
            dir.path().join("rd"),
            destination.clone(),
            ImportMode::Aggressive,
        );
        let mut first = report_without_db(&options);
        apply_import_plan(&mut first, &options).unwrap();

        let second = report_without_db(&options);

        assert_eq!(second.summary.skipped, 1);
        assert_eq!(second.summary.would_update, 0);
        assert_eq!(
            second.candidates[0].reason.as_deref(),
            Some("target_symlink_already_correct")
        );
        assert_eq!(
            std::fs::read_link(destination.join("Dune.2021.2160p")).unwrap(),
            source
        );
    }

    #[cfg(unix)]
    #[test]
    fn aggressive_mode_refuses_to_overwrite_real_file_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Dune.2021.2160p");
        let destination = dir.path().join("library");
        let target = destination.join("Dune.2021.2160p");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("Dune.2021.2160p.mkv"), b"").unwrap();
        std::fs::write(&target, b"real file").unwrap();

        let options = import_options(dir.path().join("rd"), destination, ImportMode::Aggressive);
        let mut report = report_without_db(&options);
        apply_import_plan(&mut report, &options).unwrap();

        assert_eq!(report.summary.skipped, 1);
        assert_eq!(
            report.candidates[0].reason.as_deref(),
            Some("target_is_not_symlink")
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "real file");
    }

    #[test]
    fn auto_content_routes_tvdb_to_tv_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("rd").join("Show [tvdbid-12345]");
        let tv_destination = dir.path().join("tv");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Show.S01E01.mkv"), b"").unwrap();

        let mut options = import_options(
            dir.path().join("rd"),
            dir.path().join("generic"),
            ImportMode::Preview,
        );
        options.content_type = ImportContentType::Auto;
        options.destination = None;
        options.tv_destination = Some(tv_destination.clone());

        let report = report_without_db(&options);

        assert_eq!(report.summary.tv, 1);
        assert_eq!(report.summary.movies, 0);
        assert_eq!(report.summary.unknown_content, 0);
        assert_eq!(
            report.candidates[0].target_path.as_deref(),
            Some(tv_destination.join("Show [tvdbid-12345]").as_path())
        );
    }

    #[test]
    fn route_matching_uses_probed_resolution_and_languages() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Movie.1080p/Movie.mkv"),
            ImportCandidateKind::Folder,
        );
        candidate.probed_resolution = Some("2160p".to_string());
        candidate.video_codec = Some("hevc".to_string());
        candidate.hdr_formats = vec!["hdr10".to_string()];
        candidate.audio_languages = vec!["jpn".to_string()];
        candidate.subtitle_languages = vec!["eng".to_string()];
        let route = ImportRouteXml {
            to: Some(PathBuf::from("/library/anime-subbed-4k")),
            resolution: Some("2160p".to_string()),
            quality: None,
            hdr: Some("hdr10".to_string()),
            codec: Some("hevc".to_string()),
            edition: None,
            audio: Some("jpn".to_string()),
            subtitles: Some("eng".to_string()),
            source_path_contains: None,
            release_title_contains: None,
        };

        assert!(route.matches(&candidate));
    }

    #[test]
    fn target_name_uses_canonical_title_without_unknown_year() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Some.Release.Name"),
            ImportCandidateKind::Folder,
        );
        candidate.explicit_media_id = Some("tmdb-123".to_string());
        candidate.resolved_title = Some("Resolved Title".to_string());

        assert_eq!(
            target_name_with_optional_id(&candidate, "Some.Release.Name"),
            "Resolved Title {tmdb-123}"
        );
    }

    #[test]
    fn target_name_keeps_source_name_when_external_id_is_missing() {
        let candidate = candidate_for_path(
            Path::new("/mnt/rd/Unresolved.Movie.2024"),
            ImportCandidateKind::Folder,
        );

        assert_eq!(
            target_name_with_optional_id(&candidate, "Unresolved.Movie.2024"),
            "Unresolved.Movie.2024"
        );
    }

    #[test]
    fn target_name_does_not_add_conflicting_second_id_tag() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Tagged Movie {imdb-tt1234567}"),
            ImportCandidateKind::Folder,
        );
        candidate.explicit_media_id = Some("tmdb-123".to_string());

        assert_eq!(
            target_name_with_optional_id(&candidate, "Tagged Movie {imdb-tt1234567}"),
            "Tagged Movie {imdb-tt1234567}"
        );
    }

    #[test]
    fn probe_cache_key_changes_when_file_size_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Movie.2024.mkv");
        std::fs::write(&path, b"video").unwrap();

        let first = probe_cache_key(&path, ImportProbeTool::Ffprobe).unwrap();
        std::fs::write(&path, b"video-with-more-bytes").unwrap();
        let second = probe_cache_key(&path, ImportProbeTool::Ffprobe).unwrap();

        assert_ne!(first, second);
        assert!(first.starts_with("import:probe:Ffprobe:"));
    }

    #[test]
    fn remote_lookup_selects_unique_exact_title_and_year() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Dune.2021.2160p/Dune.mkv"),
            ImportCandidateKind::Folder,
        );
        candidate.title_hint = "Dune".to_string();
        candidate.year_hint = Some(2021);
        let matches = vec![
            TmdbSearchMatch {
                id: 1,
                title: "Dune".to_string(),
                year: Some(1984),
            },
            TmdbSearchMatch {
                id: 438631,
                title: "Dune".to_string(),
                year: Some(2021),
            },
        ];

        assert_eq!(
            select_tmdb_match(&candidate, &matches),
            LookupMatchSelection::Unique(438631)
        );
    }

    #[test]
    fn remote_lookup_rejects_ambiguous_exact_matches() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Show.2021/Show.mkv"),
            ImportCandidateKind::Folder,
        );
        candidate.title_hint = "Show".to_string();
        candidate.year_hint = Some(2021);
        let matches = vec![
            TmdbSearchMatch {
                id: 10,
                title: "Show".to_string(),
                year: Some(2021),
            },
            TmdbSearchMatch {
                id: 11,
                title: "Show".to_string(),
                year: Some(2021),
            },
        ];

        assert_eq!(
            select_tmdb_match(&candidate, &matches),
            LookupMatchSelection::Ambiguous
        );
    }

    #[test]
    fn tvdb_lookup_selects_unique_exact_title_and_year() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Frieren.2023/Frieren.mkv"),
            ImportCandidateKind::Folder,
        );
        candidate.title_hint = "Frieren".to_string();
        candidate.year_hint = Some(2023);
        let matches = vec![
            TvdbSearchMatch {
                id: 1,
                title: "Frieren".to_string(),
                year: Some(2022),
            },
            TvdbSearchMatch {
                id: 424536,
                title: "Frieren".to_string(),
                year: Some(2023),
            },
        ];

        assert_eq!(
            select_tvdb_match(&candidate, &matches),
            LookupMatchSelection::Unique(424536)
        );
    }

    #[test]
    fn tvdb_lookup_marks_ambiguous_exact_matches() {
        let mut candidate = candidate_for_path(
            Path::new("/mnt/rd/Show.2021/Show.mkv"),
            ImportCandidateKind::Folder,
        );
        candidate.title_hint = "Show".to_string();
        candidate.year_hint = Some(2021);
        let matches = vec![
            TvdbSearchMatch {
                id: 10,
                title: "Show".to_string(),
                year: Some(2021),
            },
            TvdbSearchMatch {
                id: 11,
                title: "Show".to_string(),
                year: Some(2021),
            },
        ];

        assert_eq!(
            select_tvdb_match(&candidate, &matches),
            LookupMatchSelection::Ambiguous
        );
    }

    #[test]
    fn resolution_label_maps_common_heights() {
        assert_eq!(
            resolution_label(Some(3840), Some(2160)).as_deref(),
            Some("2160p")
        );
        assert_eq!(
            resolution_label(Some(1920), Some(1080)).as_deref(),
            Some("1080p")
        );
        assert_eq!(
            resolution_label(Some(1280), Some(720)).as_deref(),
            Some("720p")
        );
    }

    #[test]
    fn mediainfo_json_maps_resolution_and_languages() {
        let parsed = serde_json::from_str::<MediainfoOutput>(
            r#"
{
  "media": {
    "track": [
      { "@type": "Video", "Width": "3840", "Height": "2160", "Format": "HEVC", "HDR_Format": "Dolby Vision / HDR10", "HDR_Format_Profile": "dvhe.08.06" },
      { "@type": "Audio", "Language": "jpn" },
      { "@type": "Audio", "Language": "eng" },
      { "@type": "Text", "Language": "eng" }
    ]
  }
}
"#,
        )
        .unwrap();
        let metadata = probe_metadata_from_mediainfo(parsed);

        assert_eq!(metadata.resolution.as_deref(), Some("2160p"));
        assert_eq!(metadata.video_codec.as_deref(), Some("hevc"));
        assert_eq!(metadata.hdr_formats, vec!["dv", "dv-p8"]);
        assert_eq!(metadata.audio_languages, vec!["jpn", "eng"]);
        assert_eq!(metadata.subtitle_languages, vec!["eng"]);
    }

    #[test]
    fn ffprobe_side_data_maps_dolby_vision_profile() {
        let labels = hdr_labels_from_ffprobe_side_data(&[FfprobeSideData {
            side_data_type: Some("DOVI configuration record".to_string()),
            dv_profile: Some(8),
        }]);

        assert_eq!(labels, vec!["dv", "dv-p8"]);
    }
}
