use anyhow::Result;
use sqlx::Row;

use super::{AnimeSearchOverrideRecord, AnimeSearchOverrideSeed, Database};

impl Database {
    pub async fn list_anime_search_overrides(&self) -> Result<Vec<AnimeSearchOverrideRecord>> {
        let rows = sqlx::query(
            "SELECT
                media_id,
                preferred_title,
                extra_hints_json,
                note,
                created_at,
                updated_at
             FROM anime_search_overrides
             ORDER BY updated_at DESC, media_id ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(map_anime_search_override_row)
            .collect()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn get_anime_search_override(
        &self,
        media_id: &str,
    ) -> Result<Option<AnimeSearchOverrideRecord>> {
        let row = sqlx::query(
            "SELECT
                media_id,
                preferred_title,
                extra_hints_json,
                note,
                created_at,
                updated_at
             FROM anime_search_overrides
             WHERE media_id = ?",
        )
        .bind(media_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(map_anime_search_override_row).transpose()
    }

    pub async fn upsert_anime_search_override(&self, seed: &AnimeSearchOverrideSeed) -> Result<()> {
        let preferred_title = normalize_optional_text(seed.preferred_title.as_deref());
        let note = normalize_optional_text(seed.note.as_deref());
        let extra_hints_json = serde_json::to_string(&seed.extra_hints)?;

        sqlx::query(
            "INSERT INTO anime_search_overrides (
                media_id,
                preferred_title,
                extra_hints_json,
                note
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(media_id) DO UPDATE SET
                preferred_title = excluded.preferred_title,
                extra_hints_json = excluded.extra_hints_json,
                note = excluded.note,
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(seed.media_id.trim())
        .bind(preferred_title)
        .bind(extra_hints_json)
        .bind(note)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn delete_anime_search_override(&self, media_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM anime_search_overrides WHERE media_id = ?")
            .bind(media_id.trim())
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}

fn map_anime_search_override_row(
    row: sqlx::sqlite::SqliteRow,
) -> Result<AnimeSearchOverrideRecord> {
    let extra_hints_json: String = row.get("extra_hints_json");
    Ok(AnimeSearchOverrideRecord {
        media_id: row.get("media_id"),
        preferred_title: row.get("preferred_title"),
        extra_hints: serde_json::from_str(&extra_hints_json)?,
        note: row.get("note"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
