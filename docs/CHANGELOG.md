# Symlinkarr Changelog

## 2026-03-15 - RD 429 Fix + Cache Management

### Problem

Accounts with large RD libraries (10k+ torrents) triggered cascading `429 Too Many Requests` errors during cache sync. The root cause: `get_torrent_info` was called per-torrent for every downloaded torrent, producing 29k+ sequential API calls on large accounts.

### Code Changes

- **cache.rs**: Per-torrent `get_torrent_info` calls now capped at 150 per sync cycle. Non-downloaded torrents are stored with empty file lists (no API call). Added `sync_full()` for uncapped builds. Progress reporting via `ProgressLine`.
  - files: `src/cache.rs`
- **db.rs**: `get_rd_torrent_count` replaced with `get_rd_torrent_counts` returning `(cached_with_files, total_downloaded)` for coverage-aware fallback decisions.
  - files: `src/db.rs`
- **main.rs**: Scanner now checks cache coverage ratio — falls back to filesystem walk when below 80%. Added `symlinkarr cache build` (full uncapped sync) and `symlinkarr cache status` commands.
  - files: `src/main.rs`
- **discovery.rs**: `find_gaps` now reads from local SQLite cache instead of making a redundant `list_all_torrents` API call.
  - files: `src/discovery.rs`
- **http.rs**: Added `RATE_LIMIT_MIN_GAP_MS` (280ms) for TMDB/TVDB alongside existing `RATE_LIMIT_RD_GAP_MS` (400ms).
  - files: `src/api/http.rs`
- Removed `src/bin/stress_test.rs` (one-off diagnostic tool, no longer needed).

### New CLI Commands

- `symlinkarr cache build` — full cache build with no fetch cap (nightly cron use case)
- `symlinkarr cache status` — show cache coverage stats and current scanner mode

### Validation

- `cargo test`: 215 passed; 0 failed
- `cargo build --release`: passed
- `symlinkarr scan --dry-run`: completed successfully with filesystem fallback (102,527 source files, 69,559 matches)

## 2026-03-11 - `.env` Autoload + Plex Refresh + API Spec

### Code Changes

- config loading now autoloads `.env` and `.env.local` from the config directory and current working directory before resolving `env:VAR` secrets.
  - existing process environment variables still take precedence
  - files: `src/config.rs`, `.env.example`, `README.md`
- added optional Plex integration for targeted library refresh after successful link/relink operations.
  - files: `src/api/plex.rs`, `src/linker.rs`, `src/main.rs`, `src/config.rs`
- `status --health` now checks Plex when configured.
  - file: `src/main.rs`
- added API surface spec sheet covering current CLI/JSON/integration contracts and the planned future `/api/v1` shape.
  - file: `docs/API_SPEC.md`

### Validation

- `cargo test`
  - result: `153 passed; 0 failed`
- `cargo clippy --all-targets --all-features -- -D warnings`
  - result: passed
- `cargo build --release`
  - result: passed
- `./target/debug/symlinkarr config validate --output json`
  - result: `ok: true`

## 2026-03-09 - Config/Health Hardening + Product Docs

### Code Changes

- tautulli auth fallback now retries query-param auth on `400`, `401` and `403`, not just auth-status failures.
  - file: `src/api/tautulli.rs`
- bazarr auth fallback now retries query-param auth on `400`, `401` and `403`.
  - file: `src/api/bazarr.rs`
- config validation now checks runtime-sensitive filesystem permissions when `security.enforce_secure_permissions=true`.
  - scope:
    - secret files referenced by `secretfile:`
    - SQLite database path
    - backup directory
  - file: `src/config.rs`
- config command now supports machine-readable output:
  - `symlinkarr config validate --output json`
  - file: `src/main.rs`
- doctor now includes a `config_validation` check entry.
  - file: `src/main.rs`
- tests added for:
  - auth fallback retry conditions
  - secretfile path collection
  - insecure runtime permission detection
  - CLI parsing for config validation output
  - files: `src/api/tautulli.rs`, `src/api/bazarr.rs`, `src/config.rs`, `src/main.rs`

### Docs

- added product direction/spec sheet:
  - `docs/PRODUCT_SPEC.md`
- added design council for future implementation decisions:
  - `docs/DESIGN_COUNCIL.md`
- updated README with validation/permissions guidance.
  - file: `README.md`

### Validation

- `cargo test`
  - result: `131 passed; 0 failed`
- `cargo run -- status --health --output json`
  - result: all configured local services healthy
- `cargo run -- doctor --output json`
  - result: `0` failed checks

## 2026-02-24 - Specials Edge-Case Guard + Confidence Tier Triage

### Code Changes

- cleanup audit: season `0` (specials) handling hardened to reduce false positives.
  - `arr_untracked` is skipped for `S00`
  - `episode_out_of_range` no longer hard-fails unknown `S00`
  - `season_count_anomaly` is skipped for `S00`
  - file: `src/cleanup_audit.rs`
