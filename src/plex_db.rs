use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{QueryBuilder, Row, Sqlite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexPathRecord {
    pub path: PathBuf,
    pub deleted_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexDuplicateShowRecord {
    pub title: String,
    pub original_title: String,
    pub year: Option<i64>,
    pub guid: String,
    pub guid_kind: String,
    pub live: bool,
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

pub async fn load_duplicate_show_records(
    db_path: &Path,
    roots: &[PathBuf],
) -> Result<Vec<PlexDuplicateShowRecord>> {
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
        "WITH target_sections AS ( \
             SELECT DISTINCT library_section_id \
             FROM section_locations \
             WHERE root_path IN (",
    );

    {
        let mut separated = query.separated(", ");
        for root in roots {
            separated.push_bind(root.to_string_lossy().to_string());
        }
    }

    query.push(
        ") \
         ), duplicate_titles AS ( \
             SELECT \
                 title, \
                 COALESCE(original_title, '') AS original_title, \
                 COALESCE(year, -1) AS year \
             FROM metadata_items \
             WHERE metadata_type = 2 \
               AND library_section_id IN (SELECT library_section_id FROM target_sections) \
             GROUP BY title, COALESCE(original_title, ''), COALESCE(year, -1) \
             HAVING COUNT(*) > 1 \
         ) \
         SELECT \
             m.title, \
             COALESCE(m.original_title, '') AS original_title, \
             m.year AS year, \
             COALESCE(m.guid, '') AS guid, \
             CASE \
                 WHEN m.guid LIKE 'com.plexapp.agents.hama://anidb-%' THEN 'hama-anidb' \
                 WHEN m.guid LIKE 'com.plexapp.agents.hama://tvdb-%' THEN 'hama-tvdb' \
                 WHEN m.guid LIKE 'com.plexapp.agents.hama://tmdb-%' THEN 'hama-tmdb' \
                 WHEN m.guid LIKE 'plex://show/%' THEN 'plex-show' \
                 WHEN m.guid LIKE 'com.plexapp.agents.%' THEN 'other-agent' \
                 ELSE 'other' \
             END AS guid_kind, \
             CASE WHEN m.deleted_at IS NULL THEN 1 ELSE 0 END AS live \
         FROM metadata_items m \
         JOIN duplicate_titles d \
           ON m.title = d.title \
          AND COALESCE(m.original_title, '') = d.original_title \
          AND COALESCE(m.year, -1) = d.year \
         WHERE m.metadata_type = 2 \
           AND m.library_section_id IN (SELECT library_section_id FROM target_sections) \
         ORDER BY m.title, m.year, m.id",
    );

    let rows = query.build().fetch_all(&pool).await?;
    let records = rows
        .into_iter()
        .map(|row| PlexDuplicateShowRecord {
            title: row.try_get::<String, _>("title").unwrap_or_default(),
            original_title: row
                .try_get::<String, _>("original_title")
                .unwrap_or_default(),
            year: row.try_get::<Option<i64>, _>("year").ok().flatten(),
            guid: row.try_get::<String, _>("guid").unwrap_or_default(),
            guid_kind: row.try_get::<String, _>("guid_kind").unwrap_or_default(),
            live: row.try_get::<i64, _>("live").ok().unwrap_or(0) > 0,
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

    async fn create_duplicate_test_db(db_path: &Path, root: &Path) -> anyhow::Result<()> {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        for statement in [
            "CREATE TABLE section_locations (id INTEGER PRIMARY KEY, library_section_id INTEGER, root_path TEXT)",
            "CREATE TABLE metadata_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, metadata_type INTEGER, title TEXT, original_title TEXT, year INTEGER, guid TEXT, deleted_at INTEGER)",
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

        for (id, guid) in [
            (1_i64, "com.plexapp.agents.hama://anidb-100?lang=en"),
            (2_i64, "com.plexapp.agents.hama://tvdb-200?lang=en"),
            (3_i64, "com.plexapp.agents.hama://tvdb-201?lang=en"),
            (4_i64, "com.plexapp.agents.hama://tvdb-202?lang=en"),
        ] {
            sqlx::query(
                "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at) VALUES (?, 1, 2, ?, '', 2024, ?, NULL)",
            )
            .bind(id)
            .bind(if id <= 2 { "Show A" } else { "Show B" })
            .bind(guid)
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

    #[tokio::test]
    async fn load_duplicate_show_records_returns_duplicate_show_rows_for_target_roots() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("anime");
        std::fs::create_dir_all(&root).unwrap();
        let db_path = dir.path().join("plex.db");

        create_duplicate_test_db(&db_path, &root).await.unwrap();

        let records = load_duplicate_show_records(&db_path, std::slice::from_ref(&root))
            .await
            .unwrap();

        assert_eq!(records.len(), 4);
        assert_eq!(
            records
                .iter()
                .filter(|record| record.title == "Show A")
                .map(|record| record.guid_kind.as_str())
                .collect::<Vec<_>>(),
            vec!["hama-anidb", "hama-tvdb"]
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.title == "Show B")
                .map(|record| record.guid_kind.as_str())
                .collect::<Vec<_>>(),
            vec!["hama-tvdb", "hama-tvdb"]
        );
        assert!(records.iter().all(|record| record.live));
    }
}
