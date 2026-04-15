use anyhow::Result;
use sqlx::Row;

use super::{escape_sql_like, Database};

impl Database {
    /// Get a cached API response.
    pub async fn get_cached(&self, cache_key: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT response_json FROM api_cache
             WHERE cache_key = ?
             AND datetime(fetched_at, '+' || ttl_hours || ' hours') > datetime('now')",
        )
        .bind(cache_key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.get("response_json")))
    }

    /// Store an API response in the cache.
    pub async fn set_cached(&self, cache_key: &str, response: &str, ttl_hours: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO api_cache (cache_key, response_json, ttl_hours)
             VALUES (?, ?, ?)
             ON CONFLICT(cache_key) DO UPDATE SET
                response_json = excluded.response_json,
                fetched_at = CURRENT_TIMESTAMP,
                ttl_hours = excluded.ttl_hours",
        )
        .bind(cache_key)
        .bind(response)
        .bind(ttl_hours as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove one cached API response so the next lookup refetches it.
    pub async fn invalidate_cached(&self, cache_key: &str) -> Result<bool> {
        let deleted = sqlx::query("DELETE FROM api_cache WHERE cache_key = ?")
            .bind(cache_key)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(deleted > 0)
    }

    /// Remove cached API responses matching a key prefix.
    pub async fn invalidate_cached_prefix(&self, cache_key_prefix: &str) -> Result<u64> {
        let like_pattern = format!("{}%", escape_sql_like(cache_key_prefix));
        let deleted = sqlx::query("DELETE FROM api_cache WHERE cache_key LIKE ? ESCAPE '\\'")
            .bind(like_pattern)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(deleted)
    }

    /// Remove all entries from the API metadata cache. Returns the number of deleted rows.
    pub async fn clear_api_cache(&self) -> Result<u64> {
        let deleted = sqlx::query("DELETE FROM api_cache")
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(deleted)
    }

    /// Upsert an RD torrent record.
    pub async fn upsert_rd_torrent(
        &self,
        id: &str,
        hash: &str,
        filename: &str,
        status: &str,
        files_json: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO rd_torrents (torrent_id, hash, filename, status, files_json, scanned_at)
             VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
             ON CONFLICT(torrent_id) DO UPDATE SET
                hash = excluded.hash,
                filename = excluded.filename,
                status = excluded.status,
                files_json = excluded.files_json,
                scanned_at = CURRENT_TIMESTAMP",
        )
        .bind(id)
        .bind(hash)
        .bind(filename)
        .bind(status)
        .bind(files_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get all stored RD torrents (id, status, hash, files_json).
    pub async fn get_rd_torrents(&self) -> Result<Vec<(String, String, String, String, String)>> {
        let rows =
            sqlx::query("SELECT torrent_id, hash, filename, status, files_json FROM rd_torrents")
                .fetch_all(&self.pool)
                .await?;

        let mut results = Vec::new();
        for row in rows {
            results.push((
                row.get("torrent_id"),
                row.get("hash"),
                row.get("filename"),
                row.get("status"),
                row.get("files_json"),
            ));
        }
        Ok(results)
    }

    /// Delete an RD torrent record.
    /// Check whether a torrent with the given info hash exists in the cache
    /// with status "downloaded". Used to verify that a torrent has completed
    /// on the RD side even when it has disappeared from the Decypharr queue.
    pub async fn rd_torrent_downloaded_by_hash(&self, hash: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM rd_torrents \
             WHERE LOWER(hash) = LOWER(?) AND status = 'downloaded'",
        )
        .bind(hash)
        .fetch_one(&self.pool)
        .await?;
        let cnt: i64 = row.get("cnt");
        Ok(cnt > 0)
    }

    pub async fn delete_rd_torrent(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM rd_torrents WHERE torrent_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Return (cached_with_files, total_downloaded) counts for RD torrents.
    /// `cached_with_files` excludes torrents with empty file lists (e.g.
    /// because the per-cycle fetch cap was reached). Callers compare the two
    /// to decide whether cache coverage is sufficient or a filesystem walk is
    /// needed.
    pub async fn get_rd_torrent_counts(&self) -> Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT \
               SUM(CASE WHEN status = 'downloaded' THEN 1 ELSE 0 END) as total, \
               SUM(CASE WHEN status = 'downloaded' AND files_json != '{\"files\":[]}' THEN 1 ELSE 0 END) as cached \
             FROM rd_torrents",
        )
        .fetch_one(&self.pool)
        .await?;
        let total: i64 = row.try_get("total").unwrap_or(0);
        let cached: i64 = row.try_get("cached").unwrap_or(0);
        Ok((cached, total))
    }
}
