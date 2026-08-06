use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::SqlitePool;
use tracing::info;

mod acquisition_jobs;
mod anime_overrides;
mod cache;
mod daemon_heartbeat;
mod links;
mod maintenance;
mod migrations;
mod operations;
mod scan_runs;
mod scheduler;
#[cfg(test)]
mod tests;
mod types;

pub use operations::*;
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

const LATEST_SCHEMA_VERSION: i64 = 22;

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

        // Configure the connection options so relational safeguards and concurrency
        // tuning apply to every pooled connection, not just the first one checked out.
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
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

        sqlx::query("VACUUM INTO ?")
            .bind(snapshot_path.to_string_lossy().to_string())
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
}

fn path_to_db_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8: {:?}", path))
}
