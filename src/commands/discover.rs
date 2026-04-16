use std::collections::HashMap;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use tracing::info;

use crate::api;
use crate::api::realdebrid::RealDebridClient;
use crate::cache::cached_files_from_db;
use crate::commands::{print_json, selected_libraries};
use crate::config::Config;
use crate::db::Database;
use crate::discovery::{
    DiscoverFolderPlan, DiscoverPlacement, DiscoverPlan, DiscoverSummary, Discovery,
};
use crate::library_scanner::LibraryScanner;
use crate::linker::Linker;
use crate::matcher::{MatchRunOutput, Matcher};
use crate::source_scanner::SourceScanner;
use crate::utils::stdout_text_guard;
use crate::{DiscoverAction, OutputFormat};

pub(crate) struct DiscoverySnapshot {
    pub summary: DiscoverSummary,
    pub folders: Vec<DiscoverFolderPlan>,
    pub items: Vec<DiscoverPlacement>,
    pub status_message: Option<String>,
}

#[derive(Serialize)]
struct FolderSummary<'a> {
    library_name: &'a str,
    media_id: &'a str,
    title: &'a str,
    folder_path: String,
    existing_links: usize,
    planned_creates: usize,
    planned_updates: usize,
    blocked: usize,
}

#[derive(Serialize)]
struct PlacementSummary<'a> {
    library_name: &'a str,
    media_id: &'a str,
    title: &'a str,
    folder_path: String,
    source_path: String,
    source_name: &'a str,
    target_path: String,
    action: &'a str,
    season: Option<u32>,
    episode: Option<u32>,
}

fn cached_only_notice(cfg: &Config) -> Option<String> {
    (!cfg.has_realdebrid()).then(|| {
        "Real-Debrid API key not configured. Showing cached RD results only; live refresh is unavailable until credentials are configured."
            .to_string()
    })
}

fn build_discover_json(snapshot: &DiscoverySnapshot) -> serde_json::Value {
    let folders: Vec<FolderSummary<'_>> = snapshot
        .folders
        .iter()
        .map(|folder| FolderSummary {
            library_name: &folder.library_name,
            media_id: &folder.media_id,
            title: &folder.title,
            folder_path: folder.folder_path.display().to_string(),
            existing_links: folder.existing_links,
            planned_creates: folder.planned_creates,
            planned_updates: folder.planned_updates,
            blocked: folder.blocked,
        })
        .collect();

    let items: Vec<PlacementSummary<'_>> = snapshot
        .items
        .iter()
        .map(|item| PlacementSummary {
            library_name: &item.library_name,
            media_id: &item.media_id,
            title: &item.title,
            folder_path: item.folder_path.display().to_string(),
            source_path: item.source_path.display().to_string(),
            source_name: &item.source_name,
            target_path: item.target_path.display().to_string(),
            action: item.action.as_str(),
            season: item.season,
            episode: item.episode,
        })
        .collect();

    serde_json::json!({
        "summary": {
            "folders": snapshot.summary.folders,
            "placements": snapshot.summary.placements,
            "creates": snapshot.summary.creates,
            "updates": snapshot.summary.updates,
            "blocked": snapshot.summary.blocked,
        },
        "status_message": snapshot.status_message,
        "folders": folders,
        "items": items,
    })
}

fn build_discover_matcher(cfg: &Config) -> Matcher {
    let tmdb = if cfg.has_tmdb() {
        Some(crate::api::tmdb::TmdbClient::new(
            &cfg.api.tmdb_api_key,
            Some(&cfg.api.tmdb_read_access_token),
            cfg.api.cache_ttl_hours,
        ))
    } else {
        None
    };

    let tvdb = if cfg.has_tvdb() {
        Some(crate::api::tvdb::TvdbClient::new(
            &cfg.api.tvdb_api_key,
            cfg.api.cache_ttl_hours,
        ))
    } else {
        None
    };

    Matcher::new(
        tmdb,
        tvdb,
        cfg.matching.mode,
        cfg.matching.metadata_mode,
        cfg.matching.metadata_concurrency,
    )
}

