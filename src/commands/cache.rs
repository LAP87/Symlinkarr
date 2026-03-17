use anyhow::Result;
use tracing::info;

use crate::api::realdebrid::RealDebridClient;
use crate::cache::TorrentCache;
use crate::config::Config;
use crate::db::Database;
use crate::CacheAction;

pub(crate) async fn run_cache(cfg: &Config, db: &Database, action: CacheAction) -> Result<()> {
    if !cfg.has_realdebrid() {
        anyhow::bail!("Real-Debrid API key not configured in config.yaml");
    }

    let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
    let cache = TorrentCache::new(db, &rd_client);

    match action {
        CacheAction::Build => {
            info!("=== Symlinkarr Cache Build (full, no fetch cap) ===");
            println!("Building full RD torrent cache — this may take a while for large accounts.");
            cache.sync_full().await?;

            let (cached, total) = db.get_rd_torrent_counts().await?;
            println!(
                "\nCache build complete: {}/{} downloaded torrents have file info ({:.0}%)",
                cached,
                total,
                if total > 0 { cached as f64 / total as f64 * 100.0 } else { 0.0 }
            );
        }
        CacheAction::Status => {
            let (cached, total) = db.get_rd_torrent_counts().await?;
            let coverage = if total > 0 { cached as f64 / total as f64 * 100.0 } else { 0.0 };
            println!("RD cache status:");
            println!("  Downloaded torrents:    {}", total);
            println!("  With file info cached:  {} ({:.0}%)", cached, coverage);
            if coverage < 80.0 {
                println!("  Scanner mode:           filesystem walk (coverage < 80%)");
            } else {
                println!("  Scanner mode:           cache-based (coverage >= 80%)");
            }
        }
    }

    Ok(())
}
