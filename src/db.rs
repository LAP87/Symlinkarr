use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::info;

mod acquisition_jobs;
mod anime_overrides;
mod cache;
mod links;
mod maintenance;
mod migrations;
mod scan_runs;
#[cfg(test)]
mod tests;
mod types;

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

const LATEST_SCHEMA_VERSION: i64 = 16;

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
}

fn path_to_db_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8: {:?}", path))
}
