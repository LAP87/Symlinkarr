# Symlinkarr Web API Schema

This is the current hand-maintained schema for the built-in web API.

Base path:

```text
/api/v1
```

The API is currently local-first and intended to back the bundled web UI. There is still no full user auth layer in front of these routes, so treat it as trusted-local-network tooling rather than a public internet API.

By default, the web server binds to `127.0.0.1`. Set `web.bind_address: "0.0.0.0"` only when you intentionally want external reachability, such as a container with explicit port publishing, and pair it with `web.allow_remote: true`. Cross-origin access is not enabled by default.

For mutating browser requests, Symlinkarr now enforces two layers:

- same-origin `Origin`/`Referer` validation for browser-style `POST` requests
- an issued host-only browser session cookie (`SameSite=Strict`) that is set by same-origin `GET` responses and required on later browser mutations

Non-browser local clients that do not send `Origin` or `Referer` headers are still allowed to call mutating endpoints without that browser session cookie.

## Conventions

- Responses are JSON.
- Error responses from detail endpoints follow:

```json
{ "error": "message" }
```

- `mode` query values for scan history:
  - `any`
  - `dry`
  - `live`
- `search_missing` query values for scan history:
  - `any`
  - `only`
  - `exclude`

## `GET /api/v1/status`

Returns top-level library statistics.

Response:

```json
{
  "active_links": 0,
  "dead_links": 0,
  "total_scans": 0,
  "last_scan": "2026-03-21 21:11:00"
}
```

## `GET /api/v1/health`

Returns config/health presence flags for the core integrations.

Response:

```json
{
  "database": "healthy",
  "tmdb": "configured",
  "tvdb": "configured",
  "realdebrid": "configured"
}
```

## `GET /api/v1/discover`

Returns read-only discovery results from the RD cache, scoped to all libraries or a single library.

Query params:

- `library=<LIBRARY_NAME>` optional
- `refresh_cache=true|false` optional, defaults to `false` for cached-only discover

Status codes:

- `200 OK` on success
- `400 Bad Request` for invalid library filters
- `500 Internal Server Error` if discovery fails

Response:

```json
{
  "items": [
    {
      "rd_torrent_id": "rd-1",
      "torrent_name": "Missing.Show.S01E01.1080p.WEB-DL.mkv",
      "status": "downloaded",
      "size": 1073741824,
      "parsed_title": "Missing Show"
    }
  ],
  "status_message": "Real-Debrid API key not configured. Showing cached results only."
}
```

Notes:

- Browser/UI discover defaults to cached-only mode for lower latency and fewer inline surprises.
- Set `refresh_cache=true` only when you explicitly want a live RD cache sync before gap detection.

## `POST /api/v1/scan`

Starts the scan pipeline in the background.

Status codes:

- `202 Accepted` when the scan was accepted and is now running in the background
- `409 Conflict` when another web/API-triggered scan is already running

Request body:

```json
{
  "dry_run": true,
  "library": "Anime",
  "search_missing": false
}
```

Response:

```json
{
  "success": true,
  "message": "Scan started in background for Anime. Poll /api/v1/scan/jobs or /api/v1/scan/history for completion.",
  "created": 0,
  "updated": 0,
  "skipped": 0,
  "running": true,
  "started_at": "2026-03-29 23:59:00 UTC",
  "scope_label": "Anime",
  "search_missing": false,
  "dry_run": true
}
```

Notes:

- Web/API scan now returns immediately instead of holding the request open for the full run.
- Poll `GET /api/v1/scan/jobs` for the active background scan and `GET /api/v1/scan/history` / `GET /api/v1/scan/:id` for completed runs.

## `GET /api/v1/scan/status`

Returns the current in-memory background scan state plus the latest completed or failed background-scan outcome.

Status codes:

- `200 OK`

Response schema:

```json
{
  "active_job": {
    "id": 0,
    "status": "running",
    "started_at": "2026-03-29 23:59:00 UTC",
    "scope_label": "Anime",
    "search_missing": true,
    "dry_run": false,
    "library_items_found": 0,
    "source_items_found": 0,
    "matches_found": 0,
    "links_created": 0,
    "links_updated": 0,
    "dead_marked": 0
  },
  "last_outcome": {
    "finished_at": "2026-03-29 23:58:00 UTC",
    "scope_label": "Anime",
    "dry_run": false,
    "search_missing": true,
    "success": false,
    "message": "RD cache sync failed"
  }
}
```

