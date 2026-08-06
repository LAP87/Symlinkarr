use anyhow::{Context, Result};
use sqlx::Row;

use super::{Database, OperationConflict, OperationRunRecord};

pub const LIBRARY_OPERATION_LOCK: &str = "library-operation";
pub const OPERATION_LEASE_SECONDS: i64 = 5 * 60;

fn operation_from_row(row: sqlx::sqlite::SqliteRow) -> OperationRunRecord {
    OperationRunRecord {
        id: row.get("id"),
        lock_key: row.get("lock_key"),
        kind: row.get("kind"),
        origin: row.get("origin"),
        scope: row.get("scope"),
        status: row.get("status"),
        started_at: row.get("started_at"),
        heartbeat_at: row.get("heartbeat_at"),
        finished_at: row.get("finished_at"),
        message: row.get("message"),
        result_json: row.get("result_json"),
    }
}

impl Database {
    /// Atomically recovers an expired lease and attempts to create a new running
    /// operation. The partial unique index is the cross-process authority.
    pub async fn try_acquire_operation(
        &self,
        lock_key: &str,
        kind: &str,
        origin: &str,
        scope: Option<&str>,
    ) -> Result<std::result::Result<OperationRunRecord, OperationConflict>> {
        let mut tx = self.pool.begin().await?;
        let stale_modifier = format!("-{} seconds", OPERATION_LEASE_SECONDS);
        sqlx::query(
            "UPDATE operation_runs
             SET status = 'interrupted', finished_at = CURRENT_TIMESTAMP,
                 message = COALESCE(message, 'Interrupted after operation lease expired')
             WHERE lock_key = ? AND status = 'running'
               AND heartbeat_at < datetime('now', ?)",
        )
        .bind(lock_key)
        .bind(stale_modifier)
        .execute(&mut *tx)
        .await?;

        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO operation_runs
             (lock_key, kind, origin, scope, status, started_at, heartbeat_at)
             VALUES (?, ?, ?, ?, 'running', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(lock_key)
        .bind(kind)
        .bind(origin)
        .bind(scope)
        .execute(&mut *tx)
        .await?;

        if inserted.rows_affected() == 1 {
            let row = sqlx::query(
                "SELECT id, lock_key, kind, origin, scope, status,
                        started_at, heartbeat_at, finished_at, message, result_json
                 FROM operation_runs WHERE id = last_insert_rowid()",
            )
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Ok(operation_from_row(row)));
        }

        let row = sqlx::query(
            "SELECT id, lock_key, kind, origin, scope, status,
                    started_at, heartbeat_at, finished_at, message, result_json
             FROM operation_runs
             WHERE lock_key = ? AND status = 'running'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(lock_key)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let active = row
            .map(operation_from_row)
            .context("operation acquisition lost without an active operation")?;
        Ok(Err(OperationConflict { active }))
    }

    pub async fn heartbeat_operation(&self, id: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE operation_runs SET heartbeat_at = CURRENT_TIMESTAMP
             WHERE id = ? AND status = 'running'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn finish_operation(
        &self,
        id: i64,
        status: &str,
        message: Option<&str>,
        result_json: Option<&str>,
    ) -> Result<bool> {
        debug_assert!(matches!(status, "succeeded" | "failed" | "interrupted"));
        let result = sqlx::query(
            "UPDATE operation_runs
             SET status = ?, finished_at = CURRENT_TIMESTAMP, heartbeat_at = CURRENT_TIMESTAMP,
                 message = ?, result_json = ?
             WHERE id = ? AND status = 'running'",
        )
        .bind(status)
        .bind(message)
        .bind(result_json)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    #[cfg(test)]
    pub async fn get_operation_run(&self, id: i64) -> Result<Option<OperationRunRecord>> {
        let row = sqlx::query(
            "SELECT id, lock_key, kind, origin, scope, status,
                    started_at, heartbeat_at, finished_at, message, result_json
             FROM operation_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(operation_from_row))
    }

    pub async fn active_operation(&self, lock_key: &str) -> Result<Option<OperationRunRecord>> {
        let row = sqlx::query(
            "SELECT id, lock_key, kind, origin, scope, status,
                    started_at, heartbeat_at, finished_at, message, result_json
             FROM operation_runs
             WHERE lock_key = ? AND status = 'running'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(lock_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(operation_from_row))
    }

    #[cfg(test)]
    pub async fn list_operation_runs(&self, limit: i64) -> Result<Vec<OperationRunRecord>> {
        let rows = sqlx::query(
            "SELECT id, lock_key, kind, origin, scope, status,
                    started_at, heartbeat_at, finished_at, message, result_json
             FROM operation_runs ORDER BY id DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(operation_from_row).collect())
    }
}
