use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Connection, QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use tracing::info;

use crate::models::{LinkRecord, LinkStatus, MediaType};

mod types;
mod acquisition_jobs;
mod scan_runs;

pub use types::*;

/// Maximum number of attempts before a job stops being picked up for retry (H-10).
const MAX_JOB_ATTEMPTS: i64 = 5;
const SCOPED_ROOT_QUERY_CHUNK_SIZE: usize = 250;
const SCOPED_ROOT_IN_MEMORY_FILTER_THRESHOLD: usize = 1024;

fn escape_sql_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Database manager for Symlinkarr state.
/// Uses SQLite via sqlx for async persistence.
pub struct Database {
    pool: SqlitePool,
    db_path: PathBuf,
}

const LATEST_SCHEMA_VERSION: i64 = 14;

// SqlitePool is Clone (wraps Arc), so Database can safely be Clone
impl Clone for Database {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            db_path: self.db_path.clone(),
        }
    }
}

impl Database {
    /// Create a new database connection and run migrations.
    pub async fn new(db_path: &str) -> Result<Self> {
        let path = PathBuf::from(db_path);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        // Enable relational safeguards, then tune SQLite for concurrent CLI/daemon/web access.
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&pool)
            .await?;

        let db = Self {
            pool,
            db_path: path.clone(),
        };
        db.run_migrations().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perm = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&path, perm);
        }

        info!("Database initialized: {}", db_path);
        Ok(db)
    }

    /// Export a consistent SQLite snapshot to a standalone file.
    pub async fn export_snapshot(&self, snapshot_path: &Path) -> Result<()> {
        if let Some(parent) = snapshot_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        if snapshot_path.exists() {
            std::fs::remove_file(snapshot_path)?;
        }

        let escaped = snapshot_path.to_string_lossy().replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{escaped}'"))
            .execute(&self.pool)
            .await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perm = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(snapshot_path, perm);
        }

        Ok(())
    }

    /// Run database migrations to create/update schema.
    async fn run_migrations(&self) -> Result<()> {
        self.ensure_schema_version_table().await?;
        let mut version = self.current_schema_version().await?;
        if version == 0 {
            version = self.infer_legacy_schema_version().await?;
            if version > 0 {
                sqlx::query("INSERT OR IGNORE INTO schema_version (version) VALUES (?)")
                    .bind(version)
                    .execute(&self.pool)
                    .await?;
            }
        }

        while version < LATEST_SCHEMA_VERSION {
            let next = version + 1;
            // Each migration + version bump is atomic; a crash mid-migration
            // leaves the schema_version unchanged so the migration re-runs cleanly.
            let mut tx = self.pool.begin().await?;
            self.apply_migration_tx(&mut tx, next).await?;
            sqlx::query("INSERT OR IGNORE INTO schema_version (version) VALUES (?)")
                .bind(next)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            version = next;
        }

        Ok(())
    }

    async fn ensure_schema_version_table(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY,
                applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn current_schema_version(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COALESCE(MAX(version), 0) as version FROM schema_version")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("version"))
    }

    pub async fn schema_version(&self) -> Result<i64> {
        self.current_schema_version().await
    }

    async fn infer_legacy_schema_version(&self) -> Result<i64> {
        if self.table_exists("acquisition_jobs").await? {
            if self
                .column_exists("scan_runs", "auto_acquire_requests")
                .await
                .unwrap_or(false)
            {
                return Ok(8);
            }
            if self
                .column_exists("acquisition_jobs", "query_hints_json")
                .await
                .unwrap_or(false)
            {
                return Ok(7);
            }
            if self
                .column_exists("acquisition_jobs", "imdb_id")
                .await
                .unwrap_or(false)
            {
                return Ok(6);
            }
            return Ok(5);
        }
        if self.table_exists("link_events").await? {
            return Ok(4);
        }
        if self.table_exists("scan_runs").await? {
            return Ok(3);
        }
        if self.table_exists("rd_torrents").await? || self.table_exists("links").await? {
            return Ok(2);
        }
        Ok(0)
    }

    async fn table_exists(&self, table_name: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind(table_name)
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    async fn column_exists(&self, table_name: &str, column_name: &str) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({})", table_name);
        let rows = sqlx::query(&pragma).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .any(|row| row.get::<String, _>("name") == column_name))
    }

    /// Apply a migration within a caller-supplied transaction.  All migration
    /// DDL uses `CREATE TABLE IF NOT EXISTS` / `ALTER TABLE IF NOT EXISTS`, so
    /// re-running after a partial failure is safe.
    async fn apply_migration_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        version: i64,
    ) -> Result<()> {
        match version {
            1 => self.migration_v1_tx(tx).await,
            2 => self.migration_v2_tx(tx).await,
            3 => self.migration_v3_tx(tx).await,
            4 => self.migration_v4_tx(tx).await,
            5 => self.migration_v5_tx(tx).await,
            6 => self.migration_v6_tx(tx).await,
            7 => self.migration_v7_tx(tx).await,
            8 => self.migration_v8_tx(tx).await,
            9 => self.migration_v9_tx(tx).await,
            10 => self.migration_v10_tx(tx).await,
            11 => self.migration_v11_tx(tx).await,
            12 => self.migration_v12_tx(tx).await,
            13 => self.migration_v13_tx(tx).await,
            14 => self.migration_v14_tx(tx).await,
            _ => anyhow::bail!(
                "Unsupported schema migration version {}. This build only knows migrations 1 through 14",
                version
            ),
        }
    }

    /// Apply a migration using a fresh transaction; used by test helpers.
    #[allow(dead_code)]
    async fn apply_migration(&self, version: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.apply_migration_tx(&mut tx, version).await?;
        tx.commit().await?;
        Ok(())
    }

    // ── Transactional migration variants ──────────────────────────────────────
    // Each _tx method mirrors its pool-based counterpart but executes within a
    // supplied transaction.  All DDL uses IF NOT EXISTS / idempotent patterns so
    // re-running after a crash is safe.

    async fn migration_v1_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS links (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_path TEXT NOT NULL,
                target_path TEXT NOT NULL UNIQUE,
                media_id TEXT NOT NULL,
                media_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS api_cache (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                cache_key TEXT NOT NULL UNIQUE,
                response_json TEXT NOT NULL,
                fetched_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                ttl_hours INTEGER NOT NULL DEFAULT 168
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_links_status ON links(status)")
            .execute(&mut **tx)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_links_media_id ON links(media_id)")
            .execute(&mut **tx)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_api_cache_key ON api_cache(cache_key)")
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn migration_v2_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scan_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scan_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                library_items_found INTEGER NOT NULL DEFAULT 0,
                source_items_found INTEGER NOT NULL DEFAULT 0,
                matches_found INTEGER NOT NULL DEFAULT 0,
                links_created INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS rd_torrents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                torrent_id TEXT NOT NULL UNIQUE,
                hash TEXT NOT NULL,
                filename TEXT NOT NULL,
                status TEXT NOT NULL,
                files_json TEXT NOT NULL,
                scanned_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_rd_torrents_id ON rd_torrents(torrent_id)")
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn migration_v3_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scan_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                dry_run INTEGER NOT NULL DEFAULT 0,
                library_filter TEXT,
                run_token TEXT,
                search_missing INTEGER NOT NULL DEFAULT 0,
                library_items_found INTEGER NOT NULL DEFAULT 0,
                source_items_found INTEGER NOT NULL DEFAULT 0,
                matches_found INTEGER NOT NULL DEFAULT 0,
                links_created INTEGER NOT NULL DEFAULT 0,
                links_updated INTEGER NOT NULL DEFAULT 0,
                dead_marked INTEGER NOT NULL DEFAULT 0,
                links_removed INTEGER NOT NULL DEFAULT 0,
                links_skipped INTEGER NOT NULL DEFAULT 0,
                ambiguous_skipped INTEGER NOT NULL DEFAULT 0,
                skip_reason_json TEXT,
                runtime_checks_ms INTEGER NOT NULL DEFAULT 0,
                library_scan_ms INTEGER NOT NULL DEFAULT 0,
                source_inventory_ms INTEGER NOT NULL DEFAULT 0,
                matching_ms INTEGER NOT NULL DEFAULT 0,
                title_enrichment_ms INTEGER NOT NULL DEFAULT 0,
                linking_ms INTEGER NOT NULL DEFAULT 0,
                plex_refresh_ms INTEGER NOT NULL DEFAULT 0,
                dead_link_sweep_ms INTEGER NOT NULL DEFAULT 0,
                cache_hit_ratio REAL,
                candidate_slots INTEGER NOT NULL DEFAULT 0,
                scored_candidates INTEGER NOT NULL DEFAULT 0,
                exact_id_hits INTEGER NOT NULL DEFAULT 0,
                auto_acquire_requests INTEGER NOT NULL DEFAULT 0,
                auto_acquire_missing_requests INTEGER NOT NULL DEFAULT 0,
                auto_acquire_cutoff_requests INTEGER NOT NULL DEFAULT 0,
                auto_acquire_dry_run_hits INTEGER NOT NULL DEFAULT 0,
                auto_acquire_submitted INTEGER NOT NULL DEFAULT 0,
                auto_acquire_no_result INTEGER NOT NULL DEFAULT 0,
                auto_acquire_blocked INTEGER NOT NULL DEFAULT 0,
                auto_acquire_failed INTEGER NOT NULL DEFAULT 0,
                auto_acquire_completed_linked INTEGER NOT NULL DEFAULT 0,
                auto_acquire_completed_unlinked INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_scan_runs_run_at ON scan_runs(run_at)")
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn migration_v4_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS link_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER,
                run_token TEXT,
                event_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                action TEXT NOT NULL,
                target_path TEXT NOT NULL,
                source_path TEXT,
                media_id TEXT,
                note TEXT
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_link_events_action ON link_events(action)")
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_link_events_target ON link_events(target_path)",
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn migration_v5_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS acquisition_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                request_key TEXT NOT NULL UNIQUE,
                label TEXT NOT NULL,
                query TEXT NOT NULL,
                categories_json TEXT NOT NULL,
                arr TEXT NOT NULL,
                library_filter TEXT,
                relink_kind TEXT NOT NULL,
                relink_value TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'queued',
                release_title TEXT,
                info_hash TEXT,
                error TEXT,
                attempts INTEGER NOT NULL DEFAULT 0,
                next_retry_at TEXT,
                submitted_at TEXT,
                completed_at TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_acquisition_jobs_status
             ON acquisition_jobs(status)",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_acquisition_jobs_retry
             ON acquisition_jobs(status, next_retry_at)",
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn migration_v6_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        // column_exists uses pool; since ALTER TABLE is idempotent via the error
        // check, running it inside the tx is safe even if column already exists.
        match sqlx::query("ALTER TABLE acquisition_jobs ADD COLUMN imdb_id TEXT")
            .execute(&mut **tx)
            .await
        {
            Ok(_) => {}
            Err(err) if err.to_string().contains("duplicate column name: imdb_id") => {}
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    async fn migration_v7_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        match sqlx::query(
            "ALTER TABLE acquisition_jobs ADD COLUMN query_hints_json TEXT NOT NULL DEFAULT '[]'",
        )
        .execute(&mut **tx)
        .await
        {
            Ok(_) => {}
            Err(err)
                if err
                    .to_string()
                    .contains("duplicate column name: query_hints_json") => {}
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    async fn migration_v8_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        let alter_statements = [
            "ALTER TABLE scan_runs ADD COLUMN library_filter TEXT",
            "ALTER TABLE scan_runs ADD COLUMN search_missing INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN runtime_checks_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN library_scan_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN source_inventory_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN matching_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN title_enrichment_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN linking_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN dead_link_sweep_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN cache_hit_ratio REAL",
            "ALTER TABLE scan_runs ADD COLUMN candidate_slots INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN scored_candidates INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN exact_id_hits INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_requests INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_missing_requests INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_cutoff_requests INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_dry_run_hits INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_submitted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_no_result INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_blocked INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_failed INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_completed_linked INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN auto_acquire_completed_unlinked INTEGER NOT NULL DEFAULT 0",
        ];

        for statement in alter_statements {
            match sqlx::query(statement).execute(&mut **tx).await {
                Ok(_) => {}
                Err(err) if err.to_string().contains("duplicate column name") => {}
                Err(err) => return Err(err.into()),
            }
        }

        Ok(())
    }

    async fn migration_v9_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        // Composite index on links(status, target_path) speeds up get_links_scoped queries
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_links_status_target ON links(status, target_path)",
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn migration_v10_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        let alter_statements = [
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_requested_paths INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_unique_paths INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_planned_batches INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_coalesced_batches INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_coalesced_paths INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_refreshed_batches INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_refreshed_paths_covered INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_skipped_batches INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_unresolved_paths INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_capped_batches INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_failed_batches INTEGER NOT NULL DEFAULT 0",
        ];

        for statement in alter_statements {
            match sqlx::query(statement).execute(&mut **tx).await {
                Ok(_) => {}
                Err(err) if err.to_string().contains("duplicate column name") => {}
                Err(err) => return Err(err.into()),
            }
        }

        Ok(())
    }

    async fn migration_v11_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        match sqlx::query(
            "ALTER TABLE scan_runs ADD COLUMN plex_refresh_aborted_due_to_cap INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&mut **tx)
        .await
        {
            Ok(_) => {}
            Err(err) if err.to_string().contains("duplicate column name") => {}
            Err(err) => return Err(err.into()),
        }

        Ok(())
    }

    async fn migration_v12_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        match sqlx::query("ALTER TABLE scan_runs ADD COLUMN media_server_refresh_json TEXT")
            .execute(&mut **tx)
            .await
        {
            Ok(_) => {}
            Err(err) if err.to_string().contains("duplicate column name") => {}
            Err(err) => return Err(err.into()),
        }

        Ok(())
    }

    async fn migration_v13_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        match sqlx::query("ALTER TABLE scan_runs ADD COLUMN skip_reason_json TEXT")
            .execute(&mut **tx)
            .await
        {
            Ok(_) => {}
            Err(err) if err.to_string().contains("duplicate column name") => {}
            Err(err) => return Err(err.into()),
        }

        Ok(())
    }

    async fn migration_v14_tx(&self, tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
        match sqlx::query("ALTER TABLE scan_runs ADD COLUMN run_token TEXT")
            .execute(&mut **tx)
            .await
        {
            Ok(_) => {}
            Err(err) if err.to_string().contains("duplicate column name") => {}
            Err(err) => return Err(err.into()),
        }

        match sqlx::query("ALTER TABLE link_events ADD COLUMN run_token TEXT")
            .execute(&mut **tx)
            .await
        {
            Ok(_) => {}
            Err(err) if err.to_string().contains("duplicate column name") => {}
            Err(err) => return Err(err.into()),
        }

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_link_events_run_token_event_at
             ON link_events(run_token, event_at)",
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    #[cfg(test)]
    async fn migrate_down_one(&self, current_version: i64) -> Result<()> {
        match current_version {
            14 => {
                sqlx::query("DROP INDEX IF EXISTS idx_link_events_run_token_event_at")
                    .execute(&self.pool)
                    .await?;
                if self.column_exists("link_events", "run_token").await? {
                    sqlx::query("ALTER TABLE link_events DROP COLUMN run_token")
                        .execute(&self.pool)
                        .await?;
                }
                if self.column_exists("scan_runs", "run_token").await? {
                    sqlx::query("ALTER TABLE scan_runs DROP COLUMN run_token")
                        .execute(&self.pool)
                        .await?;
                }
            }
            13 => {
                if self.column_exists("scan_runs", "skip_reason_json").await? {
                    sqlx::query("ALTER TABLE scan_runs DROP COLUMN skip_reason_json")
                        .execute(&self.pool)
                        .await?;
                }
            }
            12 => {
                if self
                    .column_exists("scan_runs", "media_server_refresh_json")
                    .await?
                {
                    sqlx::query("ALTER TABLE scan_runs DROP COLUMN media_server_refresh_json")
                        .execute(&self.pool)
                        .await?;
                }
            }
            11 => {
                if self
                    .column_exists("scan_runs", "plex_refresh_aborted_due_to_cap")
                    .await?
                {
                    sqlx::query(
                        "ALTER TABLE scan_runs DROP COLUMN plex_refresh_aborted_due_to_cap",
                    )
                    .execute(&self.pool)
                    .await?;
                }
            }
            10 => {
                let columns = [
                    "plex_refresh_failed_batches",
                    "plex_refresh_capped_batches",
                    "plex_refresh_unresolved_paths",
                    "plex_refresh_skipped_batches",
                    "plex_refresh_refreshed_paths_covered",
                    "plex_refresh_refreshed_batches",
                    "plex_refresh_coalesced_paths",
                    "plex_refresh_coalesced_batches",
                    "plex_refresh_planned_batches",
                    "plex_refresh_unique_paths",
                    "plex_refresh_requested_paths",
                ];

                for column in columns {
                    if self.column_exists("scan_runs", column).await? {
                        sqlx::query(&format!("ALTER TABLE scan_runs DROP COLUMN {}", column))
                            .execute(&self.pool)
                            .await?;
                    }
                }
            }
            9 => {
                sqlx::query("DROP INDEX IF EXISTS idx_links_status_target")
                    .execute(&self.pool)
                    .await?;
            }
            8 => {
                let columns = [
                    "auto_acquire_completed_unlinked",
                    "auto_acquire_completed_linked",
                    "auto_acquire_failed",
                    "auto_acquire_blocked",
                    "auto_acquire_no_result",
                    "auto_acquire_submitted",
                    "auto_acquire_dry_run_hits",
                    "auto_acquire_cutoff_requests",
                    "auto_acquire_missing_requests",
                    "auto_acquire_requests",
                    "exact_id_hits",
                    "scored_candidates",
                    "candidate_slots",
                    "cache_hit_ratio",
                    "dead_link_sweep_ms",
                    "plex_refresh_ms",
                    "linking_ms",
                    "title_enrichment_ms",
                    "matching_ms",
                    "source_inventory_ms",
                    "library_scan_ms",
                    "runtime_checks_ms",
                    "search_missing",
                    "library_filter",
                ];

                for column in columns {
                    if self.column_exists("scan_runs", column).await? {
                        sqlx::query(&format!("ALTER TABLE scan_runs DROP COLUMN {}", column))
                            .execute(&self.pool)
                            .await?;
                    }
                }
            }
            7 => {
                if self
                    .column_exists("acquisition_jobs", "query_hints_json")
                    .await?
                {
                    sqlx::query("ALTER TABLE acquisition_jobs DROP COLUMN query_hints_json")
                        .execute(&self.pool)
                        .await?;
                }
            }
            6 => {
                if self.column_exists("acquisition_jobs", "imdb_id").await? {
                    sqlx::query("ALTER TABLE acquisition_jobs DROP COLUMN imdb_id")
                        .execute(&self.pool)
                        .await?;
                }
            }
            5 => {
                sqlx::query("DROP INDEX IF EXISTS idx_acquisition_jobs_retry")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP INDEX IF EXISTS idx_acquisition_jobs_status")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP TABLE IF EXISTS acquisition_jobs")
                    .execute(&self.pool)
                    .await?;
            }
            4 => {
                sqlx::query("DROP INDEX IF EXISTS idx_link_events_target")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP INDEX IF EXISTS idx_link_events_action")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP TABLE IF EXISTS link_events")
                    .execute(&self.pool)
                    .await?;
            }
            3 => {
                sqlx::query("DROP INDEX IF EXISTS idx_scan_runs_run_at")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP TABLE IF EXISTS scan_runs")
                    .execute(&self.pool)
                    .await?;
            }
            2 => {
                sqlx::query("DROP INDEX IF EXISTS idx_rd_torrents_id")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP TABLE IF EXISTS rd_torrents")
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DROP TABLE IF EXISTS scan_history")
                    .execute(&self.pool)
                    .await?;
            }
            1 => {}
            _ => anyhow::bail!("Cannot migrate down unknown version {}", current_version),
        }
        Ok(())
    }

    #[cfg(test)]
    async fn migrate_to_for_tests(&self, target_version: i64) -> Result<()> {
        let mut current = self.current_schema_version().await?;
        if !(1..=LATEST_SCHEMA_VERSION).contains(&target_version) {
            anyhow::bail!(
                "Target schema version {} out of range 1..={}",
                target_version,
                LATEST_SCHEMA_VERSION
            );
        }

        while current < target_version {
            let next = current + 1;
            self.apply_migration(next).await?;
            sqlx::query("INSERT OR IGNORE INTO schema_version (version) VALUES (?)")
                .bind(next)
                .execute(&self.pool)
                .await?;
            current = next;
        }

        while current > target_version {
            self.migrate_down_one(current).await?;
            sqlx::query("DELETE FROM schema_version WHERE version = ?")
                .bind(current)
                .execute(&self.pool)
                .await?;
            current -= 1;
        }

        Ok(())
    }

    // --- Link operations ---

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

    // --- Stats by category ---

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

    // --- Cache operations ---

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

    // --- RD Cache operations ---

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
    /// with status "downloaded".  Used to verify that a torrent has completed
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
    /// because the per-cycle fetch cap was reached).  Callers compare the two
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
        // SUM returns NULL when there are no rows
        let total: i64 = row.try_get("total").unwrap_or(0);
        let cached: i64 = row.try_get("cached").unwrap_or(0);
        Ok((cached, total))
    }

    // --- Transaction support (C-01) ---

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

    // --- Data retention / housekeeping (H-09) ---

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
    let root_set: std::collections::HashSet<&str> =
        roots.iter().map(|root| root.as_str()).collect();
    records
        .into_iter()
        .filter(|record| path_matches_any_root(&record.target_path, &root_set))
        .collect()
}

