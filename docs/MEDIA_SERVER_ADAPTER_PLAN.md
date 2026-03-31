# Media Server Adapter Plan

This document captures the next adapter steps after the first `media_servers` rollout.

## Current State

- `src/media_servers/plex.rs` is the live Plex invalidation adapter.
- `src/media_servers/emby.rs` and `src/media_servers/jellyfin.rs` now implement the first path-based invalidation pass via `POST /Library/Media/Updated`.
- `src/media_servers/plex_db.rs` owns Plex-only database inspection for reporting and anime remediation.
- `src/media_servers/mod.rs` now owns:
  - invalidation telemetry
  - mutation-scoped library-root selection
  - media-server probing for status
  - active backend selection for post-mutation invalidation

Today:

- Plex, Emby, and Jellyfin can each be configured as the one active refresh backend.
- Symlinkarr still fails closed if multiple refresh backends are enabled together.
- Plex remains the only backend with DB/report/remediation-specific code.
- Emby and Jellyfin are path-invalidation adapters only for now.

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

## Remaining Rollout

### Phase 1

Operator verification against real servers:

- confirm authenticated `POST /Library/Media/Updated` works on the deployed Emby/Jellyfin versions
- tune `refresh_batch_size`, delay, and cap defaults against real library load
- decide whether either backend needs a compatibility fallback to `POST /Library/Refresh`

### Phase 2

Only after invalidation is stable, decide whether Emby/Jellyfin need their own:

- DB compare adapters
- duplicate-show correlation
- remediation/reporting helpers

Those should live beside `plex_db.rs`, not back at repo root.

### Phase 3

If fan-out ever becomes desirable, design it deliberately. Do not silently refresh multiple servers from one mutation event until:

- telemetry stays honest per backend
- cap guards are enforced per backend
- CLI/web/API surfaces can report partial success cleanly

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
