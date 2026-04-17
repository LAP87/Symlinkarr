use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(super) fn validate_secure_permissions(cfg: &Config, report: &mut ValidationReport) {
    #[cfg(unix)]
    {
        if let Some(db_path) = secure_path_if_exists(Path::new(&cfg.db_path)) {
            validate_private_path(db_path, "db_path", report);
        }
        if let Some(backup_path) = secure_path_if_exists(&cfg.backup.path) {
            validate_private_path(backup_path, "backup.path", report);
        }
        let quarantine_path = if cfg.cleanup.prune.quarantine_path.is_absolute() {
            cfg.cleanup.prune.quarantine_path.clone()
        } else {
            cfg.backup.path.join(&cfg.cleanup.prune.quarantine_path)
        };
        if let Some(quarantine_path) = secure_path_if_exists(&quarantine_path) {
            validate_private_path(quarantine_path, "cleanup.prune.quarantine_path", report);
        }
        for secret_path in &cfg.secret_files {
            if let Some(secret_path) = secure_path_if_exists(secret_path) {
                validate_private_path(secret_path, "secretfile", report);
            }
        }
    }

    #[cfg(not(unix))]
    {
        report.warnings.push(
            "security.enforce_secure_permissions is not enforced on this platform".to_string(),
        );
    }
}

#[cfg(unix)]
fn secure_path_if_exists(path: &Path) -> Option<&Path> {
    path.exists().then_some(path)
}

#[cfg(unix)]
fn validate_private_path(path: &Path, label: &str, report: &mut ValidationReport) {
    let Ok(metadata) = std::fs::metadata(path) else {
        report.errors.push(format!(
            "{} could not be inspected for permissions: {}",
            label,
            path.display()
        ));
        return;
    };

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        report.errors.push(format!(
            "{} must not be group/world accessible: {} (mode {:o})",
            label,
            path.display(),
            mode
        ));
    }
}
