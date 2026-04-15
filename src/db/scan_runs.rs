use std::path::{Path, PathBuf};

use anyhow::Result;
use sqlx::Row;

use super::{
    path_to_db_text, Database, LinkEventHistoryRecord, LinkEventRecord, ScanHistoryRecord,
    ScanRunRecord, WebStats,
};

impl Database {
    /// Record a scan result.
    pub async fn record_scan(
        &self,
        library_items: i64,
        source_items: i64,
        matches: i64,
        links_created: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO scan_history (library_items_found, source_items_found, matches_found, links_created)
             VALUES (?, ?, ?, ?)",
        )
        .bind(library_items)
        .bind(source_items)
        .bind(matches)
        .bind(links_created)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record detailed scan lifecycle metrics.
    pub async fn record_scan_run(&self, run: &ScanRunRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO scan_runs (
                dry_run,
                library_filter,
                run_token,
                search_missing,
                library_items_found,
                source_items_found,
                matches_found,
                links_created,
                links_updated,
                dead_marked,
                links_removed,
                links_skipped,
                ambiguous_skipped,
                skip_reason_json,
                runtime_checks_ms,
                library_scan_ms,
                source_inventory_ms,
                matching_ms,
                title_enrichment_ms,
                linking_ms,
                plex_refresh_ms,
                plex_refresh_requested_paths,
                plex_refresh_unique_paths,
                plex_refresh_planned_batches,
                plex_refresh_coalesced_batches,
                plex_refresh_coalesced_paths,
                plex_refresh_refreshed_batches,
                plex_refresh_refreshed_paths_covered,
                plex_refresh_skipped_batches,
                plex_refresh_unresolved_paths,
                plex_refresh_capped_batches,
                plex_refresh_aborted_due_to_cap,
                plex_refresh_failed_batches,
                media_server_refresh_json,
                dead_link_sweep_ms,
                cache_hit_ratio,
                candidate_slots,
                scored_candidates,
                exact_id_hits,
                auto_acquire_requests,
                auto_acquire_missing_requests,
                auto_acquire_cutoff_requests,
                auto_acquire_dry_run_hits,
                auto_acquire_submitted,
                auto_acquire_no_result,
                auto_acquire_blocked,
                auto_acquire_failed,
                auto_acquire_completed_linked,
                auto_acquire_completed_unlinked
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(if run.dry_run { 1 } else { 0 })
        .bind(run.library_filter.as_deref())
        .bind(run.run_token.as_deref())
        .bind(if run.search_missing { 1 } else { 0 })
        .bind(run.library_items_found)
        .bind(run.source_items_found)
        .bind(run.matches_found)
        .bind(run.links_created)
        .bind(run.links_updated)
        .bind(run.dead_marked)
        .bind(run.links_removed)
        .bind(run.links_skipped)
        .bind(run.ambiguous_skipped)
        .bind(run.skip_reason_json.as_deref())
        .bind(run.runtime_checks_ms)
        .bind(run.library_scan_ms)
        .bind(run.source_inventory_ms)
        .bind(run.matching_ms)
        .bind(run.title_enrichment_ms)
        .bind(run.linking_ms)
        .bind(run.plex_refresh_ms)
        .bind(run.plex_refresh_requested_paths)
        .bind(run.plex_refresh_unique_paths)
        .bind(run.plex_refresh_planned_batches)
        .bind(run.plex_refresh_coalesced_batches)
        .bind(run.plex_refresh_coalesced_paths)
        .bind(run.plex_refresh_refreshed_batches)
        .bind(run.plex_refresh_refreshed_paths_covered)
        .bind(run.plex_refresh_skipped_batches)
        .bind(run.plex_refresh_unresolved_paths)
        .bind(run.plex_refresh_capped_batches)
        .bind(if run.plex_refresh_aborted_due_to_cap {
            1
        } else {
            0
        })
        .bind(run.plex_refresh_failed_batches)
        .bind(run.media_server_refresh_json.as_deref())
        .bind(run.dead_link_sweep_ms)
        .bind(run.cache_hit_ratio)
        .bind(run.candidate_slots)
        .bind(run.scored_candidates)
        .bind(run.exact_id_hits)
        .bind(run.auto_acquire_requests)
        .bind(run.auto_acquire_missing_requests)
        .bind(run.auto_acquire_cutoff_requests)
        .bind(run.auto_acquire_dry_run_hits)
        .bind(run.auto_acquire_submitted)
        .bind(run.auto_acquire_no_result)
        .bind(run.auto_acquire_blocked)
        .bind(run.auto_acquire_failed)
        .bind(run.auto_acquire_completed_linked)
        .bind(run.auto_acquire_completed_unlinked)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_link_event(&self, event: &LinkEventRecord) -> Result<()> {
        let target_path = path_to_db_text(&event.target_path)?;
        let source_path = event
            .source_path
            .as_ref()
            .map(|p| path_to_db_text(p))
            .transpose()?;

        sqlx::query(
            "INSERT INTO link_events (run_id, run_token, action, target_path, source_path, media_id, note)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.run_id)
        .bind(event.run_token.as_deref())
        .bind(&event.action)
        .bind(target_path)
        .bind(source_path)
        .bind(event.media_id.as_deref())
        .bind(event.note.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_link_event_fields(
        &self,
        action: &str,
        target_path: &Path,
        source_path: Option<&Path>,
        media_id: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        self.record_link_event_fields_with_run_token(
            None,
            action,
            target_path,
            source_path,
            media_id,
            note,
        )
        .await
    }

    pub async fn record_link_event_fields_with_run_token(
        &self,
        run_token: Option<&str>,
        action: &str,
        target_path: &Path,
        source_path: Option<&Path>,
        media_id: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        self.record_link_event(&LinkEventRecord {
            action: action.to_string(),
            target_path: target_path.to_path_buf(),
            source_path: source_path.map(|p| p.to_path_buf()),
            media_id: media_id.map(|m| m.to_string()),
            note: note.map(|n| n.to_string()),
            run_id: None,
            run_token: run_token.map(|token| token.to_string()),
        })
        .await
    }

    /// Aggregate statistics for the web dashboard.
    #[allow(dead_code)]
    pub async fn get_web_stats(&self) -> Result<WebStats> {
        let (active, dead, _total) = self.get_stats().await?;
        let scan_count: i64 = sqlx::query("SELECT COUNT(*) as cnt FROM scan_runs")
            .fetch_one(&self.pool)
            .await?
            .get("cnt");
        let last_scan: Option<String> =
            sqlx::query("SELECT run_at FROM scan_runs ORDER BY run_at DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await?
                .map(|row| row.get("run_at"));
        Ok(WebStats {
            active_links: active,
            dead_links: dead,
            total_scans: scan_count,
            last_scan,
        })
    }

    /// Return recent scan runs in reverse chronological order.
    #[allow(dead_code)]
    pub async fn get_scan_history(&self, limit: i64) -> Result<Vec<ScanHistoryRecord>> {
        let rows = sqlx::query(
            "SELECT id, run_at, dry_run, library_filter, run_token, search_missing, library_items_found, source_items_found,
                    matches_found, links_created, links_updated, dead_marked,
                    links_removed, links_skipped, ambiguous_skipped, skip_reason_json,
                    runtime_checks_ms, library_scan_ms, source_inventory_ms,
                    matching_ms, title_enrichment_ms, linking_ms, plex_refresh_ms,
                    plex_refresh_requested_paths, plex_refresh_unique_paths,
                    plex_refresh_planned_batches, plex_refresh_coalesced_batches,
                    plex_refresh_coalesced_paths, plex_refresh_refreshed_batches,
                    plex_refresh_refreshed_paths_covered, plex_refresh_skipped_batches,
                    plex_refresh_unresolved_paths, plex_refresh_capped_batches,
                    plex_refresh_aborted_due_to_cap,
                    plex_refresh_failed_batches,
                    media_server_refresh_json,
                    dead_link_sweep_ms, cache_hit_ratio, candidate_slots,
                    scored_candidates, exact_id_hits, auto_acquire_requests,
                    auto_acquire_missing_requests, auto_acquire_cutoff_requests,
                    auto_acquire_dry_run_hits, auto_acquire_submitted,
                    auto_acquire_no_result, auto_acquire_blocked, auto_acquire_failed,
                    auto_acquire_completed_linked, auto_acquire_completed_unlinked
             FROM scan_runs ORDER BY run_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(self.row_to_scan_history_record(&row));
        }
        Ok(records)
    }

    pub async fn get_scan_run(&self, id: i64) -> Result<Option<ScanHistoryRecord>> {
        let row = sqlx::query(
            "SELECT id, run_at, dry_run, library_filter, run_token, search_missing, library_items_found, source_items_found,
                    matches_found, links_created, links_updated, dead_marked,
                    links_removed, links_skipped, ambiguous_skipped, skip_reason_json,
                    runtime_checks_ms, library_scan_ms, source_inventory_ms,
                    matching_ms, title_enrichment_ms, linking_ms, plex_refresh_ms,
                    plex_refresh_requested_paths, plex_refresh_unique_paths,
                    plex_refresh_planned_batches, plex_refresh_coalesced_batches,
                    plex_refresh_coalesced_paths, plex_refresh_refreshed_batches,
                    plex_refresh_refreshed_paths_covered, plex_refresh_skipped_batches,
                    plex_refresh_unresolved_paths, plex_refresh_capped_batches,
                    plex_refresh_aborted_due_to_cap,
                    plex_refresh_failed_batches,
                    media_server_refresh_json,
                    dead_link_sweep_ms, cache_hit_ratio, candidate_slots,
                    scored_candidates, exact_id_hits, auto_acquire_requests,
                    auto_acquire_missing_requests, auto_acquire_cutoff_requests,
                    auto_acquire_dry_run_hits, auto_acquire_submitted,
                    auto_acquire_no_result, auto_acquire_blocked, auto_acquire_failed,
                    auto_acquire_completed_linked, auto_acquire_completed_unlinked
             FROM scan_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| self.row_to_scan_history_record(&row)))
    }

    pub async fn get_skip_link_events_for_run_token(
        &self,
        run_token: &str,
        limit: i64,
    ) -> Result<Vec<LinkEventHistoryRecord>> {
        let rows = sqlx::query(
            "SELECT event_at, action, target_path, source_path, media_id, note
             FROM link_events
             WHERE run_token = ?
               AND action IN ('skipped', 'dead_skipped', 'dead_marked')
             ORDER BY event_at DESC, id DESC
             LIMIT ?",
        )
        .bind(run_token)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(LinkEventHistoryRecord {
                    event_at: row.get("event_at"),
                    action: row.get("action"),
                    target_path: PathBuf::from(row.get::<String, _>("target_path")),
                    source_path: row
                        .get::<Option<String>, _>("source_path")
                        .map(PathBuf::from),
                    media_id: row.get("media_id"),
                    note: row.get("note"),
                })
            })
            .collect()
    }

    fn row_to_scan_history_record(&self, row: &sqlx::sqlite::SqliteRow) -> ScanHistoryRecord {
        ScanHistoryRecord {
            id: row.get("id"),
            started_at: row.get("run_at"),
            dry_run: row.get::<i64, _>("dry_run") != 0,
            library_filter: row.get("library_filter"),
            run_token: row.get("run_token"),
            search_missing: row.get::<i64, _>("search_missing") != 0,
            library_items_found: row.get("library_items_found"),
            source_items_found: row.get("source_items_found"),
            matches_found: row.get("matches_found"),
            links_created: row.get("links_created"),
            links_updated: row.get("links_updated"),
            dead_marked: row.get("dead_marked"),
            links_removed: row.get("links_removed"),
            links_skipped: row.get("links_skipped"),
            ambiguous_skipped: row.get("ambiguous_skipped"),
            skip_reason_json: row.get("skip_reason_json"),
            runtime_checks_ms: row.get("runtime_checks_ms"),
            library_scan_ms: row.get("library_scan_ms"),
            source_inventory_ms: row.get("source_inventory_ms"),
            matching_ms: row.get("matching_ms"),
            title_enrichment_ms: row.get("title_enrichment_ms"),
            linking_ms: row.get("linking_ms"),
            plex_refresh_ms: row.get("plex_refresh_ms"),
            plex_refresh_requested_paths: row.get("plex_refresh_requested_paths"),
            plex_refresh_unique_paths: row.get("plex_refresh_unique_paths"),
            plex_refresh_planned_batches: row.get("plex_refresh_planned_batches"),
            plex_refresh_coalesced_batches: row.get("plex_refresh_coalesced_batches"),
            plex_refresh_coalesced_paths: row.get("plex_refresh_coalesced_paths"),
            plex_refresh_refreshed_batches: row.get("plex_refresh_refreshed_batches"),
            plex_refresh_refreshed_paths_covered: row.get("plex_refresh_refreshed_paths_covered"),
            plex_refresh_skipped_batches: row.get("plex_refresh_skipped_batches"),
            plex_refresh_unresolved_paths: row.get("plex_refresh_unresolved_paths"),
            plex_refresh_capped_batches: row.get("plex_refresh_capped_batches"),
            plex_refresh_aborted_due_to_cap: row.get::<i64, _>("plex_refresh_aborted_due_to_cap")
                != 0,
            plex_refresh_failed_batches: row.get("plex_refresh_failed_batches"),
            media_server_refresh_json: row.get("media_server_refresh_json"),
            dead_link_sweep_ms: row.get("dead_link_sweep_ms"),
            cache_hit_ratio: row.get("cache_hit_ratio"),
            candidate_slots: row.get("candidate_slots"),
            scored_candidates: row.get("scored_candidates"),
            exact_id_hits: row.get("exact_id_hits"),
            auto_acquire_requests: row.get("auto_acquire_requests"),
            auto_acquire_missing_requests: row.get("auto_acquire_missing_requests"),
            auto_acquire_cutoff_requests: row.get("auto_acquire_cutoff_requests"),
            auto_acquire_dry_run_hits: row.get("auto_acquire_dry_run_hits"),
            auto_acquire_submitted: row.get("auto_acquire_submitted"),
            auto_acquire_no_result: row.get("auto_acquire_no_result"),
            auto_acquire_blocked: row.get("auto_acquire_blocked"),
            auto_acquire_failed: row.get("auto_acquire_failed"),
            auto_acquire_completed_linked: row.get("auto_acquire_completed_linked"),
            auto_acquire_completed_unlinked: row.get("auto_acquire_completed_unlinked"),
        }
    }
}
