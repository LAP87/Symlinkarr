use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{QueryBuilder, Row, Sqlite};

use super::{
    AcquisitionJobCounts, AcquisitionJobRecord, AcquisitionJobSeed, AcquisitionJobStatus,
    AcquisitionJobUpdate, AcquisitionRelinkKind, Database, MAX_JOB_ATTEMPTS,
};

impl Database {
    pub async fn enqueue_acquisition_jobs(&self, seeds: &[AcquisitionJobSeed]) -> Result<()> {
        let now = Utc::now();
        for seed in seeds {
            self.enqueue_acquisition_job(seed, now).await?;
        }
        Ok(())
    }

    async fn enqueue_acquisition_job(
        &self,
        seed: &AcquisitionJobSeed,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        let categories_json = serde_json::to_string(&seed.categories)?;
        let query_hints_json = serde_json::to_string(&seed.query_hints)?;
        let now_str = now.to_rfc3339();
        let existing = self
            .get_acquisition_job_by_request_key(&seed.request_key)
            .await?;

        if let Some(existing) = existing {
            let should_reset = self.should_reset_acquisition_job(&existing, now);
            sqlx::query(
                "UPDATE acquisition_jobs
                 SET label = ?,
                     query = ?,
                     query_hints_json = ?,
                     imdb_id = ?,
                     categories_json = ?,
                     arr = ?,
                     library_filter = ?,
                     relink_kind = ?,
                     relink_value = ?,
                     status = CASE WHEN ? THEN 'queued' ELSE status END,
                     release_title = CASE WHEN ? THEN NULL ELSE release_title END,
                     info_hash = CASE WHEN ? THEN NULL ELSE info_hash END,
                     error = CASE WHEN ? THEN NULL ELSE error END,
                     next_retry_at = CASE WHEN ? THEN NULL ELSE next_retry_at END,
                     submitted_at = CASE WHEN ? THEN NULL ELSE submitted_at END,
                     completed_at = CASE WHEN ? THEN NULL ELSE completed_at END,
                     updated_at = ?
                 WHERE id = ?",
            )
            .bind(&seed.label)
            .bind(&seed.query)
            .bind(query_hints_json)
            .bind(seed.imdb_id.as_deref())
            .bind(categories_json)
            .bind(&seed.arr)
            .bind(seed.library_filter.as_deref())
            .bind(seed.relink_kind.as_str())
            .bind(&seed.relink_value)
            .bind(should_reset)
            .bind(should_reset)
            .bind(should_reset)
            .bind(should_reset)
            .bind(should_reset)
            .bind(should_reset)
            .bind(should_reset)
            .bind(now_str)
            .bind(existing.id)
            .execute(&self.pool)
            .await?;
            return Ok(existing.id);
        }

        let result = sqlx::query(
            "INSERT INTO acquisition_jobs (
                request_key,
                label,
                query,
                query_hints_json,
                imdb_id,
                categories_json,
                arr,
                library_filter,
                relink_kind,
                relink_value,
                status,
                created_at,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
        )
        .bind(&seed.request_key)
        .bind(&seed.label)
        .bind(&seed.query)
        .bind(serde_json::to_string(&seed.query_hints)?)
        .bind(seed.imdb_id.as_deref())
        .bind(categories_json)
        .bind(&seed.arr)
        .bind(seed.library_filter.as_deref())
        .bind(seed.relink_kind.as_str())
        .bind(&seed.relink_value)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;

        Ok(result.last_insert_rowid())
    }

    fn should_reset_acquisition_job(
        &self,
        existing: &AcquisitionJobRecord,
        now: DateTime<Utc>,
    ) -> bool {
        match existing.status {
            AcquisitionJobStatus::Queued
            | AcquisitionJobStatus::Downloading
            | AcquisitionJobStatus::Relinking => false,
            AcquisitionJobStatus::Blocked
            | AcquisitionJobStatus::NoResult
            | AcquisitionJobStatus::CompletedUnlinked
            | AcquisitionJobStatus::Failed => {
                existing.next_retry_at.map(|at| at <= now).unwrap_or(true)
            }
            AcquisitionJobStatus::CompletedLinked => false,
        }
    }

