pub mod backup;
pub mod cache;
pub mod cleanup;
pub mod config;
pub mod daemon;
pub mod discover;
pub mod doctor;
pub mod queue;
pub mod repair;
pub mod report;
pub mod scan;
pub mod status;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

use crate::api::prowlarr;
use crate::config::{Config, ContentType, LibraryConfig, SourceConfig};
use crate::db::Database;
use crate::models::MediaType;
use crate::utils::{directory_path_health_with_timeout, fast_path_health, PathHealth};

pub(crate) const DIRECTORY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

// ─── Panel display helpers ─────────────────────────────────────────

const PANEL_INNER_WIDTH: usize = 40;
const PANEL_VALUE_WIDTH: usize = 12;
const PANEL_TOTAL_WIDTH: usize = PANEL_INNER_WIDTH + 2;

fn panel_left_padding() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|columns| columns.saturating_sub(PANEL_TOTAL_WIDTH) / 2)
        .unwrap_or(0)
}

pub(crate) fn panel_print_line(line: &str) {
    let pad = " ".repeat(panel_left_padding());
    println!("{pad}{line}");
}

pub(crate) fn panel_border(left: char, fill: char, right: char) {
    panel_print_line(&format!(
        "{}{}{}",
        left,
        fill.to_string().repeat(PANEL_INNER_WIDTH),
        right
    ));
}

pub(crate) fn panel_title(title: &str) {
    panel_print_line(&format!("║{title:^width$}║", width = PANEL_INNER_WIDTH));
}

pub(crate) fn panel_kv_row(label: &str, value: impl std::fmt::Display) {
    let content_width = PANEL_INNER_WIDTH.saturating_sub(2);
    let raw = format!(
        "{label:<label_width$}{value:>value_width$}",
        label_width = content_width.saturating_sub(PANEL_VALUE_WIDTH),
        value_width = PANEL_VALUE_WIDTH,
    );
    let clipped: String = raw.chars().take(content_width).collect();
    panel_print_line(&format!("║ {clipped:<width$} ║", width = content_width));
}

pub(crate) async fn print_final_summary(
    db: &Database,
    added: Option<i64>,
    removed: Option<i64>,
) -> Result<()> {
    let (active, dead, total) = db.get_stats().await?;

    println!();
    panel_border('╔', '═', '╗');
    panel_title("Symlinkarr Summary");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Active symlinks:", active);
    panel_kv_row("  Dead links:", dead);
    panel_kv_row("  Total tracked:", total);

    if added.is_some() || removed.is_some() {
        panel_border('╠', '═', '╣');
        if let Some(count) = added {
            panel_kv_row("  Added:", count);
        }
        if let Some(count) = removed {
            panel_kv_row("  Removed:", count);
        }
    }

    panel_border('╚', '═', '╝');

    Ok(())
}

// ─── Cross-cutting helpers ─────────────────────────────────────────

pub(crate) fn selected_libraries<'a>(
    cfg: &'a Config,
    library_filter: Option<&str>,
) -> Result<Vec<&'a LibraryConfig>> {
    let Some(filter) = library_filter else {
        return Ok(cfg.libraries.iter().collect());
    };

    let wanted: Vec<String> = filter
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(cfg.libraries.iter().collect());
    }

    let mut selected = Vec::new();
    for lib in &cfg.libraries {
        if wanted.iter().any(|w| lib.name.eq_ignore_ascii_case(w)) {
            selected.push(lib);
        }
    }

    let missing: Vec<_> = wanted
        .iter()
        .filter(|want| {
            !cfg.libraries
                .iter()
                .any(|lib| lib.name.eq_ignore_ascii_case(want))
        })
        .cloned()
        .collect();

    if !missing.is_empty() {
        let available = cfg
            .libraries
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "Unknown library filter(s): {}. Available: {}",
            missing.join(", "),
            available
        );
    }

    Ok(selected)
}

pub(crate) fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{}", json),
        Err(e) => println!(r#"{{"error":"json_encode_failed","detail":"{}"}}"#, e),
    }
}

pub(crate) fn prowlarr_categories(media_type: MediaType, content_type: ContentType) -> Vec<i32> {
    match (media_type, content_type) {
        (MediaType::Movie, _) => vec![prowlarr::categories::MOVIES],
        (MediaType::Tv, ContentType::Anime) => vec![prowlarr::categories::TV_ANIME],
        (MediaType::Tv, _) => vec![prowlarr::categories::TV, prowlarr::categories::TV_ANIME],
    }
}

pub(crate) fn decypharr_arr_name(
    cfg: &Config,
    media_type: MediaType,
    content_type: ContentType,
) -> &str {
    match (media_type, content_type) {
        (MediaType::Movie, _) => &cfg.decypharr.arr_name_movie,
        (MediaType::Tv, ContentType::Anime) if cfg.has_sonarr_anime() => {
            &cfg.decypharr.arr_name_anime
        }
        (MediaType::Tv, _) => &cfg.decypharr.arr_name_tv,
    }
}

