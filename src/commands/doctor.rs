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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DoctorCheckMode {
    Full,
    ReadOnly,
}

pub(crate) async fn collect_doctor_checks(
    cfg: &Config,
    db: &Database,
    mode: DoctorCheckMode,
) -> Vec<DoctorCheckResult> {
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
    let (db_parent_ok, db_parent_detail) = match mode {
        DoctorCheckMode::Full => (
            can_write_in_directory(db_parent),
            db_parent.display().to_string(),
        ),
        DoctorCheckMode::ReadOnly => inspect_directory_without_write_probe(db_parent),
    };
    checks.push(DoctorCheckResult {
        name: "db_parent_dir".to_string(),
        ok: db_parent_ok,
        detail: db_parent_detail,
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

    let (backup_ok, backup_detail) = match mode {
        DoctorCheckMode::Full => (
            can_write_in_directory(&cfg.backup.path),
            cfg.backup.path.display().to_string(),
        ),
        DoctorCheckMode::ReadOnly => inspect_directory_without_write_probe(&cfg.backup.path),
    };
    checks.push(DoctorCheckResult {
        name: "backup_dir".to_string(),
        ok: backup_ok,
        detail: backup_detail,
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

    let validation = match mode {
        DoctorCheckMode::Full => cfg.validate(),
        DoctorCheckMode::ReadOnly => cfg.validate_runtime_settings(),
    };
    let validation_detail = if matches!(mode, DoctorCheckMode::ReadOnly) {
        format!("read_only; {}", format_validation_detail(&validation))
    } else {
        format_validation_detail(&validation)
    };
    checks.push(DoctorCheckResult {
        name: "config_validation".to_string(),
        ok: validation.errors.is_empty(),
        detail: validation_detail,
    });

    checks
}

pub(crate) async fn run_doctor(cfg: &Config, db: &Database, output: OutputFormat) -> Result<()> {
    let checks = collect_doctor_checks(cfg, db, DoctorCheckMode::Full).await;

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
    let created = if path.exists() {
        false
    } else {
        if std::fs::create_dir_all(path).is_err() {
            return false;
        }
        true
    };

    #[cfg(unix)]
    if created {
        use std::os::unix::fs::PermissionsExt;

        if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).is_err() {
            return false;
        }
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

fn inspect_directory_without_write_probe(path: &std::path::Path) -> (bool, String) {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => read_only_directory_signal(path, &metadata),
        Ok(_) => (
            false,
            format!(
                "not a directory (write probe skipped in read-only mode): {}",
                path.display()
            ),
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
            false,
            format!(
                "missing (write probe skipped in read-only mode): {}",
                path.display()
            ),
        ),
        Err(err) => (
            false,
            format!(
                "could not inspect (write probe skipped in read-only mode): {} ({})",
                path.display(),
                err
            ),
        ),
    }
}

#[cfg(unix)]
fn read_only_directory_signal(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
) -> (bool, String) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o777;
    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(_) => {
            return (
                false,
                format!(
                    "exists but path contains interior NUL (write probe skipped in read-only mode; mode={:03o}): {}",
                    mode,
                    path.display()
                ),
            );
        }
    };

    let access_result = unsafe { libc::access(c_path.as_ptr(), libc::W_OK | libc::X_OK) };
    if access_result == 0 {
        (
            true,
            format!(
                "exists (write probe skipped in read-only mode; effective access allows write+traverse, mode={:03o}): {}",
                mode,
                path.display()
            ),
        )
    } else {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            (
                false,
                format!(
                    "exists but effective access denies write or traverse (write probe skipped in read-only mode; mode={:03o}): {}",
                    mode,
                    path.display()
                ),
            )
        } else {
            (
                false,
                format!(
                    "exists but effective access check failed (write probe skipped in read-only mode; mode={:03o}): {} ({})",
                    mode,
                    path.display(),
                    err
                ),
            )
        }
    }
}

#[cfg(not(unix))]
fn read_only_directory_signal(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
) -> (bool, String) {
    let writable = !metadata.permissions().readonly();

    if writable {
        (
            true,
            format!(
                "exists (write probe skipped in read-only mode; readonly flag not set): {}",
                path.display()
            ),
        )
    } else {
        (
            false,
            format!(
                "exists but readonly flag is set (write probe skipped in read-only mode): {}",
                path.display()
            ),
        )
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

    #[cfg(unix)]
    #[test]
    fn can_write_in_directory_secures_missing_path_on_create() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("doctor").join("probe");
        assert!(can_write_in_directory(&nested));

        let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn inspect_directory_without_write_probe_does_not_create_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("doctor").join("probe");
        let (ok, detail) = inspect_directory_without_write_probe(&nested);
        assert!(!ok);
        assert!(detail.contains("write probe skipped in read-only mode"));
        assert!(!nested.exists());
    }

    #[cfg(unix)]
    #[test]
    fn inspect_directory_without_write_probe_flags_non_writable_existing_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).unwrap();

        let (ok, detail) = inspect_directory_without_write_probe(&path);

        assert!(!ok);
        assert!(detail.contains("denies write or traverse"));
        assert!(detail.contains("mode=555"));
    }

    #[cfg(unix)]
    #[test]
    fn inspect_directory_without_write_probe_preserves_positive_signal_for_writable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writable");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (ok, detail) = inspect_directory_without_write_probe(&path);

        assert!(ok);
        assert!(detail.contains("effective access allows write+traverse"));
        assert!(detail.contains("mode=755"));
    }

    #[cfg(unix)]
    #[test]
    fn inspect_directory_without_write_probe_requires_execute_bit_for_directory_access() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noexec");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();

        let (ok, detail) = inspect_directory_without_write_probe(&path);

        assert!(!ok);
        assert!(detail.contains("denies write or traverse"));
        assert!(detail.contains("mode=666"));
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
