use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;

use crate::OutputFormat;
use crate::commands::{panel_border, panel_kv_row, panel_title};
use crate::config::{Config, LibraryConfig};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::models::MediaType;

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
    pretty: bool,
) -> Result<()> {
    let report = build_report(cfg, db, filter).await?;

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
) -> Result<ReportOutput> {
    let selected_libraries = selected_report_libraries(cfg, filter);
    let generated_at = Utc::now().to_rfc3339();

    if selected_libraries.is_empty() {
        return Ok(ReportOutput {
            generated_at,
            summary: Summary::default(),
            by_media_type: BTreeMap::new(),
            top_libraries: Vec::new(),
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

    let mut summary = Summary::default();
    for lib in &selected_libraries {
        let library_items = scanner.scan_library(lib);
        for item in library_items {
            let media_key = media_type_key(item.media_type).to_string();
            let media_id = item.id.to_string();
            let status = link_presence
                .get(&item.library_name)
                .map(|presence| {
                    if presence.active_media_ids.contains(&media_id) {
                        ItemLinkStatus::Linked
                    } else if presence.dead_media_ids.contains(&media_id) {
                        ItemLinkStatus::Broken
                    } else {
                        ItemLinkStatus::Missing
                    }
                })
                .unwrap_or(ItemLinkStatus::Missing);

            summary.total_library_items += 1;
            by_media_type.entry(media_key).or_default().library_items += 1;
            if let Some(entry) = by_library.get_mut(&item.library_name) {
                entry.items += 1;
            }

            match status {
                ItemLinkStatus::Linked => {
                    summary.items_with_symlinks += 1;
                    if let Some(entry) = by_library.get_mut(&item.library_name) {
                        entry.linked += 1;
                    }
                    if let Some(entry) = by_media_type.get_mut(media_type_key(item.media_type)) {
                        entry.linked += 1;
                    }
                }
                ItemLinkStatus::Broken => {
                    summary.broken_symlinks += 1;
                    if let Some(entry) = by_library.get_mut(&item.library_name) {
                        entry.broken += 1;
                    }
                    if let Some(entry) = by_media_type.get_mut(media_type_key(item.media_type)) {
                        entry.broken += 1;
                    }
                }
                ItemLinkStatus::Missing => {}
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

    Ok(ReportOutput {
        generated_at,
        summary,
        by_media_type,
        top_libraries,
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
    for link in link_records {
        let Some(library_name) = library_name_for_path(libraries, &link.target_path) else {
            continue;
        };

        let entry = presence_by_library.entry(library_name).or_default();
        match link.status {
            crate::models::LinkStatus::Active => {
                entry.active_media_ids.insert(link.media_id.clone());
                entry.dead_media_ids.remove(&link.media_id);
            }
            crate::models::LinkStatus::Dead => {
                if !entry.active_media_ids.contains(&link.media_id) {
                    entry.dead_media_ids.insert(link.media_id.clone());
                }
            }
            crate::models::LinkStatus::Removed => {}
        }
    }
    presence_by_library
}

fn library_name_for_path(libraries: &[&LibraryConfig], path: &Path) -> Option<String> {
    libraries
        .iter()
        .filter(|lib| path.starts_with(&lib.path))
        .max_by_key(|lib| lib.path.components().count())
        .map(|lib| lib.name.clone())
}

fn media_type_key(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Movie => "movie",
        MediaType::Tv => "series",
    }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemLinkStatus {
    Linked,
    Broken,
    Missing,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, ContentType, DaemonConfig,
        DecypharrConfig, DmmConfig, FeaturesConfig, MatchingConfig, PlexConfig, ProwlarrConfig,
        RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig,
        TautulliConfig, WebConfig,
    };
    use crate::db::Database;
    use crate::models::{LinkRecord, LinkStatus, MediaType};

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

        let report = build_report(&cfg, &db, None).await.unwrap();

        assert_eq!(
            report.summary,
            Summary {
                total_library_items: 4,
                items_with_symlinks: 2,
                broken_symlinks: 1,
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
                broken: 0,
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
                broken: 0,
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

        let report = build_report(&cfg, &db, Some(MediaType::Movie))
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
}
