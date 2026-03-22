# Symlinkarr Web API Schema

This is the current hand-maintained schema for the built-in web API.

Base path:

```text
/api/v1
```

The API is currently local-first and intended to back the bundled web UI. There is no auth layer in front of these routes yet, so treat it as trusted-local-network tooling rather than a public internet API.

By default, the web server binds to `127.0.0.1`. Set `web.bind_address: "0.0.0.0"` only when you intentionally want external reachability, such as a container with explicit port publishing. Cross-origin access is not enabled by default.

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

## `POST /api/v1/scan`

Triggers the scan pipeline.

Request body:

```json
{
  "dry_run": true,
  "library": "Anime"
}
```

Response:

```json
{
  "success": true,
  "message": "Scan complete: 3 created, 1 updated, 17 skipped",
  "created": 3,
  "updated": 1,
  "skipped": 17
}
```

## `GET /api/v1/scan/jobs`

Returns the most recent scan jobs in compact form.

Response element schema:

```json
{
  "id": 42,
  "started_at": "2026-03-21 21:11:00",
  "dry_run": true,
  "library_items_found": 3906,
  "source_items_found": 101542,
  "matches_found": 9924,
  "links_created": 446,
  "links_updated": 164,
  "dead_marked": 15
}
```

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

## `POST /api/v1/repair/auto`

Runs the web API repair action.

Response schema:

```json
{
  "success": true,
  "message": "Repair completed",
  "repaired": 0,
  "failed": 0
}
```

Note: this endpoint is currently a thin placeholder compared to the richer CLI repair paths.

## `POST /api/v1/cleanup/audit`

Runs a cleanup audit and writes a report.

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
  "message": "Audit complete",
  "report_path": "/path/to/report.json",
  "total_findings": 123,
  "critical": 10,
  "high": 100,
  "warning": 13
}
```

Notes:

- `scope` currently supports `anime`, `tv`, `movie`, and `all`.

## `POST /api/v1/cleanup/prune`

Applies prune against a previously generated report.

Request body:

```json
{
  "report_path": "/path/to/report.json",
  "token": "confirmation-token"
}
```

Response schema:

```json
{
  "success": true,
  "message": "Prune completed successfully",
  "removed": 17,
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