async fn collect_discovery_source_items(
    cfg: &Config,
    db: &Database,
    refresh_cache: bool,
) -> Result<(Vec<crate::models::SourceItem>, Option<String>)> {
    let mut notices = Vec::new();
    if let Some(message) = cached_only_notice(cfg) {
        notices.push(message);
    }

    if refresh_cache && cfg.has_realdebrid() {
        let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
        let cache = crate::cache::TorrentCache::new(db, &rd_client);
        if let Err(e) = cache.sync().await {
            notices.push(format!(
                "RD cache sync failed ({}). Showing cached or on-disk results only.",
                e
            ));
        }
    }

    let scanner = SourceScanner::new();
    let mut by_path = HashMap::new();

    for source in &cfg.sources {
        let cached_files = match cached_files_from_db(db, &source.path).await {
            Ok(files) => files,
            Err(e) => {
                notices.push(format!(
                    "Could not read cached RD files for source {} ({}). Falling back to filesystem scan.",
                    source.name, e
                ));
                Vec::new()
            }
        };

        let mut cached_count = 0usize;
        for (path, _) in cached_files {
            if !path.exists() {
                continue;
            }

            if let Some(item) = scanner.parse_path_for_source(&path, source) {
                cached_count += 1;
                by_path.entry(item.path.clone()).or_insert(item);
            }
        }

        if cached_count == 0 {
            for item in scanner.scan_source(source) {
                by_path.entry(item.path.clone()).or_insert(item);
            }
        }
    }

    let mut items = by_path.into_values().collect::<Vec<_>>();
    items.sort_by(|a, b| a.path.cmp(&b.path));

    Ok((items, (!notices.is_empty()).then(|| notices.join(" "))))
}

pub(crate) async fn load_discovery_snapshot(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    refresh_cache: bool,
) -> Result<DiscoverySnapshot> {
    let discover_started = Instant::now();
    let selected = selected_libraries(cfg, library_filter)?;

    let library_scan_started = Instant::now();
    let lib_scanner = LibraryScanner::new();
    let mut library_items = Vec::new();
    for lib in &selected {
        library_items.extend(lib_scanner.scan_library(lib));
    }
    library_items.sort_by_key(|item| item.title.to_lowercase());
    let library_scan_elapsed = library_scan_started.elapsed();

    let source_collect_started = Instant::now();
    let (source_items, status_message) =
        collect_discovery_source_items(cfg, db, refresh_cache).await?;
    let source_collect_elapsed = source_collect_started.elapsed();

    let matcher = build_discover_matcher(cfg);
    let match_started = Instant::now();
    let MatchRunOutput { mut matches, .. } = matcher
        .find_matches_with_telemetry(&library_items, &source_items, db)
        .await?;
    let match_elapsed = match_started.elapsed();

    let enrich_started = Instant::now();
    matcher.enrich_episode_titles(&mut matches, db).await?;
    let enrich_elapsed = enrich_started.elapsed();

    let linker = Linker::new_with_options(
        true,
        cfg.matching.mode.is_strict(),
        &cfg.symlink.naming_template,
        cfg.features.reconcile_links,
    );
    let plan_started = Instant::now();
    let plan = Discovery::new()
        .build_link_plan(db, &matches, |m| linker.build_target_path(m))
        .await?;
    let plan_elapsed = plan_started.elapsed();
    let total_elapsed = discover_started.elapsed();

    info!(
        library_filter = library_filter.unwrap_or("all"),
        refresh_cache,
        selected_libraries = selected.len(),
        library_items = library_items.len(),
        source_items = source_items.len(),
        matches = matches.len(),
        folders = plan.folders.len(),
        placements = plan.placements.len(),
        library_scan_ms = library_scan_elapsed.as_millis(),
        source_collect_ms = source_collect_elapsed.as_millis(),
        match_ms = match_elapsed.as_millis(),
        enrich_ms = enrich_elapsed.as_millis(),
        plan_ms = plan_elapsed.as_millis(),
        total_ms = total_elapsed.as_millis(),
        "discover snapshot built"
    );

    Ok(DiscoverySnapshot {
        summary: plan.summary(),
        folders: plan.folders,
        items: plan.placements,
        status_message,
    })
}

pub(crate) async fn run_discover(
    cfg: &Config,
    db: &Database,
    action: DiscoverAction,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<()> {
    let _stdout_guard = stdout_text_guard(output != OutputFormat::Json);
    match action {
        DiscoverAction::List => {
            info!("=== Symlinkarr Discover Review ===");
            let snapshot = load_discovery_snapshot(cfg, db, library_filter, true).await?;

            if output == OutputFormat::Json {
                print_json(&build_discover_json(&snapshot));
            } else {
                if let Some(message) = snapshot.status_message.as_deref() {
                    println!("   ℹ️  {}", message);
                }
                Discovery::print_summary(&DiscoverPlan {
                    folders: snapshot.folders,
                    placements: snapshot.items,
                });
            }
        }
        DiscoverAction::Add { torrent_id, arr } => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }
            if !cfg.has_decypharr() {
                anyhow::bail!("Decypharr not configured in config.yaml");
            }

            info!("=== Symlinkarr Discover Manual Handoff ===");

            let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
            let torrent_info = rd_client.get_torrent_info(&torrent_id).await?;

            let magnet = format!("magnet:?xt=urn:btih:{}", torrent_info.hash);
            println!(
                "\n🔗 Torrent: {} ({})",
                torrent_info.filename,
                bytesize(torrent_info.bytes)
            );
            println!("   Magnet hash: {}", torrent_info.hash);

            let decypharr = api::decypharr::DecypharrClient::from_config(&cfg.decypharr);
            let arr = resolve_decypharr_arr_name(&decypharr, &arr).await?;
            let _ = decypharr.add_content(&[magnet], &arr, "none").await?;

            println!("   ✅ Sent to Decypharr ({} → {})", arr, cfg.decypharr.url);
            println!(
                "   ℹ️  This is a manual handoff path. Run 'symlinkarr scan' once the download completes."
            );
        }
    }

    Ok(())
}

