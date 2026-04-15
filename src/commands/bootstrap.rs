//! Bootstrap command for creating a starter Symlinkarr installation.
//!
//! `symlinkarr bootstrap` — creates required directories and a starter config.yaml
//! with commented guidance, so a fresh install can get running quickly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

const STARTER_CONFIG: &str = r#"# Symlinkarr configuration
# See docs at https://github.com/LAP87/Symlinkarr/wiki/User-Guide

# Libraries: directories where Plex/Jellyfin/Emby expect media
libraries:
  - name: "Anime"
    path: "/mnt/media/anime"
    media_type: tv
    content_type: anime
    depth: 1

# Sources: Real-Debrid mount directories
sources:
  - name: "RealDebrid"
    path: "/mnt/decypharr/realdebrid"
    media_type: auto

# Real-Debrid API token (use secretfile: to load from a file)
realdebrid:
  api_token: ""          # Or use: secretfile:/path/to/rd-token

# Database path
db_path: "symlinkarr.db"

# Backup settings
backup:
  enabled: true
  path: "backups"
  interval_hours: 24
  max_backups: 10

# Web UI
web:
  enabled: true
  port: 8726
  bind_address: "127.0.0.1"
"#;

/// Run the bootstrap command.
///
/// Creates required directories and a starter config.yaml with guidance comments.
/// If `target_dir` is None, defaults to /app/config (Docker) or current directory.
pub fn run_bootstrap(target_dir: Option<&Path>, list_only: bool) -> Result<()> {
    let config_dir = target_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            if Path::new("/app/config").is_dir() {
                PathBuf::from("/app/config")
            } else {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }
        });

    let config_path = config_dir.join("config.yaml");
    let db_path = config_dir.join("symlinkarr.db");
    let backup_dir = config_dir.join("backups");

    // ── List mode ──────────────────────────────────────────────────
    if list_only {
        println!("🔍 Symlinkarr Bootstrap — Requirements Check\n");
        println!("   Config directory: {}", config_dir.display());

        if config_path.exists() {
            println!("   ✅ config.yaml exists");
        } else {
            println!("   ❌ config.yaml missing");
        }

        if db_path.exists() {
            println!("   ✅ symlinkarr.db exists");
        } else {
            println!("   ❌ symlinkarr.db missing (will be created on first run)");
        }

        if backup_dir.exists() {
            println!("   ✅ backups/ directory exists");
        } else {
            println!("   ❌ backups/ directory missing");
        }

        println!("\n   Required directories:");
        println!("   • {} (config directory)", config_dir.display());
        println!("   • {} (backup directory)", backup_dir.display());
        println!("\n   Required files:");
        println!("   • config.yaml");
        println!("   • Real-Debrid API token (in config or via secretfile:)");
        println!("\n   To create a starter config, run:");
        println!("   symlinkarr bootstrap --dir {}", config_dir.display());
        return Ok(());
    }

    // ── Create directories ─────────────────────────────────────────
    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir)
            .with_context(|| format!("Failed to create config directory: {}", config_dir.display()))?;
        info!("Created config directory: {}", config_dir.display());
        println!("📁 Created directory: {}", config_dir.display());
    }

    if !backup_dir.exists() {
        std::fs::create_dir_all(&backup_dir)
            .with_context(|| format!("Failed to create backup directory: {}", backup_dir.display()))?;
        info!("Created backup directory: {}", backup_dir.display());
        println!("📁 Created directory: {}", backup_dir.display());
    }

    // ── Create starter config.yaml ─────────────────────────────────
    if config_path.exists() {
        println!("⏭️  config.yaml already exists at {}, skipping", config_path.display());
        println!("\n💡 Edit your existing config.yaml, then run:");
        println!("   symlinkarr doctor    # validate configuration");
        println!("   symlinkarr scan      # first scan");
    } else {
        std::fs::write(&config_path, STARTER_CONFIG)
            .with_context(|| format!("Failed to write config.yaml to {}", config_path.display()))?;
        info!("Created starter config: {}", config_path.display());
        println!("📝 Created starter config: {}", config_path.display());
        println!("\n💡 Edit config.yaml to match your setup:");
        println!("   • Set library paths to your Plex/Jellyfin media directories");
        println!("   • Set source paths to your Real-Debrid mount directories");
        println!("   • Add your Real-Debrid API token");
        println!("\n   Then run:");
        println!("   symlinkarr doctor    # validate configuration");
        println!("   symlinkarr scan      # first scan");
    }

    Ok(())
}
