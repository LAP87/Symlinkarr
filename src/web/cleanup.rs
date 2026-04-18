use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::cleanup_audit::{CleanupReport, CleanupScope};
use crate::config::{Config, ContentType, LibraryConfig};

pub(super) fn cleanup_scope_label(scope: CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Anime => "Anime",
        CleanupScope::Tv => "TV",
        CleanupScope::Movie => "Movies",
        CleanupScope::All => "All Libraries",
    }
}

fn cleanup_scope_slug(scope: CleanupScope) -> &'static str {
    match scope {
        CleanupScope::Anime => "anime",
        CleanupScope::Tv => "tv",
        CleanupScope::Movie => "movie",
        CleanupScope::All => "all",
    }
}

pub(super) fn cleanup_libraries_label(selected_libraries: &[String]) -> String {
    match selected_libraries {
        [] => "All Libraries".to_string(),
        [single] => single.clone(),
        [first, second] => format!("{}, {}", first, second),
        [first, second, third] => format!("{}, {}, {}", first, second, third),
        many => format!("{} libraries", many.len()),
    }
}

pub(crate) fn latest_cleanup_report_path(backup_dir: &Path) -> Option<PathBuf> {
    let mut reports: Vec<_> = std::fs::read_dir(backup_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            name.to_string_lossy().starts_with("cleanup-audit-")
                && name.to_string_lossy().ends_with(".json")
        })
        .collect();

    reports.sort_by_key(|entry| {
        entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    reports.reverse();
    reports.first().map(|entry| entry.path())
}

pub(crate) fn resolve_cleanup_report_path(
    backup_dir: &Path,
    report: &str,
) -> anyhow::Result<PathBuf> {
    let trimmed = report.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Cleanup report path is required");
    }

    let requested = Path::new(trimmed);
    let backup_root = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());

    if requested.is_absolute() {
        let canonical = requested
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Cleanup report not found: {}", requested.display()))?;
        if !canonical.starts_with(&backup_root) {
            anyhow::bail!("Cleanup report must be inside the configured backup directory");
        }
        return Ok(canonical);
    }

    if requested.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("Cleanup report path must stay inside the configured backup directory");
    }

    let joined = backup_dir.join(requested);
    let joined_parent = joined.parent().unwrap_or(backup_dir);
    let canonical_parent = joined_parent.canonicalize().map_err(|_| {
        anyhow::anyhow!(
            "Cleanup report parent not found: {}",
            joined_parent.display()
        )
    })?;
    if !canonical_parent.starts_with(&backup_root) {
        anyhow::bail!("Cleanup report must be inside the configured backup directory");
    }

    let canonical = if joined.exists() {
        joined
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Cleanup report not found: {}", joined.display()))?
    } else {
        canonical_parent.join(
            joined
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Cleanup report filename is missing"))?,
        )
    };

    if !canonical.starts_with(&backup_root) {
        anyhow::bail!("Cleanup report must be inside the configured backup directory");
    }

    Ok(canonical)
}

pub(crate) fn clamp_link_list_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(100).clamp(1, 10_000)
}

pub(crate) fn load_cleanup_report(path: &Path) -> Option<CleanupReport> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

pub(crate) fn latest_cleanup_report_created_at(backup_dir: &Path) -> Option<String> {
    let path = latest_cleanup_report_path(backup_dir)?;
    let report = load_cleanup_report(&path)?;
    Some(
        report
            .created_at
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string(),
    )
}

fn effective_content_type(library: &LibraryConfig) -> ContentType {
    library
        .content_type
        .unwrap_or(ContentType::from_media_type(library.media_type))
}

pub(crate) fn infer_cleanup_scope(cfg: &Config, selected_libraries: &[String]) -> CleanupScope {
    let types: HashSet<ContentType> = cfg
        .libraries
        .iter()
        .filter(|lib| {
            selected_libraries.is_empty()
                || selected_libraries
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&lib.name))
        })
        .map(effective_content_type)
        .collect();

    if types.len() == 1 {
        match types.into_iter().next().unwrap() {
            ContentType::Anime => CleanupScope::Anime,
            ContentType::Tv => CleanupScope::Tv,
            ContentType::Movie => CleanupScope::Movie,
        }
    } else {
        CleanupScope::All
    }
}

fn library_matches_cleanup_scope(library: &LibraryConfig, scope: CleanupScope) -> bool {
    match scope {
        CleanupScope::Anime => effective_content_type(library) == ContentType::Anime,
        CleanupScope::Tv => effective_content_type(library) == ContentType::Tv,
        CleanupScope::Movie => effective_content_type(library) == ContentType::Movie,
        CleanupScope::All => true,
    }
}

pub(super) fn resolve_cleanup_libraries(
    cfg: &Config,
    scope: CleanupScope,
    selected_libraries: &[String],
) -> std::result::Result<Vec<String>, String> {
    let selected_names = selected_libraries
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();

    let selected_names_set = selected_names.iter().copied().collect::<HashSet<_>>();

    let unknown = selected_names
        .iter()
        .copied()
        .filter(|want| {
            !cfg.libraries
                .iter()
                .any(|lib| lib.name.eq_ignore_ascii_case(want))
        })
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown library filter(s): {}. Available: {}",
            unknown.join(", "),
            cfg.libraries
                .iter()
                .map(|library| library.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let canonical = cfg
        .libraries
        .iter()
        .filter(|library| {
            selected_names_set.is_empty()
                || selected_names_set
                    .iter()
                    .any(|name| library.name.eq_ignore_ascii_case(name))
        })
        .filter(|library| library_matches_cleanup_scope(library, scope))
        .map(|library| library.name.clone())
        .collect::<Vec<_>>();

    if !selected_names_set.is_empty() && canonical.is_empty() {
        return Err(format!(
            "No libraries matched scope {:?} for selection: {}",
            scope,
            selected_libraries.join(", ")
        ));
    }

    Ok(canonical)
}

pub(super) fn cleanup_audit_output_path(
    config: &Config,
    scope: CleanupScope,
    selected_libraries: &[String],
    timestamp: String,
) -> PathBuf {
    let scope_slug = cleanup_scope_slug(scope);
    if selected_libraries.len() == 1 {
        config.backup.path.join(format!(
            "cleanup-audit-{}-{}-{}.json",
            scope_slug, selected_libraries[0], timestamp
        ))
    } else if !selected_libraries.is_empty() {
        config.backup.path.join(format!(
            "cleanup-audit-{}-multi-{}-{}.json",
            scope_slug,
            selected_libraries.len(),
            timestamp
        ))
    } else {
        config
            .backup
            .path
            .join(format!("cleanup-audit-{}-{}.json", scope_slug, timestamp))
    }
}