Notes:

- `active_job` is `null` when no background scan is currently running.
- `last_outcome` carries the latest background-scan success or failure, including failures that do not produce a durable scan-history row.
- stale failed outcomes are suppressed once a newer durable `scan_runs` entry exists.

## `GET /api/v1/scan/jobs`

Returns the active background scan first when one is running, followed by recent completed scan history in compact form.

Response element schema:

```json
{
  "id": 0,
  "status": "running",
  "started_at": "2026-03-21 21:11:00",
  "scope_label": "Anime",
  "search_missing": true,
  "dry_run": true,
  "library_items_found": 3906,
  "source_items_found": 101542,
  "matches_found": 9924,
  "links_created": 446,
  "links_updated": 164,
  "dead_marked": 15
}
```

Notes:

- `status` is `running` for the synthetic in-memory active row and `completed` for history-backed rows.
- Running rows use `id: 0` until a durable history row exists at completion time.

## `GET /api/v1/scan/history`

Returns filtered scan history for UI tables and dashboards.

Query params:

- `library=<LIBRARY_NAME>`
- `mode=any|dry|live`
- `search_missing=any|only|exclude`
- `limit=<1..200>`

Example:

```text
/api/v1/scan/history?library=Anime&mode=dry&search_missing=only&limit=25
```

Response element schema:

```json
{
  "id": 42,
  "started_at": "2026-03-21 21:11:00",
  "scope_label": "Anime",
  "dry_run": true,
  "search_missing": true,
  "total_runtime_ms": 288200,
  "matches_found": 9924,
  "links_created": 446,
  "links_updated": 164,
  "cache_hit_ratio": 0.94,
  "dead_count": 17,
  "plex_refresh": {
    "runtime_ms": 3100,
    "requested_paths": 12,
    "unique_paths": 10,
    "planned_batches": 5,
    "coalesced_batches": 2,
    "coalesced_paths": 7,
    "refreshed_batches": 4,
    "refreshed_paths_covered": 12,
    "skipped_batches": 1,
    "unresolved_paths": 0,
    "capped_batches": 1,
    "failed_batches": 0
  },
  "auto_acquire": {
    "requests": 10,
    "missing_requests": 5,
    "cutoff_requests": 5,
    "dry_run_hits": 4,
    "submitted": 0,
    "no_result": 2,
    "blocked": 0,
    "failed": 0,
    "completed_linked": 0,
    "completed_unlinked": 0,
    "successes": 4
  }
}
```

Notes:

- `plex_refresh` makes refresh throttling observable in persisted history, including capped or skipped batches that previously only appeared in logs.

## `GET /api/v1/scan/:id`

Returns full detail for a single recorded scan run.

Response schema:

```json
{
  "id": 42,
  "started_at": "2026-03-21 21:11:00",
  "library_filter": "Anime",
  "scope_label": "Anime",
  "dry_run": true,
  "search_missing": true,
  "library_items_found": 3906,
  "source_items_found": 101542,
  "matches_found": 9924,
  "links_created": 446,
  "links_updated": 164,
  "dead_marked": 15,
  "links_removed": 2,
  "links_skipped": 9314,
  "ambiguous_skipped": 70,
  "runtime_checks_ms": 200,
  "library_scan_ms": 12400,
  "source_inventory_ms": 148200,
  "matching_ms": 86700,
  "title_enrichment_ms": 16400,
  "linking_ms": 20500,
  "plex_refresh_ms": 3100,
  "plex_refresh": {
    "runtime_ms": 3100,
    "requested_paths": 12,
    "unique_paths": 10,
    "planned_batches": 5,
    "coalesced_batches": 2,
    "coalesced_paths": 7,
    "refreshed_batches": 4,
    "refreshed_paths_covered": 12,
    "skipped_batches": 1,
    "unresolved_paths": 0,
    "capped_batches": 1,
    "failed_batches": 0
  },
  "dead_link_sweep_ms": 700,
  "total_runtime_ms": 288200,
  "cache_hit_ratio": 0.94,
  "candidate_slots": 77624480,
  "scored_candidates": 3171,
  "exact_id_hits": 0,
  "auto_acquire_requests": 10,
  "auto_acquire_missing_requests": 5,
  "auto_acquire_cutoff_requests": 5,
  "auto_acquire_dry_run_hits": 4,
  "auto_acquire_submitted": 0,
  "auto_acquire_no_result": 2,
  "auto_acquire_blocked": 0,
  "auto_acquire_failed": 0,
  "auto_acquire_completed_linked": 0,
  "auto_acquire_completed_unlinked": 0,
  "auto_acquire_successes": 4
}
```