pub(crate) fn is_safe_auto_acquire_query(query: &str) -> bool {
    let normalized = crate::utils::normalize(query);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }

    let has_year = tokens
        .iter()
        .any(|token| token.len() == 4 && token.chars().all(|c| c.is_ascii_digit()));
    let has_episode = tokens.iter().any(|token| {
        let lower = token.to_ascii_lowercase();
        if let Some((season, episode)) = lower.split_once('e') {
            let season = season.strip_prefix('s').unwrap_or("");
            !season.is_empty()
                && !episode.is_empty()
                && season.chars().all(|c| c.is_ascii_digit())
                && episode.chars().all(|c| c.is_ascii_digit())
        } else {
            false
        }
    });
    let strong_words = tokens
        .iter()
        .filter(|token| token.chars().any(|c| c.is_ascii_alphabetic()) && token.len() >= 4)
        .count();
    let longest_word = tokens.iter().map(|token| token.len()).max().unwrap_or(0);

    has_year || has_episode || strong_words >= 2 || longest_word >= 7
}

pub(crate) fn runtime_source_probe_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().and_then(|name| name.to_str());
    if file_name == Some("__all__") {
        return path.parent().unwrap_or(path).to_path_buf();
    }
    path.to_path_buf()
}

pub(crate) async fn runtime_source_health(path: &Path, probe_path: &Path) -> PathHealth {
    let fast_health = fast_path_health(path);
    if !fast_health.is_healthy() {
        return fast_health;
    }
    directory_path_health_with_timeout(probe_path.to_path_buf(), DIRECTORY_PROBE_TIMEOUT).await
}

pub(crate) async fn ensure_runtime_directories_healthy(
    libraries: &[&LibraryConfig],
    sources: &[SourceConfig],
) -> Result<()> {
    for lib in libraries {
        let health =
            directory_path_health_with_timeout(lib.path.clone(), DIRECTORY_PROBE_TIMEOUT).await;
        if !health.is_healthy() {
            anyhow::bail!(
                "Library '{}' is not healthy: {}",
                lib.name,
                health.describe(&lib.path)
            );
        }
    }

    for src in sources {
        let probe_path = runtime_source_probe_path(&src.path);
        let health = runtime_source_health(&src.path, &probe_path).await;
        if !health.is_healthy() {
            anyhow::bail!(
                "Source '{}' is not healthy: {}",
                src.name,
                health.describe(&probe_path)
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LibraryConfig, SourceConfig};
    use crate::models::MediaType;

    #[test]
    fn safe_auto_acquire_queries_require_enough_signal() {
        assert!(!is_safe_auto_acquire_query("It"));
        assert!(!is_safe_auto_acquire_query("You"));
        assert!(!is_safe_auto_acquire_query("Arcane"));
        assert!(is_safe_auto_acquire_query("Severance"));
        assert!(is_safe_auto_acquire_query("The Matrix 1999"));
        assert!(is_safe_auto_acquire_query("Breaking Bad S01E01"));
    }

    #[test]
    fn runtime_source_probe_uses_mount_root_for_all_directory() {
        let path = Path::new("/mnt/decypharr/realdebrid/__all__");
        assert_eq!(
            runtime_source_probe_path(path),
            PathBuf::from("/mnt/decypharr/realdebrid")
        );
    }

    #[test]
    fn runtime_source_probe_leaves_normal_paths_unchanged() {
        let path = Path::new("/srv/media/source");
        assert_eq!(runtime_source_probe_path(path), path);
    }

    #[tokio::test]
    async fn ensure_runtime_directories_healthy_accepts_existing_library_and_source() {
        let dir = tempfile::tempdir().unwrap();
        let library_path = dir.path().join("library");
        let source_path = dir.path().join("source");
        std::fs::create_dir_all(&library_path).unwrap();
        std::fs::create_dir_all(&source_path).unwrap();

        let library = LibraryConfig {
            name: "Anime".to_string(),
            path: library_path,
            media_type: MediaType::Tv,
            content_type: None,
            depth: 1,
        };
        let source = SourceConfig {
            name: "RealDebrid".to_string(),
            path: source_path,
            media_type: "auto".to_string(),
        };

        ensure_runtime_directories_healthy(&[&library], &[source])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ensure_runtime_directories_healthy_rejects_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let library_path = dir.path().join("library");
        std::fs::create_dir_all(&library_path).unwrap();

        let library = LibraryConfig {
            name: "Anime".to_string(),
            path: library_path,
            media_type: MediaType::Tv,
            content_type: None,
            depth: 1,
        };
        let source = SourceConfig {
            name: "RealDebrid".to_string(),
            path: dir.path().join("missing-source"),
            media_type: "auto".to_string(),
        };

        let err = ensure_runtime_directories_healthy(&[&library], &[source])
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Source 'RealDebrid' is not healthy"),
            "unexpected error: {err}"
        );
    }
}
