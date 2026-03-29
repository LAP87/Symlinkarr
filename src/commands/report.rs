use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use rayon::prelude::*;
use serde::Serialize;
use walkdir::WalkDir;

use crate::anime_roots::collect_anime_root_duplicate_groups;
use crate::commands::{panel_border, panel_kv_row, panel_title};
use crate::config::{Config, ContentType, LibraryConfig};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::models::MediaType;
use crate::plex_db;
use crate::utils::normalize;
use crate::OutputFormat;

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

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct PathSample {
    count: usize,
    samples: Vec<PathBuf>,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct PathCompareOutput {
    filesystem_symlinks: usize,
    db_active_links: usize,
    plex_indexed_files: Option<usize>,
    plex_deleted_paths: Option<usize>,
    fs_not_in_db: PathSample,
    db_not_on_fs: PathSample,
    fs_not_in_plex: Option<PathSample>,
    db_not_in_plex: Option<PathSample>,
    plex_not_on_fs: Option<PathSample>,
    plex_deleted_and_known_missing_source: Option<PathSample>,
    plex_deleted_without_known_missing_source: Option<PathSample>,
    all_three: Option<usize>,
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
    live_rows: usize,
    guid_kinds: Vec<String>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct CorrelatedAnimeDuplicateSample {
    normalized_title: String,
    tagged_roots: Vec<PathBuf>,
    untagged_roots: Vec<PathBuf>,
    plex_live_rows: usize,
    plex_guid_kinds: Vec<String>,
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
    pub(crate) pretty: bool,
}

pub(crate) async fn run_report(
    cfg: &Config,
    db: &Database,
    options: ReportOptions<'_>,
) -> Result<()> {
    let report = build_report(
        cfg,
        db,
        options.filter,
        options.library_filter,
        options.plex_db_path,
        options.full_anime_duplicates,
    )
    .await?;

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
        OutputFormat::Text => emit_text_report(&report),
    }

    Ok(())
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

    let anime_duplicates =
        build_anime_duplicate_audit(&selected_libraries, plex_db_path, full_anime_duplicates)
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

async fn build_path_compare(
    libraries: &[&LibraryConfig],
    roots: &[PathBuf],
    link_records: &[crate::models::LinkRecord],
    plex_db_path: Option<&Path>,
) -> Result<PathCompareOutput> {
    // Build db_active_links first (needed regardless)
    let mut db_active_links: HashSet<PathBuf> = HashSet::new();
    let mut known_missing_source_paths: HashSet<PathBuf> = HashSet::new();
    for link in link_records
        .iter()
        .filter(|link| link.status == crate::models::LinkStatus::Active)
    {
        db_active_links.insert(link.target_path.clone());
        if !link.source_path.exists() {
            known_missing_source_paths.insert(link.target_path.clone());
        }
    }

    // Fast path without Plex DB: still compute filesystem vs DB drift, but skip Plex lookups.
    if plex_db_path.is_none() {
        let filesystem_scan = collect_filesystem_symlink_paths(libraries);
        let filesystem_symlinks = filesystem_scan.paths;
        known_missing_source_paths.extend(filesystem_scan.missing_source_paths);

        return Ok(PathCompareOutput {
            filesystem_symlinks: filesystem_symlinks.len(),
            db_active_links: db_active_links.len(),
            plex_indexed_files: None,
            plex_deleted_paths: None,
            fs_not_in_db: sample_difference(&filesystem_symlinks, &db_active_links),
            db_not_on_fs: sample_difference(&db_active_links, &filesystem_symlinks),
            fs_not_in_plex: None,
            db_not_in_plex: None,
            plex_not_on_fs: None,
            plex_deleted_and_known_missing_source: None,
            plex_deleted_without_known_missing_source: None,
            all_three: None,
        });
    }

    let plex_path_records = plex_db::load_path_records(plex_db_path.unwrap(), roots).await?;
    let filesystem_scan = collect_filesystem_symlink_paths(libraries);
    let filesystem_symlinks = filesystem_scan.paths;
    known_missing_source_paths.extend(filesystem_scan.missing_source_paths);

    let plex_indexed_files = plex_path_records
        .iter()
        .map(|record| record.path.clone())
        .collect::<HashSet<_>>();
    let plex_deleted_paths: HashSet<PathBuf> = plex_path_records
        .iter()
        .filter(|record| record.deleted_only)
        .map(|record| record.path.clone())
        .collect();

    let all_three = Some(
        filesystem_symlinks
            .iter()
            .filter(|path| db_active_links.contains(*path) && plex_indexed_files.contains(*path))
            .count(),
    );

    Ok(PathCompareOutput {
        filesystem_symlinks: filesystem_symlinks.len(),
        db_active_links: db_active_links.len(),
        plex_indexed_files: Some(plex_indexed_files.len()),
        plex_deleted_paths: Some(plex_deleted_paths.len()),
        fs_not_in_db: sample_difference(&filesystem_symlinks, &db_active_links),
        db_not_on_fs: sample_difference(&db_active_links, &filesystem_symlinks),
        fs_not_in_plex: Some(sample_difference(&filesystem_symlinks, &plex_indexed_files)),
        db_not_in_plex: Some(sample_difference(&db_active_links, &plex_indexed_files)),
        plex_not_on_fs: Some(sample_difference(&plex_indexed_files, &filesystem_symlinks)),
        plex_deleted_and_known_missing_source: Some(sample_intersection(
            &plex_deleted_paths,
            &known_missing_source_paths,
        )),
        plex_deleted_without_known_missing_source: Some(sample_difference(
            &plex_deleted_paths,
            &known_missing_source_paths,
        )),
        all_three,
    })
}

async fn build_anime_duplicate_audit(
    libraries: &[&LibraryConfig],
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
    ) = if let Some(db_path) = plex_db_path {
        let roots: Vec<PathBuf> = anime_libraries.iter().map(|lib| lib.path.clone()).collect();
        let records = plex_db::load_duplicate_show_records(db_path, &roots).await?;
        let summary = summarize_plex_duplicate_show_records(&records, anime_sample_limit);
        let correlated_groups = correlate_anime_duplicate_groups(
            &collect_anime_root_duplicate_groups(&anime_libraries),
            &summary.all_groups,
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
        )
    } else {
        (None, None, None, None, None, None)
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
        live_rows: usize,
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
        if record.live {
            bucket.live_rows += 1;
        }
        bucket.guid_kinds.push(record.guid_kind.clone());
    }

    let mut all_groups = Vec::new();
    let mut hama_anidb_tvdb_groups = 0;

    for ((title, original_title, year), bucket) in grouped {
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
            live_rows: bucket.live_rows,
            guid_kinds: unique_guid_kinds,
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
                plex_live_rows: plex_group.live_rows,
                plex_guid_kinds: plex_group.guid_kinds.clone(),
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

struct FilesystemSymlinkScan {
    paths: HashSet<PathBuf>,
    missing_source_paths: HashSet<PathBuf>,
}

fn collect_filesystem_symlink_paths(libraries: &[&LibraryConfig]) -> FilesystemSymlinkScan {
    let results: Vec<_> = libraries
        .par_iter()
        .map(|lib| {
            let mut paths = HashSet::new();
            let mut missing_source_paths = HashSet::new();
            for entry in WalkDir::new(&lib.path).follow_links(false) {
                let Ok(entry) = entry else {
                    continue;
                };
                if entry.file_type().is_symlink() {
                    let path = entry.path().to_path_buf();
                    if symlink_source_missing(&path) {
                        missing_source_paths.insert(path.clone());
                    }
                    paths.insert(path);
                }
            }
            (paths, missing_source_paths)
        })
        .collect();

    let mut all_paths = HashSet::new();
    let mut all_missing = HashSet::new();
    for (paths, missing) in results {
        all_paths.extend(paths);
        all_missing.extend(missing);
    }

    FilesystemSymlinkScan {
        paths: all_paths,
        missing_source_paths: all_missing,
    }
}

const PATH_SAMPLE_LIMIT: usize = 10;

fn sample_difference(left: &HashSet<PathBuf>, right: &HashSet<PathBuf>) -> PathSample {
    let mut diff: Vec<PathBuf> = left
        .iter()
        .filter(|path| !right.contains(*path))
        .cloned()
        .collect();
    diff.sort();
    PathSample {
        count: diff.len(),
        samples: diff.into_iter().take(PATH_SAMPLE_LIMIT).collect(),
    }
}

fn sample_intersection(left: &HashSet<PathBuf>, right: &HashSet<PathBuf>) -> PathSample {
    let mut paths: Vec<PathBuf> = left
        .iter()
        .filter(|path| right.contains(*path))
        .cloned()
        .collect();
    paths.sort();
    PathSample {
        count: paths.len(),
        samples: paths.into_iter().take(PATH_SAMPLE_LIMIT).collect(),
    }
}

fn symlink_source_missing(path: &Path) -> bool {
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    let resolved = if target.is_absolute() {
        target
    } else {
        path.parent()
            .map(|parent| parent.join(&target))
            .unwrap_or(target)
    };
    !resolved.exists()
}

fn emit_text_report(report: &ReportOutput) {
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
                        "    - {}{} [{} live rows] <{}>",
                        sample.title, year, sample.live_rows, guid_kinds
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
                        "    - {} [{} live rows] <{}>",
                        sample.normalized_title, sample.plex_live_rows, guid_kinds
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
    }

    panel_border('╚', '═', '╝');
    println!();
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(c).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, ContentType, DaemonConfig,
        DecypharrConfig, DmmConfig, FeaturesConfig, MatchingConfig, PlexConfig, ProwlarrConfig,
        RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig,
        TautulliConfig, WebConfig,
    };
    use crate::db::Database;
    use crate::models::{LinkRecord, LinkStatus, MediaType};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    // === Unit tests for pure helper functions ===

    #[test]
    fn test_media_type_key_movie() {
        assert_eq!(media_type_key(MediaType::Movie), "movie");
    }

    #[test]
    fn test_media_type_key_tv() {
        assert_eq!(media_type_key(MediaType::Tv), "series");
    }

    #[test]
    fn test_sample_difference_left_only() {
        let left: HashSet<PathBuf> = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ]
        .into_iter()
        .collect();
        let right: HashSet<PathBuf> = vec![PathBuf::from("/b")].into_iter().collect();
        let result = sample_difference(&left, &right);
        assert_eq!(result.count, 2);
        assert!(result.samples.contains(&PathBuf::from("/a")));
        assert!(result.samples.contains(&PathBuf::from("/c")));
    }

    #[test]
    fn test_sample_difference_empty() {
        let left: HashSet<PathBuf> = HashSet::new();
        let right: HashSet<PathBuf> = HashSet::new();
        let result = sample_difference(&left, &right);
        assert_eq!(result.count, 0);
        assert!(result.samples.is_empty());
    }

    #[test]
    fn test_sample_difference_identical() {
        let set: HashSet<PathBuf> = vec![PathBuf::from("/x")].into_iter().collect();
        let result = sample_difference(&set, &set);
        assert_eq!(result.count, 0);
        assert!(result.samples.is_empty());
    }

    #[test]
    fn test_sample_intersection() {
        let left: HashSet<PathBuf> = vec![PathBuf::from("/a"), PathBuf::from("/b")]
            .into_iter()
            .collect();
        let right: HashSet<PathBuf> = vec![PathBuf::from("/b"), PathBuf::from("/c")]
            .into_iter()
            .collect();
        let result = sample_intersection(&left, &right);
        assert_eq!(result.count, 1);
        assert_eq!(result.samples, vec![PathBuf::from("/b")]);
    }

    #[test]
    fn test_sample_intersection_empty() {
        let left: HashSet<PathBuf> = vec![PathBuf::from("/a")].into_iter().collect();
        let right: HashSet<PathBuf> = vec![PathBuf::from("/b")].into_iter().collect();
        let result = sample_intersection(&left, &right);
        assert_eq!(result.count, 0);
        assert!(result.samples.is_empty());
    }

    #[test]
    fn test_collect_anime_root_duplicates_detects_legacy_and_tagged_pairs() {
        let dir = tempfile::TempDir::new().unwrap();
        let anime = dir.path().join("anime");
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(anime.join("Black Clover")).unwrap();
        std::fs::create_dir_all(anime.join("Black Clover (2017) {tvdb-331753}")).unwrap();
        std::fs::create_dir_all(anime.join("Blue Lock (2022) {tvdb-408629}")).unwrap();

        let library = LibraryConfig {
            name: "Anime".to_string(),
            path: anime.clone(),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        };

        let groups = collect_anime_root_duplicate_groups(&[&library]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].normalized_title, "Black Clover");
        assert_eq!(groups[0].untagged_roots, vec![anime.join("Black Clover")]);
        assert_eq!(
            groups[0].tagged_roots,
            vec![anime.join("Black Clover (2017) {tvdb-331753}")]
        );
    }

    #[test]
    fn test_summarize_plex_duplicate_show_records_counts_hama_splits() {
        let records = [
            plex_db::PlexDuplicateShowRecord {
                title: "Show A".to_string(),
                original_title: String::new(),
                year: Some(2024),
                guid: "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
                guid_kind: "hama-anidb".to_string(),
                live: true,
            },
            plex_db::PlexDuplicateShowRecord {
                title: "Show A".to_string(),
                original_title: String::new(),
                year: Some(2024),
                guid: "com.plexapp.agents.hama://tvdb-200?lang=en".to_string(),
                guid_kind: "hama-tvdb".to_string(),
                live: true,
            },
            plex_db::PlexDuplicateShowRecord {
                title: "Show B".to_string(),
                original_title: String::new(),
                year: Some(2025),
                guid: "com.plexapp.agents.hama://tvdb-201?lang=en".to_string(),
                guid_kind: "hama-tvdb".to_string(),
                live: true,
            },
            plex_db::PlexDuplicateShowRecord {
                title: "Show B".to_string(),
                original_title: String::new(),
                year: Some(2025),
                guid: "com.plexapp.agents.hama://tvdb-202?lang=en".to_string(),
                guid_kind: "hama-tvdb".to_string(),
                live: false,
            },
        ];
        let summary = summarize_plex_duplicate_show_records(&records, PATH_SAMPLE_LIMIT);

        assert_eq!(summary.total_groups, 2);
        assert_eq!(summary.hama_anidb_tvdb_groups, 1);
        assert_eq!(summary.other_groups, 1);
        assert_eq!(summary.all_groups.len(), 2);
        assert_eq!(summary.sample_groups.len(), 2);
        assert_eq!(summary.sample_groups[0].live_rows, 2);
        assert_eq!(summary.sample_groups[1].live_rows, 1);

        let limited = summarize_plex_duplicate_show_records(&records, 1);
        assert_eq!(limited.sample_groups.len(), 1);
        assert_eq!(limited.total_groups, 2);
        assert_eq!(limited.all_groups.len(), 2);
    }

    #[test]
    fn test_correlate_anime_duplicate_groups_matches_hama_split_titles() {
        let filesystem_groups = vec![crate::anime_roots::AnimeRootDuplicateGroup {
            normalized_title: "Show A".to_string(),
            tagged_roots: vec![PathBuf::from("/anime/Show A (2024) {tvdb-1}")],
            untagged_roots: vec![PathBuf::from("/anime/Show A")],
        }];
        let plex_groups = vec![
            PlexDuplicateShowSample {
                title: "Show A".to_string(),
                original_title: String::new(),
                year: Some(2024),
                live_rows: 2,
                guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            },
            PlexDuplicateShowSample {
                title: "Show B".to_string(),
                original_title: String::new(),
                year: Some(2024),
                live_rows: 2,
                guid_kinds: vec!["hama-tvdb".to_string(), "hama-tvdb".to_string()],
            },
        ];

        let correlated = correlate_anime_duplicate_groups(&filesystem_groups, &plex_groups);
        assert_eq!(
            correlated,
            vec![CorrelatedAnimeDuplicateSample {
                normalized_title: "Show A".to_string(),
                tagged_roots: vec![PathBuf::from("/anime/Show A (2024) {tvdb-1}")],
                untagged_roots: vec![PathBuf::from("/anime/Show A")],
                plex_live_rows: 2,
                plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            }]
        );
    }

    #[test]
    fn test_symlink_source_missing_broken() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("broken_link");
        std::os::unix::fs::symlink(std::path::Path::new("/nonexistent_target"), &link_path)
            .unwrap();
        assert!(symlink_source_missing(&link_path));
    }

    #[test]
    fn test_symlink_source_missing_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("target_file");
        std::fs::write(&target, b"content").unwrap();
        let link_path = dir.path().join("valid_link");
        std::os::unix::fs::symlink(&target, &link_path).unwrap();
        assert!(!symlink_source_missing(&link_path));
    }

    #[test]
    fn test_collect_link_presence_active_and_dead() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let series = dir.path().join("series");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&series).unwrap();

        let movie_a = movies.join("Movie A {tmdb-1}");
        let series_b = series.join("Show B {tvdb-2}");
        std::fs::create_dir_all(&movie_a).unwrap();
        std::fs::create_dir_all(&series_b).unwrap();

        let movie_lib = LibraryConfig {
            name: "Movies".to_string(),
            path: movies.clone(),
            media_type: crate::models::MediaType::Movie,
            content_type: None,
            depth: 1,
        };
        let series_lib = LibraryConfig {
            name: "Series".to_string(),
            path: series.clone(),
            media_type: crate::models::MediaType::Tv,
            content_type: None,
            depth: 1,
        };
        let libraries = vec![&movie_lib, &series_lib];

        let link_a = LinkRecord {
            id: None,
            source_path: PathBuf::from("/rd/movie-a.mkv"),
            target_path: movie_a.join("Movie A.mkv"),
            media_id: "tmdb-1".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };
        let link_b = LinkRecord {
            id: None,
            source_path: PathBuf::from("/rd/show-b-s01e01.mkv"),
            target_path: series_b.join("Show B - S01E01.mkv"),
            media_id: "tvdb-2".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Dead,
            created_at: None,
            updated_at: None,
        };
        let link_c = LinkRecord {
            id: None,
            source_path: PathBuf::from("/rd/removed.mkv"),
            target_path: movies.join("Removed.mkv"),
            media_id: "tmdb-99".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Removed,
            created_at: None,
            updated_at: None,
        };

        let link_records = vec![link_a.clone(), link_b.clone(), link_c.clone()];
        let presence = collect_link_presence(&libraries, &link_records);

        // Movies: 1 active, 0 dead
        let movie_presence = presence.get("Movies").unwrap();
        assert!(movie_presence.active_media_ids.contains("tmdb-1"));
        assert!(!movie_presence.active_media_ids.contains("tmdb-99")); // Removed is not active
        assert!(!movie_presence.dead_media_ids.contains("tmdb-1"));
        assert!(!movie_presence.dead_media_ids.contains("tmdb-99"));

        // Series: 0 active, 1 dead
        let series_presence = presence.get("Series").unwrap();
        assert!(series_presence.active_media_ids.is_empty());
        assert!(series_presence.dead_media_ids.contains("tvdb-2"));

        // All libraries pre-filled even with no links
        assert!(presence.contains_key("Movies"));
        assert!(presence.contains_key("Series"));
    }

    #[test]
    fn test_collect_link_presence_unknown_library() {
        // Link to a path not under any configured library should be ignored
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        std::fs::create_dir_all(&movies).unwrap();

        let movie_lib = LibraryConfig {
            name: "Movies".to_string(),
            path: movies.clone(),
            media_type: crate::models::MediaType::Movie,
            content_type: None,
            depth: 1,
        };
        let libraries = vec![&movie_lib];

        // Link pointing to /somewhere/else entirely
        let orphan_link = LinkRecord {
            id: None,
            source_path: PathBuf::from("/rd/unknown.mkv"),
            target_path: PathBuf::from("/unknown/path/link.mkv"),
            media_id: "tmdb-999".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };

        let presence = collect_link_presence(&libraries, &[orphan_link]);
        // Should still have Movies pre-filled
        let movie_presence = presence.get("Movies").unwrap();
        assert!(movie_presence.active_media_ids.is_empty());
        assert!(movie_presence.dead_media_ids.is_empty());
    }

    fn test_config(movies: PathBuf, anime: PathBuf, source: PathBuf, db_path: String) -> Config {
        Config {
            libraries: vec![
                LibraryConfig {
                    name: "Movies".to_string(),
                    path: movies,
                    media_type: MediaType::Movie,
                    content_type: Some(ContentType::Movie),
                    depth: 1,
                },
                LibraryConfig {
                    name: "Anime".to_string(),
                    path: anime,
                    media_type: MediaType::Tv,
                    content_type: Some(ContentType::Anime),
                    depth: 1,
                },
            ],
            sources: vec![SourceConfig {
                name: "RD".to_string(),
                path: source,
                media_type: "auto".to_string(),
            }],
            api: ApiConfig::default(),
            realdebrid: RealDebridConfig::default(),
            decypharr: DecypharrConfig::default(),
            dmm: DmmConfig::default(),
            backup: BackupConfig::default(),
            db_path,
            log_level: "info".to_string(),
            daemon: DaemonConfig::default(),
            symlink: SymlinkConfig::default(),
            matching: MatchingConfig::default(),
            prowlarr: ProwlarrConfig::default(),
            bazarr: BazarrConfig::default(),
            tautulli: TautulliConfig::default(),
            plex: PlexConfig::default(),
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

    async fn create_test_plex_db(
        db_path: &Path,
        movie_root: &Path,
        anime_root: &Path,
        indexed_paths: &[PathBuf],
    ) {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        for statement in [
            "CREATE TABLE section_locations (id INTEGER PRIMARY KEY, library_section_id INTEGER, root_path TEXT, available BOOLEAN, scanned_at INTEGER, created_at INTEGER, updated_at INTEGER)",
            "CREATE TABLE metadata_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, metadata_type INTEGER, title TEXT, original_title TEXT, year INTEGER, guid TEXT, deleted_at INTEGER)",
            "CREATE TABLE media_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, section_location_id INTEGER, metadata_item_id INTEGER, deleted_at INTEGER)",
            "CREATE TABLE media_parts (id INTEGER PRIMARY KEY, media_item_id INTEGER, file TEXT, deleted_at INTEGER)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }

        sqlx::query("INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?), (2, 2, ?)")
            .bind(movie_root.to_string_lossy().to_string())
            .bind(anime_root.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();

        for (idx, path) in indexed_paths.iter().enumerate() {
            let media_item_id = (idx + 1) as i64;
            let metadata_item_id = media_item_id;
            let section_location_id = if path.starts_with(movie_root) { 1 } else { 2 };
            let library_section_id = section_location_id;
            sqlx::query(
                "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at) VALUES (?, ?, 4, '', '', NULL, '', NULL)",
            )
            .bind(metadata_item_id)
            .bind(library_section_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO media_items (id, library_section_id, section_location_id, metadata_item_id, deleted_at) VALUES (?, ?, ?, ?, NULL)",
            )
            .bind(media_item_id)
            .bind(library_section_id)
            .bind(section_location_id)
            .bind(metadata_item_id)
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "INSERT INTO media_parts (id, media_item_id, file, deleted_at) VALUES (?, ?, ?, NULL)",
            )
            .bind(media_item_id)
            .bind(media_item_id)
            .bind(path.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        }

        pool.close().await;
    }

    async fn mark_test_plex_path_deleted(db_path: &Path, path: &Path) {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        sqlx::query("UPDATE media_parts SET deleted_at = 1 WHERE file = ?")
            .bind(path.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();

        pool.close().await;
    }

    #[tokio::test]
    async fn test_build_report_groups_by_library_and_media_type() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        std::fs::create_dir_all(movies.join("Movie A {tmdb-1}")).unwrap();
        std::fs::create_dir_all(movies.join("Movie B {tmdb-2}")).unwrap();
        std::fs::create_dir_all(anime.join("Show A {tvdb-10}")).unwrap();
        std::fs::create_dir_all(anime.join("Show B {tvdb-11}")).unwrap();

        let db_path = dir.path().join("test.db");
        let cfg = test_config(
            movies.clone(),
            anime.clone(),
            source.clone(),
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("movie-a.mkv"),
            target_path: movies.join("Movie A {tmdb-1}/Movie A.mkv"),
            media_id: "tmdb-1".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("movie-b.mkv"),
            target_path: movies.join("Movie B {tmdb-2}/Movie B.mkv"),
            media_id: "tmdb-2".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Dead,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("show-a.mkv"),
            target_path: anime.join("Show A {tvdb-10}/Season 01/Show A - S01E01.mkv"),
            media_id: "tvdb-10".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("show-a-stale.mkv"),
            target_path: anime.join("Show A {tvdb-10}/Season 01/Show A - S01E02.mkv"),
            media_id: "tvdb-10".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Dead,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let report = build_report(&cfg, &db, None, None, None, false)
            .await
            .unwrap();

        assert_eq!(
            report.summary,
            Summary {
                total_library_items: 4,
                items_with_symlinks: 2,
                broken_symlinks: 2,
                missing_from_rd: 2,
            }
        );
        assert_eq!(
            report.by_media_type.get("movie"),
            Some(&MediaTypeInfo {
                library_items: 2,
                linked: 1,
                broken: 1,
            })
        );
        assert_eq!(
            report.by_media_type.get("series"),
            Some(&MediaTypeInfo {
                library_items: 2,
                linked: 1,
                broken: 1,
            })
        );
        assert_eq!(report.top_libraries.len(), 2);
        assert!(report.top_libraries.iter().any(|lib| lib
            == &LibraryInfo {
                name: "Movies".to_string(),
                items: 2,
                linked: 1,
                broken: 1,
            }));
        assert!(report.top_libraries.iter().any(|lib| lib
            == &LibraryInfo {
                name: "Anime".to_string(),
                items: 2,
                linked: 1,
                broken: 1,
            }));
    }

    #[tokio::test]
    async fn test_build_report_applies_media_type_filter() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        std::fs::create_dir_all(movies.join("Movie A {tmdb-1}")).unwrap();
        std::fs::create_dir_all(anime.join("Show A {tvdb-10}")).unwrap();

        let db_path = dir.path().join("test.db");
        let cfg = test_config(
            movies.clone(),
            anime.clone(),
            source.clone(),
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("movie-a.mkv"),
            target_path: movies.join("Movie A {tmdb-1}/Movie A.mkv"),
            media_id: "tmdb-1".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("show-a.mkv"),
            target_path: anime.join("Show A {tvdb-10}/Season 01/Show A - S01E01.mkv"),
            media_id: "tvdb-10".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let report = build_report(&cfg, &db, Some(MediaType::Movie), None, None, false)
            .await
            .unwrap();

        assert_eq!(
            report.summary,
            Summary {
                total_library_items: 1,
                items_with_symlinks: 1,
                broken_symlinks: 0,
                missing_from_rd: 0,
            }
        );
        assert_eq!(report.by_media_type.len(), 1);
        assert_eq!(report.top_libraries.len(), 1);
        assert_eq!(report.top_libraries[0].name, "Movies");
    }

    #[tokio::test]
    async fn test_build_report_path_compare_tracks_fs_db_and_plex_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        let movie_a_dir = movies.join("Movie A {tmdb-1}");
        let movie_b_dir = movies.join("Movie B {tmdb-2}");
        let show_a_dir = anime.join("Show A {tvdb-10}/Season 01");
        let show_b_dir = anime.join("Show B {tvdb-11}/Season 01");
        std::fs::create_dir_all(&movie_a_dir).unwrap();
        std::fs::create_dir_all(&movie_b_dir).unwrap();
        std::fs::create_dir_all(&show_a_dir).unwrap();
        std::fs::create_dir_all(&show_b_dir).unwrap();

        let source_a = source.join("movie-a-source.mkv");
        let source_b = source.join("movie-b-source.mkv");
        std::fs::write(&source_a, b"a").unwrap();
        std::fs::write(&source_b, b"b").unwrap();

        let movie_a_link = movie_a_dir.join("Movie A.mkv");
        let movie_b_link = movie_b_dir.join("Movie B.mkv");
        symlink(&source_a, &movie_a_link).unwrap();
        symlink(&source_b, &movie_b_link).unwrap();

        let db_path = dir.path().join("test.db");
        let plex_db_path = dir.path().join("plex.db");
        let cfg = test_config(
            movies.clone(),
            anime.clone(),
            source.clone(),
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let db_only_path = show_a_dir.join("Show A - S01E01.mkv");
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source_a.clone(),
            target_path: movie_a_link.clone(),
            media_id: "tmdb-1".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("show-a-source.mkv"),
            target_path: db_only_path.clone(),
            media_id: "tvdb-10".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let plex_only_path = show_b_dir.join("Show B - S01E01.mkv");
        create_test_plex_db(
            &plex_db_path,
            &movies,
            &anime,
            &[movie_a_link.clone(), plex_only_path.clone()],
        )
        .await;

        let report = build_report(&cfg, &db, None, None, Some(&plex_db_path), false)
            .await
            .unwrap();

        assert_eq!(report.path_compare.filesystem_symlinks, 2);
        assert_eq!(report.path_compare.db_active_links, 2);
        assert_eq!(report.path_compare.plex_indexed_files, Some(2));
        assert_eq!(report.path_compare.plex_deleted_paths, Some(0));
        assert_eq!(report.path_compare.fs_not_in_db.count, 1);
        assert_eq!(report.path_compare.db_not_on_fs.count, 1);
        assert_eq!(
            report.path_compare.fs_not_in_plex.as_ref().unwrap().count,
            1
        );
        assert_eq!(
            report.path_compare.db_not_in_plex.as_ref().unwrap().count,
            1
        );
        assert_eq!(
            report.path_compare.plex_not_on_fs.as_ref().unwrap().count,
            1
        );
        assert_eq!(
            report
                .path_compare
                .plex_deleted_and_known_missing_source
                .as_ref()
                .unwrap()
                .count,
            0
        );
        assert_eq!(
            report
                .path_compare
                .plex_deleted_without_known_missing_source
                .as_ref()
                .unwrap()
                .count,
            0
        );
        assert_eq!(report.path_compare.all_three, Some(1));

        assert_eq!(report.path_compare.fs_not_in_db.samples, vec![movie_b_link]);
        assert_eq!(report.path_compare.db_not_on_fs.samples, vec![db_only_path]);
        assert_eq!(
            report.path_compare.plex_not_on_fs.as_ref().unwrap().samples,
            vec![plex_only_path]
        );
    }

    #[tokio::test]
    async fn test_build_report_path_compare_without_plex_db_still_tracks_fs_db_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        let movie_a_dir = movies.join("Movie A {tmdb-1}");
        let movie_b_dir = movies.join("Movie B {tmdb-2}");
        let show_a_dir = anime.join("Show A {tvdb-10}/Season 01");
        std::fs::create_dir_all(&movie_a_dir).unwrap();
        std::fs::create_dir_all(&movie_b_dir).unwrap();
        std::fs::create_dir_all(&show_a_dir).unwrap();

        let source_a = source.join("movie-a-source.mkv");
        let source_b = source.join("movie-b-source.mkv");
        std::fs::write(&source_a, b"a").unwrap();
        std::fs::write(&source_b, b"b").unwrap();

        let movie_a_link = movie_a_dir.join("Movie A.mkv");
        let movie_b_link = movie_b_dir.join("Movie B.mkv");
        symlink(&source_a, &movie_a_link).unwrap();
        symlink(&source_b, &movie_b_link).unwrap();

        let db_path = dir.path().join("test.db");
        let cfg = test_config(
            movies.clone(),
            anime.clone(),
            source.clone(),
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let db_only_path = show_a_dir.join("Show A - S01E01.mkv");
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source_a.clone(),
            target_path: movie_a_link.clone(),
            media_id: "tmdb-1".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();
        db.insert_link(&LinkRecord {
            id: None,
            source_path: source.join("show-a-source.mkv"),
            target_path: db_only_path.clone(),
            media_id: "tvdb-10".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        let report = build_report(&cfg, &db, None, None, None, false)
            .await
            .unwrap();

        assert_eq!(report.path_compare.filesystem_symlinks, 2);
        assert_eq!(report.path_compare.db_active_links, 2);
        assert_eq!(report.path_compare.plex_indexed_files, None);
        assert_eq!(report.path_compare.plex_deleted_paths, None);
        assert_eq!(report.path_compare.fs_not_in_db.count, 1);
        assert_eq!(report.path_compare.db_not_on_fs.count, 1);
        assert_eq!(report.path_compare.fs_not_in_db.samples, vec![movie_b_link]);
        assert_eq!(report.path_compare.db_not_on_fs.samples, vec![db_only_path]);
        assert_eq!(report.path_compare.fs_not_in_plex, None);
        assert_eq!(report.path_compare.db_not_in_plex, None);
        assert_eq!(report.path_compare.plex_not_on_fs, None);
        assert_eq!(report.path_compare.all_three, None);
    }

    #[tokio::test]
    async fn test_build_report_path_compare_does_not_treat_plex_deleted_as_truth_without_missing_source(
    ) {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        let movie_a_dir = movies.join("Movie A {tmdb-1}");
        std::fs::create_dir_all(&movie_a_dir).unwrap();
        let source_a = source.join("movie-a-source.mkv");
        std::fs::write(&source_a, b"a").unwrap();
        let movie_a_link = movie_a_dir.join("Movie A.mkv");
        symlink(&source_a, &movie_a_link).unwrap();

        let db_path = dir.path().join("test.db");
        let plex_db_path = dir.path().join("plex.db");
        let cfg = test_config(
            movies.clone(),
            anime.clone(),
            source.clone(),
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: source_a.clone(),
            target_path: movie_a_link.clone(),
            media_id: "tmdb-1".to_string(),
            media_type: MediaType::Movie,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        create_test_plex_db(
            &plex_db_path,
            &movies,
            &anime,
            std::slice::from_ref(&movie_a_link),
        )
        .await;
        mark_test_plex_path_deleted(&plex_db_path, &movie_a_link).await;

        let report = build_report(&cfg, &db, None, None, Some(&plex_db_path), false)
            .await
            .unwrap();

        assert_eq!(report.path_compare.plex_indexed_files, Some(1));
        assert_eq!(report.path_compare.plex_deleted_paths, Some(1));
        assert_eq!(
            report
                .path_compare
                .plex_deleted_and_known_missing_source
                .as_ref()
                .unwrap()
                .count,
            0
        );
        assert_eq!(
            report
                .path_compare
                .plex_deleted_without_known_missing_source
                .as_ref()
                .unwrap()
                .count,
            1
        );
        assert_eq!(
            report
                .path_compare
                .plex_deleted_without_known_missing_source
                .as_ref()
                .unwrap()
                .samples,
            vec![movie_a_link]
        );
    }

    #[tokio::test]
    async fn test_build_report_includes_anime_duplicate_audit_without_plex_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        std::fs::create_dir_all(anime.join("Aldnoah.Zero")).unwrap();
        std::fs::create_dir_all(anime.join("Aldnoah.Zero (2014) {tvdb-279827}")).unwrap();
        std::fs::create_dir_all(anime.join("Blue Lock (2022) {tvdb-408629}")).unwrap();

        let db_path = dir.path().join("test.db");
        let cfg = test_config(
            movies,
            anime.clone(),
            source,
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let report = build_report(&cfg, &db, Some(MediaType::Tv), None, None, false)
            .await
            .unwrap();

        let anime_duplicates = report.anime_duplicates.expect("anime audit present");
        assert_eq!(anime_duplicates.filesystem_mixed_root_groups, 1);
        assert_eq!(anime_duplicates.filesystem_sample_groups.len(), 1);
        assert_eq!(
            anime_duplicates.filesystem_sample_groups[0].normalized_title,
            "Aldnoah.Zero"
        );
        assert_eq!(anime_duplicates.plex_duplicate_show_groups, None);
        assert_eq!(anime_duplicates.correlated_hama_split_groups, None);
        assert_eq!(anime_duplicates.correlated_sample_groups, None);
    }

    #[tokio::test]
    async fn test_build_report_full_anime_duplicates_disables_sample_cap() {
        let dir = tempfile::TempDir::new().unwrap();
        let movies = dir.path().join("movies");
        let anime = dir.path().join("anime");
        let source = dir.path().join("rd");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(&anime).unwrap();
        std::fs::create_dir_all(&source).unwrap();

        for idx in 0..(PATH_SAMPLE_LIMIT + 3) {
            let title = format!("Show {idx:02}");
            std::fs::create_dir_all(anime.join(&title)).unwrap();
            std::fs::create_dir_all(anime.join(format!("{title} (2024) {{tvdb-{}}}", 1000 + idx)))
                .unwrap();
        }

        let db_path = dir.path().join("test.db");
        let cfg = test_config(
            movies,
            anime,
            source,
            db_path.to_string_lossy().into_owned(),
        );
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        let sampled = build_report(&cfg, &db, Some(MediaType::Tv), None, None, false)
            .await
            .unwrap();
        let full = build_report(&cfg, &db, Some(MediaType::Tv), None, None, true)
            .await
            .unwrap();

        let sampled_duplicates = sampled
            .anime_duplicates
            .expect("sampled anime audit present");
        let full_duplicates = full.anime_duplicates.expect("full anime audit present");

        assert_eq!(
            sampled_duplicates.filesystem_mixed_root_groups,
            PATH_SAMPLE_LIMIT + 3
        );
        assert_eq!(
            sampled_duplicates.filesystem_sample_groups.len(),
            PATH_SAMPLE_LIMIT
        );
        assert_eq!(
            full_duplicates.filesystem_sample_groups.len(),
            PATH_SAMPLE_LIMIT + 3
        );
    }
}
