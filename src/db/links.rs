use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::{QueryBuilder, Row, Sqlite};

use super::{
    path_to_db_text, Database, DeadLinkSeed, MediaTypeStats,
    SCOPED_ROOT_IN_MEMORY_FILTER_THRESHOLD, SCOPED_ROOT_QUERY_CHUNK_SIZE,
};
use crate::models::{LinkRecord, LinkStatus, MediaType};

impl Database {
    /// Insert a new link record. Returns the row ID.
    pub async fn insert_link(&self, record: &LinkRecord) -> Result<i64> {
        let media_type = match record.media_type {
            MediaType::Tv => "tv",
            MediaType::Movie => "movie",
        };

        let status = match record.status {
            LinkStatus::Active => "active",
            LinkStatus::Dead => "dead",
            LinkStatus::Removed => "removed",
        };
        let source_path = path_to_db_text(&record.source_path)?;
        let target_path = path_to_db_text(&record.target_path)?;

        let result = sqlx::query(
            "INSERT INTO links (source_path, target_path, media_id, media_type, status)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(target_path) DO UPDATE SET
                source_path = excluded.source_path,
                media_id = excluded.media_id,
                status = excluded.status,
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(source_path)
        .bind(target_path)
        .bind(&record.media_id)
        .bind(media_type)
        .bind(status)
        .execute(&self.pool)
        .await?;

        Ok(result.last_insert_rowid())
    }

    /// Get all links with a given status.
    pub async fn get_links_by_status(&self, status: LinkStatus) -> Result<Vec<LinkRecord>> {
        self.get_links_by_status_limited(status, None).await
    }

    pub async fn get_links_by_status_limited(
        &self,
        status: LinkStatus,
        limit: Option<i64>,
    ) -> Result<Vec<LinkRecord>> {
        let status_str = match status {
            LinkStatus::Active => "active",
            LinkStatus::Dead => "dead",
            LinkStatus::Removed => "removed",
        };

        let rows = if let Some(limit) = limit {
            sqlx::query(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links
                 WHERE status = ?
                 ORDER BY id DESC
                 LIMIT ?",
            )
            .bind(status_str)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links
                 WHERE status = ?",
            )
            .bind(status_str)
            .fetch_all(&self.pool)
            .await?
        };

        let records = rows
            .iter()
            .map(|row| self.row_to_link_record(row))
            .collect::<Result<Vec<_>>>()?;

        Ok(records)
    }

