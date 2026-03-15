use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use unicode_normalization::UnicodeNormalization;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
