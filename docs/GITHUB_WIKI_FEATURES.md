# Symlinkarr Features Explained

This page is written as a wiki-ready explainer for what Symlinkarr does and why each feature exists.

## What Symlinkarr Is

Symlinkarr is a local-first daemon and operator tool that keeps a Real-Debrid-backed media library usable without hand-sorting files.

In plain language:

- your source mount is the pile of downloaded files
- your library folders are the clean shelves Plex, Emby, Jellyfin, Sonarr, or Radarr expect
- Symlinkarr keeps the shelves tidy by creating and maintaining symlinks instead of moving the real files around

## Current RC Closeout

Symlinkarr is in `RC-hardening`, not broad feature expansion.

Remaining pre-RC work is now mostly operator proof, not new features:

- cut the RC commit/tag/release intentionally from a clean worktree
- keep the known anime-specials and legacy-anime remediation limits explicit in shipped docs/help

The live checklist is tracked in [RC_ROADMAP.md](RC_ROADMAP.md).

## Core Features

### `scan`

In plain English:
Look at what you have, look at what your media apps expect, and connect the dots.

What it does:

- scans configured library roots
- scans configured source roots
- matches likely source files to library items
- creates or updates symlinks
- optionally builds missing-content acquisition requests

### `repair`

In plain English:
If a shortcut points to nowhere, try to find the right file and reattach it.

What it does:

- detects dead symlinks
- searches the source catalog for the best replacement
- relinks the dead entry when there is a safe match
- leaves clear failure results when it cannot repair automatically

### `cleanup`

In plain English:
Find old junk first, then delete it only when you explicitly approve it.

What it does:

- lists dead links
- builds cleanup audit reports
- previews prune candidates
- requires confirmation tokens before destructive prune actions
- keeps quarantine/rollback-friendly flows for risky cleanup work

### `discover`

In plain English:
Show me which RD-backed files Symlinkarr would place into which tagged folders.

What it does:

- compares RD-backed source files against tagged library folders
- previews concrete source-to-target placements, including season paths
- keeps the web/UI side read-only until unattended placement is trustworthy

### `queue`

In plain English:
Keep track of “stuff Symlinkarr meant to do later.”

What it does:

- lists persistent auto-acquire jobs
- shows blocked, failed, no-result, and completed-unlinked states
- lets you retry classes of stuck work safely

### `backup`

In plain English:
Make a save point before something goes wrong.

What it does:

- snapshots Symlinkarr database/state
- lets you inspect available backups
- restores previous state when needed

### `cache`

In plain English:
Build the cheat sheet Symlinkarr uses, and clear the specific sticky notes that turned out to be wrong.

What it does:

- builds or refreshes RD torrent/file-info cache entries
- reports RD cache coverage and health
- invalidates one sticky metadata entry, a whole metadata family prefix, or the anime-lists cache when TMDB/TVDB/anime-lists data looks stale
- can clear all sticky metadata so later lookups repopulate it from upstream APIs

Why the metadata cache is intentionally sticky:

- TMDB/TVDB data changes far less often than API quotas or timeouts hurt you
- targeted refresh is usually better than expiring everything on a timer
- sticky cache keeps anime matching and scan speed more predictable

### `doctor`

In plain English:
Check whether the room is safe before you start moving furniture.

What it does:

- runs preflight health checks
- verifies DB schema/version and writable path expectations
- catches config mistakes
- catches mount/runtime issues before destructive work

### `report`

In plain English:
Export a readable summary for operator review instead of forcing people to dig through logs.

What it does:

- generates structured reports from current Symlinkarr state
- includes anime remediation-oriented reporting
- helps explain what changed, what is blocked, and what still needs operator action

### `daemon`

In plain English:
Do the normal maintenance on a schedule so you do not have to babysit it.

What it does:

- runs scan cycles repeatedly
- can also host the built-in web UI when configured

### `web`

In plain English:
Give me a dashboard and buttons instead of making me remember every CLI incantation.

What it does:

- serves the built-in operator UI
- serves the JSON API used by the UI and automation clients
- exposes status/history/reporting surfaces

## Supporting Features

### Media-server refresh

In plain English:
After Symlinkarr changes links, tell Plex/Emby/Jellyfin what changed so they do not stay stale.

What it does:

- sends targeted invalidation/refresh requests
- coalesces noisy refresh storms
- serializes concurrent refreshes so multiple runs do not stampede your media server

### Background jobs

In plain English:
Let the slow work keep going without forcing the browser or API request to wait around.

What it does:

- runs scan, repair, and cleanup-audit jobs in the background
- exposes current job state and last outcome in the UI/API

### Anime remediation

In plain English:
Find legacy anime folder/link messes and turn them into a reviewable fix plan.

What it does:

- compares Plex DB/library state with current symlink structure
- groups likely duplicate/legacy cases
- generates preview/apply reports instead of making silent changes

Known limit:

- anime specials without usable anime-lists numbering hints may still need manual search terms, because many indexers are weak at `S00Exx`-style anime queries

## Security Modes

### `local-only`

In plain English:
Only this machine should talk to it, so convenience wins.

- loopback bind
- trusted local operator mode
- no built-in auth required by default

### `remote operator`

In plain English:
If the UI is reachable over the network, treat it like a real control panel.

- remote bind
- explicit Basic auth for the built-in UI
- browser session and CSRF protections for remote HTML mutations

### `scripted operator`

In plain English:
Bots and scripts can get their own key instead of pretending to be a browser.

- API key for automation clients
- complements operator login, not a replacement for remote UI auth

## Operational Concepts

### `VACUUM`

In plain English:
Repack the SQLite database so it stops carrying empty holes around.

Good:

- smaller DB file
- less fragmentation

Bad:

- rewrites the whole DB
- can block writes while it runs
- better as a scheduled maintenance task than as surprise daytime work

Daemon note:

- keep it disabled by default
- if you enable it, schedule it for a quiet local hour
- scheduled vacuum now uses a dedicated maintenance connection instead of borrowing a normal app-pool slot for the full run

### `SourceConfig.media_type`

In plain English:
Tell Symlinkarr how to interpret a source root before it tries to parse filenames there.

Allowed values:

- `auto`
- `anime`
- `tv`
- `movie`

Why keep it as a string in config:

- easy to read and edit
- friendly for humans

Why validate it strictly:

- typos should fail fast instead of silently acting like `auto`

## Scope Reminder

Symlinkarr is not trying to be a downloader-orchestrator that replaces everything else in your stack.

For `v1.0`, the center of the product is:

- scan
- link
- repair
- cleanup
- backup/restore
- operator UI/API

Everything else should support that core, not redefine it.
