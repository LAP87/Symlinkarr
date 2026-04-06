# Symlinkarr RC Roadmap

This is the current, scope-correct roadmap for reaching a believable `v1.0 RC`.

It intentionally reflects Symlinkarr as a local-first symlink daemon and operator tool, not as a general media-ops platform.

## Product Line

See [Product Scope](./PRODUCT_SCOPE.md).

Short version:

- Symlinkarr is a deterministic scan/link/repair/cleanup tool
- the web UI is an operator surface
- media-server integrations are supporting adapters
- acquisition and remediation helpers are optional, not the definition of the product

## Current Position

The project is now in `RC-hardening`, not feature-discovery.

Already solid:

- scan, match, link, repair, cleanup audit/prune, backup/restore
- local-first CLI workflow
- operator web UI and JSON API
- Plex / Emby / Jellyfin refresh integration
- persisted telemetry and status/history screens
- repair and cleanup flows with stronger runtime safety guards

Recently tightened:

- remote web exposure now requires explicit auth for the HTML UI
- mutation guards in the web layer are stricter
- repair DB/filesystem behavior is safer under failure
- repair now uses atomic temp-swap semantics instead of remove-then-create
- DB path handling no longer silently truncates invalid path text
- HTTP health checks and client construction fail louder
- matcher/discovery/API edge cases from the latest RC audit were burned down
- media refresh lock/defer/drain behavior has now been exercised under real concurrent load against configured Plex / Emby / Jellyfin backends
- local config now validates `SourceConfig.media_type` against explicit allowed values instead of silently accepting typos
- feature-guide docs now exist as wiki-ready source and are surfaced from `symlinkarr --help`
- release/Docker basics are less fragile
- metadata cache policy is back to long-lived by default; freshness is now treated as a targeted invalidation problem, not a short-TTL problem
- operators can now invalidate or clear sticky metadata cache entries from CLI/API instead of waiting for broad cache expiry
- multi-episode source files now expand into per-episode destination slots instead of leaving later episodes missing
- scheduled `VACUUM` now runs on a separate maintenance connection instead of monopolizing the normal SQLite pool
- backup restore now version-gates backup manifests so future schema changes fail loudly instead of restoring with silent defaults
- full backups now also capture a sibling SQLite snapshot, and current-format manifests validate integrity before list/restore trust them
- the wiki-style feature guide is now part of the normal docs/help surface under `docs/GITHUB_WIKI_FEATURES.md`

## Must Finish Before `v1.0 RC`

### 1. Final Runtime Validation

The remaining RC work is mostly proving and packaging, not broad new implementation.

Must finish:

- validate scheduled `VACUUM` behavior in a real daemon maintenance window
- do one final operator-style pass over scan, repair, prune, restore, and remediation on realistic data
- keep rollback semantics and skip reasons visible in the operator surfaces

### 2. Release Surface Hygiene

The code surface is close to RC; the release surface needs to match.

Must finish:

- cut the RC version and changelog intentionally
- verify release artifacts and Docker image paths one more time
- make sure docs describe current defaults, current cache policy, and current security modes

### 3. Known-Limit Documentation

What remains in the code is mostly narrow behavior worth documenting rather than blocking the RC outright.

Must finish:

- keep the anime-specials limitations explicit where automatic acquisition still depends on upstream naming/mapping quality
- keep stale audit docs marked as snapshots rather than live blocker lists
- keep feature-guide/help wording aligned with the intended public-facing explanation level

## Important, But Not RC-Blocking

- broader anime remediation eligibility work
- deeper Emby/Jellyfin compare logic
- more acquisition automation
- richer dashboards and cosmetic UI polish
- coverage gates in CI
- supply-chain/signing polish beyond the current baseline

## Explicitly Not Required For `v1.0`

- turning Symlinkarr into a downloader/orchestrator
- automatic whole-library remediation by default
- watcher-first or event-driven runtime as the primary model
- broad feature expansion for edge media-server workflows

## Immediate Next Slices

If work resumes right now, the best next slices are:

1. validate scheduled `VACUUM` and one final daemon maintenance pass on realistic data
2. cut the RC version, changelog, and artifact/release surface intentionally
3. review remaining known limitations and mark them clearly for operators instead of letting old audits imply hidden blockers

## Known Limits To Acknowledge In `v1.0 RC`

- anime specials without good anime-lists hints may still need manual search terms, because many indexers are weak at `S00Exx`-style anime queries

## Supporting Docs

- [README.md](../README.md)
- [Product Scope](./PRODUCT_SCOPE.md)
- [CLI manual](CLI_MANUAL.md)
- [API schema](API_SCHEMA.md)
- [Changelog](CHANGELOG.md)
