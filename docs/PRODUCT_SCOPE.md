# Symlinkarr Product Scope

## Purpose

Symlinkarr is a local-first symlink manager for Real-Debrid-backed media libraries.

Its job is to keep an existing library deterministic and repairable:

- scan source mounts and tagged library folders
- create and update stable symlinks
- detect dead, stale, or misplaced links
- repair broken links when a safe replacement exists
- support backup and restore of Symlinkarr-managed state
- optionally notify Plex, Emby, or Jellyfin after mutations

The product is not trying to become a full media manager, downloader, or orchestration platform.

## v1.0 Contract

For `v1.0 RC` and `v1.0`, the stable core is:

- scan
- match
- link
- repair
- cleanup audit / prune
- backup / restore
- status / health / observability
- optional web UI and JSON API for operating those flows

## Security Model

Symlinkarr is designed for local-first operation.

- Loopback-only operation may expose read-only UI/API behavior without strict auth.
- Loopback-only operation is a trusted mode and may expose mutating UI/API behavior without built-in auth.
- Any remote exposure must be treated as operator access.
- Mutating operations must remain guarded.
- Browser-driven mutations should use same-origin session and CSRF gates when the built-in UI is remotely exposed.
- The built-in HTML UI is an operator surface, not a public dashboard.

Practical rule:

- local-only: convenience-first is acceptable
- remote-enabled: require real operator auth and explicit safety gates

## Integrations

These integrations are in scope when they help the core workflow:

- TMDB / TVDB for metadata-assisted matching
- Sonarr / Radarr / Bazarr / Prowlarr / Tautulli where they improve diagnosis or operator workflows
- Plex / Emby / Jellyfin invalidation after Symlinkarr mutations
- Real-Debrid-backed mounts such as Zurg or Decypharr

These integrations are supporting features, not the product definition.

## Out of Scope For v1.0

The following should not define the product and should not block release unless they directly affect core safety:

- turning Symlinkarr into a general-purpose acquisition manager
- broad automatic remediation of whole legacy backlogs
- media-server-specific deep compare engines as a release requirement
- event-driven watcher mode as the default runtime model
- highly polished dashboards or cosmetic UI work

## Design Bias

Prefer:

- safe previews before destructive actions
- deterministic behavior over aggressive automation
- explicit operator feedback over silent fallback behavior
- local-first ergonomics over multi-tenant or internet-facing assumptions

Avoid:

- adding new automation surfaces without matching observability and rollback semantics
- coupling release readiness to anime-only remediation expansion
- treating every adjacent media-ops problem as Symlinkarr scope

## Current Position

Today Symlinkarr is best described as:

`a local-first symlink daemon and repair/cleanup tool with an operator UI`

That is the product line the repo should optimize around unless explicitly re-scoped.
