use anyhow::Result;
use sqlx::Row;

use super::{Database, SourcePin};

impl Database {
    pub async fn list_source_pins(&self) -> Result<Vec<SourcePin>> {
        let rows = sqlx::query(
            "SELECT source_folder, media_id, origin, note, updated_at
             FROM source_pins
             ORDER BY source_folder ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| SourcePin {
                source_folder: row.get("source_folder"),
                media_id: row.get("media_id"),
                origin: row.get("origin"),
                note: row.get("note"),
                updated_at: row.get("updated_at"),
            })
            .collect())
    }

    /// Insert or replace pins in one transaction. Returns how many rows changed value
    /// (new folders or a folder moved to another media id); re-importing identical pins
    /// counts zero.
    pub async fn upsert_source_pins(
        &self,
        pins: &[(String, String, String, Option<String>)],
    ) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        let mut changed = 0usize;
        for (folder, media_id, origin, note) in pins {
            let result = sqlx::query(
                "INSERT INTO source_pins (source_folder, media_id, origin, note)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(source_folder) DO UPDATE SET
                    media_id = excluded.media_id,
                    origin = excluded.origin,
                    note = excluded.note,
                    updated_at = CURRENT_TIMESTAMP
                 WHERE source_pins.media_id <> excluded.media_id",
            )
            .bind(folder)
            .bind(media_id)
            .bind(origin)
            .bind(note)
            .execute(&mut *tx)
            .await?;
            changed += result.rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(changed)
    }

    /// Returns true when a row was removed.
    pub async fn delete_source_pin(&self, source_folder: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM source_pins WHERE source_folder = ?")
            .bind(source_folder)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