fn filter_dead_link_seeds_by_roots(
    seeds: Vec<DeadLinkSeed>,
    roots: &[String],
) -> Vec<DeadLinkSeed> {
    let root_set: std::collections::HashSet<&str> =
        roots.iter().map(|root| root.as_str()).collect();
    seeds
        .into_iter()
        .filter(|seed| path_matches_any_root(&seed.target_path, &root_set))
        .collect()
}

fn path_matches_any_root(path: &Path, root_set: &std::collections::HashSet<&str>) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor
            .to_str()
            .map(|text| root_set.contains(text.trim_end_matches('/')))
            .unwrap_or(false)
    })
}

fn path_to_db_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8: {:?}", path))
}

fn escape_sql_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_link(source: &str, target: &str) -> LinkRecord {
        LinkRecord {
            id: None,
            source_path: PathBuf::from(source),
            target_path: PathBuf::from(target),
            media_id: "tvdb-12345".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn test_insert_and_get_active_links() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
        let row_id = db.insert_link(&record).await.unwrap();
        assert!(row_id > 0);

        let active = db.get_active_links().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].media_id, "tvdb-12345");
    }

    #[tokio::test]
    async fn test_get_active_links_limited_applies_limit_in_sql() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.insert_link(&sample_link(
            "/mnt/rd/show/ep01.mkv",
            "/plex/show/S01E01.mkv",
        ))
        .await
        .unwrap();
        db.insert_link(&sample_link(
            "/mnt/rd/show/ep02.mkv",
            "/plex/show/S01E02.mkv",
        ))
        .await
        .unwrap();
        db.insert_link(&sample_link(
            "/mnt/rd/show/ep03.mkv",
            "/plex/show/S01E03.mkv",
        ))
        .await
        .unwrap();

        let active = db.get_active_links_limited(2).await.unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(
            active[0].target_path,
            PathBuf::from("/plex/show/S01E03.mkv")
        );
        assert_eq!(
            active[1].target_path,
            PathBuf::from("/plex/show/S01E02.mkv")
        );
    }

    #[tokio::test]
    async fn test_mark_dead() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
        db.insert_link(&record).await.unwrap();

        db.mark_dead("/plex/show/S01E01.mkv").await.unwrap();

        let active = db.get_active_links().await.unwrap();
        assert_eq!(active.len(), 0);

        let dead = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
        assert_eq!(dead.len(), 1);
    }

    #[tokio::test]
    async fn test_get_dead_link_seeds_scoped_filters_by_target_root() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let mut series = sample_link("/mnt/rd/series/ep01.mkv", "/plex/Series/Show/S01E01.mkv");
        series.media_id = "tvdb-series".to_string();
        let mut movies = sample_link("/mnt/rd/movies/m1.mkv", "/plex/Movies/Movie (2020).mkv");
        movies.media_id = "tmdb-movie".to_string();

        db.insert_link(&series).await.unwrap();
        db.insert_link(&movies).await.unwrap();
        db.mark_dead_path(&series.target_path).await.unwrap();
        db.mark_dead_path(&movies.target_path).await.unwrap();

        let roots = vec![PathBuf::from("/plex/Series")];
        let scoped = db.get_dead_link_seeds_scoped(Some(&roots)).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].target_path,
            PathBuf::from("/plex/Series/Show/S01E01.mkv")
        );
    }

    #[tokio::test]
    async fn test_get_active_links_scoped_filters_by_target_root() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let mut anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
        anime.media_id = "tvdb-anime".to_string();
        let mut series = sample_link("/mnt/rd/series/ep01.mkv", "/plex/Series/Show/S01E01.mkv");
        series.media_id = "tvdb-series".to_string();

        db.insert_link(&anime).await.unwrap();
        db.insert_link(&series).await.unwrap();

        let roots = vec![PathBuf::from("/plex/Anime")];
        let scoped = db.get_active_links_scoped(Some(&roots)).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].target_path,
            PathBuf::from("/plex/Anime/Show/S01E01.mkv")
        );
    }

    #[tokio::test]
    async fn test_get_active_links_scoped_batches_large_root_lists() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
        db.insert_link(&anime).await.unwrap();

        let mut roots = (0..1200)
            .map(|i| PathBuf::from(format!("/plex/Noise/{i}")))
            .collect::<Vec<_>>();
        roots.push(PathBuf::from("/plex/Anime"));

        let scoped = db.get_active_links_scoped(Some(&roots)).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].target_path,
            PathBuf::from("/plex/Anime/Show/S01E01.mkv")
        );
    }

    #[tokio::test]
    async fn test_get_dead_link_seeds_scoped_batches_large_root_lists() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
        db.insert_link(&anime).await.unwrap();
        db.mark_dead_path(&anime.target_path).await.unwrap();

        let mut roots = (0..1200)
            .map(|i| PathBuf::from(format!("/plex/Noise/{i}")))
            .collect::<Vec<_>>();
        roots.push(PathBuf::from("/plex/Anime"));

        let scoped = db.get_dead_link_seeds_scoped(Some(&roots)).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].target_path,
            PathBuf::from("/plex/Anime/Show/S01E01.mkv")
        );
    }

    #[tokio::test]
    async fn test_get_links_by_targets_returns_only_exact_matches() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
        let series = sample_link("/mnt/rd/series/ep01.mkv", "/plex/Series/Show/S01E01.mkv");

        db.insert_link(&anime).await.unwrap();
        db.insert_link(&series).await.unwrap();

        let paths = vec![
            PathBuf::from("/plex/Anime/Show/S01E01.mkv"),
            PathBuf::from("/plex/Anime/Show/S01E01.mkv"),
            PathBuf::from("/plex/Missing/Show/S01E01.mkv"),
        ];
        let scoped = db.get_links_by_targets(&paths).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].target_path,
            PathBuf::from("/plex/Anime/Show/S01E01.mkv")
        );
    }

    #[tokio::test]
    async fn test_link_exists() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        assert!(!db.link_exists("/plex/show/S01E01.mkv").await.unwrap());

        let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
        db.insert_link(&record).await.unwrap();
        assert!(db.link_exists("/plex/show/S01E01.mkv").await.unwrap());
    }

    #[tokio::test]
    async fn test_get_stats() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let (active, dead, total) = db.get_stats().await.unwrap();
        assert_eq!((active, dead, total), (0, 0, 0));

        db.insert_link(&sample_link("/a", "/b")).await.unwrap();
        db.insert_link(&sample_link("/c", "/d")).await.unwrap();
        db.mark_dead("/d").await.unwrap();

        let (active, dead, total) = db.get_stats().await.unwrap();
        assert_eq!(active, 1);
        assert_eq!(dead, 1);
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn test_cache_set_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Miss
        assert!(db.get_cached("tmdb:12345").await.unwrap().is_none());

        // Set
        db.set_cached("tmdb:12345", r#"{"title":"Test"}"#, 168)
            .await
            .unwrap();

        // Hit
        let cached = db.get_cached("tmdb:12345").await.unwrap();
        assert!(cached.is_some());
        assert!(cached.unwrap().contains("Test"));
    }

    #[tokio::test]
    async fn test_cache_invalidation_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.set_cached("tmdb:invalidate-me", r#"{"title":"Test"}"#, 168)
            .await
            .unwrap();
        assert!(db.invalidate_cached("tmdb:invalidate-me").await.unwrap());
        assert!(db.get_cached("tmdb:invalidate-me").await.unwrap().is_none());
        assert!(!db.invalidate_cached("tmdb:invalidate-me").await.unwrap());
    }

    #[tokio::test]
    async fn test_cache_prefix_invalidation_removes_matching_entries_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.set_cached("tmdb:tv:1", r#"{"title":"One"}"#, 168)
            .await
            .unwrap();
        db.set_cached("tmdb:tv:external_ids:1", r#"{"imdb_id":"tt1"}"#, 168)
            .await
            .unwrap();
        db.set_cached("tmdb:movie:1", r#"{"title":"Movie"}"#, 168)
            .await
            .unwrap();

        let deleted = db.invalidate_cached_prefix("tmdb:tv:").await.unwrap();
        assert_eq!(deleted, 2);
        assert!(db.get_cached("tmdb:tv:1").await.unwrap().is_none());
        assert!(db
            .get_cached("tmdb:tv:external_ids:1")
            .await
            .unwrap()
            .is_none());
        assert!(db.get_cached("tmdb:movie:1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_record_scan() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        // Should not panic
        db.record_scan(100, 500, 42, 10).await.unwrap();
    }

    #[tokio::test]
    async fn test_upsert_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let r1 = sample_link("/mnt/rd/old.mkv", "/plex/show/ep.mkv");
        db.insert_link(&r1).await.unwrap();

        // Upsert with same target_path but different source
        let mut r2 = sample_link("/mnt/rd/new.mkv", "/plex/show/ep.mkv");
        r2.media_id = "tmdb-99999".to_string();
        db.insert_link(&r2).await.unwrap();

        let active = db.get_active_links().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].source_path, PathBuf::from("/mnt/rd/new.mkv"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_insert_link_non_utf8_path_fails() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let invalid = PathBuf::from(OsString::from_vec(vec![0xf0, 0x28, 0x8c, 0xbc]));
        let record = LinkRecord {
            id: None,
            source_path: invalid,
            target_path: PathBuf::from("/plex/show/S01E01.mkv"),
            media_id: "tvdb-12345".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        };

        let result = db.insert_link(&record).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_migrations_can_move_down_and_up() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(
            db.current_schema_version().await.unwrap(),
            LATEST_SCHEMA_VERSION
        );
        assert!(db.table_exists("scan_runs").await.unwrap());
        assert!(db.table_exists("link_events").await.unwrap());
        assert!(db.table_exists("acquisition_jobs").await.unwrap());

        db.migrate_to_for_tests(2).await.unwrap();
        assert_eq!(db.current_schema_version().await.unwrap(), 2);
        assert!(!db.table_exists("scan_runs").await.unwrap());
        assert!(!db.table_exists("link_events").await.unwrap());
        assert!(!db.table_exists("acquisition_jobs").await.unwrap());

        db.migrate_to_for_tests(LATEST_SCHEMA_VERSION)
            .await
            .unwrap();
        assert_eq!(
            db.current_schema_version().await.unwrap(),
            LATEST_SCHEMA_VERSION
        );
        assert!(db.table_exists("scan_runs").await.unwrap());
        assert!(db.table_exists("link_events").await.unwrap());
        assert!(db.table_exists("acquisition_jobs").await.unwrap());
    }

    #[tokio::test]
    async fn test_latest_migration_creates_links_status_target_index() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_links_status_target'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

        assert_eq!(index_count, 1);
    }

    #[tokio::test]
    async fn test_latest_migration_creates_plex_refresh_abort_column() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        assert!(db
            .column_exists("scan_runs", "plex_refresh_aborted_due_to_cap")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_latest_migration_creates_media_server_refresh_json_column() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        assert!(db
            .column_exists("scan_runs", "media_server_refresh_json")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_latest_migration_creates_skip_reason_json_column() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        assert!(db
            .column_exists("scan_runs", "skip_reason_json")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_record_link_event_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.record_link_event_fields(
            "created",
            Path::new("/plex/show/S01E01.mkv"),
            Some(Path::new("/mnt/rd/show/ep01.mkv")),
            Some("tvdb-12345"),
            Some("test-event"),
        )
        .await
        .unwrap();

        let row = sqlx::query(
            "SELECT action, target_path, source_path, media_id, note FROM link_events ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let action: String = row.get("action");
        let target_path: String = row.get("target_path");
        let source_path: Option<String> = row.get("source_path");
        let media_id: Option<String> = row.get("media_id");
        let note: Option<String> = row.get("note");

        assert_eq!(action, "created");
        assert_eq!(target_path, "/plex/show/S01E01.mkv");
        assert_eq!(source_path.as_deref(), Some("/mnt/rd/show/ep01.mkv"));
        assert_eq!(media_id.as_deref(), Some("tvdb-12345"));
        assert_eq!(note.as_deref(), Some("test-event"));
    }

    #[tokio::test]
    async fn test_has_active_link_for_episode_matches_slot_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.insert_link(&LinkRecord {
            id: None,
            source_path: PathBuf::from("/mnt/rd/show/ep09.mkv"),
            target_path: PathBuf::from("/plex/Show/Season 01/Show - S01E09.mkv"),
            media_id: "tvdb-12345".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap();

        assert!(db
            .has_active_link_for_episode("tvdb-12345", 1, 9)
            .await
            .unwrap());
        assert!(!db
            .has_active_link_for_episode("tvdb-12345", 1, 10)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_acquisition_jobs_deduplicate_and_resume_when_due() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed = AcquisitionJobSeed {
            request_key: "media:tvdb-12345".to_string(),
            label: "Test Show".to_string(),
            query: "Test Show S01E01".to_string(),
            query_hints: vec!["Example Alt 1".to_string()],
            imdb_id: None,
            categories: vec![5000],
            arr: "sonarr".to_string(),
            library_filter: Some("TV".to_string()),
            relink_kind: AcquisitionRelinkKind::MediaId,
            relink_value: "tvdb-12345".to_string(),
        };

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
            .await
            .unwrap();

        let active = db.get_manageable_acquisition_jobs().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].status, AcquisitionJobStatus::Queued);
        assert_eq!(active[0].categories, vec![5000]);
        assert_eq!(active[0].query_hints, vec!["Example Alt 1".to_string()]);
        let counts = db.get_acquisition_job_counts().await.unwrap();
        assert_eq!(counts.queued, 1);
        assert_eq!(counts.active_total(), 1);

        let future_retry = Utc::now() + chrono::Duration::minutes(10);
        db.update_acquisition_job_state(
            active[0].id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Failed,
                release_title: None,
                info_hash: None,
                error: Some("rate limited".to_string()),
                next_retry_at: Some(future_retry),
                submitted_at: None,
                completed_at: None,
                increment_attempts: true,
            },
        )
        .await
        .unwrap();

        assert!(db
            .get_manageable_acquisition_jobs()
            .await
            .unwrap()
            .is_empty());

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
            .await
            .unwrap();
        assert!(db
            .get_manageable_acquisition_jobs()
            .await
            .unwrap()
            .is_empty());

        db.update_acquisition_job_state(
            active[0].id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Failed,
                release_title: None,
                info_hash: None,
                error: Some("retry now".to_string()),
                next_retry_at: Some(Utc::now() - chrono::Duration::minutes(1)),
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();

        db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
            query: "Test Show S01E01 1080p".to_string(),
            ..seed
        }])
        .await
        .unwrap();

        let retried = db.get_manageable_acquisition_jobs().await.unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].status, AcquisitionJobStatus::Queued);
        assert_eq!(retried[0].query, "Test Show S01E01 1080p");
        assert_eq!(retried[0].attempts, 1);
        assert!(retried[0].error.is_none());
    }

    #[tokio::test]
    async fn test_completed_linked_jobs_do_not_reset_on_reenqueue() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed = AcquisitionJobSeed {
            request_key: "episode:tvdb-12345:1:1".to_string(),
            label: "Test Show S01E01".to_string(),
            query: "Test Show S01E01".to_string(),
            query_hints: vec!["Alt Title 1".to_string()],
            imdb_id: None,
            categories: vec![5070],
            arr: "sonarr-anime".to_string(),
            library_filter: Some("Anime".to_string()),
            relink_kind: AcquisitionRelinkKind::MediaEpisode,
            relink_value: "tvdb-12345|1|1".to_string(),
        };

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
            .await
            .unwrap();
        let job = db
            .get_manageable_acquisition_jobs()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        db.update_acquisition_job_state(
            job.id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::CompletedLinked,
                release_title: Some("[SubsPlease] Test Show - 01".to_string()),
                info_hash: Some("abc123".to_string()),
                error: None,
                next_retry_at: None,
                submitted_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
                increment_attempts: false,
            },
        )
        .await
        .unwrap();

        db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
            query: "Test Show S01E01 upgrade".to_string(),
            ..seed
        }])
        .await
        .unwrap();

        assert!(db
            .get_manageable_acquisition_jobs()
            .await
            .unwrap()
            .is_empty());

        let stored = db
            .list_acquisition_jobs(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.request_key == "episode:tvdb-12345:1:1")
            .unwrap();
        assert_eq!(stored.status, AcquisitionJobStatus::CompletedLinked);
        assert_eq!(
            stored.release_title.as_deref(),
            Some("[SubsPlease] Test Show - 01")
        );
    }

    // ── Test 1: housekeeping retention boundaries ──────────────────────────────

    #[tokio::test]
    async fn test_housekeeping_retention_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // scan_runs: one old (>90 days), one recent
        sqlx::query(
            "INSERT INTO scan_runs (run_at, dry_run, library_items_found, source_items_found, \
             matches_found, links_created, links_updated, dead_marked, links_removed, \
             links_skipped, ambiguous_skipped) \
             VALUES (datetime('now', '-100 days'), 0, 10, 20, 5, 3, 1, 0, 0, 0, 0)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO scan_runs (run_at, dry_run, library_items_found, source_items_found, \
             matches_found, links_created, links_updated, dead_marked, links_removed, \
             links_skipped, ambiguous_skipped) \
             VALUES (datetime('now', '-10 days'), 0, 5, 10, 3, 2, 0, 0, 0, 0, 0)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        // link_events: one old (>30 days), one recent
        sqlx::query(
            "INSERT INTO link_events (event_at, action, target_path) \
             VALUES (datetime('now', '-40 days'), 'created', '/plex/old.mkv')",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO link_events (event_at, action, target_path) \
             VALUES (datetime('now', '-5 days'), 'created', '/plex/recent.mkv')",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        // acquisition_jobs: old completed_linked (>30 days), recent completed_linked,
        // active queued job (should NEVER be deleted regardless of age)
        sqlx::query(
            "INSERT INTO acquisition_jobs \
             (request_key, label, query, categories_json, arr, relink_kind, relink_value, \
              status, updated_at, created_at) \
             VALUES ('key-old-done', 'Old Done', 'q', '[]', 'sonarr', 'media_id', 'tvdb-1', \
                     'completed_linked', datetime('now', '-40 days'), datetime('now', '-40 days'))",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO acquisition_jobs \
             (request_key, label, query, categories_json, arr, relink_kind, relink_value, \
              status, updated_at, created_at) \
             VALUES ('key-recent-done', 'Recent Done', 'q', '[]', 'sonarr', 'media_id', 'tvdb-2', \
                     'completed_linked', datetime('now', '-5 days'), datetime('now', '-5 days'))",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO acquisition_jobs \
             (request_key, label, query, categories_json, arr, relink_kind, relink_value, \
              status, updated_at, created_at) \
             VALUES ('key-active', 'Active Job', 'q', '[]', 'sonarr', 'media_id', 'tvdb-3', \
                     'queued', datetime('now', '-100 days'), datetime('now', '-100 days'))",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let stats = db.housekeeping_with_vacuum(false).await.unwrap();

        assert_eq!(stats.scan_runs_deleted, 1, "only old scan_run deleted");
        assert_eq!(stats.link_events_deleted, 1, "only old link_event deleted");
        assert_eq!(stats.old_jobs_deleted, 1, "only old completed job deleted");
        assert_eq!(
            stats.expired_api_cache_deleted, 0,
            "no expired API cache rows in this fixture"
        );

        // Verify recent scan_run survives
        let remaining_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_runs")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(remaining_runs, 1);

        // Verify recent link_event survives
        let remaining_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_events")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(remaining_events, 1);

        // Verify active (queued) job is never deleted, recent completed_linked survives
        let remaining_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acquisition_jobs")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(remaining_jobs, 2, "active + recent completed both survive");
    }

    #[tokio::test]
    async fn test_housekeeping_with_vacuum_uses_maintenance_connection() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("vacuum.db");
        let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

        db.set_cached("tmdb:tv:expired", "{\"title\":\"expired\"}", 0)
            .await
            .unwrap();

        let stats = db.housekeeping_with_vacuum(true).await.unwrap();

        assert_eq!(stats.expired_api_cache_deleted, 1);
        assert!(db.get_cached("tmdb:tv:expired").await.unwrap().is_none());
        assert!(
            db_path.exists(),
            "database file should still exist after VACUUM"
        );
    }

    // ── Test 2: recover_stale_downloading_jobs ─────────────────────────────────

    fn make_seed(request_key: &str, label: &str, relink_value: &str) -> AcquisitionJobSeed {
        AcquisitionJobSeed {
            request_key: request_key.to_string(),
            label: label.to_string(),
            query: "some query".to_string(),
            query_hints: Vec::new(),
            imdb_id: None,
            categories: vec![5000],
            arr: "sonarr".to_string(),
            library_filter: None,
            relink_kind: AcquisitionRelinkKind::MediaId,
            relink_value: relink_value.to_string(),
        }
    }

    #[tokio::test]
    async fn test_recover_stale_downloading_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed_stale = make_seed("key1", "Stale Job", "tvdb-1");
        let seed_recent = make_seed("key2", "Recent Job", "tvdb-2");
        let seed_queued = make_seed("key3", "Queued Job", "tvdb-3");

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_stale))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_recent))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_queued))
            .await
            .unwrap();

        let jobs = db.get_manageable_acquisition_jobs().await.unwrap();
        let stale_job = jobs.iter().find(|j| j.request_key == "key1").unwrap();
        let recent_job = jobs.iter().find(|j| j.request_key == "key2").unwrap();

        // Set stale job to downloading with old submitted_at (>60 min ago)
        let old_submitted = Utc::now() - chrono::Duration::hours(3);
        db.update_acquisition_job_state(
            stale_job.id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Downloading,
                release_title: Some("Some.Release".to_string()),
                info_hash: Some("abc".to_string()),
                error: None,
                next_retry_at: None,
                submitted_at: Some(old_submitted),
                completed_at: None,
                increment_attempts: true,
            },
        )
        .await
        .unwrap();

        // Set recent job to downloading with submitted_at < 60 min ago
        let recent_submitted = Utc::now() - chrono::Duration::minutes(10);
        db.update_acquisition_job_state(
            recent_job.id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Downloading,
                release_title: Some("Other.Release".to_string()),
                info_hash: Some("def".to_string()),
                error: None,
                next_retry_at: None,
                submitted_at: Some(recent_submitted),
                completed_at: None,
                increment_attempts: true,
            },
        )
        .await
        .unwrap();

        let recovered = db.recover_stale_downloading_jobs(60).await.unwrap();
        assert_eq!(recovered, 1, "only one stale job recovered");

        // Stale job is now failed with "stale" in error message
        let stale_stored = db
            .list_acquisition_jobs(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.request_key == "key1")
            .unwrap();
        assert_eq!(stale_stored.status, AcquisitionJobStatus::Failed);
        assert!(
            stale_stored
                .error
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains("stale"),
            "error should mention stale"
        );

        // Recent downloading job is still downloading
        let recent_stored = db
            .list_acquisition_jobs(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.request_key == "key2")
            .unwrap();
        assert_eq!(recent_stored.status, AcquisitionJobStatus::Downloading);

        // Queued job is unchanged
        let queued_stored = db
            .list_acquisition_jobs(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.request_key == "key3")
            .unwrap();
        assert_eq!(queued_stored.status, AcquisitionJobStatus::Queued);
    }

    // ── Test 3: MAX_JOB_ATTEMPTS gate ─────────────────────────────────────────

    #[tokio::test]
    async fn test_max_job_attempts_gate() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed = make_seed("key-maxed", "Maxed Job", "tvdb-99");
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
            .await
            .unwrap();

        // Set attempts = 5 via raw SQL to hit the MAX_JOB_ATTEMPTS boundary
        sqlx::query("UPDATE acquisition_jobs SET attempts = 5 WHERE request_key = 'key-maxed'")
            .execute(&db.pool)
            .await
            .unwrap();

        let manageable = db.get_manageable_acquisition_jobs().await.unwrap();
        assert!(
            manageable.is_empty(),
            "job with 5 attempts should be excluded"
        );
    }

    // ── Test 4: retry_acquisition_jobs by status ───────────────────────────────

    #[tokio::test]
    async fn test_retry_acquisition_jobs_by_status() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed_failed = make_seed("key-failed", "Failed Job", "tvdb-1");
        let seed_blocked = make_seed("key-blocked", "Blocked Job", "tvdb-2");
        let seed_no_result = make_seed("key-no-result", "No Result Job", "tvdb-3");

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_failed))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_blocked))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_no_result))
            .await
            .unwrap();

        let jobs = db.get_manageable_acquisition_jobs().await.unwrap();
        let failed_id = jobs
            .iter()
            .find(|j| j.request_key == "key-failed")
            .unwrap()
            .id;
        let blocked_id = jobs
            .iter()
            .find(|j| j.request_key == "key-blocked")
            .unwrap()
            .id;
        let no_result_id = jobs
            .iter()
            .find(|j| j.request_key == "key-no-result")
            .unwrap()
            .id;

        // Set statuses directly
        db.update_acquisition_job_state(
            failed_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Failed,
                release_title: None,
                info_hash: None,
                error: Some("failed".to_string()),
                next_retry_at: None,
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();
        db.update_acquisition_job_state(
            blocked_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Blocked,
                release_title: None,
                info_hash: None,
                error: Some("blocked".to_string()),
                next_retry_at: None,
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();
        db.update_acquisition_job_state(
            no_result_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::NoResult,
                release_title: None,
                info_hash: None,
                error: None,
                next_retry_at: None,
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();

        // Retry only failed
        let retried = db
            .retry_acquisition_jobs(&[AcquisitionJobStatus::Failed])
            .await
            .unwrap();
        assert_eq!(retried, 1);

        let failed_now = db
            .list_acquisition_jobs(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.id == failed_id)
            .unwrap();
        assert_eq!(failed_now.status, AcquisitionJobStatus::Queued);

        let blocked_now = db
            .list_acquisition_jobs(None, 10)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.id == blocked_id)
            .unwrap();
        assert_eq!(blocked_now.status, AcquisitionJobStatus::Blocked);

        // Retry blocked + no_result
        let retried2 = db
            .retry_acquisition_jobs(&[
                AcquisitionJobStatus::Blocked,
                AcquisitionJobStatus::NoResult,
            ])
            .await
            .unwrap();
        assert_eq!(retried2, 2);
    }

    // ── Test 5: get_manageable_acquisition_jobs ordering ──────────────────────

    #[tokio::test]
    async fn test_manageable_jobs_priority_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed_queued = make_seed("key-queued", "Queued", "tvdb-1");
        let seed_dl = make_seed("key-dl", "Downloading", "tvdb-2");
        let seed_rl = make_seed("key-rl", "Relinking", "tvdb-3");

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_queued))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_dl))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_rl))
            .await
            .unwrap();

        let jobs = db.get_manageable_acquisition_jobs().await.unwrap();
        let dl_id = jobs.iter().find(|j| j.request_key == "key-dl").unwrap().id;
        let rl_id = jobs.iter().find(|j| j.request_key == "key-rl").unwrap().id;

        db.update_acquisition_job_state(
            dl_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Downloading,
                release_title: None,
                info_hash: None,
                error: None,
                next_retry_at: None,
                submitted_at: Some(Utc::now()),
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();

        db.update_acquisition_job_state(
            rl_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Relinking,
                release_title: None,
                info_hash: None,
                error: None,
                next_retry_at: None,
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();

        let ordered = db.get_manageable_acquisition_jobs().await.unwrap();
        assert_eq!(ordered.len(), 3);
        assert_eq!(ordered[0].status, AcquisitionJobStatus::Downloading);
        assert_eq!(ordered[1].status, AcquisitionJobStatus::Relinking);
        assert_eq!(ordered[2].status, AcquisitionJobStatus::Queued);
    }

    // ── Test 6: list_acquisition_jobs with status filter ──────────────────────

    #[tokio::test]
    async fn test_list_acquisition_jobs_status_filter() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let seed_q = make_seed("key-q", "Queued", "tvdb-1");
        let seed_f = make_seed("key-f", "Failed", "tvdb-2");
        let seed_b = make_seed("key-b", "Blocked", "tvdb-3");

        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_q))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_f))
            .await
            .unwrap();
        db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_b))
            .await
            .unwrap();

        let all_jobs = db.get_manageable_acquisition_jobs().await.unwrap();
        let f_id = all_jobs
            .iter()
            .find(|j| j.request_key == "key-f")
            .unwrap()
            .id;
        let b_id = all_jobs
            .iter()
            .find(|j| j.request_key == "key-b")
            .unwrap()
            .id;

        db.update_acquisition_job_state(
            f_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Failed,
                release_title: None,
                info_hash: None,
                error: Some("err".to_string()),
                next_retry_at: None,
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();
        db.update_acquisition_job_state(
            b_id,
            &AcquisitionJobUpdate {
                status: AcquisitionJobStatus::Blocked,
                release_title: None,
                info_hash: None,
                error: Some("blocked".to_string()),
                next_retry_at: None,
                submitted_at: None,
                completed_at: None,
                increment_attempts: false,
            },
        )
        .await
        .unwrap();

        let failed_only = db
            .list_acquisition_jobs(Some(&[AcquisitionJobStatus::Failed]), 10)
            .await
            .unwrap();
        assert_eq!(failed_only.len(), 1);
        assert_eq!(failed_only[0].status, AcquisitionJobStatus::Failed);

        let all_listed = db.list_acquisition_jobs(None, 10).await.unwrap();
        assert_eq!(all_listed.len(), 3);
    }

    // ── Test 7: RD torrent operations ─────────────────────────────────────────

    #[tokio::test]
    async fn test_rd_torrent_operations() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Insert two torrents: one downloaded (with files), one waiting_files
        db.upsert_rd_torrent(
            "id1",
            "HASH1",
            "Show S01 1080p",
            "downloaded",
            r#"{"files":[{"path":"/ep01.mkv","bytes":1000000000,"id":1}]}"#,
        )
        .await
        .unwrap();

        db.upsert_rd_torrent(
            "id2",
            "HASH2",
            "Movie 2020",
            "waiting_files",
            r#"{"files":[]}"#,
        )
        .await
        .unwrap();

        // get_rd_torrents returns both
        let all = db.get_rd_torrents().await.unwrap();
        assert_eq!(all.len(), 2);

        // rd_torrent_downloaded_by_hash: case-insensitive
        assert!(db.rd_torrent_downloaded_by_hash("HASH1").await.unwrap());
        assert!(db.rd_torrent_downloaded_by_hash("hash1").await.unwrap());
        assert!(!db.rd_torrent_downloaded_by_hash("HASH2").await.unwrap());
        assert!(!db
            .rd_torrent_downloaded_by_hash("HASH_UNKNOWN")
            .await
            .unwrap());

        // get_rd_torrent_counts: (cached_with_files, total_downloaded)
        // id1 is downloaded with non-empty files -> cached=1, total=1
        let (cached, total) = db.get_rd_torrent_counts().await.unwrap();
        assert_eq!(total, 1, "only downloaded torrents counted");
        assert_eq!(cached, 1, "id1 has non-empty files_json");

        // delete_rd_torrent removes id1
        db.delete_rd_torrent("id1").await.unwrap();
        let remaining = db.get_rd_torrents().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "id2");
    }

    // ── Test 8: record_scan_run roundtrip ─────────────────────────────────────

    #[tokio::test]
    async fn test_record_scan_run_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let run = ScanRunRecord {
            dry_run: true,
            run_token: Some("scan-run-db".to_string()),
            library_items_found: 42,
            source_items_found: 100,
            matches_found: 38,
            links_created: 10,
            links_updated: 5,
            dead_marked: 2,
            links_removed: 1,
            links_skipped: 3,
            ambiguous_skipped: 7,
            skip_reason_json: Some(
                r#"{"already_correct":2,"source_missing_before_link":1,"ambiguous_match":7}"#
                    .to_string(),
            ),
            library_filter: Some("Anime".to_string()),
            search_missing: true,
            runtime_checks_ms: 11,
            library_scan_ms: 22,
            source_inventory_ms: 33,
            matching_ms: 44,
            title_enrichment_ms: 55,
            linking_ms: 66,
            plex_refresh_ms: 77,
            plex_refresh_requested_paths: 12,
            plex_refresh_unique_paths: 10,
            plex_refresh_planned_batches: 5,
            plex_refresh_coalesced_batches: 2,
            plex_refresh_coalesced_paths: 7,
            plex_refresh_refreshed_batches: 4,
            plex_refresh_refreshed_paths_covered: 11,
            plex_refresh_skipped_batches: 1,
            plex_refresh_unresolved_paths: 0,
            plex_refresh_capped_batches: 1,
            plex_refresh_aborted_due_to_cap: true,
            plex_refresh_failed_batches: 0,
            media_server_refresh_json: Some(
                r#"[{"server":"plex","requested_targets":2,"refresh":{"requested_paths":2,"unique_paths":2,"planned_batches":1,"coalesced_batches":0,"coalesced_paths":0,"refreshed_batches":1,"refreshed_paths_covered":2,"skipped_batches":0,"unresolved_paths":0,"capped_batches":0,"aborted_due_to_cap":false,"failed_batches":0}}]"#.to_string(),
            ),
            dead_link_sweep_ms: 88,
            cache_hit_ratio: Some(1.0),
            candidate_slots: 1234,
            scored_candidates: 56,
            exact_id_hits: 7,
            auto_acquire_requests: 10,
            auto_acquire_missing_requests: 5,
            auto_acquire_cutoff_requests: 5,
            auto_acquire_dry_run_hits: 8,
            auto_acquire_submitted: 0,
            auto_acquire_no_result: 2,
            auto_acquire_blocked: 0,
            auto_acquire_failed: 0,
            auto_acquire_completed_linked: 0,
            auto_acquire_completed_unlinked: 0,
        };

        db.record_scan_run(&run).await.unwrap();

        let row = sqlx::query(
            "SELECT dry_run, library_filter, search_missing, library_items_found, source_items_found, matches_found, \
             links_created, links_updated, dead_marked, links_removed, links_skipped, \
             ambiguous_skipped, runtime_checks_ms, plex_refresh_planned_batches, plex_refresh_capped_batches, \
             plex_refresh_aborted_due_to_cap, media_server_refresh_json, \
             cache_hit_ratio, candidate_slots, auto_acquire_requests, auto_acquire_dry_run_hits, auto_acquire_no_result \
             FROM scan_runs ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let dry_run: i64 = row.get("dry_run");
        assert_eq!(dry_run, 1, "dry_run stored as 1");
        let library_filter: Option<String> = row.get("library_filter");
        assert_eq!(library_filter.as_deref(), Some("Anime"));
        let search_missing: i64 = row.get("search_missing");
        assert_eq!(search_missing, 1);
        let lib: i64 = row.get("library_items_found");
        assert_eq!(lib, 42);
        let src: i64 = row.get("source_items_found");
        assert_eq!(src, 100);
        let matches: i64 = row.get("matches_found");
        assert_eq!(matches, 38);
        let created: i64 = row.get("links_created");
        assert_eq!(created, 10);
        let updated: i64 = row.get("links_updated");
        assert_eq!(updated, 5);
        let dead: i64 = row.get("dead_marked");
        assert_eq!(dead, 2);
        let removed: i64 = row.get("links_removed");
        assert_eq!(removed, 1);
        let skipped: i64 = row.get("links_skipped");
        assert_eq!(skipped, 3);
        let ambiguous: i64 = row.get("ambiguous_skipped");
        assert_eq!(ambiguous, 7);
        let runtime_checks_ms: i64 = row.get("runtime_checks_ms");
        assert_eq!(runtime_checks_ms, 11);
        let planned_batches: i64 = row.get("plex_refresh_planned_batches");
        assert_eq!(planned_batches, 5);
        let capped_batches: i64 = row.get("plex_refresh_capped_batches");
        assert_eq!(capped_batches, 1);
        let aborted_due_to_cap: i64 = row.get("plex_refresh_aborted_due_to_cap");
        assert_eq!(aborted_due_to_cap, 1);
        let media_server_refresh_json: Option<String> = row.get("media_server_refresh_json");
        assert!(media_server_refresh_json
            .unwrap()
            .contains("\"server\":\"plex\""));
        let cache_hit_ratio: f64 = row.get("cache_hit_ratio");
        assert_eq!(cache_hit_ratio, 1.0);
        let candidate_slots: i64 = row.get("candidate_slots");
        assert_eq!(candidate_slots, 1234);
        let auto_acquire_requests: i64 = row.get("auto_acquire_requests");
        assert_eq!(auto_acquire_requests, 10);
        let auto_acquire_dry_run_hits: i64 = row.get("auto_acquire_dry_run_hits");
        assert_eq!(auto_acquire_dry_run_hits, 8);
        let auto_acquire_no_result: i64 = row.get("auto_acquire_no_result");
        assert_eq!(auto_acquire_no_result, 2);
    }

    // ── Test 9: mark_removed and get_link_by_target ────────────────────────────

    #[tokio::test]
    async fn test_mark_removed_and_get_link_by_target() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
        db.insert_link(&record).await.unwrap();

        // get_link_by_target_path returns it
        let found = db
            .get_link_by_target_path(Path::new("/plex/show/S01E01.mkv"))
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().status, LinkStatus::Active);

        // mark_removed_path transitions to Removed
        db.mark_removed_path(Path::new("/plex/show/S01E01.mkv"))
            .await
            .unwrap();

        let removed_links = db.get_links_by_status(LinkStatus::Removed).await.unwrap();
        assert_eq!(removed_links.len(), 1);
        assert_eq!(
            removed_links[0].target_path,
            PathBuf::from("/plex/show/S01E01.mkv")
        );

        // get_active_links does not include it
        let active = db.get_active_links().await.unwrap();
        assert!(active.is_empty());
    }

    // ── Test 10: insert_link_in_tx – commit and rollback ──────────────────────

    #[tokio::test]
    async fn test_insert_link_in_tx_commit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");

        let mut tx = db.begin().await.unwrap();
        let id = db.insert_link_in_tx(&record, &mut tx).await.unwrap();
        assert!(id > 0);
        tx.commit().await.unwrap();

        let active = db.get_active_links().await.unwrap();
        assert_eq!(active.len(), 1);
    }

    #[tokio::test]
    async fn test_insert_link_in_tx_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");

        {
            let mut tx = db.begin().await.unwrap();
            db.insert_link_in_tx(&record, &mut tx).await.unwrap();
            // Drop tx without committing — implicit rollback
        }

        let active = db.get_active_links().await.unwrap();
        assert!(active.is_empty(), "rolled-back insert should not persist");
    }

    // ── Test 11: escape_sql_like_pattern ──────────────────────────────────────

    #[tokio::test]
    async fn test_escape_sql_like_pattern_special_chars() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Insert a link whose target path contains % and _
        let record = sample_link(
            "/mnt/rd/show/ep01.mkv",
            "/plex/100%_Show/Season 01/S01E01.mkv",
        );
        db.insert_link(&record).await.unwrap();

        // Also insert a link that should NOT match
        let other = sample_link("/mnt/rd/other/ep01.mkv", "/plex/Other/S01E01.mkv");
        db.insert_link(&other).await.unwrap();

        // Scope to the exact directory containing % and _
        let roots = vec![PathBuf::from("/plex/100%_Show")];
        let scoped = db.get_links_scoped(Some(&roots)).await.unwrap();

        // Only the link under the special-character path should match
        assert_eq!(scoped.len(), 1);
        assert_eq!(
            scoped[0].target_path,
            PathBuf::from("/plex/100%_Show/Season 01/S01E01.mkv")
        );
    }

    // ── Test 12: get_links_scoped (all statuses) ──────────────────────────────

    #[tokio::test]
    async fn test_get_links_scoped_all_statuses() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Three links under /plex/TV, one under /plex/Movies
        let active = sample_link("/mnt/rd/a.mkv", "/plex/TV/Show/S01E01.mkv");
        let mut dead = sample_link("/mnt/rd/b.mkv", "/plex/TV/Show/S01E02.mkv");
        dead.media_id = "tvdb-dead".to_string();
        let mut removed = sample_link("/mnt/rd/c.mkv", "/plex/TV/Show/S01E03.mkv");
        removed.media_id = "tvdb-removed".to_string();
        let other = sample_link("/mnt/rd/d.mkv", "/plex/Movies/Movie.mkv");

        db.insert_link(&active).await.unwrap();
        db.insert_link(&dead).await.unwrap();
        db.insert_link(&removed).await.unwrap();
        db.insert_link(&other).await.unwrap();

        db.mark_dead_path(Path::new("/plex/TV/Show/S01E02.mkv"))
            .await
            .unwrap();
        db.mark_removed_path(Path::new("/plex/TV/Show/S01E03.mkv"))
            .await
            .unwrap();

        let roots = vec![PathBuf::from("/plex/TV")];
        let scoped = db.get_links_scoped(Some(&roots)).await.unwrap();

        assert_eq!(scoped.len(), 3, "active + dead + removed under /plex/TV");
        let statuses: Vec<LinkStatus> = scoped.iter().map(|l| l.status).collect();
        assert!(statuses.contains(&LinkStatus::Active));
        assert!(statuses.contains(&LinkStatus::Dead));
        assert!(statuses.contains(&LinkStatus::Removed));

        // Movies link not included
        assert!(!scoped
            .iter()
            .any(|l| l.target_path == Path::new("/plex/Movies/Movie.mkv")));
    }

    // ── Test 13: empty DB edge cases ──────────────────────────────────────────

    #[tokio::test]
    async fn test_empty_db_edge_cases() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let (cached, total) = db.get_rd_torrent_counts().await.unwrap();
        assert_eq!((cached, total), (0, 0));

        let counts = db.get_acquisition_job_counts().await.unwrap();
        assert_eq!(counts.queued, 0);
        assert_eq!(counts.downloading, 0);
        assert_eq!(counts.relinking, 0);
        assert_eq!(counts.blocked, 0);
        assert_eq!(counts.no_result, 0);
        assert_eq!(counts.failed, 0);
        assert_eq!(counts.completed_unlinked, 0);
        assert_eq!(counts.active_total(), 0);

        let stats = db.housekeeping_with_vacuum(false).await.unwrap();
        assert_eq!(stats.scan_runs_deleted, 0);
        assert_eq!(stats.link_events_deleted, 0);
        assert_eq!(stats.old_jobs_deleted, 0);
        assert_eq!(stats.expired_api_cache_deleted, 0);
    }

    #[tokio::test]
    async fn test_get_dead_links() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let r = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
        db.insert_link(&r).await.unwrap();
        db.mark_dead("/plex/show/S01E01.mkv").await.unwrap();

        let dead = db.get_dead_links().await.unwrap();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].status, LinkStatus::Dead);
        assert_eq!(dead[0].target_path, PathBuf::from("/plex/show/S01E01.mkv"));
    }

    #[tokio::test]
    async fn test_get_web_stats() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Initially all zeros, no last_scan
        let stats = db.get_web_stats().await.unwrap();
        assert_eq!(stats.active_links, 0);
        assert_eq!(stats.dead_links, 0);
        assert_eq!(stats.total_scans, 0);
        assert!(stats.last_scan.is_none());

        // Insert one active, one dead link
        db.insert_link(&sample_link("/mnt/a", "/plex/a"))
            .await
            .unwrap();
        db.insert_link(&sample_link("/mnt/b", "/plex/b"))
            .await
            .unwrap();
        db.mark_dead("/plex/b").await.unwrap();

        // Insert a scan_run
        db.record_scan_run(&ScanRunRecord {
            dry_run: false,
            library_items_found: 10,
            source_items_found: 20,
            matches_found: 5,
            links_created: 2,
            skip_reason_json: None,
            ..Default::default()
        })
        .await
        .unwrap();

        let stats = db.get_web_stats().await.unwrap();
        assert_eq!(stats.active_links, 1);
        assert_eq!(stats.dead_links, 1);
        assert_eq!(stats.total_scans, 1);
        assert!(stats.last_scan.is_some());
    }

    #[tokio::test]
    async fn test_get_scan_history() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Insert 3 scan runs
        for i in 0..3i64 {
            db.record_scan_run(&ScanRunRecord {
                dry_run: i == 0,
                library_items_found: i * 10,
                source_items_found: i * 20,
                matches_found: i * 5,
                links_created: i,
                links_updated: i,
                dead_marked: 0,
                links_removed: 0,
                links_skipped: 0,
                ambiguous_skipped: 0,
                skip_reason_json: None,
                ..Default::default()
            })
            .await
            .unwrap();
        }

        // get_scan_history(2) returns 2 in reverse chronological order (latest first)
        let history = db.get_scan_history(2).await.unwrap();
        assert_eq!(history.len(), 2);
        // Most recent has library_items_found=20 (i=2), next has 10 (i=1)
        assert_eq!(history[0].library_items_found, 20);
        assert_eq!(history[1].library_items_found, 10);

        // Full history returns all 3
        let all = db.get_scan_history(10).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_get_scan_run_returns_specific_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        db.record_scan_run(&ScanRunRecord {
            dry_run: true,
            library_filter: Some("Anime".to_string()),
            search_missing: true,
            library_items_found: 10,
            source_items_found: 20,
            matches_found: 5,
            links_created: 1,
            links_updated: 2,
            skip_reason_json: None,
            ..Default::default()
        })
        .await
        .unwrap();
        db.record_scan_run(&ScanRunRecord {
            dry_run: false,
            library_filter: Some("Movies".to_string()),
            search_missing: false,
            library_items_found: 30,
            source_items_found: 40,
            matches_found: 15,
            links_created: 3,
            links_updated: 4,
            skip_reason_json: None,
            ..Default::default()
        })
        .await
        .unwrap();

        let latest = db.get_scan_history(1).await.unwrap();
        let run = db.get_scan_run(latest[0].id).await.unwrap().unwrap();

        assert_eq!(run.id, latest[0].id);
        assert_eq!(run.library_filter.as_deref(), Some("Movies"));
        assert_eq!(run.matches_found, 15);
        assert_eq!(run.links_created, 3);
    }

    #[tokio::test]
    async fn database_enables_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let enabled: i64 = sqlx::query("PRAGMA foreign_keys")
            .fetch_one(&db.pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(enabled, 1);
    }
}
