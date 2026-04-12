use anyhow::Result;
use tracing::info;

use crate::api::realdebrid::RealDebridClient;
use crate::cache::TorrentCache;
use crate::config::Config;
use crate::db::Database;
use crate::CacheAction;

pub(crate) async fn run_cache(cfg: &Config, db: &Database, action: CacheAction) -> Result<()> {
    match action {
        CacheAction::Build => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }
            let rd_client = RealDebridClient::from_config(&cfg.realdebrid);
            let cache = TorrentCache::new(db, &rd_client);

            info!("=== Symlinkarr Cache Build (full, no fetch cap) ===");
            println!("Building full RD torrent cache — this may take a while for large accounts.");
            cache.sync_full().await?;

            let (cached, total) = db.get_rd_torrent_counts().await?;
            println!(
                "\nCache build complete: {}/{} downloaded torrents have file info ({:.0}%)",
                cached,
                total,
                if total > 0 {
                    cached as f64 / total as f64 * 100.0
                } else {
                    0.0
                }
            );
        }
        CacheAction::Status => {
            if !cfg.has_realdebrid() {
                anyhow::bail!("Real-Debrid API key not configured in config.yaml");
            }
            let (cached, total) = db.get_rd_torrent_counts().await?;
            let coverage = if total > 0 {
                cached as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            println!("RD cache status:");
            println!("  Downloaded torrents:    {}", total);
            println!("  With file info cached:  {} ({:.0}%)", cached, coverage);
            if coverage < 80.0 {
                println!("  Scanner mode:           filesystem walk (coverage < 80%)");
            } else {
                println!("  Scanner mode:           cache-based (coverage >= 80%)");
            }
        }
        CacheAction::Invalidate { key } => {
            let deleted = invalidate_metadata_cache(db, &key).await?;
            if deleted > 0 {
                println!(
                    "Invalidated {} cached metadata entry/entries matching {:?}",
                    deleted, key
                );
            } else {
                println!("No cached metadata found matching {:?}", key);
            }
        }
        CacheAction::Clear => {
            let deleted = clear_metadata_cache(db).await?;
            println!("Cleared {} cached metadata entries", deleted);
        }
    }

    Ok(())
}

/// Invalidate cached API metadata matching a key or prefix.
///
/// Supported key formats:
///   - Exact key: `tmdb:tv:12345`, `tvdb:series:67890`
///   - Prefix: `tmdb:tv:`, `tmdb:movie:`, `tvdb:series:`
///   - Short media ID: `tmdb:12345` → invalidates `tmdb:tv:12345` and `tmdb:movie:12345`
///   - Keyword: `anime-lists` → invalidates the anime-lists XML cache
pub(crate) async fn invalidate_metadata_cache(db: &Database, key: &str) -> Result<u64> {
    if key.ends_with(':') {
        let deleted = db.invalidate_cached_prefix(key).await?;
        if deleted > 0 {
            info!("Invalidated metadata cache key prefix: {}", key);
        }
        return Ok(deleted);
    }

    // Expand short-form media IDs into the actual cache key patterns.
    let keys_to_try: Vec<String> = if key.starts_with("tmdb:tv:")
        || key.starts_with("tmdb:movie:")
        || key.starts_with("tvdb:series:")
        || key.starts_with("tmdb:") && key.contains(":external_ids:")
    {
        // Exact key
        vec![key.to_string()]
    } else if let Some(id) = key.strip_prefix("tmdb:") {
        // Short form: try both tv and movie
        vec![
            format!("tmdb:tv:{}", id),
            format!("tmdb:movie:{}", id),
            format!("tmdb:tv:external_ids:{}", id),
            format!("tmdb:movie:external_ids:{}", id),
        ]
    } else if let Some(id) = key.strip_prefix("tvdb:") {
        vec![format!("tvdb:series:{}", id)]
    } else if key == "anime-lists" {
        vec!["anime-lists:anime-list.xml:v1".to_string()]
    } else {
        // Try as-is
        vec![key.to_string()]
    };

    let mut total_deleted = 0u64;
    for cache_key in &keys_to_try {
        if db.invalidate_cached(cache_key).await? {
            info!("Invalidated metadata cache key: {}", cache_key);
            total_deleted += 1;
        }
    }
    Ok(total_deleted)
}

/// Clear all entries from the API metadata cache.
pub(crate) async fn clear_metadata_cache(db: &Database) -> Result<u64> {
    let deleted = db.clear_api_cache().await?;
    info!("Cleared {} entries from API metadata cache", deleted);
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[tokio::test]
    async fn invalidate_short_tmdb_key_expands_to_metadata_and_external_ids() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
            .await
            .unwrap();

        db.set_cached("tmdb:tv:12345", "{\"title\":\"Example\"}", 1)
            .await
            .unwrap();
        db.set_cached("tmdb:movie:12345", "{\"title\":\"Example\"}", 1)
            .await
            .unwrap();
        db.set_cached("tmdb:tv:external_ids:12345", "{\"imdb_id\":\"tt1\"}", 1)
            .await
            .unwrap();
        db.set_cached("tmdb:movie:external_ids:12345", "{\"imdb_id\":\"tt2\"}", 1)
            .await
            .unwrap();

        let deleted = invalidate_metadata_cache(&db, "tmdb:12345").await.unwrap();
        assert_eq!(deleted, 4);
        assert!(db.get_cached("tmdb:tv:12345").await.unwrap().is_none());
        assert!(db.get_cached("tmdb:movie:12345").await.unwrap().is_none());
        assert!(db
            .get_cached("tmdb:tv:external_ids:12345")
            .await
            .unwrap()
            .is_none());
        assert!(db
            .get_cached("tmdb:movie:external_ids:12345")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn invalidate_prefix_key_removes_matching_family() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
            .await
            .unwrap();

        db.set_cached("tmdb:tv:12345", "{\"title\":\"Show\"}", 24)
            .await
            .unwrap();
        db.set_cached("tmdb:tv:external_ids:12345", "{\"imdb_id\":\"tt1\"}", 24)
            .await
            .unwrap();
        db.set_cached("tmdb:movie:12345", "{\"title\":\"Movie\"}", 24)
            .await
            .unwrap();

        let deleted = invalidate_metadata_cache(&db, "tmdb:tv:").await.unwrap();
        assert_eq!(deleted, 2);
        assert!(db.get_cached("tmdb:tv:12345").await.unwrap().is_none());
        assert!(db
            .get_cached("tmdb:tv:external_ids:12345")
            .await
            .unwrap()
            .is_none());
        assert!(db.get_cached("tmdb:movie:12345").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn clear_metadata_cache_removes_all_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
            .await
            .unwrap();

        db.set_cached("tmdb:tv:1", "{\"title\":\"Show\"}", 1)
            .await
            .unwrap();
        db.set_cached("tvdb:series:2", "{\"title\":\"Series\"}", 1)
            .await
            .unwrap();

        let deleted = clear_metadata_cache(&db).await.unwrap();
        assert_eq!(deleted, 2);
        assert!(db.get_cached("tmdb:tv:1").await.unwrap().is_none());
        assert!(db.get_cached("tvdb:series:2").await.unwrap().is_none());
    }
}
