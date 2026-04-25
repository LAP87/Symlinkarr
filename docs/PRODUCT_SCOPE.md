# Symlinkarr Product Scope

## Purpose

Symlinkarr is a symlink manager for Real-Debrid-backed media libraries.

Its job is to keep an existing library clean and repairable:

- scan source mounts and tagged library folders
- create and update stable symlinks
- detect dead, stale, or misplaced links
- repair broken links when a safe replacement exists
- support backup and restore of Symlinkarr-managed state
- optionally tell Plex, Emby, or Jellyfin to refresh after link changes

Symlinkarr is not trying to become a full media manager or downloader.

## v1.0 Scope

For `v1.0`, the stable core is:

- scan
- match
- link
- repair
- cleanup audit / prune
- backup / restore
- status / health
- optional web UI and JSON API for running those tasks

## Security Model

Symlinkarr is designed to run locally by default.

- Loopback-only operation may expose read-only UI/API behavior without strict auth.
- Loopback-only operation is trusted mode and may allow write actions without built-in auth.
- Remote access must use real login protection.
- Write actions must stay protected.
- Browser forms should use a same-origin session and CSRF checks when the built-in UI is exposed remotely.
- The built-in HTML UI is for private admin use, not a public dashboard.

Practical rule:

- local-only: convenience-first is acceptable
- remote-enabled: require login protection and safety checks

## Integrations

These integrations are in scope when they help the core app:

- TMDB / TVDB for metadata-assisted matching
- Sonarr / Radarr / Bazarr / Prowlarr / Tautulli where they help explain or repair library state
- Plex / Emby / Jellyfin refresh after Symlinkarr changes
- Real-Debrid-backed mounts such as Zurg or Decypharr

These integrations are supporting features, not the product definition.

## Out of Scope For v1.0

The following should not define the product and should not block release unless they directly affect core safety:

- turning Symlinkarr into a general-purpose acquisition manager
- broad automatic cleanup of whole legacy backlogs
- media-server-specific deep compare engines as a release requirement
- event-driven watcher mode as the default runtime model
- chasing UI polish beyond what makes the app clear and usable

## Design Bias

Prefer:

- safe previews before destructive actions
- repeatable behavior over aggressive automation
- clear feedback over silent fallback behavior
- simple local use over internet-facing assumptions

Avoid:

- adding new automation without matching safety and rollback paths
- tying release readiness to anime-only cleanup expansion
- treating every adjacent media-ops problem as Symlinkarr scope

## Current Position

Today Symlinkarr is best described as:

`a symlink daemon and repair/cleanup tool with a private web UI`

That is the product line the repo should optimize around unless explicitly re-scoped.
