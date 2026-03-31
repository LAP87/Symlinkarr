use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tokio::time::sleep;

use crate::api::plex::{plan_refresh_batches, PlexClient, PlexRefreshPlan};
use crate::config::Config;
use crate::utils::user_println;

use super::LibraryRefreshTelemetry;

fn emit_refresh_line(emit_text: bool, message: impl AsRef<str>) {
    if emit_text {
        user_println(message);
    }
}

pub(crate) async fn refresh_library_paths(
    cfg: &Config,
    refresh_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryRefreshTelemetry> {
    let mut telemetry = LibraryRefreshTelemetry {
        requested_paths: refresh_paths.len(),
        ..LibraryRefreshTelemetry::default()
    };
    if refresh_paths.is_empty() || !cfg.has_plex_refresh() {
        return Ok(telemetry);
    }

    let plex = PlexClient::new(&cfg.plex.url, &cfg.plex.token);
    let sections = plex.get_sections().await?;
    let planned = plan_refresh_batches(
        &sections,
        refresh_paths,
        cfg.plex.refresh_coalesce_threshold,
    );
    let (plan, dropped_batches) =
        enforce_refresh_batch_limit(planned, cfg.plex.max_refresh_batches_per_run);

    telemetry.unique_paths = plan.unique_paths;
    telemetry.planned_batches = plan.batches.len() + dropped_batches;
    telemetry.coalesced_batches = plan.coalesced_batches;
    telemetry.coalesced_paths = plan.coalesced_paths;
    telemetry.unresolved_paths = plan.unresolved_paths.len();
    telemetry.capped_batches = dropped_batches;

    for path in &plan.unresolved_paths {
        emit_refresh_line(
            emit_text,
            format!(
                "   ⚠️  Plex: no matching library section found for {}",
                path.display()
            ),
        );
    }
    telemetry.skipped_batches += plan.unresolved_paths.len();
    if dropped_batches > 0 {
        if cfg.plex.abort_refresh_when_capped {
            telemetry.aborted_due_to_cap = true;
            telemetry.skipped_batches += telemetry.planned_batches;
            emit_refresh_line(
                emit_text,
                format!(
                    "   ⚠️  Plex: refresh plan needed {} request(s), exceeding cap {}. Aborted all targeted refreshes to protect Plex.",
                    telemetry.planned_batches, cfg.plex.max_refresh_batches_per_run
                ),
            );
            if telemetry.coalesced_batches > 0 {
                emit_refresh_line(
                    emit_text,
                    format!(
                        "   📺 Plex: coalesced {} path(s) into {} library-root refresh(es) before the cap guard stopped the run",
                        telemetry.coalesced_paths, telemetry.coalesced_batches
                    ),
                );
            }
            if telemetry.skipped_batches > 0 {
                emit_refresh_line(
                    emit_text,
                    format!(
                        "   ⚠️  Plex: {} refresh request(s) were not queued",
                        telemetry.skipped_batches
                    ),
                );
            }
            return Ok(telemetry);
        }

        telemetry.skipped_batches += dropped_batches;
        emit_refresh_line(
            emit_text,
            format!(
                "   ⚠️  Plex: capped refresh plan at {} request(s); {} request(s) skipped to reduce load",
                cfg.plex.max_refresh_batches_per_run, dropped_batches
            ),
        );
    }

    let refresh_delay = Duration::from_millis(cfg.plex.refresh_delay_ms);
    let batch_count = plan.batches.len();
    for (idx, batch) in plan.batches.into_iter().enumerate() {
        match plex
            .refresh_path(&batch.section_key, &batch.refresh_path)
            .await
        {
            Ok(_) => {
                telemetry.refreshed_batches += 1;
                telemetry.refreshed_paths_covered += batch.covered_paths;
            }
            Err(err) => {
                emit_refresh_line(
                    emit_text,
                    format!(
                        "   ⚠️  Plex: refresh failed for {} (section '{}'): {}",
                        batch.refresh_path.display(),
                        batch.section_title,
                        err
                    ),
                );
                telemetry.failed_batches += 1;
                telemetry.skipped_batches += 1;
            }
        }

        if refresh_delay > Duration::ZERO && idx + 1 < batch_count {
            sleep(refresh_delay).await;
        }
    }

    if telemetry.refreshed_batches > 0 {
        emit_refresh_line(
            emit_text,
            format!(
                "   📺 Plex: targeted refresh queued for {} request(s) covering {} path(s)",
                telemetry.refreshed_batches, telemetry.refreshed_paths_covered
            ),
        );
    }
    if telemetry.coalesced_batches > 0 {
        emit_refresh_line(
            emit_text,
            format!(
                "   📺 Plex: coalesced {} path(s) into {} library-root refresh(es)",
                telemetry.coalesced_paths, telemetry.coalesced_batches
            ),
        );
    }
    if telemetry.skipped_batches > 0 {
        emit_refresh_line(
            emit_text,
            format!(
                "   ⚠️  Plex: {} refresh request(s) were not queued",
                telemetry.skipped_batches
            ),
        );
    }

    Ok(telemetry)
}

pub(crate) async fn probe_sections(cfg: &Config) -> Result<usize> {
    let plex = PlexClient::new(&cfg.plex.url, &cfg.plex.token);
    Ok(plex.get_sections().await?.len())
}

fn enforce_refresh_batch_limit(
    mut plan: PlexRefreshPlan,
    max_batches: usize,
) -> (PlexRefreshPlan, usize) {
    if max_batches == 0 || plan.batches.len() <= max_batches {
        return (plan, 0);
    }

    let dropped_batches = plan.batches.len().saturating_sub(max_batches);
    plan.batches.truncate(max_batches);
    (plan, dropped_batches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::plex::PlexRefreshBatch;

    #[test]
    fn enforce_refresh_batch_limit_keeps_small_plans_intact() {
        let plan = PlexRefreshPlan {
            batches: vec![
                PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/a"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
                PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/b"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
            ],
            ..PlexRefreshPlan::default()
        };

        let (limited, dropped) = enforce_refresh_batch_limit(plan, 2);
        assert_eq!(limited.batches.len(), 2);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn enforce_refresh_batch_limit_truncates_large_plans() {
        let plan = PlexRefreshPlan {
            batches: vec![
                PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/a"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
                PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/b"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
                PlexRefreshBatch {
                    section_key: "1".to_string(),
                    section_title: "Anime".to_string(),
                    refresh_path: PathBuf::from("/library/c"),
                    covered_paths: 1,
                    coalesced_to_root: false,
                },
            ],
            ..PlexRefreshPlan::default()
        };

        let (limited, dropped) = enforce_refresh_batch_limit(plan, 2);
        assert_eq!(limited.batches.len(), 2);
        assert_eq!(limited.batches[0].refresh_path, PathBuf::from("/library/a"));
        assert_eq!(limited.batches[1].refresh_path, PathBuf::from("/library/b"));
        assert_eq!(dropped, 1);
    }
}