Not found:

```json
{ "error": "Scan run 9999 not found" }
```

Notes:

- `plex_refresh_ms` remains the phase runtime for compatibility, while the nested `plex_refresh` object exposes request pressure, coalescing, capping, failures, and actual queued coverage.

## `GET /api/v1/report/anime-remediation`

Returns the ranked remediation backlog for correlated anime legacy-root and Plex Hama AniDB/TVDB split groups.

Query params:

- `plex_db=<PATH>` optional override for Plex's library database path
- `full=true|false` optional; when `true`, returns the full backlog instead of the default sample-limited slice

If `plex_db` is omitted, Symlinkarr tries a few common local Plex DB paths first.

Example:

```text
/api/v1/report/anime-remediation?full=true
```

Response schema:

```json
{
  "generated_at": "2026-03-30T19:47:32+02:00",
  "plex_db_path": "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db",
  "full": true,
  "filesystem_mixed_root_groups": 582,
  "plex_duplicate_show_groups": 373,
  "plex_hama_anidb_tvdb_groups": 371,
  "correlated_hama_split_groups": 106,
  "remediation_groups": 106,
  "returned_groups": 106,
  "groups": [
    {
      "normalized_title": "Mobile Suit Gundam SEED",
      "recommended_tagged_root": {
        "path": "/mnt/storage/plex/anime/Mobile Suit Gundam SEED (2002) {tvdb-254931}",
        "filesystem_symlinks": 49,
        "db_active_links": 49
      },
      "alternate_tagged_roots": [],
      "legacy_roots": [
        {
          "path": "/mnt/storage/plex/anime/Mobile Suit Gundam SEED",
          "filesystem_symlinks": 99,
          "db_active_links": 0
        }
      ],
      "plex_total_rows": 2,
      "plex_live_rows": 2,
      "plex_deleted_rows": 0,
      "plex_guid_kinds": ["hama-anidb", "hama-tvdb"],
      "plex_guids": [
        "com.plexapp.agents.hama://anidb-252?lang=en",
        "com.plexapp.agents.hama://tvdb-254931?lang=en"
      ]
    }
  ]
}
```

Error example when no Plex DB path can be resolved:

```json
{ "error": "Plex DB path is required or must exist at a standard local path" }
```

## `POST /api/v1/repair/auto`

Starts the repair flow in the background.

Status codes:

- `202 Accepted` when the repair flow was accepted and is now running in the background
- `409 Conflict` when another scan, cleanup audit, or repair run is already active

Response schema:

```json
{
  "success": true,
  "message": "Repair started in background for All Libraries. Poll /api/v1/repair/status for the finished outcome.",
  "repaired": 0,
  "failed": 0,
  "skipped": 0,
  "stale": 0,
  "running": true,
  "started_at": "2026-03-29 23:59:00 UTC",
  "scope_label": "All Libraries"
}
```

Notes:

- this route now returns immediately instead of holding the request open for the full repair pass
- the background worker still runs the same core repair flow as CLI `repair auto`, without the CLI-only self-heal prompt/output layer

## `GET /api/v1/repair/status`

Returns the current in-memory background repair state plus the latest completed repair outcome.

Status codes:

- `200 OK`

Response schema:

```json
{
  "active_job": {
    "status": "running",
    "started_at": "2026-03-29 23:59:00 UTC",
    "scope_label": "All Libraries"
  },
  "last_outcome": {
    "finished_at": "2026-03-30 00:00:05 UTC",
    "scope_label": "All Libraries",
    "success": true,
    "message": "Repair completed: 1 repaired, 0 unrepairable, 0 skipped, 0 stale record(s).",
    "repaired": 1,
    "failed": 0,
    "skipped": 0,
    "stale": 0
  }
}
```

