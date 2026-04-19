use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;
use tracing::info;

use super::{AutoAcquireBatchSummary, LinkProcessSummary, MatchResult, ScanTelemetry};
use crate::linker::DeadLinkSummary;
use crate::utils::user_println;

pub(crate) fn build_skip_reason_json(
    telemetry: &ScanTelemetry,
    link_summary: &LinkProcessSummary,
    dead_summary: &DeadLinkSummary,
    auto_acquire_summary: &AutoAcquireBatchSummary,
) -> Result<Option<String>> {
    let reasons =
        aggregate_skip_reasons(telemetry, link_summary, dead_summary, auto_acquire_summary);
    if reasons.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(&reasons)?))
    }
}

pub(crate) fn aggregate_skip_reasons(
    telemetry: &ScanTelemetry,
    link_summary: &LinkProcessSummary,
    dead_summary: &DeadLinkSummary,
    auto_acquire_summary: &AutoAcquireBatchSummary,
) -> BTreeMap<String, i64> {
    let mut reasons: BTreeMap<String, i64> = BTreeMap::new();

    for (reason, count) in &telemetry.match_stats.skip_reasons {
        *reasons.entry(reason.clone()).or_insert(0) += *count as i64;
    }
    for (reason, count) in &link_summary.skip_reasons {
        *reasons.entry(reason.clone()).or_insert(0) += *count as i64;
    }
    for (reason, count) in &dead_summary.skip_reasons {
        *reasons.entry(reason.clone()).or_insert(0) += *count as i64;
    }
    for (reason, count) in &auto_acquire_summary.reason_counts {
        *reasons.entry(reason.clone()).or_insert(0) += *count as i64;
    }

    reasons
}

pub(crate) fn log_scan_telemetry(
    telemetry: &ScanTelemetry,
    matches: &[MatchResult],
    link_summary: &LinkProcessSummary,
) {
    info!(
        "Scan phase telemetry: runtime_checks={}, library_scan={}, source_inventory={}, matching={}, title_enrichment={}, linking={}, media_refresh={}, dead_link_sweep={}",
        fmt_duration(telemetry.runtime_checks),
        fmt_duration(telemetry.library_scan),
        fmt_duration(telemetry.source_inventory),
        fmt_duration(telemetry.match_total),
        fmt_duration(telemetry.episode_title_enrichment),
        fmt_duration(telemetry.linking),
        fmt_duration(telemetry.plex_refresh),
        fmt_duration(telemetry.dead_link_sweep),
    );

    info!(
        "Scan telemetry details: cache_hit_ratio={}, cached_items={}, filesystem_items={}, metadata_alias_prep={}, candidate_scan={}, destination_reduce={}, metadata_errors={}, worker_count={}, candidate_slots={}, scored_candidates={}, exact_id_hits={}, ambiguous_skipped={}, refresh_requested_paths={}, refresh_unique_paths={}, refresh_batches={}, coalesced_batches={}, refreshed_batches={}, refreshed_paths_covered={}, skipped_refresh_batches={}, capped_refresh_batches={}, refresh_aborted_due_to_cap={}, refresh_deferred_due_to_lock={}, failed_refresh_batches={}, unresolved_refresh_paths={}",
        telemetry
            .source_inventory_stats
            .cache_hit_ratio()
            .map(|ratio| format!("{:.0}%", ratio * 100.0))
            .unwrap_or_else(|| "n/a".to_string()),
        telemetry.source_inventory_stats.cached_items,
        telemetry.source_inventory_stats.filesystem_items,
        fmt_duration(telemetry.match_stats.metadata_alias_prep),
        fmt_duration(telemetry.match_stats.candidate_scan),
        fmt_duration(telemetry.match_stats.destination_reduce),
        telemetry.match_stats.metadata_errors,
        telemetry.match_stats.worker_count,
        telemetry.match_stats.prefiltered_library_candidates,
        telemetry.match_stats.scored_candidates,
        telemetry.match_stats.exact_id_hits,
        telemetry.match_stats.ambiguous_skipped,
        telemetry.plex_refresh_stats.requested_paths,
        telemetry.plex_refresh_stats.unique_paths,
        telemetry.plex_refresh_stats.planned_batches,
        telemetry.plex_refresh_stats.coalesced_batches,
        telemetry.plex_refresh_stats.refreshed_batches,
        telemetry.plex_refresh_stats.refreshed_paths_covered,
        telemetry.plex_refresh_stats.skipped_batches,
        telemetry.plex_refresh_stats.capped_batches,
        telemetry.plex_refresh_stats.aborted_due_to_cap,
        telemetry.plex_refresh_stats.deferred_due_to_lock,
        telemetry.plex_refresh_stats.failed_batches,
        telemetry.plex_refresh_stats.unresolved_paths,
    );

    user_println(format!(
        "   📊 Scan telemetry: checks={} | library={} | source={} | match={} | titles={} | link={} | refresh={} | dead={}",
        fmt_duration(telemetry.runtime_checks),
        fmt_duration(telemetry.library_scan),
        fmt_duration(telemetry.source_inventory),
        fmt_duration(telemetry.match_total),
        fmt_duration(telemetry.episode_title_enrichment),
        fmt_duration(telemetry.linking),
        fmt_duration(telemetry.plex_refresh),
        fmt_duration(telemetry.dead_link_sweep),
    ));
    user_println(format!(
        "   📊 Scan details: matches={} created={} updated={} skipped={} ambiguous={} candidates={} scored={} exact-id={} cache-hit={} refresh={}/{} skipped={} capped={}{}{}",
        matches.len(),
        link_summary.created,
        link_summary.updated,
        link_summary.skipped,
        telemetry.match_stats.ambiguous_skipped,
        telemetry.match_stats.prefiltered_library_candidates,
        telemetry.match_stats.scored_candidates,
        telemetry.match_stats.exact_id_hits,
        telemetry
            .source_inventory_stats
            .cache_hit_ratio()
            .map(|ratio| format!("{:.0}%", ratio * 100.0))
            .unwrap_or_else(|| "n/a".to_string()),
        telemetry.plex_refresh_stats.refreshed_batches,
        telemetry.plex_refresh_stats.planned_batches,
        telemetry.plex_refresh_stats.skipped_batches,
        telemetry.plex_refresh_stats.capped_batches,
        if telemetry.plex_refresh_stats.aborted_due_to_cap {
            " aborted"
        } else {
            ""
        },
        if telemetry.plex_refresh_stats.deferred_due_to_lock {
            " deferred"
        } else {
            ""
        },
    ));
}

pub(crate) fn fmt_duration(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

pub(crate) fn duration_ms_i64(duration: Duration) -> i64 {
    duration
        .as_millis()
        .min(i64::MAX as u128)
        .try_into()
        .unwrap_or(i64::MAX)
}
