# Media Server Adapter Plan

This document captures the next adapter steps after the `media_servers` refactor.

## Current State

- `src/media_servers/plex.rs` is the live invalidation adapter.
- `src/media_servers/plex_db.rs` owns Plex-only database inspection for reporting and anime remediation.
- `src/media_servers/mod.rs` now owns:
  - invalidation telemetry
  - mutation-scoped library-root selection
  - primary media-server probing for status
  - the adapter boundary for future Emby and Jellyfin support

Today, only Plex is active. Emby and Jellyfin are intentionally scaffolded but not wired into config or runtime selection yet.

## Verified Upstream APIs

### Emby

Official REST reference confirms:

- `POST /Library/Refresh`
  - starts a library scan
  - requires administrator authentication
- `POST /Library/Media/Updated`
  - reports externally changed media
  - body carries update paths (`PostUpdatedMedia`)

Source references:

- `https://dev.emby.media/reference/RestAPI/LibraryService/postLibraryRefresh.html`
- `https://dev.emby.media/reference/RestAPI/LibraryService/postLibraryMediaUpdated.html`

These make Emby a good fit for the same two-layer strategy used in Symlinkarr today:

1. targeted mutation reporting when we know the changed paths
2. guarded library refresh fallback when we need a wider rescan

### Jellyfin

Official OpenAPI confirms:

- `POST /Library/Refresh`
  - starts a library scan
  - requires elevation
- `POST /Library/Media/Updated`
  - accepts external media updates
  - request body: `MediaUpdateInfoDto`
- `POST /Items/{itemId}/Refresh`
  - refreshes metadata for one item when a server item id is known

Source reference:

- `https://api.jellyfin.org/openapi/jellyfin-openapi-stable.json`

This gives Jellyfin the same basic path as Emby, with one extra future option:

- if Symlinkarr later stores Jellyfin item ids, item-level refresh becomes possible

## Recommended Rollout

### Phase 1

Add explicit config sections for `emby` and `jellyfin`, mirroring the current refresh-oriented Plex shape:

- `url`
- `api_key`
- `refresh_enabled`
- pacing / cap fields aligned with the current Plex guardrails

Do not auto-enable these just because software is installed locally.

### Phase 2

Teach `media_servers::configured_invalidation_server()` to select exactly one active backend:

- Plex
- Emby
- Jellyfin

If multiple are enabled at once, fail closed until fan-out is intentionally designed.

### Phase 3

Implement targeted invalidation adapters:

- Emby:
  - prefer `POST /Library/Media/Updated` for changed paths
  - fall back to `POST /Library/Refresh` when targeted reporting is not sufficient
- Jellyfin:
  - prefer `POST /Library/Media/Updated`
  - optionally use `POST /Items/{itemId}/Refresh` once item-id mapping exists
  - fall back to `POST /Library/Refresh`

### Phase 4

Only after invalidation is stable, decide whether Emby/Jellyfin need their own:

- DB compare adapters
- duplicate-show correlation
- remediation/reporting helpers

Those should live beside `plex_db.rs`, not back at repo root.

## Safety Rules

- Keep the current cap-guard model. Adapter parity matters more than feature count.
- Mutation follow-ups must stay honest in CLI, web, and JSON API.
- No success-shaped no-ops: if a backend is unconfigured, report that clearly.
- Mount/runtime health gates remain mandatory before destructive operations.
- Changed-root scoping remains the default. Avoid refreshing every selected library when a smaller invalidation set is known.

## Open Questions

- Should Symlinkarr support one active media-server backend at a time, or deliberate fan-out to multiple backends?
- Do we want item-id persistence for Jellyfin/Emby later, or is path-based invalidation enough for `1.0`?
- Should future Emby/Jellyfin health checks expose section/library counts in the same `status --health` shape as Plex?
