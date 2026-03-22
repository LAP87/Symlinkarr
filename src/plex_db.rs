use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{QueryBuilder, Row, Sqlite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexPathRecord {
    pub path: PathBuf,
    pub deleted_only: bool,
}

pub async fn load_path_records(db_path: &Path, roots: &[PathBuf]) -> Result<Vec<PlexPathRecord>> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT mp.file, \
                MAX(CASE WHEN mp.deleted_at IS NULL AND mi.deleted_at IS NULL AND (md.deleted_at IS NULL OR md.id IS NULL) THEN 1 ELSE 0 END) AS has_live, \
                MAX(CASE WHEN mp.deleted_at IS NOT NULL OR mi.deleted_at IS NOT NULL OR (md.deleted_at IS NOT NULL AND md.id IS NOT NULL) THEN 1 ELSE 0 END) AS has_deleted \
         FROM media_parts mp \
         JOIN media_items mi ON mp.media_item_id = mi.id \
         LEFT JOIN metadata_items md ON mi.metadata_item_id = md.id \
         JOIN section_locations sl ON mi.section_location_id = sl.id \
         WHERE mp.file IS NOT NULL AND mp.file != '' AND sl.root_path IN (",
    );

    {
        let mut separated = query.separated(", ");
        for root in roots {
            separated.push_bind(root.to_string_lossy().to_string());
        }
    }
    query.push(") GROUP BY mp.file");

    let rows = query.build().fetch_all(&pool).await?;
    let records = rows
        .into_iter()
        .filter_map(|row| {
            let file = row.try_get::<String, _>("file").ok()?;
            let path = PathBuf::from(file);
            if !roots.iter().any(|root| path.starts_with(root)) {
                return None;
            }
            let has_live = row.try_get::<i64, _>("has_live").ok().unwrap_or(0) > 0;
            let has_deleted = row.try_get::<i64, _>("has_deleted").ok().unwrap_or(0) > 0;
            Some(PlexPathRecord {
                path,
                deleted_only: has_deleted && !has_live,
            })
        })
        .collect();

    pool.close().await;
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    use sqlx::Executor;

    async fn create_test_db(
        db_path: &Path,
        root: &Path,
        rows: &[(&Path, bool)],
    ) -> anyhow::Result<()> {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        for statement in [
            "CREATE TABLE section_locations (id INTEGER PRIMARY KEY, library_section_id INTEGER, root_path TEXT)",
            "CREATE TABLE metadata_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, deleted_at INTEGER)",
            "CREATE TABLE media_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, section_location_id INTEGER, metadata_item_id INTEGER, deleted_at INTEGER)",
            "CREATE TABLE media_parts (id INTEGER PRIMARY KEY, media_item_id INTEGER, file TEXT, deleted_at INTEGER)",
        ] {
            pool.execute(statement).await?;
        }

        sqlx::query(
            "INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?)",
        )
        .bind(root.to_string_lossy().to_string())
        .execute(&pool)
        .await?;

        for (idx, (path, deleted)) in rows.iter().enumerate() {
            let id = (idx + 1) as i64;
            let deleted_at = deleted.then_some(1_i64);

            sqlx::query(
                "INSERT INTO metadata_items (id, library_section_id, deleted_at) VALUES (?, 1, ?)",
            )
            .bind(id)
            .bind(deleted_at)
            .execute(&pool)
            .await?;

            sqlx::query(
                "INSERT INTO media_items (id, library_section_id, section_location_id, metadata_item_id, deleted_at) VALUES (?, 1, 1, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(deleted_at)
            .execute(&pool)
            .await?;

            sqlx::query(
                "INSERT INTO media_parts (id, media_item_id, file, deleted_at) VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(path.to_string_lossy().to_string())
            .bind(deleted_at)
            .execute(&pool)
            .await?;
        }

        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn load_path_records_requires_deleted_without_any_live_row() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        let db_path = dir.path().join("plex.db");
        let shared_path = root.join("Show/Season 01/Episode.mkv");

        create_test_db(
            &db_path,
            &root,
            &[(&shared_path, false), (&shared_path, true)],
        )
        .await
        .unwrap();

        let records = load_path_records(&db_path, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, shared_path);
        assert!(!records[0].deleted_only);
    }
}
