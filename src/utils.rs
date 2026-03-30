use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;

/// Known video file extensions
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "wmv", "flv", "mov", "webm", "m4v", "ts", "mpg", "mpeg",
];

pub fn path_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub fn cached_source_exists(
    path: &Path,
    source_exists_cache: &mut HashMap<PathBuf, bool>,
    parent_exists_cache: &mut HashMap<PathBuf, bool>,
) -> bool {
    if let Some(exists) = source_exists_cache.get(path) {
        return *exists;
    }

    if let Some(parent) = path.parent() {
        if let Some(parent_exists) = parent_exists_cache.get(parent) {
            if !parent_exists {
                source_exists_cache.insert(path.to_path_buf(), false);
                return false;
            }
        } else {
            let exists = parent.exists();
            parent_exists_cache.insert(parent.to_path_buf(), exists);
            if !exists {
                source_exists_cache.insert(path.to_path_buf(), false);
                return false;
            }
        }
    }

    let exists = path.exists();
    source_exists_cache.insert(path.to_path_buf(), exists);
    exists
}

pub fn cached_source_health(
    path: &Path,
    source_health_cache: &mut HashMap<PathBuf, PathHealth>,
    parent_health_cache: &mut HashMap<PathBuf, PathHealth>,
) -> PathHealth {
    if let Some(health) = source_health_cache.get(path) {
        return health.clone();
    }

    if let Some(parent) = path.parent() {
        let parent_health = if let Some(health) = parent_health_cache.get(parent) {
            health.clone()
        } else {
            let health = fast_path_health(parent);
            parent_health_cache.insert(parent.to_path_buf(), health.clone());
            health
        };

        if !parent_health.is_healthy() {
            source_health_cache.insert(path.to_path_buf(), parent_health.clone());
            return parent_health;
        }
    }

    let health = fast_path_health(path);
    source_health_cache.insert(path.to_path_buf(), health.clone());
    health
}

use tokio::task;
use tokio::time;

const ENOTCONN_RAW_OS_ERROR: i32 = 107;
static STDOUT_TEXT_ENABLED: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathHealth {
    Healthy,
    Missing,
    TransportDisconnected,
    Timeout,
    IoError(String),
}

impl PathHealth {
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    pub fn blocks_destructive_ops(&self) -> bool {
        matches!(
            self,
            Self::TransportDisconnected | Self::Timeout | Self::IoError(_)
        )
    }

    pub fn describe(&self, path: &Path) -> String {
        match self {
            Self::Healthy => path.display().to_string(),
            Self::Missing => format!("{} (missing)", path.display()),
            Self::TransportDisconnected => format!(
                "{} (transport endpoint is not connected; restart/remount the FUSE source)",
                path.display()
            ),
            Self::Timeout => format!(
                "{} (timed out while probing directory; mount may be hung)",
                path.display()
            ),
            Self::IoError(err) => format!("{} ({})", path.display(), err),
        }
    }
}

pub fn fast_path_health(path: &Path) -> PathHealth {
    match std::fs::symlink_metadata(path) {
        Ok(_) => PathHealth::Healthy,
        Err(err) => classify_path_error(err),
    }
}

pub fn directory_path_health(path: &Path) -> PathHealth {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                match std::fs::read_dir(path) {
                    Ok(mut entries) => {
                        let _ = entries.next();
                        PathHealth::Healthy
                    }
                    Err(err) => classify_path_error(err),
                }
            } else {
                PathHealth::Healthy
            }
        }
        Err(err) => classify_path_error(err),
    }
}

pub async fn directory_path_health_with_timeout(path: PathBuf, timeout: Duration) -> PathHealth {
    match time::timeout(
        timeout,
        task::spawn_blocking(move || directory_path_health(&path)),
    )
    .await
    {
        Ok(Ok(health)) => health,
        Ok(Err(err)) => PathHealth::IoError(format!("probe task failed: {}", err)),
        Err(_) => PathHealth::Timeout,
    }
}

fn classify_path_error(err: io::Error) -> PathHealth {
    match err.raw_os_error() {
        Some(enoent_or_enotdir) if enoent_or_enotdir == 2 || enoent_or_enotdir == 20 => {
            PathHealth::Missing
        }
        Some(ENOTCONN_RAW_OS_ERROR) => PathHealth::TransportDisconnected,
        _ => PathHealth::IoError(err.to_string()),
    }
}