- tests: added coverage for unknown-specials behavior in episode range logic.
  - file: `src/cleanup_audit.rs`

### Validation

- pre-change snapshot:
  - command: `cargo run -q -- backup create`
  - result: `backups/backup-20260224-193859.json` (`56654` symlinks)
- formatting and tests:
  - command: `cargo fmt && cargo test -q`
  - result: `94 passed; 0 failed`
- anime cleanup audit (after specials hardening):
  - command:
    `RUST_LOG=info cargo run -q -- cleanup audit --scope anime --out backups/cleanup-reports/symlinkarr-cleanup-anime-specials-hardened.json`
  - result:
    - findings: `23368`
    - critical: `1102`
    - high: `21537`
    - warning: `729`
    - suppression log: `suppressed 146 season_count_anomaly warnings`
- prune preview:
  - command:
    `cargo run -q -- cleanup prune --report backups/cleanup-reports/symlinkarr-cleanup-anime-specials-hardened.json`
  - result:
    - candidates: `22994`
    - high/critical candidates: `22639`
    - safe duplicate-warning candidates: `355`
    - removed: `0` (preview)

### Measured Impact

- vs previous warning-hardened report:
  - total: `-9`
  - critical: `-23`
  - high: `+14`
  - warning: unchanged
- season `0` noise removed:
  - `S00 + episode_out_of_range`: `24 -> 0`
  - `S00 + arr_untracked`: `24 -> 0`
- Lord El-Melloi specials example:
  - findings: `2 -> 0`
  - critical: `2 -> 0`

### Confidence Tier Artifacts

- tier summary JSON (A/B/C confidence buckets):
  - `backups/cleanup-reports/symlinkarr-cleanup-anime-specials-hardened-tier-summary.json`
  - counts:
    - A (strong evidence): `1509`
    - B (likely mismatch): `2754`
    - C (mostly duplicate/count noise): `19105`
- tier A flat list:
  - `backups/cleanup-reports/symlinkarr-cleanup-anime-specials-hardened-tier-A.tsv`

## 2026-02-24 - Warning Signal Hardening + Safe Duplicate Auto-Prune

### Code Changes

- cleanup audit: `season_count_anomaly` warning-only entries are now suppressed when the same show season already has stronger findings (`high`/`critical`).
  - effect: less warning noise in already-escalated seasons
  - file: `src/cleanup_audit.rs`
- cleanup prune: now includes a safe warning-only auto-prune path for duplicate episode slots.
  - scope:
    - severity `warning`
    - reason set exactly `duplicate_episode_slot`
    - same `media_id + season + episode`
    - same `source_path`
    - no `high`/`critical` finding in that same slot
  - behavior:
    - keeps one deterministic symlink per identical source
    - prunes only extra symlinks
  - file: `src/cleanup_audit.rs`
- cleanup prune CLI output now shows candidate breakdown:
  - `High/Critical candidates`
  - `Safe duplicate-warning candidates`
  - file: `src/main.rs`
- tests: added coverage for warning suppression and safe duplicate-prune candidate selection.
  - file: `src/cleanup_audit.rs`
- docs: updated cleanup runbook for new warning suppression and safe warning auto-prune behavior.
  - file: `docs/CLEANUP_AUDIT.md`

### Validation

- pre-change snapshot:
  - command: `cargo run -q -- backup create`
  - result: `backups/backup-20260224-190851.json` (`56654` symlinks)
- formatting and tests:
  - command: `cargo fmt && cargo test -q`
  - result: `92 passed; 0 failed`
- anime cleanup audit (after warning hardening):
  - command:
    `RUST_LOG=info cargo run -q -- cleanup audit --scope anime --out backups/cleanup-reports/symlinkarr-cleanup-anime-warning-hardened.json`
  - result:
    - findings: `23377`
    - critical: `1125`
    - high: `21523`
    - warning: `729`
    - suppression log: `suppressed 146 season_count_anomaly warnings`
- prune preview (after safe-warning prune logic):
  - command:
    `cargo run -q -- cleanup prune --report backups/cleanup-reports/symlinkarr-cleanup-anime-warning-hardened.json`
  - result:
    - candidates: `23003`
    - high/critical candidates: `22648`
    - safe duplicate-warning candidates: `355`
    - removed: `0` (preview)

## 2026-02-24 - Logic Hardening + Dry-Run Verification

### Code Changes

- matcher: metadata lookup failures (for example `No data for TVDB <id>`) no longer abort matching.
  - behavior now: warn, fallback to folder-title alias, continue.
  - file: `src/matcher.rs`
- matcher: metadata failure logging capped to first 20 detailed warnings, plus one summary warning.
  - file: `src/matcher.rs`