    pub async fn get_links_by_status_scoped(
        &self,
        status: LinkStatus,
        allowed_target_roots: Option<&[PathBuf]>,
    ) -> Result<Vec<LinkRecord>> {
        let status_str = match status {
            LinkStatus::Active => "active",
            LinkStatus::Dead => "dead",
            LinkStatus::Removed => "removed",
        };

        let Some(roots) = normalize_scoped_root_texts(allowed_target_roots)? else {
            return self.get_links_by_status(status).await;
        };
        if roots.len() > SCOPED_ROOT_IN_MEMORY_FILTER_THRESHOLD {
            let records = self.get_links_by_status(status).await?;
            return Ok(filter_link_records_by_roots(records, &roots));
        }

        let mut records = Vec::new();
        for chunk in roots.chunks(SCOPED_ROOT_QUERY_CHUNK_SIZE) {
            let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links
                 WHERE status = ",
            );
            qb.push_bind(status_str);
            push_target_root_like_clause(&mut qb, chunk);

            let rows = qb.build().fetch_all(&self.pool).await?;
            records.extend(
                rows.iter()
                    .map(|row| self.row_to_link_record(row))
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        dedupe_link_records(records)
    }

    pub async fn get_links_scoped(
        &self,
        allowed_target_roots: Option<&[PathBuf]>,
    ) -> Result<Vec<LinkRecord>> {
        let Some(roots) = normalize_scoped_root_texts(allowed_target_roots)? else {
            let rows = sqlx::query(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links",
            )
            .fetch_all(&self.pool)
            .await?;
            return rows
                .iter()
                .map(|row| self.row_to_link_record(row))
                .collect::<Result<Vec<_>>>();
        };
        if roots.len() > SCOPED_ROOT_IN_MEMORY_FILTER_THRESHOLD {
            let rows = sqlx::query(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links",
            )
            .fetch_all(&self.pool)
            .await?;
            let records = rows
                .iter()
                .map(|row| self.row_to_link_record(row))
                .collect::<Result<Vec<_>>>()?;
            return Ok(filter_link_records_by_roots(records, &roots));
        }

        let mut records = Vec::new();
        for chunk in roots.chunks(SCOPED_ROOT_QUERY_CHUNK_SIZE) {
            let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links",
            );
            push_target_root_like_clause(&mut qb, chunk);

            let rows = qb.build().fetch_all(&self.pool).await?;
            records.extend(
                rows.iter()
                    .map(|row| self.row_to_link_record(row))
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        dedupe_link_records(records)
    }

    pub async fn get_links_by_targets(&self, target_paths: &[PathBuf]) -> Result<Vec<LinkRecord>> {
        if target_paths.is_empty() {
            return Ok(Vec::new());
        }

        let mut normalized = Vec::with_capacity(target_paths.len());
        for path in target_paths {
            normalized.push(path_to_db_text(path)?.to_string());
        }
        normalized.sort();
        normalized.dedup();

        let mut records = Vec::new();
        for chunk in normalized.chunks(500) {
            let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
                "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
                 FROM links
                 WHERE target_path IN (",
            );

            {
                let mut separated = qb.separated(", ");
                for path in chunk {
                    separated.push_bind(path);
                }
            }
            qb.push(")");

            let rows = qb.build().fetch_all(&self.pool).await?;
            records.extend(
                rows.iter()
                    .map(|row| self.row_to_link_record(row))
                    .collect::<Result<Vec<_>>>()?,
            );
        }

        Ok(records)
    }

    /// Get all active links.
    #[allow(dead_code)] // Kept as a simple compatibility wrapper
    pub async fn get_active_links(&self) -> Result<Vec<LinkRecord>> {
        self.get_links_by_status(LinkStatus::Active).await
    }

    pub async fn get_active_links_limited(&self, limit: i64) -> Result<Vec<LinkRecord>> {
        self.get_links_by_status_limited(LinkStatus::Active, Some(limit))
            .await
    }

    pub async fn get_active_links_scoped(
        &self,
        allowed_target_roots: Option<&[PathBuf]>,
    ) -> Result<Vec<LinkRecord>> {
        self.get_links_by_status_scoped(LinkStatus::Active, allowed_target_roots)
            .await
    }

    /// Check whether any active link exists for a media ID.
    pub async fn has_active_link_for_media(&self, media_id: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM links WHERE media_id = ? AND status = 'active'",
        )
        .bind(media_id)
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    /// Check whether an active link exists for a specific episode slot.
    pub async fn has_active_link_for_episode(
        &self,
        media_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<bool> {
        let slot = format!("S{:02}E{:02}", season, episode);
        let pattern = format!("%{}%", slot);
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt
             FROM links
             WHERE media_id = ?
               AND status = 'active'
               AND target_path LIKE ?",
        )
        .bind(media_id)
        .bind(pattern)
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    pub async fn get_dead_link_seeds_scoped(
        &self,
        allowed_target_roots: Option<&[PathBuf]>,
    ) -> Result<Vec<DeadLinkSeed>> {
        let Some(roots) = normalize_scoped_root_texts(allowed_target_roots)? else {
            return self.get_dead_link_seeds_root_chunk(&[]).await;
        };
        if roots.len() > SCOPED_ROOT_IN_MEMORY_FILTER_THRESHOLD {
            let seeds = self.get_dead_link_seeds_root_chunk(&[]).await?;
            return Ok(filter_dead_link_seeds_by_roots(seeds, &roots));
        }

        let mut seeds = Vec::new();
        for chunk in roots.chunks(SCOPED_ROOT_QUERY_CHUNK_SIZE) {
            seeds.extend(self.get_dead_link_seeds_root_chunk(chunk).await?);
        }

        dedupe_dead_link_seeds(seeds)
    }

    async fn get_dead_link_seeds_root_chunk(&self, roots: &[String]) -> Result<Vec<DeadLinkSeed>> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT source_path, target_path, media_id, media_type
             FROM links
             WHERE status = 'dead'",
        );
        if !roots.is_empty() {
            push_target_root_like_clause(&mut qb, roots);
        }

        let rows = qb.build().fetch_all(&self.pool).await?;

        let mut seeds = Vec::with_capacity(rows.len());
        for row in rows {
            let media_type_str: String = row.get("media_type");
            let media_type = match media_type_str.as_str() {
                "movie" => MediaType::Movie,
                _ => MediaType::Tv,
            };

            seeds.push(DeadLinkSeed {
                source_path: PathBuf::from(row.get::<String, _>("source_path")),
                target_path: PathBuf::from(row.get::<String, _>("target_path")),
                media_id: row.get("media_id"),
                media_type,
            });
        }

        Ok(seeds)
    }

    /// Mark a link as dead (source file disappeared).
    pub async fn mark_dead(&self, target_path: &str) -> Result<()> {
        sqlx::query(
            "UPDATE links SET status = 'dead', updated_at = CURRENT_TIMESTAMP
             WHERE target_path = ?",
        )
        .bind(target_path)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a link as dead using a filesystem path.
    pub async fn mark_dead_path(&self, target_path: &Path) -> Result<()> {
        self.mark_dead(path_to_db_text(target_path)?).await
    }

    /// Remove a link record by target path.
    #[allow(dead_code)] // Planned for future use
    pub async fn mark_removed(&self, target_path: &str) -> Result<()> {
        sqlx::query(
            "UPDATE links SET status = 'removed', updated_at = CURRENT_TIMESTAMP
             WHERE target_path = ?",
        )
        .bind(target_path)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a link as removed using a filesystem path.
    pub async fn mark_removed_path(&self, target_path: &Path) -> Result<()> {
        self.mark_removed(path_to_db_text(target_path)?).await
    }

    /// Get a link record by target path.
    pub async fn get_link_by_target(&self, target_path: &str) -> Result<Option<LinkRecord>> {
        let row = sqlx::query(
            "SELECT id, source_path, target_path, media_id, media_type, status, created_at, updated_at
             FROM links WHERE target_path = ?",
        )
        .bind(target_path)
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some(r) => Some(self.row_to_link_record(&r)?),
            None => None,
        })
    }

    /// Get a link record by target filesystem path.
    pub async fn get_link_by_target_path(&self, target_path: &Path) -> Result<Option<LinkRecord>> {
        self.get_link_by_target(path_to_db_text(target_path)?).await
    }

    /// Check if a link already exists for a target path.
    #[allow(dead_code)] // Used in tests and kept for CLI diagnostics
    pub async fn link_exists(&self, target_path: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM links WHERE target_path = ? AND status = 'active'",
        )
        .bind(target_path)
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    /// Get statistics about current links.
    pub async fn get_stats(&self) -> Result<(i64, i64, i64)> {
        let row = sqlx::query(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END), 0) as active,
                COALESCE(SUM(CASE WHEN status = 'dead' THEN 1 ELSE 0 END), 0) as dead,
                COUNT(*) as total
             FROM links",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok((row.get("active"), row.get("dead"), row.get("total")))
    }

