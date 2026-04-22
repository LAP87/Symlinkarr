# Symlinkarr RC Roadmap

This document reflects the path through `v1.0.0-rc.11` and the transition into post-RC polish.

## Current Position — RC Completed

The RC phase ended at `1.0.0-rc.11`. The codebase now carries:

- dashboard live activity feed and "needs attention" layer
- Tautulli streaming guard across repair, cleanup, and dashboard
- anime override store with validation and operator UX
- completed structural file splits for all major hotspots
- wiki split into focused operator-task pages
- tighter result/transition page guidance
- 700 tests passing, clippy clean, release build verified

## RC Verification Archive

All original RC checklist items were validated by `2026-04-12`:

- daemon scan completed on realistic data after clean Docker restart
- `doctor` and `status --health` passed against the live stack
- `repair scan`, `repair auto --dry-run`, `backup restore --dry-run` run against real mounts
- restore root enforcement fixed for library symlink classification
- scheduled `VACUUM` completed in daemon maintenance window and resumed cleanly
- `cleanup prune` preview exercised against real anime audit with expected blocking
- anime remediation applied safely against live Plex DB (`Angels of Death`, 16 quarantined)
- release Docker paths verified against live `/opt/stacks/symlinkarr` container
- local release binary + `.sha256` produced matching artifacts
- `discover --output json list` clean on stdout after DB fix
- live `repair auto` repaired one real dead link and persisted the remaining orphan

## Post-RC Direction

Symlinkarr should now optimize for **operator confidence under real runtime conditions**.

That means the next work should reduce:
- uncertainty during background activity
- fear around destructive automation
- ambiguity in edge-case media identity handling
- context switching between UI, docs, and terminal

### Tier 1 — Next

1. **Polish anime override UX** — make the override list readable, filterable, and explain its effect clearly
2. **Dashboard drilldown** — link feed events directly to their scan_run / cleanup_result / repair detail pages
3. **Keep docs in sync** — README, CLI_MANUAL, API_SCHEMA should track every new endpoint and config field

### Tier 2 — After Tier 1

4. **Targeted structural cleanup** — only if upcoming feature work pushes logic back into `main.rs`, `auto_acquire.rs`, or `repair.rs`
5. **MediaServerAdapter trait** — only when a 4th media-server integration becomes concrete
6. **Revisit streaming guard** — only if real operator incidents prove active-playback collisions matter in practice

### Explicitly Not Now

- more themes as a primary track
- raw log streaming as first observability answer
- broad connector collection
- plugin/marketplace architecture

## Known Limits To Acknowledge

- anime specials without good anime-lists hints may still need manual search terms, because many indexers are weak at `S00Exx`-style anime queries
- anime override currently covers search titles and hints only; episode/season remapping is out of scope for `v1.0`

## Supporting Docs

- [README.md](../../README.md)
- [Product Scope](./PRODUCT_SCOPE.md)
- [CLI manual](../CLI_MANUAL.md)
- [API schema](../API_SCHEMA.md)
- [Changelog](../CHANGELOG.md)
- [Design Council notes](./DESIGN_COUNCIL_2026-04-22_REMAINING_WORK.md)
