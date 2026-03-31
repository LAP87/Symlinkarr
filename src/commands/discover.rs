use anyhow::Result;
use serde::Serialize;
use tracing::info;

use crate::api;
use crate::api::realdebrid::RealDebridClient;
use crate::commands::{print_json, selected_libraries};
use crate::config::Config;
use crate::db::Database;
use crate::discovery;
use crate::discovery::DiscoveredItem;
use crate::library_scanner::LibraryScanner;
use crate::{DiscoverAction, OutputFormat};

pub(crate) struct DiscoverySnapshot {
    pub items: Vec<DiscoveredItem>,
    pub status_message: Option<String>,
}

#[derive(Serialize)]
struct GapSummary<'a> {
    rd_torrent_id: &'a str,
    status: &'a str,
    size: i64,
    parsed_title: &'a str,
}

fn cached_only_notice(cfg: &Config) -> Option<String> {
    (!cfg.has_realdebrid()).then(|| {
        "Real-Debrid API key not configured. Showing cached RD results only; live refresh is unavailable until credentials are configured."
            .to_string()
    })
}

fn build_discover_json(gaps: &[DiscoveredItem], status_message: Option<&str>) -> serde_json::Value {
    let out: Vec<GapSummary<'_>> = gaps
        .iter()
        .map(|g| GapSummary {
            rd_torrent_id: &g.rd_torrent_id,
            status: &g.status,
            size: g.size,
            parsed_title: &g.parsed_title,
        })
        .collect();

    serde_json::json!({
        "count": out.len(),
        "status_message": status_message,
        "items": out,
    })
}

pub(crate) async fn load_discovery_snapshot(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    refresh_cache: bool,
) -> Result<DiscoverySnapshot> {
    let selected = selected_libraries(cfg, library_filter)?;
    let lib_scanner = LibraryScanner::new();
    let mut library_items = Vec::new();
    for lib in &selected {
        library_items.extend(lib_scanner.scan_library(lib));
    }

    let mut notices = Vec::new();
    if let Some(message) = cached_only_notice(cfg) {
        notices.push(message);
    }

    if refresh_cache && cfg.has_realdebrid() {
        let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
        let cache = crate::cache::TorrentCache::new(db, &rd_client);
        if let Err(e) = cache.sync().await {
            notices.push(format!(
                "RD cache sync failed ({}). Showing cached results only.",
                e
            ));
        }
    }

    let disc = discovery::Discovery::new();
    let items = disc.find_gaps(db, &library_items).await?;
    Ok(DiscoverySnapshot {
        items,
        status_message: (!notices.is_empty()).then(|| notices.join(" ")),
    })
}

pub(crate) async fn run_discover(
    cfg: &Config,
    db: &Database,
    action: DiscoverAction,
    library_filter: Option<&str>,
    output: OutputFormat,
) -> Result<()> {
    match action {
        DiscoverAction::List => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }

            info!("=== Symlinkarr Discovery ===");
            let snapshot = load_discovery_snapshot(cfg, db, library_filter, true).await?;
            let gaps = snapshot.items;

            if output == OutputFormat::Json {
                print_json(&build_discover_json(
                    &gaps,
                    snapshot.status_message.as_deref(),
                ));
            } else {
                if let Some(message) = snapshot.status_message.as_deref() {
                    println!("   ℹ️  {}", message);
                }
                discovery::Discovery::print_summary(&gaps);
            }
        }
        DiscoverAction::Add { torrent_id, arr } => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }
            if !cfg.has_decypharr() {
                anyhow::bail!("Decypharr not configured in config.yaml");
            }

            info!("=== Symlinkarr Discover Add ===");

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
            println!("   ℹ️  Run 'symlinkarr scan' once the download completes.");
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

        let snapshot = load_discovery_snapshot(&cfg, &db, None, false)
            .await
            .unwrap();

        assert!(snapshot.items.is_empty());
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
        let item = DiscoveredItem {
            rd_torrent_id: "rd-1".to_string(),
            torrent_name: "Show.S01E01.1080p.WEB-DL.mkv".to_string(),
            status: "downloaded".to_string(),
            size: 1_073_741_824,
            parsed_title: "Show".to_string(),
        };

        let payload = build_discover_json(
            &[item],
            Some("RD cache sync failed (timeout). Showing cached results only."),
        );

        assert_eq!(payload["count"], 1);
        assert_eq!(
            payload["status_message"],
            "RD cache sync failed (timeout). Showing cached results only."
        );
        assert_eq!(payload["items"][0]["rd_torrent_id"], "rd-1");
    }
}