/// Normalize a string for comparison:
/// lowercase, remove special characters, collapse whitespace.
pub fn normalize(s: &str) -> String {
    let s = s.nfc().collect::<String>();
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn stdout_text_enabled() -> bool {
    STDOUT_TEXT_ENABLED.load(Ordering::Relaxed)
}

pub struct StdoutTextGuard {
    previous: bool,
}

impl Drop for StdoutTextGuard {
    fn drop(&mut self) {
        STDOUT_TEXT_ENABLED.store(self.previous, Ordering::Relaxed);
    }
}

pub fn stdout_text_guard(enabled: bool) -> StdoutTextGuard {
    let previous = stdout_text_enabled();
    STDOUT_TEXT_ENABLED.store(previous && enabled, Ordering::Relaxed);
    StdoutTextGuard { previous }
}

pub fn user_println(message: impl AsRef<str>) {
    if stdout_text_enabled() {
        println!("{}", message.as_ref());
    }
}

pub struct ProgressLine {
    label: String,
    enabled: bool,
    is_tty: bool,
}

impl ProgressLine {
    pub fn new(label: impl Into<String>) -> Self {
        let enabled = stdout_text_enabled();
        Self {
            label: label.into(),
            enabled,
            is_tty: enabled && io::stdout().is_terminal(),
        }
    }

    pub fn update(&mut self, detail: impl AsRef<str>) {
        let message = format!("   ⏳ {} {}", self.label, detail.as_ref());
        self.render(&message, false);
    }

    pub fn is_tty(&self) -> bool {
        self.is_tty
    }

    pub fn finish(&mut self, detail: impl AsRef<str>) {
        let message = format!("   ✅ {} {}", self.label, detail.as_ref());
        self.render(&message, true);
    }

    fn render(&mut self, message: &str, newline: bool) {
        if !self.enabled {
            return;
        }

        if !self.is_tty {
            println!("{message}");
            return;
        }

        let mut stdout = io::stdout();
        let _ = if newline {
            writeln!(stdout, "\r\x1b[2K{}", message)
        } else {
            write!(stdout, "\r\x1b[2K{}", message)
        };
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === path_under_roots ===

    #[test]
    fn path_under_roots_single_match() {
        let roots = [PathBuf::from("/mnt/storage")];
        assert!(path_under_roots(Path::new("/mnt/storage/film"), &roots));
        assert!(path_under_roots(
            Path::new("/mnt/storage/film/Movie {tmdb-1}"),
            &roots
        ));
    }

    #[test]
    fn path_under_roots_no_match() {
        let roots = [PathBuf::from("/mnt/storage")];
        assert!(!path_under_roots(
            Path::new("/home/lenny/Downloads"),
            &roots
        ));
    }

    #[test]
    fn path_under_roots_multiple_roots() {
        let roots = vec![
            PathBuf::from("/mnt/storage/film"),
            PathBuf::from("/mnt/storage/serier"),
        ];
        assert!(path_under_roots(
            Path::new("/mnt/storage/film/Movie {tmdb-1}"),
            &roots
        ));
        assert!(path_under_roots(
            Path::new("/mnt/storage/serier/Show {tvdb-1}"),
            &roots
        ));
        assert!(!path_under_roots(Path::new("/mnt/storage/other"), &roots));
    }

    // === normalize ===

    #[test]
    fn normalize_lowercase() {
        assert_eq!(normalize("Hello World"), "hello world");
    }

    #[test]
    fn normalize_removes_special_chars() {
        assert_eq!(normalize("Movie.Name.2024"), "movie name 2024");
    }

    #[test]
    fn normalize_nfc_normalization() {
        // NFC vs NFD normalization - é as single char vs decomposed
        let composed = normalize("Café");
        assert!(composed.contains("cafe") || composed.contains("caf"));
    }

    #[test]
    fn normalize_trims_whitespace() {
        assert_eq!(normalize("  Hello   World  "), "hello world");
    }

    #[test]
    fn normalize_empty_string() {
        assert_eq!(normalize(""), "");
    }

    // === PathHealth ===

    #[test]
    fn fast_health_marks_missing_paths() {
        let path = Path::new("/definitely/missing/symlinkarr-test");
        assert_eq!(fast_path_health(path), PathHealth::Missing);
    }

    #[test]
    fn transport_error_is_classified_explicitly() {
        let err = io::Error::from_raw_os_error(ENOTCONN_RAW_OS_ERROR);
        assert_eq!(classify_path_error(err), PathHealth::TransportDisconnected);
    }

    #[test]
    fn cached_source_health_short_circuits_missing_parent() {
        let root = tempfile::TempDir::new().unwrap();
        let missing = root.path().join("missing-parent").join("missing-file.mkv");
        let mut source_cache = HashMap::new();
        let mut parent_cache = HashMap::new();

        let health = cached_source_health(&missing, &mut source_cache, &mut parent_cache);
        assert_eq!(health, PathHealth::Missing);

        let parent = missing.parent().unwrap().to_path_buf();
        assert_eq!(parent_cache.get(&parent), Some(&PathHealth::Missing));
        assert_eq!(source_cache.get(&missing), Some(&PathHealth::Missing));
    }

    #[test]
    fn cached_source_health_propagates_cached_unhealthy_parent() {
        let path = PathBuf::from("/mnt/rd/file.mkv");
        let parent = path.parent().unwrap().to_path_buf();
        let mut source_cache = HashMap::new();
        let mut parent_cache = HashMap::new();
        parent_cache.insert(parent, PathHealth::TransportDisconnected);

        let health = cached_source_health(&path, &mut source_cache, &mut parent_cache);
        assert_eq!(health, PathHealth::TransportDisconnected);
        assert_eq!(
            source_cache.get(&path),
            Some(&PathHealth::TransportDisconnected)
        );
    }

    #[test]
    fn path_health_blocks_destructive_ops_only_for_unhealthy_states() {
        assert!(!PathHealth::Healthy.blocks_destructive_ops());
        assert!(!PathHealth::Missing.blocks_destructive_ops());
        assert!(PathHealth::TransportDisconnected.blocks_destructive_ops());
        assert!(PathHealth::Timeout.blocks_destructive_ops());
        assert!(PathHealth::IoError("boom".to_string()).blocks_destructive_ops());
    }

    #[test]
    fn stdout_text_guard_restores_previous_state() {
        assert!(stdout_text_enabled());
        let outer = stdout_text_guard(false);
        assert!(!stdout_text_enabled());
        {
            let _inner = stdout_text_guard(true);
            assert!(!stdout_text_enabled());
        }
        assert!(!stdout_text_enabled());
        drop(outer);
        assert!(stdout_text_enabled());
    }
}