fn bytesize(bytes: i64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{:.1} GB", gb)
    } else {
        let mb = bytes as f64 / 1_048_576.0;
        format!("{:.1} MB", mb)
    }
}

async fn resolve_decypharr_arr_name(
    client: &api::decypharr::DecypharrClient,
    requested: &str,
) -> Result<String> {
    let arrs = client.get_arrs().await?;
    if let Some(arr) = arrs.iter().find(|arr| {
        normalize_decypharr_arr_name(&arr.name) == normalize_decypharr_arr_name(requested)
    }) {
        return Ok(arr.name.clone());
    }

    let available = arrs
        .iter()
        .map(|arr| arr.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "Decypharr Arr '{}' not found. Available: {}",
        requested,
        available
    );
}

fn normalize_decypharr_arr_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType,
        DaemonConfig, DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig,
        MediaBrowserConfig, PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig,
        SecurityConfig, SonarrConfig, SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
    };
    use crate::db::Database;
    use crate::discovery::DiscoverPlacementAction;
    use crate::models::MediaType;

    #[test]
    fn normalize_decypharr_arr_name_ignores_separators() {
        assert_eq!(normalize_decypharr_arr_name("sonarr_anime"), "sonarranime");
        assert_eq!(normalize_decypharr_arr_name("sonarr-anime"), "sonarranime");
    }

    fn test_config(root: &std::path::Path) -> Config {
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

    #[tokio::test]
    async fn load_discovery_snapshot_surfaces_missing_rd_credentials_even_in_cached_mode() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let db = Database::new(&cfg.db_path).await.unwrap();
        std::fs::create_dir_all(dir.path().join("anime").join("Missing Show {tvdb-1}")).unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("rd")
                .join("Missing.Show.S01E01.1080p.WEB-DL"),
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("rd")
                .join("Missing.Show.S01E01.1080p.WEB-DL")
                .join("Missing.Show.S01E01.1080p.WEB-DL.mkv"),
            b"video",
        )
        .unwrap();
        db.upsert_rd_torrent(
            "rd-1",
            "hash-1",
            "Missing.Show.S01E01.1080p.WEB-DL.mkv",
            "downloaded",
            r#"{"files":[{"selected":1,"bytes":1073741824,"path":"Missing.Show.S01E01.1080p.WEB-DL.mkv"}]}"#,
        )
        .await
        .unwrap();

        let snapshot = load_discovery_snapshot(&cfg, &db, None, false)
            .await
            .unwrap();

        assert_eq!(snapshot.summary.creates, 1);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].action, DiscoverPlacementAction::Create);
        assert!(snapshot
            .status_message
            .as_deref()
            .unwrap_or_default()
            .contains("Real-Debrid API key not configured"));
        assert!(snapshot
            .status_message
            .as_deref()
            .unwrap_or_default()
            .contains("live refresh is unavailable"));
    }

    #[test]
    fn build_discover_json_embeds_status_message_without_side_channel_text() {
        let payload = build_discover_json(&DiscoverySnapshot {
            summary: DiscoverSummary {
                folders: 1,
                placements: 1,
                creates: 1,
                updates: 0,
                blocked: 0,
            },
            folders: vec![DiscoverFolderPlan {
                library_name: "Anime".to_string(),
                media_id: "tvdb-1".to_string(),
                title: "Show".to_string(),
                folder_path: "/library/Show {tvdb-1}".into(),
                existing_links: 0,
                planned_creates: 1,
                planned_updates: 0,
                blocked: 0,
            }],
            items: vec![DiscoverPlacement {
                library_name: "Anime".to_string(),
                media_id: "tvdb-1".to_string(),
                title: "Show".to_string(),
                folder_path: "/library/Show {tvdb-1}".into(),
                source_path: "/rd/Show.S01E01/Show.S01E01.mkv".into(),
                target_path: "/library/Show {tvdb-1}/Season 01/Show - S01E01.mkv".into(),
                source_name: "Show.S01E01.mkv".to_string(),
                action: DiscoverPlacementAction::Create,
                season: Some(1),
                episode: Some(1),
            }],
            status_message: Some(
                "RD cache sync failed (timeout). Showing cached results only.".to_string(),
            ),
        });

        assert_eq!(payload["summary"]["placements"], 1);
        assert_eq!(payload["summary"]["creates"], 1);
        assert_eq!(
            payload["status_message"],
            "RD cache sync failed (timeout). Showing cached results only."
        );
        assert_eq!(payload["items"][0]["action"], "create");
        assert_eq!(payload["folders"][0]["planned_creates"], 1);
    }
}
