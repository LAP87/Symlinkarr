use anyhow::Result;
use serde::Serialize;
use tracing::info;

use crate::api;
use crate::api::realdebrid::RealDebridClient;
use crate::commands::{print_json, selected_libraries};
use crate::config::Config;
use crate::db::Database;
use crate::discovery;
use crate::library_scanner::LibraryScanner;
use crate::{DiscoverAction, OutputFormat};

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

            let rd_client = RealDebridClient::from_config(&cfg.realdebrid);

            let lib_scanner = LibraryScanner::new();
            let selected = selected_libraries(cfg, library_filter)?;
            let mut library_items = Vec::new();
            for lib in &selected {
                library_items.extend(lib_scanner.scan_library(lib));
            }

            {
                use crate::cache::TorrentCache;
                let cache = TorrentCache::new(db, &rd_client);
                if let Err(e) = cache.sync().await {
                    tracing::warn!("Failed to sync RD cache for discovery: {}", e);
                }
            }

            let disc = discovery::Discovery::new();
            let gaps = disc.find_gaps(db, &library_items).await?;

            if output == OutputFormat::Json {
                #[derive(Serialize)]
                struct GapSummary<'a> {
                    rd_torrent_id: &'a str,
                    status: &'a str,
                    size: i64,
                    parsed_title: &'a str,
                }

                let out: Vec<GapSummary<'_>> = gaps
                    .iter()
                    .map(|g| GapSummary {
                        rd_torrent_id: &g.rd_torrent_id,
                        status: &g.status,
                        size: g.size,
                        parsed_title: &g.parsed_title,
                    })
                    .collect();
                print_json(&serde_json::json!({
                    "count": out.len(),
                    "items": out,
                }));
            } else {
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

    #[test]
    fn normalize_decypharr_arr_name_ignores_separators() {
        assert_eq!(normalize_decypharr_arr_name("sonarr_anime"), "sonarranime");
        assert_eq!(normalize_decypharr_arr_name("sonarr-anime"), "sonarranime");
    }
}
