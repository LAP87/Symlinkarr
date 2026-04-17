use super::*;

pub async fn run_prune(
    cfg: &Config,
    db: &Database,
    report_path: &Path,
    apply: bool,
    include_legacy_anime_roots: bool,
    max_delete: Option<usize>,
    confirmation_token: Option<&str>,
) -> Result<PruneOutcome> {
    if apply {
        ensure_runtime_sources_healthy(&cfg.sources, "cleanup prune apply").await?;
    }

    let json = std::fs::read_to_string(report_path)?;
    let mut report: CleanupReport = serde_json::from_str(&json)?;
    hydrate_report_db_tracked_flags(db, &mut report).await?;
    let tracked_paths: HashSet<_> = report
        .findings
        .iter()
        .filter(|finding| finding.db_tracked)
        .map(|finding| finding.symlink_path.clone())
        .collect();
    let plan = build_prune_plan(
        &report,
        cfg.cleanup.prune.quarantine_foreign,
        include_legacy_anime_roots,
    );

    info!(
        "Cleanup prune: {} high/critical + {} safe duplicate candidates ({} total unique; {} managed delete / {} foreign quarantine)",
        plan.high_or_critical_candidates,
        plan.safe_warning_duplicate_candidates,
        plan.candidate_paths.len(),
        plan.managed_candidates,
        plan.foreign_candidates,
    );

    if !apply {
        return Ok(PruneOutcome {
            candidates: plan.candidate_paths.len(),
            blocked_candidates: plan.blocked_candidates,
            high_or_critical_candidates: plan.high_or_critical_candidates,
            safe_warning_duplicate_candidates: plan.safe_warning_duplicate_candidates,
            legacy_anime_root_candidates: plan.legacy_anime_root_candidates,
            legacy_anime_root_groups: plan.legacy_anime_root_groups.clone(),
            managed_candidates: plan.managed_candidates,
            foreign_candidates: plan.foreign_candidates,
            reason_counts: plan.reason_counts.clone(),
            blocked_reason_summary: plan.blocked_reason_summary.clone(),
            removed: 0,
            quarantined: 0,
            skipped: 0,
            confirmation_token: plan.confirmation_token,
            affected_paths: plan.candidate_paths,
        });
    }

    if let Some(applied_at) = report.applied_at {
        anyhow::bail!(
            "Refusing prune apply: this report was already applied at {}",
            applied_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    if plan.candidate_paths.is_empty() {
        if let Some(blocked) = plan.blocked_reason_summary.first() {
            anyhow::bail!(
                "Refusing prune apply: no actionable candidates remain; {} path(s) are blocked by current policy ({})",
                blocked.candidates,
                blocked.code
            );
        }
        anyhow::bail!("Refusing prune apply: report contains no actionable prune candidates");
    }

    let delete_cap = max_delete.unwrap_or(cfg.cleanup.prune.default_max_delete);
    if cfg.cleanup.prune.enforce_policy {
        if report.version != 1 {
            anyhow::bail!(
                "Refusing prune apply: unsupported report version {} (expected 1)",
                report.version
            );
        }

        let age = Utc::now().signed_duration_since(report.created_at);
        if age.num_hours() > cfg.cleanup.prune.max_report_age_hours as i64 {
            anyhow::bail!(
                "Refusing prune apply: report is too old ({}h > max {}h)",
                age.num_hours(),
                cfg.cleanup.prune.max_report_age_hours
            );
        }

        let provided = confirmation_token.unwrap_or("");
        if provided.is_empty() || provided != plan.confirmation_token {
            anyhow::bail!(
                "Refusing prune apply: invalid or missing confirmation token. Re-run preview and pass --confirm-token {}",
                plan.confirmation_token
            );
        }
    }

    if plan.candidate_paths.len() > delete_cap {
        anyhow::bail!(
            "Refusing prune apply: {} candidates exceeds delete cap {} (use --max-delete to override)",
            plan.candidate_paths.len(),
            delete_cap
        );
    }

    if cfg.backup.enabled {
        let backup = BackupManager::new(&cfg.backup);
        let extra_snapshot_paths: Vec<_> = plan
            .candidate_paths
            .iter()
            .filter(|path| !tracked_paths.contains(*path))
            .cloned()
            .collect();
        backup
            .create_safety_snapshot_with_extras(db, "cleanup-prune", &extra_snapshot_paths)
            .await?;
    }

    let mut removed = 0usize;
    let mut quarantined = 0usize;
    let mut skipped = 0usize;

    for symlink_path in &plan.candidate_paths {
        if cfg.security.enforce_roots && !path_is_within_roots(symlink_path, &library_roots(cfg)) {
            warn!(
                "Cleanup prune: skipping {:?} (outside configured library roots)",
                symlink_path
            );
            let _ = db
                .record_link_event_fields(
                    "prune_skipped",
                    symlink_path,
                    None,
                    None,
                    Some("outside_library_roots"),
                )
                .await;
            skipped += 1;
            continue;
        }

        match std::fs::symlink_metadata(symlink_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    match plan.dispositions.get(symlink_path).copied() {
                        Some(PruneDisposition::Delete) => {
                            if let Err(e) = std::fs::remove_file(symlink_path) {
                                warn!("Cleanup prune: failed removing {:?}: {}", symlink_path, e);
                                let _ = db
                                    .record_link_event_fields(
                                        "prune_skipped",
                                        symlink_path,
                                        None,
                                        None,
                                        Some("delete_failed"),
                                    )
                                    .await;
                                skipped += 1;
                            } else {
                                removed += 1;
                                if let Err(e) = db.mark_removed_path(symlink_path).await {
                                    warn!(
                                        "Cleanup prune: removed {:?} but failed DB mark_removed: {}",
                                        symlink_path, e
                                    );
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_removed",
                                            symlink_path,
                                            None,
                                            None,
                                            Some("db_mark_removed_failed"),
                                        )
                                        .await;
                                } else {
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_removed",
                                            symlink_path,
                                            None,
                                            None,
                                            None,
                                        )
                                        .await;
                                }
                            }
                        }
                        Some(PruneDisposition::Quarantine) => {
                            if !cfg.cleanup.prune.quarantine_foreign {
                                let _ = db
                                    .record_link_event_fields(
                                        "prune_skipped",
                                        symlink_path,
                                        None,
                                        None,
                                        Some("foreign_quarantine_disabled"),
                                    )
                                    .await;
                                skipped += 1;
                                continue;
                            }

                            match quarantine_symlink_for_cleanup(cfg, symlink_path) {
                                Ok(destination) => {
                                    let note = format!("quarantined_to={}", destination.display());
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_quarantined",
                                            symlink_path,
                                            None,
                                            None,
                                            Some(&note),
                                        )
                                        .await;
                                    quarantined += 1;
                                }
                                Err(e) => {
                                    warn!(
                                        "Cleanup prune: failed quarantining {:?}: {}",
                                        symlink_path, e
                                    );
                                    let _ = db
                                        .record_link_event_fields(
                                            "prune_skipped",
                                            symlink_path,
                                            None,
                                            None,
                                            Some("quarantine_failed"),
                                        )
                                        .await;
                                    skipped += 1;
                                }
                            }
                        }
                        None => {
                            let _ = db
                                .record_link_event_fields(
                                    "prune_skipped",
                                    symlink_path,
                                    None,
                                    None,
                                    Some("missing_prune_disposition"),
                                )
                                .await;
                            skipped += 1;
                        }
                    }
                } else {
                    warn!("Cleanup prune: skipping {:?} (not a symlink)", symlink_path);
                    let _ = db
                        .record_link_event_fields(
                            "prune_skipped",
                            symlink_path,
                            None,
                            None,
                            Some("not_symlink"),
                        )
                        .await;
                    skipped += 1;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _ = db
                    .record_link_event_fields(
                        "prune_skipped",
                        symlink_path,
                        None,
                        None,
                        Some("not_found"),
                    )
                    .await;
                skipped += 1;
            }
            Err(e) => {
                warn!("Cleanup prune: could not inspect {:?}: {}", symlink_path, e);
                let _ = db
                    .record_link_event_fields(
                        "prune_skipped",
                        symlink_path,
                        None,
                        None,
                        Some("metadata_failed"),
                    )
                    .await;
                skipped += 1;
            }
        }
    }

    // Stamp applied_at and write back so this report cannot be reused
    report.applied_at = Some(Utc::now());
    if let Ok(updated_json) = serde_json::to_string_pretty(&report) {
        if let Err(e) = std::fs::write(report_path, &updated_json) {
            warn!(
                "Cleanup prune: applied successfully but failed to stamp report: {}",
                e
            );
        }
    }

    Ok(PruneOutcome {
        candidates: plan.candidate_paths.len(),
        blocked_candidates: plan.blocked_candidates,
        high_or_critical_candidates: plan.high_or_critical_candidates,
        safe_warning_duplicate_candidates: plan.safe_warning_duplicate_candidates,
        legacy_anime_root_candidates: plan.legacy_anime_root_candidates,
        legacy_anime_root_groups: plan.legacy_anime_root_groups,
        managed_candidates: plan.managed_candidates,
        foreign_candidates: plan.foreign_candidates,
        reason_counts: plan.reason_counts,
        blocked_reason_summary: plan.blocked_reason_summary,
        removed,
        quarantined,
        skipped,
        confirmation_token: plan.confirmation_token,
        affected_paths: plan.candidate_paths,
    })
}