- matching config: added `matching.metadata_mode` (`full | cache_only | off`).
  - default: `full`
  - active project config now set to `full`
  - files: `src/config.rs`, `config.yaml`
- matcher: metadata policy now enforced in both alias matching and link title enrichment.
  - `cache_only` reads DB cache only and performs zero new TMDB/TVDB requests
  - `off` disables metadata entirely
  - file: `src/matcher.rs`
- tests: added matcher coverage for `metadata_mode=off` and `metadata_mode=cache_only`.
  - file: `src/matcher.rs`
- cleanup audit: `season_count_anomaly` now flags only excess-count seasons (too many links), not missing episodes.
  - file: `src/cleanup_audit.rs`
- cleanup audit: `season_count_anomaly` hardened with hybrid threshold.
  - rule: `actual/expected >= 1.2` and excess `>= max(2, ceil(expected * 0.15))`
  - effect: catches duplicate-heavy seasons earlier than prior `> 1.5` ratio-only rule
  - file: `src/cleanup_audit.rs`
- tests: added season-count anomaly threshold coverage for small/medium/large seasons.
  - file: `src/cleanup_audit.rs`
- cleanup audit: metadata loading now follows `matching.metadata_mode`.
  - file: `src/cleanup_audit.rs`
- docs: metadata policy and severity clarifications.
  - files: `README.md`, `docs/CLEANUP_AUDIT.md`

### Validation

- formatting and tests:
  - command: `cargo fmt && cargo test -q`
  - result: `88 passed; 0 failed`
- pre-change snapshot:
  - command: `cargo run -q -- backup create`
  - result: `backups/backup-20260224-185003.json` (`56654` symlinks)
- metadata-light verification (`cache_only`):
  - command:
    `timeout 240s env RUST_LOG=info cargo run -q -- dry-run --verbose`
  - result:
    - matching started with `metadata=CacheOnly`
    - no TVDB authentication/API metadata lookup during matching
    - run intentionally stopped by timeout (`exit 124`)
- config update:
  - `matching.metadata_mode` switched from `cache_only` to `full` in active `config.yaml`
- full dry-run (verbose/info, with network):
  - command: `RUST_LOG=info cargo run -q -- dry-run --verbose`
  - completed successfully after metadata-fallback patch
  - latest `scan_history` row:
    - `library_items_found=11951`
    - `source_items_found=102261`
    - `matches_found=68722`
    - `links_created=0` (dry-run)
  - dead-link pass in same run reported `10` dead links handled

### Cleanup Audit

- anime cleanup audit:
  - command:
    `RUST_LOG=info cargo run -q -- cleanup audit --scope anime --out /tmp/symlinkarr-cleanup-anime-20260224.json`
  - result:
    - findings: `23733`
    - critical: `1125`
    - high: `21268`
    - warning: `1340`
    - scanned symlinks: `39269`
- anime cleanup audit after season anomaly threshold hardening:
  - command:
    `RUST_LOG=info cargo run -q -- cleanup audit --scope anime --out backups/cleanup-reports/symlinkarr-cleanup-anime-threshold-v2.json`
  - result:
    - findings: `23499`
    - critical: `1068`
    - high: `21150`
    - warning: `1281`
    - scanned symlinks: `39300`
  - environment note:
    - Sonarr was unreachable in this run (`Operation not permitted`), so `arr_untracked` signals were absent.
- prune preview:
  - command:
    `cargo run -q -- cleanup prune --report /tmp/symlinkarr-cleanup-anime-20260224.json`
  - result:
    - candidates: `22393`
    - removed: `0` (preview mode)

### Notable Audit Signals

- severity distribution for anime report:
  - high: `21268 / 23733` (`89.61%`)
  - critical: `1125 / 23733` (`4.74%`)
  - warning: `1340 / 23733` (`5.65%`)
- high findings are mostly one repeat pattern:
  - `duplicate_episode_slot + season_count_anomaly`: `18132` (`85.25%` of all high)
- entries containing `LostYears`: `210` findings
  - high: `164`
  - warning: `46`
  - top reasons among these:
    - `duplicate_episode_slot` (194)
    - `season_count_anomaly` (180)

### Artifacts

- snapshot tarball:
  - `backups/project-snapshots/symlinkarr-project-20260223-220745.tar.gz`
- snapshot sha256:
  - `c2ca3449009876b92299c099eb9d3c6aad955919f03f99c26380a4d5bd78c802`
- cleanup report (copied to project backup path):
  - `backups/cleanup-reports/symlinkarr-cleanup-anime-20260224.json`
- cleanup report (threshold hardening run):
  - `backups/cleanup-reports/symlinkarr-cleanup-anime-threshold-v2.json`
- pre-change safety backup:
  - `backups/backup-20260224-185003.json`
