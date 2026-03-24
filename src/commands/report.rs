use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use rayon::prelude::*;
use serde::Serialize;
use walkdir::WalkDir;

use crate::commands::{panel_border, panel_kv_row, panel_title};
use crate::config::{Config, LibraryConfig};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::models::MediaType;
use crate::plex_db;
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

pub(crate) async fn run_report(
    cfg: &Config,
    db: &Database,
    output_format: OutputFormat,
    filter: Option<MediaType>,
    plex_db_path: Option<&Path>,
    pretty: bool,
) -> Result<()> {
    let report = build_report(cfg, db, filter, plex_db_path).await?;

    match output_format {
        OutputFormat::Json => {
            if pretty {
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
    plex_db_path: Option<&Path>,
) -> Result<ReportOutput> {
    let selected_libraries = selected_report_libraries(cfg, filter);
    let generated_at = Utc::now().to_rfc3339();

    if selected_libraries.is_empty() {
        return Ok(ReportOutput {
            generated_at,
            summary: Summary::default(),
            by_media_type: BTreeMap::new(),
            top_libraries: Vec::new(),
            path_compare: PathCompareOutput::default(),
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
    let link_records = db.get_links_scoped(Some(&selected_roots)).await?;
    let link_presence = collect_link_presence(&selected_libraries, &link_records);

    // Scan all libraries in parallel for best performance
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
    })
}

fn selected_report_libraries(cfg: &Config, filter: Option<MediaType>) -> Vec<&LibraryConfig> {
    cfg.libraries
        .iter()
        .filter(|lib| filter.is_none_or(|media_type| lib.media_type == media_type))
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
    for link in link_records.iter().filter(|link| link.status == crate::models::LinkStatus::Active) {
        db_active_links.insert(link.target_path.clone());
        if !link.source_path.exists() {
            known_missing_source_paths.insert(link.target_path.clone());
        }
    }

    // Load Plex path records first (if requested)
    let plex_path_records = match plex_db_path {
        Some(path) => Some(plex_db::load_path_records(path, roots).await?),
        None => None,
    };

    // Fast path: skip the expensive WalkDir filesystem scan when plex path compare is not requested.
    // This saves ~2-3s on cold runs (228k filesystem entries) for the common CLI case.
    if plex_db_path.is_none() {
        return Ok(PathCompareOutput {
            filesystem_symlinks: 0,
            db_active_links: db_active_links.len(),
            plex_indexed_files: None,
            plex_deleted_paths: None,
            fs_not_in_db: PathSample::default(),
            db_not_on_fs: sample_difference(&db_active_links, &HashSet::new()),
            fs_not_in_plex: None,
            db_not_in_plex: None,
            plex_not_on_fs: None,
            plex_deleted_and_known_missing_source: None,
            plex_deleted_without_known_missing_source: None,
            all_three: None,
        });
    }

    let filesystem_scan = collect_filesystem_symlink_paths(libraries);
    let filesystem_symlinks = filesystem_scan.paths;
    known_missing_source_paths.extend(filesystem_scan.missing_source_paths);

    let plex_path_records = plex_path_records.unwrap();
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
        plex_deleted_and_known_missing_source: Some(sample_intersection(&plex_deleted_paths, &known_missing_source_paths)),
        plex_deleted_without_known_missing_source: Some(sample_difference(&plex_deleted_paths, &known_missing_source_paths)),
        all_three,
    })
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
        panel_kv_row("  Plex deleted + known missing source:", sample.count);
    }
    if let Some(sample) = &report
        .path_compare
        .plex_deleted_without_known_missing_source
    {
        panel_kv_row("  Plex deleted w/o known missing source:", sample.count);
    }
    if let Some(all_three) = report.path_compare.all_three {
        panel_kv_row("  In all three:", all_three);
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
        let left: HashSet<PathBuf> = vec![PathBuf::from("/a"), PathBuf::from("/b")].into_iter().collect();
        let right: HashSet<PathBuf> = vec![PathBuf::from("/b"), PathBuf::from("/c")].into_iter().collect();
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
    fn test_symlink_source_missing_broken() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("broken_link");
        std::os::unix::fs::symlink(
            std::path::Path::new("/nonexistent_target"),
            &link_path,
        )
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
            "CREATE TABLE metadata_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, deleted_at INTEGER)",
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
                "INSERT INTO metadata_items (id, library_section_id, deleted_at) VALUES (?, ?, NULL)",
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

        let report = build_report(&cfg, &db, None, None).await.unwrap();

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

        let report = build_report(&cfg, &db, Some(MediaType::Movie), None)
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

        let report = build_report(&cfg, &db, None, Some(&plex_db_path))
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

        let report = build_report(&cfg, &db, None, Some(&plex_db_path))
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
}