pub(crate) fn quarantine_symlink_for_cleanup(cfg: &Config, symlink_path: &Path) -> Result<PathBuf> {
    let target = std::fs::read_link(symlink_path)?;
    let resolved_target = resolve_link_target(symlink_path, &target);
    let destination = quarantine_destination(cfg, symlink_path);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if let Err(err) = std::fs::rename(symlink_path, &destination) {
        if err.raw_os_error() != Some(libc::EXDEV) {
            return Err(err.into());
        }
    } else {
        return Ok(destination);
    }

    let staging_path = quarantine_staging_path(symlink_path);
    std::fs::rename(symlink_path, &staging_path)?;

    if let Err(create_err) = create_symlink_like(&resolved_target, &destination) {
        let rollback_result = std::fs::rename(&staging_path, symlink_path);
        return match rollback_result {
            Ok(()) => Err(anyhow::anyhow!(
                "failed to stage quarantined symlink at {}: {}",
                destination.display(),
                create_err
            )),
            Err(rollback_err) => Err(anyhow::anyhow!(
                "failed to stage quarantined symlink at {}: {}; rollback also failed for {}: {}",
                destination.display(),
                create_err,
                symlink_path.display(),
                rollback_err
            )),
        };
    }

    if let Err(remove_err) = std::fs::remove_file(&staging_path) {
        return Err(anyhow::anyhow!(
            "quarantine staging cleanup failed for {}: {}",
            staging_path.display(),
            remove_err
        ));
    }

    Ok(destination)
}