    pub async fn get_manageable_acquisition_jobs(&self) -> Result<Vec<AcquisitionJobRecord>> {
        let now = Utc::now().to_rfc3339();
        let rows = sqlx::query(
            "SELECT
                id,
                request_key,
                label,
                query,
                query_hints_json,
                imdb_id,
                categories_json,
                arr,
                library_filter,
                relink_kind,
                relink_value,
                status,
                release_title,
                info_hash,
                error,
                attempts,
                next_retry_at,
                submitted_at,
                completed_at
             FROM acquisition_jobs
             WHERE (
                    status IN ('queued', 'downloading', 'relinking')
                    OR (
                        status IN ('blocked', 'no_result', 'completed_unlinked', 'failed')
                        AND (next_retry_at IS NULL OR next_retry_at <= ?)
                    )
                   )
               AND attempts < ?
             ORDER BY
                CASE status
                    WHEN 'downloading' THEN 0
                    WHEN 'relinking' THEN 1
                    ELSE 2
                END,
                id ASC",
        )
        .bind(now)
        .bind(MAX_JOB_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| self.row_to_acquisition_job(&row))
            .collect()
    }

    pub async fn list_acquisition_jobs(
        &self,
        statuses: Option<&[AcquisitionJobStatus]>,
        limit: usize,
    ) -> Result<Vec<AcquisitionJobRecord>> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT
                id,
                request_key,
                label,
                query,
                query_hints_json,
                imdb_id,
                categories_json,
                arr,
                library_filter,
                relink_kind,
                relink_value,
                status,
                release_title,
                info_hash,
                error,
                attempts,
                next_retry_at,
                submitted_at,
                completed_at
             FROM acquisition_jobs",
        );

        if let Some(statuses) = statuses.filter(|statuses| !statuses.is_empty()) {
            qb.push(" WHERE status IN (");
            let mut separated = qb.separated(", ");
            for status in statuses {
                separated.push_bind(status.as_str());
            }
            separated.push_unseparated(")");
        }

        qb.push(" ORDER BY id DESC LIMIT ").push_bind(limit as i64);

        let rows = qb.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| self.row_to_acquisition_job(&row))
            .collect()
    }

    pub async fn retry_acquisition_jobs(&self, statuses: &[AcquisitionJobStatus]) -> Result<u64> {
        if statuses.is_empty() {
            return Ok(0);
        }

        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "UPDATE acquisition_jobs
             SET status = 'queued',
                 error = NULL,
                 next_retry_at = NULL,
                 release_title = NULL,
                 info_hash = NULL,
                 submitted_at = NULL,
                 completed_at = NULL,
                 updated_at = ",
        );
        qb.push_bind(Utc::now().to_rfc3339());
        qb.push(" WHERE status IN (");
        let mut separated = qb.separated(", ");
        for status in statuses {
            separated.push_bind(status.as_str());
        }
        separated.push_unseparated(")");

        let result = qb.build().execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    pub async fn update_acquisition_job_state(
        &self,
        id: i64,
        update: &AcquisitionJobUpdate,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let next_retry_at = update.next_retry_at.map(|value| value.to_rfc3339());
        let submitted_at = update.submitted_at.map(|value| value.to_rfc3339());
        let completed_at = update.completed_at.map(|value| value.to_rfc3339());

        sqlx::query(
            "UPDATE acquisition_jobs
             SET status = ?,
                 release_title = ?,
                 info_hash = ?,
                 error = ?,
                 next_retry_at = ?,
                 submitted_at = ?,
                 completed_at = ?,
                 attempts = attempts + CASE WHEN ? THEN 1 ELSE 0 END,
                 updated_at = ?
             WHERE id = ?",
        )
        .bind(update.status.as_str())
        .bind(update.release_title.as_deref())
        .bind(update.info_hash.as_deref())
        .bind(update.error.as_deref())
        .bind(next_retry_at)
        .bind(submitted_at)
        .bind(completed_at)
        .bind(update.increment_attempts)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_acquisition_job_counts(&self) -> Result<AcquisitionJobCounts> {
        let row = sqlx::query(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END), 0) AS queued,
                COALESCE(SUM(CASE WHEN status = 'downloading' THEN 1 ELSE 0 END), 0) AS downloading,
                COALESCE(SUM(CASE WHEN status = 'relinking' THEN 1 ELSE 0 END), 0) AS relinking,
                COALESCE(SUM(CASE WHEN status = 'blocked' THEN 1 ELSE 0 END), 0) AS blocked,
                COALESCE(SUM(CASE WHEN status = 'no_result' THEN 1 ELSE 0 END), 0) AS no_result,
                COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS failed,
                COALESCE(SUM(CASE WHEN status = 'completed_unlinked' THEN 1 ELSE 0 END), 0) AS completed_unlinked
             FROM acquisition_jobs",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(AcquisitionJobCounts {
            queued: row.get("queued"),
            downloading: row.get("downloading"),
            relinking: row.get("relinking"),
            blocked: row.get("blocked"),
            no_result: row.get("no_result"),
            failed: row.get("failed"),
            completed_unlinked: row.get("completed_unlinked"),
        })
    }

    /// Recover jobs stuck in `Downloading` after a daemon crash.
    ///
    /// Jobs that were `Downloading` when the daemon crashed will never progress.
    /// This resets them to `Failed` with a short retry window so the queue drains normally.
    pub async fn recover_stale_downloading_jobs(&self, timeout_minutes: u64) -> Result<u32> {
        let cutoff = chrono::Utc::now() - chrono::Duration::minutes(timeout_minutes as i64);
        let cutoff_str = cutoff.to_rfc3339();

        let rows = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT id, submitted_at FROM acquisition_jobs WHERE status = 'downloading'",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut recovered = 0u32;
        for (id, submitted_at_opt) in rows {
            let is_stale = match submitted_at_opt.as_deref() {
                Some(s) => s < cutoff_str.as_str(),
                None => true,
            };
            if is_stale {
                let next_retry = (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339();
                sqlx::query(
                    "UPDATE acquisition_jobs
                     SET status = 'failed',
                         error = 'Recovered from stale Downloading state after daemon restart',
                         next_retry_at = ?,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?",
                )
                .bind(&next_retry)
                .bind(id)
                .execute(&self.pool)
                .await?;
                recovered += 1;
            }
        }
        Ok(recovered)
    }

    async fn get_acquisition_job_by_request_key(
        &self,
        request_key: &str,
    ) -> Result<Option<AcquisitionJobRecord>> {
        let row = sqlx::query(
            "SELECT
                id,
                request_key,
                label,
                query,
                query_hints_json,
                imdb_id,
                categories_json,
                arr,
                library_filter,
                relink_kind,
                relink_value,
                status,
                release_title,
                info_hash,
                error,
                attempts,
                next_retry_at,
                submitted_at,
                completed_at
             FROM acquisition_jobs
             WHERE request_key = ?",
        )
        .bind(request_key)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| self.row_to_acquisition_job(&row)).transpose()
    }

    fn row_to_acquisition_job(
        &self,
        row: &sqlx::sqlite::SqliteRow,
    ) -> Result<AcquisitionJobRecord> {
        let categories_json: String = row.get("categories_json");
        let query_hints_json: String = row.get("query_hints_json");
        let relink_kind: String = row.get("relink_kind");
        let status: String = row.get("status");

        Ok(AcquisitionJobRecord {
            id: row.get("id"),
            request_key: row.get("request_key"),
            label: row.get("label"),
            query: row.get("query"),
            query_hints: serde_json::from_str(&query_hints_json)?,
            imdb_id: row.get("imdb_id"),
            categories: serde_json::from_str(&categories_json)?,
            arr: row.get("arr"),
            library_filter: row.get("library_filter"),
            relink_kind: AcquisitionRelinkKind::from_db(&relink_kind)?,
            relink_value: row.get("relink_value"),
            status: AcquisitionJobStatus::from_db(&status)?,
            release_title: row.get("release_title"),
            info_hash: row.get("info_hash"),
            error: row.get("error"),
            attempts: row.get("attempts"),
            next_retry_at: parse_optional_datetime(row, "next_retry_at")?,
            submitted_at: parse_optional_datetime(row, "submitted_at")?,
            completed_at: parse_optional_datetime(row, "completed_at")?,
        })
    }
}

fn parse_optional_datetime(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<Option<DateTime<Utc>>> {
    let value: Option<String> = row.try_get(column)?;
    match value {
        Some(value) => Ok(Some(parse_datetime(&value)?)),
        None => Ok(None),
    }
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }

    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")?;
    Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}
