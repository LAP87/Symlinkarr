use anyhow::Result;
use serde::Serialize;

use crate::commands::{
    print_json, runtime_source_health, runtime_source_probe_path, DIRECTORY_PROBE_TIMEOUT,
};
use crate::config::Config;
use crate::db::Database;
use crate::utils::directory_path_health_with_timeout;
use crate::OutputFormat;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorCheckResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

pub(crate) async fn collect_doctor_checks(cfg: &Config, db: &Database) -> Vec<DoctorCheckResult> {
    let mut checks = Vec::new();

    match db.get_stats().await {
        Ok((active, dead, total)) => checks.push(DoctorCheckResult {
            name: "database".to_string(),
            ok: true,
            detail: format!(
                "reachable (active={}, dead={}, total={})",
                active, dead, total
            ),
        }),
        Err(e) => checks.push(DoctorCheckResult {
            name: "database".to_string(),
            ok: false,
            detail: format!("unreachable: {}", e),
        }),
    }
    match db.schema_version().await {
        Ok(version) => checks.push(DoctorCheckResult {
            name: "db_schema_version".to_string(),
            ok: version >= 1,
            detail: version.to_string(),
        }),
        Err(e) => checks.push(DoctorCheckResult {
            name: "db_schema_version".to_string(),
            ok: false,
            detail: e.to_string(),
        }),
    }
    let db_parent = std::path::Path::new(&cfg.db_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    checks.push(DoctorCheckResult {
        name: "db_parent_dir".to_string(),
        ok: can_write_in_directory(db_parent),
        detail: db_parent.display().to_string(),
    });

    for lib in &cfg.libraries {
        let health =
            directory_path_health_with_timeout(lib.path.clone(), DIRECTORY_PROBE_TIMEOUT).await;
        checks.push(DoctorCheckResult {
            name: format!("library:{}", lib.name),
            ok: health.is_healthy(),
            detail: health.describe(&lib.path),
        });
    }

    for src in &cfg.sources {
        let probe_path = runtime_source_probe_path(&src.path);
        let health = runtime_source_health(&src.path, &probe_path).await;
        checks.push(DoctorCheckResult {
            name: format!("source:{}", src.name),
            ok: health.is_healthy(),
            detail: health.describe(&probe_path),
        });
    }

    checks.push(DoctorCheckResult {
        name: "backup_dir".to_string(),
        ok: can_write_in_directory(&cfg.backup.path),
        detail: cfg.backup.path.display().to_string(),
    });
    checks.push(DoctorCheckResult {
        name: "backup.max_safety_backups".to_string(),
        ok: cfg.backup.max_safety_backups > 0,
        detail: cfg.backup.max_safety_backups.to_string(),
    });

    checks.push(DoctorCheckResult {
        name: "security.enforce_roots".to_string(),
        ok: cfg.security.enforce_roots,
        detail: cfg.security.enforce_roots.to_string(),
    });
    checks.push(DoctorCheckResult {
        name: "security.require_secret_provider".to_string(),
        ok: cfg.security.require_secret_provider,
        detail: cfg.security.require_secret_provider.to_string(),
    });
    checks.push(DoctorCheckResult {
        name: "cleanup.prune.enforce_policy".to_string(),
        ok: cfg.cleanup.prune.enforce_policy,
        detail: cfg.cleanup.prune.enforce_policy.to_string(),
    });

    let validation = cfg.validate();
    checks.push(DoctorCheckResult {
        name: "config_validation".to_string(),
        ok: validation.errors.is_empty(),
        detail: format_validation_detail(&validation),
    });

    checks
}

pub(crate) async fn run_doctor(cfg: &Config, db: &Database, output: OutputFormat) -> Result<()> {
    let checks = collect_doctor_checks(cfg, db).await;

    if output == OutputFormat::Json {
        let failed = checks.iter().filter(|c| !c.ok).count();
        print_json(&serde_json::json!({
            "checks": checks,
            "failed": failed,
        }));
    } else {
        println!("🩺 Symlinkarr Doctor");
        for c in &checks {
            let icon = if c.ok { "✅" } else { "❌" };
            println!("   {} {:<34} {}", icon, c.name, c.detail);
        }
    }

    Ok(())
}

fn can_write_in_directory(path: &std::path::Path) -> bool {
    if std::fs::create_dir_all(path).is_err() {
        return false;
    }

    let probe = path.join(format!(
        ".symlinkarr-write-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));

    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn format_validation_detail(report: &crate::config::ValidationReport) -> String {
    let mut detail = format!(
        "errors={}, warnings={}",
        report.errors.len(),
        report.warnings.len()
    );

    if !report.errors.is_empty() {
        detail.push_str("; ");
        detail.push_str(&report.errors.join(" | "));
    } else if !report.warnings.is_empty() {
        detail.push_str("; ");
        detail.push_str(&report.warnings.join(" | "));
    }

    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_write_in_directory_creates_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("doctor").join("probe");
        assert!(can_write_in_directory(&nested));
        assert!(nested.exists());
    }

    #[test]
    fn format_validation_detail_includes_counts_and_messages() {
        let report = crate::config::ValidationReport {
            errors: vec!["missing library".to_string()],
            warnings: vec!["plaintext secret".to_string()],
        };
        let detail = format_validation_detail(&report);
        assert!(detail.contains("errors=1"));
        assert!(detail.contains("warnings=1"));
        assert!(detail.contains("missing library"));
        assert!(!detail.contains("plaintext secret"));

        let warnings_only = crate::config::ValidationReport {
            errors: Vec::new(),
            warnings: vec!["backup.max_safety_backups=0".to_string()],
        };
        let warning_detail = format_validation_detail(&warnings_only);
        assert!(warning_detail.contains("errors=0"));
        assert!(warning_detail.contains("warnings=1"));
        assert!(warning_detail.contains("backup.max_safety_backups=0"));
    }
}