## `POST /api/v1/cleanup/audit`

Starts a cleanup audit in the background. The finished report is written under the configured backup directory and becomes visible from the web cleanup page.

Status codes:

- `202 Accepted` when the audit was queued successfully
- `409 Conflict` when another scan or cleanup audit is already running
- `400 Bad Request` for invalid scope values

Request body:

```json
{
  "scope": "anime"
}
```

Response schema:

```json
{
  "success": true,
  "message": "Cleanup audit started in background for Anime. Poll /api/v1/cleanup/audit/jobs or inspect /cleanup for the finished report.",
  "report_path": "",
  "total_findings": 0,
  "critical": 0,
  "high": 0,
  "warning": 0,
  "running": true,
  "started_at": "2026-03-29 12:34:56 UTC",
  "scope_label": "Anime",
  "libraries_label": "All Libraries"
}
```

Notes:

- `scope` currently supports `anime`, `tv`, `movie`, and `all`.
- `report_path` stays empty until the background audit has finished and produced a report.
- `report_path` in follow-up prune requests must resolve inside the configured Symlinkarr backup directory.

## `GET /api/v1/cleanup/audit/status`

Returns the current in-memory cleanup-audit state plus the latest completed or failed background-audit outcome.

Status codes:

- `200 OK`

Response schema:

```json
{
  "active_job": {
    "status": "running",
    "started_at": "2026-03-29 12:34:56 UTC",
    "scope_label": "Anime",
    "libraries_label": "All Libraries"
  },
  "last_outcome": {
    "finished_at": "2026-03-29 12:40:00 UTC",
    "scope_label": "Anime",
    "libraries_label": "All Libraries",
    "success": true,
    "message": "Report written to /path/to/report.json",
    "report_path": "/path/to/report.json"
  }
}
```

Notes:

- `active_job` is `null` when no cleanup audit is currently running.
- `last_outcome` carries the latest background cleanup-audit success or failure, including failures that never produced a report file.
- stale failed outcomes are suppressed once a newer durable cleanup report exists on disk.

## `GET /api/v1/cleanup/audit/jobs`

Returns the currently running cleanup audit job, if any.

Status codes:

- `200 OK`

Response schema:

```json
[
  {
    "status": "running",
    "started_at": "2026-03-29 12:34:56 UTC",
    "scope_label": "Anime",
    "libraries_label": "All Libraries"
  }
]
```

## `POST /api/v1/cleanup/prune`

Applies prune against a previously generated report.

Status codes:

- `200 OK` on success
- `400 Bad Request` when prune validation fails, including invalid tokens or bad report input

Request body:

```json
{
  "report_path": "/path/to/report.json",
  "token": "confirmation-token"
}
```

Notes:

- `report_path` must resolve inside the configured Symlinkarr backup directory. Arbitrary filesystem paths are rejected.

Response schema:

```json
{
  "success": true,
  "message": "Prune applied",
  "candidates": 17,
  "managed_candidates": 17,
  "foreign_candidates": 0,
  "removed": 17,
  "quarantined": 0,
  "skipped": 2
}
```

## `GET /api/v1/links`

Returns active links by default, or dead links when `status=dead`.

Query params:

- `limit=<N>` default `100`
- `status=dead`

Response element schema:

```json
{
  "id": 1,
  "source_path": "/mnt/rd/file.mkv",
  "target_path": "/mnt/storage/plex/Show/Season 01/S01E01.mkv",
  "media_id": "tvdb-12345",
  "media_type": "Tv",
  "status": "Active",
  "created_at": "2026-03-21 10:00:00",
  "updated_at": "2026-03-21 11:00:00"
}
```

## `GET /api/v1/config/validate`

Returns config validation status.

Response schema:

```json
{
  "valid": true,
  "errors": [],
  "warnings": []
}
```

## `GET /api/v1/doctor`

Returns a preflight checklist.

Response schema:

```json
{
  "all_passed": true,
  "checks": [
    {
      "check": "Library 'Anime' exists",
      "passed": true,
      "message": "/mnt/storage/plex/anime: exists"
    }
  ]
}
```
