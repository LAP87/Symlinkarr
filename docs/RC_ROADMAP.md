# Symlinkarr RC Roadmap

This roadmap reflects the current state of the `codex/anime-duplicate-audit` branch and the
latest live findings from Plex, Symlinkarr, and the anime remediation work.

## Current State

- anime duplicate reporting can now export the full backlog with `--full-anime-duplicates`
- live anime audit currently shows:
  - `582` mixed filesystem root groups
  - `373` Plex duplicate show groups
  - `371` Hama AniDB/TVDB split groups
  - `106` correlated filesystem+Plex groups
- cleanup can quarantine legacy anime roots as `foreign` instead of deleting them
- Plex refresh pacing and batch caps are now configurable
- remediation exports now include live/deleted Plex row counts and exact Plex GUIDs

## Top Priorities

### 1. Guarded Remediation Workflow

Build a first-class operator workflow for the `106` correlated anime groups.

Why it matters:

- audit/report value is capped until there is a safe path from diagnosis to action
- this is the core trust-building step toward `1.0 RC`

What it should include:

- dry-run and apply modes
- explicit mount-health gating before file operations
- rollback or quarantine-first behavior by default
- per-title action summaries before apply

### 2. Plex Overload Detection and Throttling

Plex instability is still a real RC blocker.

Why it matters:

- autonomous remediation is not acceptable if Plex can die under refresh pressure
- scan and remediation need safe defaults for real homelab installs

What it should include:

- observable refresh telemetry in persisted scan history
- overload-aware refresh throttling or kill-switch behavior
- clearer operator warnings when refresh load is capped or skipped

### 3. Mount and Runtime Safety Parity

Continue making destructive and semi-destructive paths obey the same runtime safety rules.

Why it matters:

- Real-Debrid users depend on remote mounts and flaky mounts are a known foot-gun
- RC requires consistent safety behavior across CLI, web, cleanup, repair, and remediation

What it should include:

- no mutation when mounts are unhealthy
- same safety posture in CLI and web/API
- no success-shaped no-ops for blocked operations

### 4. API Schema and Operator Docs

The web/API surface is growing faster than the hand-maintained docs.

Why it matters:

- a broader user base needs clear API/CLI behavior
- automation and dashboards depend on stable schema documentation

What it should include:

- refresh `docs/API_SCHEMA.md` for current background-job behavior
- document report/export additions and remediation-oriented fields
- keep README and CLI manual aligned with real command/config surface

### 5. Anime Remediation UX

Expose the correlated backlog in a more operationally useful way.

Why it matters:

- `106` correlated groups is now actionable, but still too manual
- a user should not need ad hoc JSON parsing to decide what to fix next

What it should include:

- filtered exports or API views for correlated groups only
- visibility into `all live` vs `partially stale` vs `metadata ghost` groups
- clearer grouping around legacy root, tagged root, and Plex GUID split

## Safe To Keep Shipping Tonight

- live scans and reports with full logs and before/after snapshots
- documentation and API schema refreshes
- non-destructive export and telemetry improvements
- remediation planning and preview improvements

## Do Not Ship Yet

- automatic permanent deletion of duplicate groups
- blind remediation of all correlated anime groups without an operator gate
- broader non-anime duplicate cleanup based on the anime heuristics
- `1.0 RC` until the guarded remediation workflow and Plex stability story are stronger