fn quarantine_destination(cfg: &Config, symlink_path: &Path) -> PathBuf {
    let quarantine_root = if cfg.cleanup.prune.quarantine_path.is_absolute() {
        cfg.cleanup.prune.quarantine_path.clone()
    } else {
        cfg.backup.path.join(&cfg.cleanup.prune.quarantine_path)
    };

    let relative = library_roots(cfg)
        .into_iter()
        .find_map(|root| {
            symlink_path.strip_prefix(&root).ok().map(|rel| {
                let label = root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or("library");
                PathBuf::from(label).join(rel)
            })
        })
        .unwrap_or_else(|| {
            symlink_path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("unknown.symlink"))
        });

    unique_quarantine_path(quarantine_root.join(relative))
}

fn unique_quarantine_path(initial: PathBuf) -> PathBuf {
    if !initial.exists() {
        return initial;
    }

    let parent = initial
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = initial
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("quarantine");
    let ext = initial.extension().and_then(|e| e.to_str());

    for idx in 1.. {
        let filename = match ext {
            Some(ext) => format!("{stem}.quarantine-{idx}.{ext}"),
            None => format!("{stem}.quarantine-{idx}"),
        };
        let candidate = parent.join(filename);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("quarantine path search should always terminate")
}

fn quarantine_staging_path(original: &Path) -> PathBuf {
    let parent = original
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = original
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("quarantine-staging");

    for idx in 0.. {
        let candidate = parent.join(format!(".{name}.symlinkarr-quarantine-{idx}.tmp"));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("staging path search should always terminate")
}

#[cfg(unix)]
fn create_symlink_like(target: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, destination)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink_like(target: &Path, destination: &Path) -> Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}