    /// Get statistics about links grouped by media type.
    #[allow(dead_code)]
    pub async fn get_stats_by_media_type(&self) -> Result<Vec<MediaTypeStats>> {
        let rows = sqlx::query(
            "SELECT
                media_type,
                COUNT(*) as library_items,
                COALESCE(SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END), 0) as linked,
                COALESCE(SUM(CASE WHEN status = 'dead' THEN 1 ELSE 0 END), 0) as broken
             FROM links
             GROUP BY media_type
             ORDER BY library_items DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| MediaTypeStats {
                media_type: row.get("media_type"),
                library_items: row.get("library_items"),
                linked: row.get("linked"),
                broken: row.get("broken"),
            })
            .collect())
    }

    /// Begin a database transaction.  Callers can use this to coordinate
    /// DB writes with filesystem operations:
    ///   let mut tx = db.begin().await?;
    ///   db.insert_link_in_tx(&record, &mut tx).await?;
    ///   std::os::unix::fs::symlink(...)?;  // FS op
    ///   tx.commit().await?;              // only commit after FS succeeds
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, Sqlite>> {
        Ok(self.pool.begin().await?)
    }

    /// Insert a link record within a caller-supplied transaction.
    pub async fn insert_link_in_tx(
        &self,
        record: &LinkRecord,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
    ) -> Result<i64> {
        let source_path = path_to_db_text(&record.source_path)?;
        let target_path = path_to_db_text(&record.target_path)?;
        let media_type = match record.media_type {
            MediaType::Movie => "movie",
            MediaType::Tv => "tv",
        };
        let row = sqlx::query(
            "INSERT INTO links (source_path, target_path, media_id, media_type, status)
             VALUES (?, ?, ?, ?, 'active')
             ON CONFLICT(target_path) DO UPDATE SET
               source_path = excluded.source_path,
               media_id    = excluded.media_id,
               media_type  = excluded.media_type,
               status      = 'active',
               updated_at  = CURRENT_TIMESTAMP
             RETURNING id",
        )
        .bind(source_path)
        .bind(target_path)
        .bind(&record.media_id)
        .bind(media_type)
        .fetch_one(&mut **tx)
        .await?;
        Ok(row.get("id"))
    }

    /// Get all dead links (convenience wrapper for the web UI).
    #[allow(dead_code)]
    pub async fn get_dead_links(&self) -> Result<Vec<LinkRecord>> {
        self.get_links_by_status(LinkStatus::Dead).await
    }

    pub async fn get_dead_links_limited(&self, limit: i64) -> Result<Vec<LinkRecord>> {
        self.get_links_by_status_limited(LinkStatus::Dead, Some(limit))
            .await
    }

    fn row_to_link_record(&self, row: &sqlx::sqlite::SqliteRow) -> Result<LinkRecord> {
        let media_type_str: String = row.get("media_type");
        let status_str: String = row.get("status");

        let media_type = match media_type_str.as_str() {
            "movie" => MediaType::Movie,
            _ => MediaType::Tv,
        };

        let status = match status_str.as_str() {
            "dead" => LinkStatus::Dead,
            "removed" => LinkStatus::Removed,
            _ => LinkStatus::Active,
        };

        Ok(LinkRecord {
            id: Some(row.get("id")),
            source_path: PathBuf::from(row.get::<String, _>("source_path")),
            target_path: PathBuf::from(row.get::<String, _>("target_path")),
            media_id: row.get("media_id"),
            media_type,
            status,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
}

fn normalize_scoped_root_texts(
    allowed_target_roots: Option<&[PathBuf]>,
) -> Result<Option<Vec<String>>> {
    let Some(roots) = allowed_target_roots else {
        return Ok(None);
    };
    if roots.is_empty() {
        return Ok(None);
    }

    let mut normalized = Vec::with_capacity(roots.len());
    for root in roots {
        normalized.push(path_to_db_text(root)?.trim_end_matches('/').to_string());
    }
    normalized.sort();
    normalized.dedup();

    Ok((!normalized.is_empty()).then_some(normalized))
}

fn push_target_root_like_clause(qb: &mut QueryBuilder<'_, Sqlite>, roots: &[String]) {
    if roots.is_empty() {
        return;
    }

    let has_where = qb.sql().contains(" WHERE ");
    if has_where {
        qb.push(" AND (");
    } else {
        qb.push(" WHERE (");
    }

    let mut first = true;
    for root in roots {
        if !first {
            qb.push(" OR ");
        }
        first = false;
        let like_pattern = format!("{}/%", escape_sql_like_pattern(root));
        qb.push("target_path LIKE ")
            .push_bind(like_pattern)
            .push(" ESCAPE '\\'");
    }
    qb.push(")");
}

fn dedupe_link_records(records: Vec<LinkRecord>) -> Result<Vec<LinkRecord>> {
    let mut by_target = std::collections::HashMap::with_capacity(records.len());
    for record in records {
        let key = path_to_db_text(&record.target_path)?.to_string();
        by_target.entry(key).or_insert(record);
    }
    Ok(by_target.into_values().collect())
}

fn dedupe_dead_link_seeds(seeds: Vec<DeadLinkSeed>) -> Result<Vec<DeadLinkSeed>> {
    let mut by_target = std::collections::HashMap::with_capacity(seeds.len());
    for seed in seeds {
        let key = path_to_db_text(&seed.target_path)?.to_string();
        by_target.entry(key).or_insert(seed);
    }
    Ok(by_target.into_values().collect())
}

fn filter_link_records_by_roots(records: Vec<LinkRecord>, roots: &[String]) -> Vec<LinkRecord> {
    let root_set: HashSet<&str> = roots.iter().map(|root| root.as_str()).collect();
    records
        .into_iter()
        .filter(|record| path_matches_any_root(&record.target_path, &root_set))
        .collect()
}

fn filter_dead_link_seeds_by_roots(
    seeds: Vec<DeadLinkSeed>,
    roots: &[String],
) -> Vec<DeadLinkSeed> {
    let root_set: HashSet<&str> = roots.iter().map(|root| root.as_str()).collect();
    seeds
        .into_iter()
        .filter(|seed| path_matches_any_root(&seed.target_path, &root_set))
        .collect()
}

fn path_matches_any_root(path: &Path, root_set: &HashSet<&str>) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor
            .to_str()
            .map(|text| root_set.contains(text.trim_end_matches('/')))
            .unwrap_or(false)
    })
}

fn escape_sql_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
