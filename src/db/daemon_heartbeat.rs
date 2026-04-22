use anyhow::Result;
use sqlx::Row;

use super::{DaemonHeartbeatRecord, Database};

impl Database {
    pub async fn record_daemon_heartbeat(&self, phase: &str, detail: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO daemon_heartbeat (singleton, last_seen_at, phase, detail)
             VALUES (1, CURRENT_TIMESTAMP, ?, ?)
             ON CONFLICT(singleton) DO UPDATE SET
                 last_seen_at = CURRENT_TIMESTAMP,
                 phase = excluded.phase,
                 detail = excluded.detail",
        )
        .bind(phase)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_daemon_heartbeat(&self) -> Result<Option<DaemonHeartbeatRecord>> {
        let row = sqlx::query(
            "SELECT last_seen_at, phase, detail
             FROM daemon_heartbeat
             WHERE singleton = 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| DaemonHeartbeatRecord {
            last_seen_at: row.get("last_seen_at"),
            phase: row.get("phase"),
            detail: row.get("detail"),
        }))
    }
}
