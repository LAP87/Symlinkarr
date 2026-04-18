use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::anime_roots::collect_anime_root_duplicate_groups;
use crate::commands::{panel_border, panel_kv_row, panel_title};
use crate::config::{Config, ContentType, LibraryConfig};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::media_servers::plex_db;
use crate::models::MediaType;
use crate::utils::normalize;
use crate::OutputFormat;

use self::path_compare::{build_path_compare, PathCompareOutput, PATH_SAMPLE_LIMIT};
#[cfg(test)]
use self::path_compare::{sample_difference, sample_intersection, symlink_source_missing};

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct Summary {
    total_library_items: i64,
    items_with_symlinks: i64,
    broken_symlinks: i64,
    missing_from_rd: i64,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct MediaTypeInfo {
    library_items: i64,
    linked: i64,
    broken: i64,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct LibraryInfo {
    name: String,
    items: i64,
    linked: i64,
    broken: i64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
struct ReportOutput {
    generated_at: String,
    summary: Summary,
    by_media_type: BTreeMap<String, MediaTypeInfo>,
    top_libraries: Vec<LibraryInfo>,
    path_compare: PathCompareOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    anime_duplicates: Option<AnimeDuplicateAuditOutput>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct AnimeRootDuplicateSample {
    normalized_title: String,
    tagged_roots: Vec<PathBuf>,
    untagged_roots: Vec<PathBuf>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct PlexDuplicateShowSample {
    title: String,
    original_title: String,
    year: Option<i64>,
    total_rows: usize,
    live_rows: usize,
    deleted_rows: usize,
    guid_kinds: Vec<String>,
    guids: Vec<String>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct CorrelatedAnimeDuplicateSample {
    normalized_title: String,
    tagged_roots: Vec<PathBuf>,
    untagged_roots: Vec<PathBuf>,
    plex_total_rows: usize,
    plex_live_rows: usize,
    plex_deleted_rows: usize,
    plex_guid_kinds: Vec<String>,
    plex_guids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AnimeRootUsageSample {
    pub path: PathBuf,
    pub filesystem_symlinks: usize,
    pub db_active_links: usize,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AnimeRemediationSample {
    pub normalized_title: String,
    pub recommended_tagged_root: AnimeRootUsageSample,
    pub alternate_tagged_roots: Vec<AnimeRootUsageSample>,
    pub legacy_roots: Vec<AnimeRootUsageSample>,
    pub plex_total_rows: usize,
    pub plex_live_rows: usize,
    pub plex_deleted_rows: usize,
    pub plex_guid_kinds: Vec<String>,
    pub plex_guids: Vec<String>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct AnimeDuplicateAuditOutput {
    filesystem_mixed_root_groups: usize,
    filesystem_sample_groups: Vec<AnimeRootDuplicateSample>,
    plex_duplicate_show_groups: Option<usize>,
    plex_hama_anidb_tvdb_groups: Option<usize>,
    plex_other_duplicate_show_groups: Option<usize>,
    plex_sample_groups: Option<Vec<PlexDuplicateShowSample>>,
    correlated_hama_split_groups: Option<usize>,
    correlated_sample_groups: Option<Vec<CorrelatedAnimeDuplicateSample>>,
    remediation_groups: Option<usize>,
    remediation_sample_groups: Option<Vec<AnimeRemediationSample>>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct AnimeRemediationReportOutput {
    pub generated_at: String,
    pub filesystem_mixed_root_groups: usize,
    pub plex_duplicate_show_groups: usize,
    pub plex_hama_anidb_tvdb_groups: usize,
    pub correlated_hama_split_groups: usize,
    pub remediation_groups: usize,
    pub returned_groups: usize,
    pub groups: Vec<AnimeRemediationSample>,
}

struct LibraryScannerItem {
    library_name: String,
    media_type: MediaType,
    media_id: String,
}

#[derive(Default)]
struct LinkPresence {
    active_media_ids: HashSet<String>,
    dead_media_ids: HashSet<String>,
}

pub(crate) struct ReportOptions<'a> {
    pub(crate) output_format: OutputFormat,
    pub(crate) filter: Option<MediaType>,
    pub(crate) library_filter: Option<&'a str>,
    pub(crate) plex_db_path: Option<&'a Path>,
    pub(crate) full_anime_duplicates: bool,
    pub(crate) anime_remediation_tsv_path: Option<&'a Path>,
    pub(crate) pretty: bool,
}

pub(crate) async fn run_report(
    cfg: &Config,
    db: &Database,
    options: ReportOptions<'_>,
) -> Result<()> {
    let effective_full_anime_duplicates =
        options.full_anime_duplicates || options.anime_remediation_tsv_path.is_some();
    let report = build_report(
        cfg,
        db,
        options.filter,
        options.library_filter,
        options.plex_db_path,
        effective_full_anime_duplicates,
    )
    .await?;

    if let Some(tsv_path) = options.anime_remediation_tsv_path {
        write_anime_remediation_tsv(tsv_path, &report)?;
    }

    match options.output_format {
        OutputFormat::Json => {
            if options.pretty {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e))
                );
            } else {
                println!("{}", serde_json::to_string(&report).unwrap_or_default());
            }
        }
        OutputFormat::Text => emit_text_report(&report, options.anime_remediation_tsv_path),
    }

    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn build_anime_remediation_report(
    cfg: &Config,
    db: &Database,
    plex_db_path: &Path,
    full: bool,
) -> Result<Option<AnimeRemediationReportOutput>> {
    let anime_libraries: Vec<&LibraryConfig> = cfg
        .libraries
        .iter()
        .filter(|lib| lib.content_type == Some(ContentType::Anime))
        .collect();
    if anime_libraries.is_empty() {
        return Ok(None);
    }

    let roots: Vec<PathBuf> = anime_libraries.iter().map(|lib| lib.path.clone()).collect();
    let link_records = db.get_links_scoped(Some(&roots)).await?;
    let generated_at = Utc::now().to_rfc3339();
    let anime_duplicates =
        build_anime_duplicate_audit(&anime_libraries, &link_records, Some(plex_db_path), full)
            .await?;
    let Some(anime_duplicates) = anime_duplicates else {
        return Ok(None);
    };

    Ok(Some(AnimeRemediationReportOutput {
        generated_at,
        filesystem_mixed_root_groups: anime_duplicates.filesystem_mixed_root_groups,
        plex_duplicate_show_groups: anime_duplicates.plex_duplicate_show_groups.unwrap_or(0),
        plex_hama_anidb_tvdb_groups: anime_duplicates.plex_hama_anidb_tvdb_groups.unwrap_or(0),
        correlated_hama_split_groups: anime_duplicates.correlated_hama_split_groups.unwrap_or(0),
        remediation_groups: anime_duplicates.remediation_groups.unwrap_or(0),
        returned_groups: anime_duplicates
            .remediation_sample_groups
            .as_ref()
            .map_or(0, Vec::len),
        groups: anime_duplicates
            .remediation_sample_groups
            .unwrap_or_default(),
    }))
}

async fn build_report(
    cfg: &Config,
    db: &Database,
    filter: Option<MediaType>,
    library_filter: Option<&str>,
    plex_db_path: Option<&Path>,
    full_anime_duplicates: bool,
) -> Result<ReportOutput> {
    let selected_libraries = selected_report_libraries(cfg, filter, library_filter);
    let generated_at = Utc::now().to_rfc3339();

    if selected_libraries.is_empty() {
        return Ok(ReportOutput {
            generated_at,
            summary: Summary::default(),
            by_media_type: BTreeMap::new(),
            top_libraries: Vec::new(),
            path_compare: PathCompareOutput::default(),
            anime_duplicates: None,
        });
    }

    let scanner = LibraryScanner::new();
    let mut by_library: HashMap<String, LibraryInfo> = selected_libraries
        .iter()
        .map(|lib| {
            (
                lib.name.clone(),
                LibraryInfo {
                    name: lib.name.clone(),
                    ..LibraryInfo::default()
                },
            )
        })
        .collect();
    let mut by_media_type: BTreeMap<String, MediaTypeInfo> = BTreeMap::new();

    let selected_roots: Vec<_> = selected_libraries
        .iter()
        .map(|lib| lib.path.clone())
        .collect();

    // Run DB query and library scan in parallel using tokio::spawn
    let db_handle = tokio::spawn({
        let db = db.clone();
        let roots = selected_roots.clone();
        async move { db.get_links_scoped(Some(&roots)).await }
    });

    // Library scan is CPU-bound with file I/O, run in parallel with DB query
    let all_library_items: Vec<Vec<LibraryScannerItem>> = selected_libraries
        .par_iter()
        .map(|lib| {
            scanner
                .scan_library(lib)
                .into_iter()
                .map(|item| LibraryScannerItem {
                    library_name: item.library_name,
                    media_type: item.media_type,
                    media_id: item.id.to_string(),
                })
                .collect()
        })
        .collect();

    // Await DB results after library scan has started
    let link_records = db_handle.await??;
    let link_presence = collect_link_presence(&selected_libraries, &link_records);

    let mut summary = Summary::default();
    for library_items in &all_library_items {
        for item in library_items {
            let media_key = media_type_key(item.media_type).to_string();
            let (has_active, has_dead) = link_presence
                .get(&item.library_name)
                .map(|presence| {
                    (
                        presence.active_media_ids.contains(&item.media_id),
                        presence.dead_media_ids.contains(&item.media_id),
                    )
                })
                .unwrap_or((false, false));

            summary.total_library_items += 1;
            by_media_type.entry(media_key).or_default().library_items += 1;
            if let Some(entry) = by_library.get_mut(&item.library_name) {
                entry.items += 1;
            }

            if has_active {
                summary.items_with_symlinks += 1;
                if let Some(entry) = by_library.get_mut(&item.library_name) {
                    entry.linked += 1;
                }
                if let Some(entry) = by_media_type.get_mut(media_type_key(item.media_type)) {
                    entry.linked += 1;
                }
            }

            if has_dead {
                summary.broken_symlinks += 1;
                if let Some(entry) = by_library.get_mut(&item.library_name) {
                    entry.broken += 1;
                }
                if let Some(entry) = by_media_type.get_mut(media_type_key(item.media_type)) {
                    entry.broken += 1;
                }
            }
        }
    }

    summary.missing_from_rd = summary
        .total_library_items
        .saturating_sub(summary.items_with_symlinks);

    let mut top_libraries: Vec<_> = by_library
        .into_values()
        .filter(|lib| lib.items > 0)
        .collect();
    top_libraries.sort_by(|a, b| b.items.cmp(&a.items).then_with(|| a.name.cmp(&b.name)));
    top_libraries.truncate(10);

    let anime_duplicates = build_anime_duplicate_audit(
        &selected_libraries,
        &link_records,
        plex_db_path,
        full_anime_duplicates,
    )
    .await?;
    let path_compare = build_path_compare(
        &selected_libraries,
        &selected_roots,
        &link_records,
        plex_db_path,
    )
    .await?;

    Ok(ReportOutput {
        generated_at,
        summary,
        by_media_type,
        top_libraries,
        path_compare,
        anime_duplicates,
    })
}

fn selected_report_libraries<'a>(
    cfg: &'a Config,
    filter: Option<MediaType>,
    library_filter: Option<&str>,
) -> Vec<&'a LibraryConfig> {
    cfg.libraries
        .iter()
        .filter(|lib| filter.is_none_or(|media_type| lib.media_type == media_type))
        .filter(|lib| {
            library_filter.is_none_or(|library_name| lib.name.eq_ignore_ascii_case(library_name))
        })
        .collect()
}

fn collect_link_presence(
    libraries: &[&LibraryConfig],
    link_records: &[crate::models::LinkRecord],
) -> HashMap<String, LinkPresence> {
    let mut presence_by_library: HashMap<String, LinkPresence> = HashMap::new();
    // Pre-fill with all libraries so we get consistent output ordering
    for lib in libraries {
        presence_by_library.entry(lib.name.clone()).or_default();
    }
    for link in link_records {
        // Find the most-specific matching library (longest path prefix)
        let library_name = libraries
            .iter()
            .filter(|lib| link.target_path.starts_with(&lib.path))
            .max_by_key(|lib| lib.path.components().count())
            .map(|lib| lib.name.clone());

        if let Some(name) = library_name {
            let entry = presence_by_library.entry(name).or_default();
            match link.status {
                crate::models::LinkStatus::Active => {
                    entry.active_media_ids.insert(link.media_id.clone());
                }
                crate::models::LinkStatus::Dead => {
                    entry.dead_media_ids.insert(link.media_id.clone());
                }
                crate::models::LinkStatus::Removed => {}
            }
        }
    }
    presence_by_library
}

fn media_type_key(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Movie => "movie",
        MediaType::Tv => "series",
    }
}

async fn build_anime_duplicate_audit(
    libraries: &[&LibraryConfig],
    link_records: &[crate::models::LinkRecord],
    plex_db_path: Option<&Path>,
    full_anime_duplicates: bool,
) -> Result<Option<AnimeDuplicateAuditOutput>> {
    let anime_libraries: Vec<&LibraryConfig> = libraries
        .iter()
        .copied()
        .filter(|lib| lib.content_type == Some(ContentType::Anime))
        .collect();
    if anime_libraries.is_empty() {
        return Ok(None);
    }

    let duplicate_groups = collect_anime_root_duplicate_groups(&anime_libraries);
    let filesystem_mixed_root_groups = duplicate_groups.len();
    let anime_sample_limit = if full_anime_duplicates {
        usize::MAX
    } else {
        PATH_SAMPLE_LIMIT
    };
    let filesystem_sample_groups = duplicate_groups
        .into_iter()
        .take(anime_sample_limit)
        .map(|group| AnimeRootDuplicateSample {
            normalized_title: group.normalized_title,
            tagged_roots: group.tagged_roots,
            untagged_roots: group.untagged_roots,
        })
        .collect();

    let (
        plex_duplicate_show_groups,
        plex_hama_anidb_tvdb_groups,
        plex_other_duplicate_show_groups,
        plex_sample_groups,
        correlated_hama_split_groups,
        correlated_sample_groups,
        remediation_groups,
        remediation_sample_groups,
    ) = if let Some(db_path) = plex_db_path {
        let roots: Vec<PathBuf> = anime_libraries.iter().map(|lib| lib.path.clone()).collect();
        let records = plex_db::load_duplicate_show_records(db_path, &roots).await?;
        let summary = summarize_plex_duplicate_show_records(&records, anime_sample_limit);
        let correlated_groups = correlate_anime_duplicate_groups(
            &collect_anime_root_duplicate_groups(&anime_libraries),
            &summary.all_groups,
        );
        let remediation_groups_all = build_anime_remediation_samples(
            &correlated_groups,
            &collect_anime_root_usage(&anime_libraries, link_records),
        );
        (
            Some(summary.total_groups),
            Some(summary.hama_anidb_tvdb_groups),
            Some(summary.other_groups),
            Some(summary.sample_groups),
            Some(correlated_groups.len()),
            Some(
                correlated_groups
                    .into_iter()
                    .take(anime_sample_limit)
                    .collect(),
            ),
            Some(remediation_groups_all.len()),
            Some(
                remediation_groups_all
                    .into_iter()
                    .take(anime_sample_limit)
                    .collect(),
            ),
        )
    } else {
        (None, None, None, None, None, None, None, None)
    };

    Ok(Some(AnimeDuplicateAuditOutput {
        filesystem_mixed_root_groups,
        filesystem_sample_groups,
        plex_duplicate_show_groups,
        plex_hama_anidb_tvdb_groups,
        plex_other_duplicate_show_groups,
        plex_sample_groups,
        correlated_hama_split_groups,
        correlated_sample_groups,
        remediation_groups,
        remediation_sample_groups,
    }))
}

#[derive(Default)]
struct PlexDuplicateSummary {
    total_groups: usize,
    hama_anidb_tvdb_groups: usize,
    other_groups: usize,
    all_groups: Vec<PlexDuplicateShowSample>,
    sample_groups: Vec<PlexDuplicateShowSample>,
}

fn summarize_plex_duplicate_show_records(
    records: &[plex_db::PlexDuplicateShowRecord],
    sample_limit: usize,
) -> PlexDuplicateSummary {
    #[derive(Default)]
    struct Bucket {
        total_rows: usize,
        live_rows: usize,
        guids: Vec<String>,
        guid_kinds: Vec<String>,
    }

    let mut grouped: BTreeMap<(String, String, Option<i64>), Bucket> = BTreeMap::new();
    for record in records {
        let bucket = grouped
            .entry((
                record.title.clone(),
                record.original_title.clone(),
                record.year,
            ))
            .or_default();
        bucket.total_rows += 1;
        if record.live {
            bucket.live_rows += 1;
        }
        bucket.guids.push(record.guid.clone());
        bucket.guid_kinds.push(record.guid_kind.clone());
    }

    let mut all_groups = Vec::new();
    let mut hama_anidb_tvdb_groups = 0;

    for ((title, original_title, year), bucket) in grouped {
        let mut unique_guids: Vec<String> = bucket.guids.into_iter().collect();
        unique_guids.sort();
        unique_guids.dedup();

        let mut unique_guid_kinds: Vec<String> = bucket.guid_kinds.into_iter().collect();
        unique_guid_kinds.sort();
        unique_guid_kinds.dedup();

        if unique_guid_kinds.iter().any(|kind| kind == "hama-anidb")
            && unique_guid_kinds.iter().any(|kind| kind == "hama-tvdb")
        {
            hama_anidb_tvdb_groups += 1;
        }

        all_groups.push(PlexDuplicateShowSample {
            title,
            original_title,
            year,
            total_rows: bucket.total_rows,
            live_rows: bucket.live_rows,
            deleted_rows: bucket.total_rows.saturating_sub(bucket.live_rows),
            guid_kinds: unique_guid_kinds,
            guids: unique_guids,
        });
    }

    let total_groups = all_groups.len();
    let sample_groups = all_groups.iter().take(sample_limit).cloned().collect();

    PlexDuplicateSummary {
        total_groups,
        hama_anidb_tvdb_groups,
        other_groups: total_groups.saturating_sub(hama_anidb_tvdb_groups),
        all_groups,
        sample_groups,
    }
}

fn correlate_anime_duplicate_groups(
    filesystem_groups: &[crate::anime_roots::AnimeRootDuplicateGroup],
    plex_groups: &[PlexDuplicateShowSample],
) -> Vec<CorrelatedAnimeDuplicateSample> {
    let mut plex_by_title: HashMap<String, Vec<&PlexDuplicateShowSample>> = HashMap::new();
    for group in plex_groups {
        let is_hama_split = group.guid_kinds.iter().any(|kind| kind == "hama-anidb")
            && group.guid_kinds.iter().any(|kind| kind == "hama-tvdb");
        if !is_hama_split {
            continue;
        }

        plex_by_title
            .entry(normalize(&group.title))
            .or_default()
            .push(group);
    }

    let mut correlated = Vec::new();
    for fs_group in filesystem_groups {
        let Some(plex_matches) = plex_by_title.get(&normalize(&fs_group.normalized_title)) else {
            continue;
        };

        for plex_group in plex_matches {
            correlated.push(CorrelatedAnimeDuplicateSample {
                normalized_title: fs_group.normalized_title.clone(),
                tagged_roots: fs_group.tagged_roots.clone(),
                untagged_roots: fs_group.untagged_roots.clone(),
                plex_total_rows: plex_group.total_rows,
                plex_live_rows: plex_group.live_rows,
                plex_deleted_rows: plex_group.deleted_rows,
                plex_guid_kinds: plex_group.guid_kinds.clone(),
                plex_guids: plex_group.guids.clone(),
            });
        }
    }

    correlated.sort_by(|a, b| {
        b.untagged_roots
            .len()
            .cmp(&a.untagged_roots.len())
            .then_with(|| a.normalized_title.cmp(&b.normalized_title))
    });
    correlated
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AnimeRootUsage {
    filesystem_symlinks: usize,
    db_active_links: usize,
}

fn collect_anime_root_usage(
    anime_libraries: &[&LibraryConfig],
    link_records: &[crate::models::LinkRecord],
) -> HashMap<PathBuf, AnimeRootUsage> {
    let mut by_root: HashMap<PathBuf, AnimeRootUsage> = HashMap::new();

    for library in anime_libraries {
        for entry in WalkDir::new(&library.path).follow_links(false) {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_symlink() {
                continue;
            }
            let Some(root) = anime_show_root_for_path(anime_libraries, entry.path()) else {
                continue;
            };
            by_root.entry(root).or_default().filesystem_symlinks += 1;
        }
    }

    for link in link_records
        .iter()
        .filter(|link| link.status == crate::models::LinkStatus::Active)
    {
        let Some(root) = anime_show_root_for_path(anime_libraries, &link.target_path) else {
            continue;
        };
        by_root.entry(root).or_default().db_active_links += 1;
    }

    by_root
}

fn anime_show_root_for_path(anime_libraries: &[&LibraryConfig], path: &Path) -> Option<PathBuf> {
    let library = anime_libraries
        .iter()
        .filter(|lib| path.starts_with(&lib.path))
        .max_by_key(|lib| lib.path.components().count())?;
    let first_component = path.strip_prefix(&library.path).ok()?.components().next()?;
    Some(library.path.join(first_component.as_os_str()))
}

fn build_anime_remediation_samples(
    correlated_groups: &[CorrelatedAnimeDuplicateSample],
    root_usage: &HashMap<PathBuf, AnimeRootUsage>,
) -> Vec<AnimeRemediationSample> {
    let mut samples = Vec::new();

    for group in correlated_groups {
        let mut tagged_roots: Vec<_> = group
            .tagged_roots
            .iter()
            .map(|path| AnimeRootUsageSample {
                path: path.clone(),
                filesystem_symlinks: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.filesystem_symlinks),
                db_active_links: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.db_active_links),
            })
            .collect();
        tagged_roots.sort_by(compare_root_usage);

        let Some(recommended_tagged_root) = tagged_roots.first().cloned() else {
            continue;
        };

        let mut legacy_roots: Vec<_> = group
            .untagged_roots
            .iter()
            .map(|path| AnimeRootUsageSample {
                path: path.clone(),
                filesystem_symlinks: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.filesystem_symlinks),
                db_active_links: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.db_active_links),
            })
            .collect();
        legacy_roots.sort_by(compare_root_usage);

        samples.push(AnimeRemediationSample {
            normalized_title: group.normalized_title.clone(),
            recommended_tagged_root,
            alternate_tagged_roots: tagged_roots.into_iter().skip(1).collect(),
            legacy_roots,
            plex_total_rows: group.plex_total_rows,
            plex_live_rows: group.plex_live_rows,
            plex_deleted_rows: group.plex_deleted_rows,
            plex_guid_kinds: group.plex_guid_kinds.clone(),
            plex_guids: group.plex_guids.clone(),
        });
    }

    samples.sort_by(|left, right| {
        remediation_impact(right)
            .cmp(&remediation_impact(left))
            .then_with(|| left.normalized_title.cmp(&right.normalized_title))
    });
    samples
}

fn compare_root_usage(
    left: &AnimeRootUsageSample,
    right: &AnimeRootUsageSample,
) -> std::cmp::Ordering {
    right
        .db_active_links
        .cmp(&left.db_active_links)
        .then_with(|| right.filesystem_symlinks.cmp(&left.filesystem_symlinks))
        .then_with(|| left.path.cmp(&right.path))
}

fn remediation_impact(sample: &AnimeRemediationSample) -> (usize, usize, usize) {
    let legacy_fs = sample
        .legacy_roots
        .iter()
        .map(|root| root.filesystem_symlinks)
        .sum();
    let legacy_db = sample
        .legacy_roots
        .iter()
        .map(|root| root.db_active_links)
        .sum();
    (legacy_fs, legacy_db, sample.plex_live_rows)
}

fn write_anime_remediation_tsv(path: &Path, report: &ReportOutput) -> Result<()> {
    let Some(anime_duplicates) = &report.anime_duplicates else {
        anyhow::bail!("Anime remediation TSV export requires an anime library selection");
    };
    let Some(samples) = &anime_duplicates.remediation_sample_groups else {
        anyhow::bail!(
            "Anime remediation TSV export requires --plex-db so correlated groups can be resolved"
        );
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut out = String::from(
        "normalized_title\tlegacy_root_paths\tlegacy_filesystem_symlinks\tlegacy_db_active_links\trecommended_tagged_root\trecommended_tagged_root_fs\trecommended_tagged_root_db\tplex_live_rows\tplex_deleted_rows\tplex_guid_kinds\tplex_guids\n",
    );

    for sample in samples {
        let legacy_paths = sample
            .legacy_roots
            .iter()
            .map(|root| root.path.display().to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        let legacy_fs: usize = sample
            .legacy_roots
            .iter()
            .map(|root| root.filesystem_symlinks)
            .sum();
        let legacy_db: usize = sample
            .legacy_roots
            .iter()
            .map(|root| root.db_active_links)
            .sum();
        let guid_kinds = sample.plex_guid_kinds.join(" | ");
        let guids = sample.plex_guids.join(" | ");

        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            sample.normalized_title.replace('\t', " "),
            legacy_paths.replace('\t', " "),
            legacy_fs,
            legacy_db,
            sample
                .recommended_tagged_root
                .path
                .display()
                .to_string()
                .replace('\t', " "),
            sample.recommended_tagged_root.filesystem_symlinks,
            sample.recommended_tagged_root.db_active_links,
            sample.plex_live_rows,
            sample.plex_deleted_rows,
            guid_kinds.replace('\t', " "),
            guids.replace('\t', " "),
        ));
    }

    std::fs::write(path, out)?;
    Ok(())
}

fn emit_text_report(report: &ReportOutput, anime_remediation_tsv_path: Option<&Path>) {
    println!();
    panel_border('╔', '═', '╗');
    panel_title("Symlinkarr Report");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Total library items:", report.summary.total_library_items);
    panel_kv_row("  Items with symlinks:", report.summary.items_with_symlinks);
    panel_kv_row("  Items with dead links:", report.summary.broken_symlinks);
    panel_kv_row("  Missing from RD:", report.summary.missing_from_rd);

    if !report.by_media_type.is_empty() {
        panel_border('╠', '═', '╣');
        panel_title("By Media Type");
        panel_border('╠', '═', '╣');
        for (media_type, info) in &report.by_media_type {
            let label = format!("  {}:", capitalize(media_type));
            let value = format!(
                "{} items ({} linked, {} broken)",
                info.library_items, info.linked, info.broken
            );
            panel_kv_row(&label, value);
        }
    }

    if !report.top_libraries.is_empty() {
        panel_border('╠', '═', '╣');
        panel_title("Top Libraries");
        panel_border('╠', '═', '╣');
        for lib in &report.top_libraries {
            let label = format!("  {}:", lib.name);
            panel_kv_row(
                &label,
                format!(
                    "{} items ({} linked, {} broken)",
                    lib.items, lib.linked, lib.broken
                ),
            );
        }
    }

    panel_border('╠', '═', '╣');
    panel_title("Path Compare");
    panel_border('╠', '═', '╣');
    panel_kv_row(
        "  Filesystem symlinks:",
        report.path_compare.filesystem_symlinks,
    );
    panel_kv_row("  DB active links:", report.path_compare.db_active_links);
    if let Some(plex_count) = report.path_compare.plex_indexed_files {
        panel_kv_row("  Plex indexed files:", plex_count);
    }
    if let Some(plex_deleted) = report.path_compare.plex_deleted_paths {
        panel_kv_row("  Plex deleted-only paths:", plex_deleted);
    }
    panel_kv_row("  FS not in DB:", report.path_compare.fs_not_in_db.count);
    panel_kv_row("  DB not on FS:", report.path_compare.db_not_on_fs.count);
    if let Some(sample) = &report.path_compare.fs_not_in_plex {
        panel_kv_row("  FS not in Plex:", sample.count);
    }
    if let Some(sample) = &report.path_compare.db_not_in_plex {
        panel_kv_row("  DB not in Plex:", sample.count);
    }
    if let Some(sample) = &report.path_compare.plex_not_on_fs {
        panel_kv_row("  Plex not on FS:", sample.count);
    }
    if let Some(sample) = &report.path_compare.plex_deleted_and_known_missing_source {
        panel_kv_row("  Plex del + src missing:", sample.count);
    }
    if let Some(sample) = &report
        .path_compare
        .plex_deleted_without_known_missing_source
    {
        panel_kv_row("  Plex del, src intact:", sample.count);
    }
    if let Some(all_three) = report.path_compare.all_three {
        panel_kv_row("  In all three:", all_three);
    }

    if let Some(anime_duplicates) = &report.anime_duplicates {
        panel_border('╠', '═', '╣');
        panel_title("Anime Duplicates");
        panel_border('╠', '═', '╣');
        panel_kv_row(
            "  Mixed roots:",
            anime_duplicates.filesystem_mixed_root_groups,
        );
        if let Some(groups) = anime_duplicates.plex_duplicate_show_groups {
            panel_kv_row("  Plex dup groups:", groups);
        }
        if let Some(groups) = anime_duplicates.plex_hama_anidb_tvdb_groups {
            panel_kv_row("  HAMA split groups:", groups);
        }
        if let Some(groups) = anime_duplicates.plex_other_duplicate_show_groups {
            panel_kv_row("  Other dup groups:", groups);
        }
        if let Some(groups) = anime_duplicates.correlated_hama_split_groups {
            panel_kv_row("  Correlated HAMA+FS:", groups);
        }
        if let Some(groups) = anime_duplicates.remediation_groups {
            panel_kv_row("  Remediation groups:", groups);
        }

        if !anime_duplicates.filesystem_sample_groups.is_empty() {
            println!("  Sample mixed roots:");
            for sample in &anime_duplicates.filesystem_sample_groups {
                println!("    - {}", sample.normalized_title);
                if let Some(path) = sample.untagged_roots.first() {
                    println!("      legacy: {}", path.display());
                }
                if let Some(path) = sample.tagged_roots.first() {
                    println!("      tagged: {}", path.display());
                }
            }
        }

        if let Some(samples) = &anime_duplicates.plex_sample_groups {
            if !samples.is_empty() {
                println!("  Sample Plex duplicate groups:");
                for sample in samples {
                    let year = sample
                        .year
                        .map(|year| format!(" ({year})"))
                        .unwrap_or_default();
                    let guid_kinds = sample.guid_kinds.join(", ");
                    println!(
                        "    - {}{} [{} total, {} live, {} deleted] <{}>",
                        sample.title,
                        year,
                        sample.total_rows,
                        sample.live_rows,
                        sample.deleted_rows,
                        guid_kinds
                    );
                }
            }
        }

        if let Some(samples) = &anime_duplicates.correlated_sample_groups {
            if !samples.is_empty() {
                println!("  Sample correlated duplicate groups:");
                for sample in samples {
                    let guid_kinds = sample.plex_guid_kinds.join(", ");
                    println!(
                        "    - {} [{} total, {} live, {} deleted] <{}>",
                        sample.normalized_title,
                        sample.plex_total_rows,
                        sample.plex_live_rows,
                        sample.plex_deleted_rows,
                        guid_kinds
                    );
                    if let Some(path) = sample.untagged_roots.first() {
                        println!("      legacy: {}", path.display());
                    }
                    if let Some(path) = sample.tagged_roots.first() {
                        println!("      tagged: {}", path.display());
                    }
                }
            }
        }

        if let Some(samples) = &anime_duplicates.remediation_sample_groups {
            if !samples.is_empty() {
                println!("  Sample remediation plan:");
                for sample in samples {
                    let legacy_fs: usize = sample
                        .legacy_roots
                        .iter()
                        .map(|root| root.filesystem_symlinks)
                        .sum();
                    let legacy_db: usize = sample
                        .legacy_roots
                        .iter()
                        .map(|root| root.db_active_links)
                        .sum();
                    println!(
                        "    - {} [legacy fs={}, legacy db={}]",
                        sample.normalized_title, legacy_fs, legacy_db
                    );
                    println!(
                        "      keep: {} (fs={}, db={})",
                        sample.recommended_tagged_root.path.display(),
                        sample.recommended_tagged_root.filesystem_symlinks,
                        sample.recommended_tagged_root.db_active_links
                    );
                    if let Some(root) = sample.legacy_roots.first() {
                        println!(
                            "      legacy: {} (fs={}, db={})",
                            root.path.display(),
                            root.filesystem_symlinks,
                            root.db_active_links
                        );
                    }
                    if !sample.alternate_tagged_roots.is_empty() {
                        println!(
                            "      alt tagged roots: {}",
                            sample.alternate_tagged_roots.len()
                        );
                    }
                }
            }
        }
    }

    panel_border('╚', '═', '╝');
    if let Some(path) = anime_remediation_tsv_path {
        println!("  Anime remediation TSV: {}", path.display());
    }
    println!();
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(c).collect(),
    }
}

mod path_compare;
#[cfg(test)]
mod tests;
