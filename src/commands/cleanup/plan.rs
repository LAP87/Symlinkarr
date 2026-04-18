use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use chrono::Utc;

use crate::cleanup_audit::{self, CleanupScope};
use crate::config::Config;

use super::{AnimeRemediationPlanReport, ANIME_REMEDIATION_REPORT_VERSION};

fn default_plex_db_candidates() -> [&'static str; 3] {
    [
        "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
        "/config/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
    ]
}

fn canonical_plex_db_path(path: PathBuf) -> Option<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    let canonical = path.canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }

    canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| ext.eq_ignore_ascii_case("db"))?;

    Some(canonical)
}

pub(super) fn resolve_plex_db_path(query_path: Option<&str>) -> Option<PathBuf> {
    if let Some(requested) = query_path.map(str::trim).filter(|value| !value.is_empty()) {
        return canonical_plex_db_path(PathBuf::from(requested));
    }

    default_plex_db_candidates()
        .into_iter()
        .map(PathBuf::from)
        .find_map(canonical_plex_db_path)
}

pub(super) fn default_anime_remediation_report_path(cfg: &Config) -> PathBuf {
    cfg.backup.path.join(format!(
        "anime-remediation-{}.json",
        Utc::now().format("%Y%m%d-%H%M%S")
    ))
}

pub(super) fn load_anime_remediation_plan_report(
    path: &Path,
) -> Result<AnimeRemediationPlanReport> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub(super) fn validate_anime_remediation_plan_report(
    report: &AnimeRemediationPlanReport,
) -> Result<()> {
    if report.version != ANIME_REMEDIATION_REPORT_VERSION {
        anyhow::bail!(
            "Unsupported anime remediation report version {} (expected {})",
            report.version,
            ANIME_REMEDIATION_REPORT_VERSION
        );
    }

    if report.cleanup_report.scope != CleanupScope::Anime {
        anyhow::bail!("Anime remediation report contains a non-anime cleanup payload");
    }

    if report.cleanup_report.findings.iter().any(|finding| {
        !finding
            .reasons
            .contains(&cleanup_audit::FindingReason::LegacyAnimeRootDuplicate)
            || finding.legacy_anime_root.is_none()
    }) {
        anyhow::bail!("Anime remediation report contains non-remediation cleanup findings");
    }

    Ok(())
}

pub(super) fn write_temp_cleanup_report(
    backup_root: &Path,
    report: &cleanup_audit::CleanupReport,
) -> Result<PathBuf> {
    let temp_path = backup_root.join(format!(
        "anime-remediation-apply-{}.tmp.json",
        Utc::now().format("%Y%m%d-%H%M%S-%3f")
    ));
    if let Some(parent) = temp_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&temp_path, serde_json::to_string_pretty(report)?)?;
    Ok(temp_path)
}
