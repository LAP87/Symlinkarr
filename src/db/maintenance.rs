use anyhow::Result;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

use super::{Database, HousekeepingStats};

impl Database {
    /// Delete old records and optionally reclaim free pages.
    ///
    /// Full `VACUUM` can block writers for noticeable time on larger databases,
    /// so it is intentionally opt-in for scheduled maintenance windows.
    pub async fn housekeeping_with_vacuum(&self, run_vacuum: bool) -> Result<HousekeepingStats> {
        let scan_runs_deleted =
            sqlx::query("DELETE FROM scan_runs WHERE run_at < datetime('now', '-90 days')")
                .execute(&self.pool)
                .await?
                .rows_affected();

        let link_events_deleted =
            sqlx::query("DELETE FROM link_events WHERE event_at < datetime('now', '-30 days')")
                .execute(&self.pool)
                .await?
                .rows_affected();

        let old_jobs_deleted = sqlx::query(
            "DELETE FROM acquisition_jobs
             WHERE status IN ('completed_linked', 'completed_unlinked')
               AND updated_at < datetime('now', '-30 days')",
        )
        .execute(&self.pool)
        .await?
        .rows_affected();

        let expired_api_cache_deleted = sqlx::query(
            "DELETE FROM api_cache
             WHERE datetime(fetched_at, '+' || ttl_hours || ' hours') <= datetime('now')",
        )
        .execute(&self.pool)
        .await?
        .rows_affected();

        // Encourage SQLite to update query planner statistics.
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;
        if run_vacuum {
            // Reclaim free pages during a deliberate maintenance window without tying up the
            // main pool's writer slot for the full duration.
            self.run_maintenance_vacuum().await?;
        }

        Ok(HousekeepingStats {
            scan_runs_deleted,
            link_events_deleted,
            old_jobs_deleted,
            expired_api_cache_deleted,
        })
    }

    async fn run_maintenance_vacuum(&self) -> Result<()> {
        let options = SqliteConnectOptions::new()
            .filename(&self.db_path)
            .create_if_missing(false);
        let mut conn = SqliteConnection::connect_with(&options).await?;
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&mut conn)
            .await?;
        sqlx::query("VACUUM").execute(&mut conn).await?;
        conn.close().await?;
        Ok(())
    }
}
