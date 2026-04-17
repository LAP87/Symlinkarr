use anyhow::Result;
use sqlx::{Row, Sqlite, Transaction};

use super::{Database, LATEST_SCHEMA_VERSION};

impl Database {
    /// Run database migrations to create/update schema.
    pub(super) async fn run_migrations(&self) -> Result<()> {
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

    pub(super) async fn ensure_schema_version_table(&self) -> Result<()> {
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

    pub(super) async fn current_schema_version(&self) -> Result<i64> {
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

    pub(super) async fn table_exists(&self, table_name: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind(table_name)
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.get("cnt");
        Ok(count > 0)
    }

    pub(super) async fn column_exists(&self, table_name: &str, column_name: &str) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({})", table_name);
        let rows = sqlx::query(&pragma).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .any(|row| row.get::<String, _>("name") == column_name))
    }

    /// Apply a migration within a caller-supplied transaction. All migration
    /// DDL uses `CREATE TABLE IF NOT EXISTS` / `ALTER TABLE IF NOT EXISTS`, so
    /// re-running after a partial failure is safe.
    async fn apply_migration_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
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

    // Each _tx method mirrors its pool-based counterpart but executes within a
    // supplied transaction. All DDL uses IF NOT EXISTS / idempotent patterns so
    // re-running after a crash is safe.

    async fn migration_v1_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v2_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v3_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v4_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v5_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v6_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v7_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v8_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v9_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_links_status_target ON links(status, target_path)",
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn migration_v10_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v11_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v12_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v13_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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

    async fn migration_v14_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
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
    pub(super) async fn migrate_to_for_tests(&self, target_version: i64) -> Result<()> {
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
}
