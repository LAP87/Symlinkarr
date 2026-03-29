# Symlinkarr Changelog

## Release Target

- package version for this push: `0.2.0-beta.1`
- posture: `stable core, evolving ops`
- intended use: local-first host or Docker installs, with Windows 11 users running through WSL2 or a Linux container

## 2026-03-29 - Cleanup Audit Backgrounding + Local Repo Cleanup

### Code Changes

- web/API cleanup audit triggering now runs in the background instead of holding the request open for the full audit.
  - web cleanup pages show an active background-audit banner
  - `POST /api/v1/cleanup/audit` now returns `202 Accepted`
  - `GET /api/v1/cleanup/audit/jobs` exposes the currently running audit job
  - scan and cleanup audit now share one background-job gate so they cannot start concurrently by racing separate mutexes
  - files: `src/web/mod.rs`, `src/web/templates.rs`, `src/web/handlers.rs`, `src/web/api/mod.rs`, `src/web/ui/cleanup.html`, `src/web/ui/cleanup_result.html`
- web/API background scan and cleanup audit now retain the last completed or failed outcome in-memory so operators can see the latest failure without digging through logs.
  - added `GET /api/v1/scan/status` and `GET /api/v1/cleanup/audit/status`
  - web scan/cleanup pages now show the most recent failed background outcome directly
  - files: `src/web/mod.rs`, `src/web/templates.rs`, `src/web/handlers.rs`, `src/web/api/mod.rs`, `src/web/ui/scan.html`, `src/web/ui/scan_result.html`, `src/web/ui/cleanup.html`, `src/web/ui/cleanup_result.html`

### Docs

- documented the new background cleanup-audit posture and JSON API polling surface.
  - file: `docs/API_SCHEMA.md`

## 2026-03-29 - Web Scan Backgrounding + Discover/Doctor Honesty

### Code Changes

- web/API scan triggering now runs in the background instead of holding the request open for the full scan.
  - web pages show an active background-scan banner
  - `POST /api/v1/scan` now returns `202 Accepted`
  - `GET /api/v1/scan/jobs` now prepends a synthetic `running` row when a background scan is active
  - files: `src/web/mod.rs`, `src/web/templates.rs`, `src/web/handlers.rs`, `src/web/api/mod.rs`, `src/web/ui/scan.html`, `src/web/ui/scan_result.html`
- read-only doctor checks now preserve a non-writable signal for existing directories without doing a write probe, using effective access checks instead of raw write-bit heuristics.
  - files: `src/commands/doctor.rs`, `src/web/handlers.rs`, `src/web/api/mod.rs`
- discover JSON output remains machine-parseable on RD cache sync failure, and cached-only discover now explicitly surfaces missing RD credentials.
  - files: `src/commands/discover.rs`, `src/web/handlers.rs`, `src/web/api/mod.rs`

### Docs

- documented the new background web/API scan posture and updated web/API wording.
  - files: `README.md`, `docs/API_SCHEMA.md`

### Validation

- `CARGO_TARGET_DIR=/home/lenny/.cache/symlinkarr-rc-safety cargo test -q`
  - result: `453 passed; 0 failed`
- `CARGO_TARGET_DIR=/home/lenny/.cache/symlinkarr-rc-safety cargo clippy --all-targets --all-features -- -D warnings`
  - result: passed

## 2026-03-22 - WSL2 Development Guide

### Docs

- added a dedicated Windows 11 `WSL2` development guide so host development can stay Unix-correct without pretending native Windows runtime is supported.
  - files: `README.md`, `docs/DEV_SETUP_WSL.md`

## 2026-03-22 - Web Exposure Hardening + Cleanup Audit API Summary

### Code Changes

- web UI bind address is now configurable through `web.bind_address`, with loopback default for host installs.
  - files: `src/config.rs`, `src/web/mod.rs`, `config.example.yaml`, `config.docker.yaml`
- the bundled web/API server no longer enables permissive cross-origin access by default.
  - file: `src/web/mod.rs`
- `POST /api/v1/cleanup/audit` now returns the real report summary instead of placeholder zeroes.
  - files: `src/web/api/mod.rs`, `src/web/mod.rs`

### Docs

- documented `web.bind_address`, safer web exposure defaults, and the WSL2/Linux-container requirement for Windows 11 users.
  - files: `README.md`, `docs/CLI_MANUAL.md`, `docs/API_SCHEMA.md`

### Validation

- `cargo test web::tests::cleanup_audit_api_returns_report_summary -- --nocapture`
- `cargo test config::tests::config_load_parses_web_bind_address -- --nocapture`

## 2026-03-22 - Cleanup Audit Throughput + Plex Path Compare

### Code Changes

- cleanup audit now collects symlink entries before metadata/Arr loading and scopes metadata work to referenced media IDs only.
  - files: `src/cleanup_audit.rs`
- cleanup audit metadata fetch now reuses the matcher's cache-first metadata path, fixing TMDB movie-vs-TV dispatch and collapsing full-library audit runtime from a near-stalled metadata crawl to a practical run.
  - files: `src/cleanup_audit.rs`, `src/matcher.rs`
- cleanup prune preview now carries optional `alternate_match` context through to the UI so alternate-owner findings show the better candidate directly.
  - files: `src/cleanup_audit.rs`, `src/web/templates.rs`, `src/web/ui/prune_preview.html`
- report command now supports optional Plex DB path comparison via `--plex-db`.
  - compares actual filesystem symlink paths, active Symlinkarr DB link targets, and Plex-indexed `media_parts.file` paths under the selected library roots
  - files: `src/main.rs`, `src/commands/report.rs`, `src/plex_db.rs`
- Plex path compare now treats `deleted_at` as advisory only.
  - `plex_deleted_and_known_missing_source` is the strong signal; `plex_deleted_without_known_missing_source` is explicitly separated so transient RD-mount outages do not become false cleanup truth
  - files: `src/commands/report.rs`, `src/plex_db.rs`

### Validation

- `cargo test -- --nocapture`
  - result: `298 passed; 0 failed`
- `cargo clippy --all-targets --all-features -- -D warnings`
  - result: passed
- `target/debug/symlinkarr cleanup audit --scope all --out backups/cleanup-audit-all-altcontext-20260322-185300.json`
  - result:
    - findings: `194812`
    - critical: `7907`
    - high: `176547`
    - warning: `10358`
    - active symlinks: `62253`
    - dead links: `211`
- `cargo run -- report --output json --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db"`
  - result:
    - filesystem symlinks: `221924`
    - DB active links: `62253`
    - Plex indexed files: `203903`
    - Plex deleted-only paths: `110`
    - Plex deleted + known missing source: `0`
    - in all three: `7523`

## 2026-03-21 - Web CLI + Documentation Refresh

### Code Changes

- added a dedicated `symlinkarr web` subcommand so the web UI can run without starting daemon mode.
  - file: `src/main.rs`
- added CLI parsing coverage for the new `web` command.
  - file: `src/main.rs`

### Docs

- added a full CLI manual covering current top-level commands, subcommands, flags, and examples.
  - file: `docs/CLI_MANUAL.md`
- added a hand-maintained web/API schema for `/api/v1`.
  - file: `docs/API_SCHEMA.md`
- updated `README.md` to reflect the actual current command surface, web startup path, and live docs.
  - file: `README.md`

### Validation

- `cargo run -- --help`
  - result: includes `web`
- `cargo run -- web --help`
  - result: passed
- `cargo test cli_accepts_web_subcommand -- --nocapture`
  - result: passed

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
