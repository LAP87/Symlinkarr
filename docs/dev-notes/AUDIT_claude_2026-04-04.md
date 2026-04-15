# Symlinkarr V1.0 RC Audit — 2026-04-04

**Auditor:** Claude Opus 4.6 (6 parallel sub-agents)
**Codebase:** `v0.3.0-beta.1` @ branch `codex/media-server-hardening`
**Scope:** Core logic, API clients, config/DB/models, anime/media servers, web UI/security, build/test/deploy
**Status:** All 6 audit agents completed successfully.

> Status note, 2026-04-04: this file is a point-in-time audit snapshot, not the current blocker list. The former RC blockers `B-01` through `B-05` have since been addressed. Use [RC_ROADMAP.md](./RC_ROADMAP.md) for the live release-gate view.

---

## RC Blockers

These must be fixed before any V1.0 RC tag. They are correctness bugs that will manifest in production.

| # | Severity | File | Finding |
|---|----------|------|---------|
| B-01 | CRITICAL | `src/matcher.rs:1044-1088` | Exact media-ID path match bypasses `source_shape_matches_media_type()` — movies can match episodic sources (and vice versa) when the path contains an embedded ID like `tvdb-81189`. Fix: add shape validation before pushing the candidate in the fast path. |
| B-02 | CRITICAL | `src/discovery.rs:157-167` | `titles_match("", "")` returns `true` because the equality check runs before the empty-string guard. Empty parsed titles create false gap suppression — real missing content is hidden. Fix: move the empty guard before the equality check. |
| B-03 | CRITICAL | `src/db.rs:~1543` | `get_stats_by_library()` queries a non-existent column `library_name` in the `links` table. Will crash at runtime. Currently guarded by `#[allow(dead_code)]` but is a ticking bomb. Fix: either add the column via migration or derive from `target_path`. |
| B-04 | CRITICAL | `src/db.rs:~2220` | `insert_link_in_tx()` uses `to_str().unwrap_or_default()` instead of `path_to_db_text()` — silently inserts empty strings for non-UTF-8 paths, corrupting the database. The sibling `insert_link()` does this correctly. Fix: use `path_to_db_text()` with `?`. |
| B-05 | CRITICAL | `src/api/tmdb.rs` (6 endpoints) + `src/api/tvdb.rs` (2 endpoints) | 8 API endpoints skip HTTP status checks before JSON deserialization. A 401/404/429 produces a confusing serde error ("missing field") instead of a clear HTTP error. Every other client in the codebase does this correctly. Fix: add `resp.status().is_success()` check before `.json()`. |
| B-06 | CRITICAL | `.gitignore` | `/.secrets/` directory is NOT in `.gitignore` (only `/secrets/` is). The `.secrets/` dir contains live API keys (`emby_api_key`, `jellyfin_api_key` with `0600` perms). A single `git add -A` leaks real credentials. Fix: add `/.secrets/` to `.gitignore` immediately. |
| B-07 | CRITICAL | `Cargo.toml:12` | `serde_yaml = "0.9"` is deprecated/unmaintained since late 2024 (by David Tolnay). No security fixes. Fix: migrate to `serde_yml` (API-compatible drop-in). |
| B-08 | CRITICAL | `Cargo.toml:13` | `reqwest = "0.11"` is one major version behind (0.12 stable since early 2025). No upstream patches for HTTP client issues. Fix: upgrade to `reqwest = "0.12"`. |
| B-09 | CRITICAL | `src/web/handlers.rs:109-119`, `src/web/api/mod.rs:1105-1114` | Plex DB path is user-controlled with no confinement. `plex_db` query param accepts arbitrary filesystem paths — only checks `.exists()`. No canonicalization, no `..` rejection, no directory confinement. File existence oracle + possible info disclosure via SQLite error strings. Fix: validate against allow-list or reject `..` and require `.db` extension. |

**Lines affected for B-05:**
- `tmdb.rs:115` — `get_tv_metadata`
- `tmdb.rs:188` — `get_movie_metadata`
- `tmdb.rs:248` — `get_external_ids`
- `tmdb.rs:263` — `get_tv_aliases`
- `tmdb.rs:273` — `get_movie_aliases`
- `tmdb.rs:283` — `get_season_details`
- `tvdb.rs:117` — `authenticate`
- `tvdb.rs:278` — `fetch_episodes`

---

## HIGH Severity

| # | File | Finding |
|---|------|---------|
| H-01 | `src/repair.rs:959-1001` | `repair_link` uses non-atomic remove+create. TOCTOU gap where another process could create a directory at the path between remove and symlink. The linker already uses atomic temp+rename — repair should too. |
| H-02 | `src/matcher.rs:169` | `Semaphore::acquire()` error silently ignored. If semaphore is closed during shutdown, tasks proceed without rate limiting. Use `.await.expect(...)` or propagate. |
| H-03 | `src/cleanup_audit.rs` | Race condition between stale audit report and prune execution. A concurrent `sync` run can re-create symlinks listed in the report. Mitigated by confirmation tokens and `max_report_age_hours`, but needs documentation that sync+prune must not run concurrently. |
| H-04 | `src/config.rs:~323` | `SourceConfig.media_type` is `String` accepting any value. `"tvv"` passes validation silently. Should be an enum `{ Tv, Movie, Auto }`. |
| H-05 | `src/models.rs` | `LinkRecord.media_id` is `String` but `LibraryItem.id` is `MediaId` enum — no compile-time guarantee of well-formedness. No `FromStr` impl for round-trip validation. |
| H-06 | `src/models.rs:147-148` | `LinkRecord` timestamps are `Option<String>` instead of `Option<DateTime<Utc>>`. Other records (`AcquisitionJobRecord`) do this correctly. |
| H-07 | `src/config.rs:~1168` | No validation that library/source paths are absolute. Relative paths cause inconsistent behavior in daemon mode. |
| H-08 | `src/db.rs:~2280` | `VACUUM` in `housekeeping()` acquires exclusive DB lock, blocking all concurrent writers. Use `PRAGMA incremental_vacuum` or run only when quiescent. |
| H-09 | `src/db.rs` | `PRAGMA foreign_keys = ON` never set. Orphaned `link_events` rows accumulate when `scan_runs` are cleaned by housekeeping. |
| H-10 | `src/api/http.rs:76-96` | `apply_rate_limit` silently skips limiting when `try_clone()` or `build()` fails. Requests with non-clonable bodies bypass rate limiting entirely. |
| H-11 | `src/api/tvdb.rs:130-137` | `TvdbClient` requires `&mut self` for metadata fetching due to mutable token field, preventing concurrent use. Consider `Arc<RwLock<Option<String>>>` for the token. |
| H-12 | `src/web/mod.rs:1042-1045` | Browser session cookie compared with `==` (short-circuiting), not constant-time. CSRF token comparison on line 1047 correctly uses `constant_time_str_eq`. Timing side-channel could allow token recovery. Fix: use `constant_time_str_eq` for session cookie too. |
| H-13 | `src/main.rs:24` | Blanket `#[allow(dead_code, unused_imports, unused_variables)]` on entire `mod web` — suppresses ALL compiler warnings for 2000+ lines of auth/CSRF/handler code. Dead code in auth layer could hide security gaps. Fix: remove blanket allow, fix individual warnings. |
| H-14 | `src/commands/daemon.rs`, `src/web/mod.rs:1320` | No graceful shutdown handling. Neither daemon loop nor web server handle `SIGTERM`/`SIGINT`. Docker `stop` sends SIGTERM — without handling, container waits 10s then kills. Active DB writes could corrupt SQLite. Fix: use `tokio::signal` + `axum::serve(...).with_graceful_shutdown(...)`. |
| H-15 | `Dockerfile` | No `HEALTHCHECK` instruction despite having `/api/v1/health` endpoint. Docker/orchestrators cannot determine container health. Fix: add `HEALTHCHECK` using CLI health command. |
| H-16 | `Dockerfile` | No `EXPOSE 8726` instruction. Deployment metadata gap. |
| H-17 | `config.docker.yaml:136-144` | Docker config has `bind_address: "0.0.0.0"` + `allow_remote: true` with empty username/password. Runtime correctly refuses this combo, but Docker users following the example get a startup crash. Fix: set `web.enabled: false` or add placeholder secret refs. |
| H-18 | `src/main.rs:348-351` | `config.log_level` is parsed/stored but never wired to tracing subscriber. Config setting is misleading — does nothing. `RUST_LOG` env var is the only actual mechanism. Fix: wire to `EnvFilter` or remove the config field. |

---

## MEDIUM Severity

| # | File | Finding |
|---|------|---------|
| M-01 | `src/discovery.rs:166` | Containment matching is overly broad — short library titles like "Up" or "IT" match almost every torrent title, hiding real gaps. Add minimum length guard for containment. |
| M-02 | `src/source_scanner.rs:156-161` | File-level symlinks included in scan (but not dir symlinks). If RD mount has symlinks to same file, duplicates enter the match pipeline. |
| M-03 | `src/source_scanner.rs:24-25` | Year extraction regex can grab resolution-like numbers from codec strings (e.g., `x265.2024p`). Range check helps but doesn't eliminate all false positives. |
| M-04 | `src/matcher.rs:265-267` | `allow_global_fallback` disabled only if *every* library item is anime. A 99% anime / 1% TV library still gets expensive global fallback. |
| M-05 | `src/linker.rs:523-524` | Missing season/episode silently defaults to 1 instead of erroring. Masks upstream matcher bugs and creates incorrect links. |
| M-06 | `src/repair.rs:667` | `scan_for_dead_symlinks` hardcodes `content_type: ContentType::Tv` for all filesystem-detected dead links. Movies get wrong parser for replacement candidates. |
| M-07 | `src/repair.rs:939` | `candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap())` panics on NaN scores. Use `unwrap_or(Ordering::Equal)` or `total_cmp`. |
| M-08 | `src/auto_acquire.rs:32` | `failed_retry_minutes` saturating math works correctly only because of the `.min(180)` cap — intent unclear without the cap. |
| M-09 | `src/db.rs:~492` | Unparameterized `format!` for table/column names in migration code. Not exploitable (hardcoded strings) but poor pattern for V1.0. |
| M-10 | `src/db.rs` | `api_cache` expired rows never evicted. Table grows unboundedly. Add cleanup to housekeeping. |
| M-11 | `src/db.rs` | Legacy `scan_history` table still written to but never cleaned by housekeeping. |
| M-12 | `src/config.rs` | Default `cache_ttl_hours` is 87,600 (10 years) in code but 720 (30 days) in `config.example.yaml`. Misleading. |
| M-13 | `src/config.rs` | `daemon.interval_minutes` has no minimum validation. Value of 0 causes tight loop. |
| M-14 | `src/backup.rs:~252` | `restore()` has no transaction wrapping — crash mid-restore leaves DB partially restored. |
| M-15 | `src/db.rs:~1353` | `has_active_link_for_episode` uses `LIKE '%S01E01%'` on full `target_path` — can false-positive on parent directory names. |
| M-16 | `src/main.rs` | No distinct exit codes. All errors return 1 via `anyhow::Result`. Harder to script. |
| M-17 | `src/api/http.rs:193-196` | No jitter in exponential backoff. Concurrent retries create thundering-herd effect. |
| M-18 | `src/api/http.rs:215-220` | `retry_after_wait` only handles integer seconds, not RFC 7231 HTTP-date format. |
| M-19 | `src/api/tmdb.rs:78` | No authentication validation at construction. Empty API key sends unauthenticated requests that TMDB rejects. |
| M-20 | `src/api/realdebrid.rs:137-138` | `list_all_torrents` pagination: if `limit` is 0, becomes infinite loop. |
| M-21 | `src/api/bazarr.rs:200-209` | `trigger_sync` swallows partial failures — series search can fail while movies succeeds, returning `Ok(())`. |
| M-22 | `src/api/decypharr.rs:265-275` | `add_content` uses multipart form which bypasses retry (non-clonable body). Single attempt, no retry on transient failure. |
| M-23 | `src/api/decypharr.rs:307-348` | `list_torrents` pagination has no max-page safety valve (unlike RD client). |
| M-24 | `src/anime_identity.rs:186-197` | Ambiguous episode resolution returns `None` silently — no diagnostic logging for debugging. |
| M-25 | `src/media_servers/plex_db.rs:28-35` | No SQLite busy timeout configured for Plex DB reads. Can fail with `SQLITE_BUSY` under Plex write load. One-line fix: `.busy_timeout(Duration::from_secs(5))`. |
| M-26 | `src/media_servers/mod.rs` | No trait-based media server abstraction — dispatch via match arms on `MediaServerKind`. Pragmatic for 3 backends but harder to extend. |
| M-27 | `src/anime_roots.rs:44` | `to_str()` silently skips non-UTF-8 directory names. Acceptable but worth noting. |
| M-28 | `Cargo.toml:47` + `src/web/mod.rs:216` | `panic = "abort"` in release profile but web module uses `catch_unwind()` for background jobs. With abort, `catch_unwind` is a no-op — panics kill the daemon instead of being caught. Fix: remove `panic = "abort"` OR remove `catch_unwind`. |
| M-29 | `src/web/ui/base.html:7-12` | External CDN deps (htmx, Alpine.js, Google Fonts) lack Subresource Integrity (SRI) hashes. Compromised CDN = arbitrary JS injection. Also breaks offline/air-gapped deployments. htmx 1.9.10 is old (2.x current). Fix: add SRI hashes or bundle locally. |
| M-30 | `src/web/static/themes/theme-switcher.js:49` | Theme switcher uses wrong URL path `/src/web/static/themes/...` instead of `/static/themes/...`. All theme CSS loads will 404. Fix: change to `/static/themes/`. |
| M-31 | `src/web/mod.rs:1091-1093` | Basic auth: username is `.trim()`-ed from config but password is not. Inconsistent — a whitespace-only password passes `has_basic_auth()` but is nearly impossible to match via browsers. |
| M-32 | `src/web/handlers.rs:314` (pattern throughout) | Template render errors return raw error string as HTML body via `unwrap_or_else(\|e\| e.to_string())`. May expose file paths, template syntax, internal struct debug output. Fix: log error, return generic error page. |
| M-33 | `src/web/mod.rs:1185-1203` | Fallback session token uses timestamp+PID (weak entropy) if both `getrandom` and `/dev/urandom` fail. Fix: refuse to start web server without secure RNG. |
| M-34 | `src/web/ui/base.html`, `src/web/mod.rs` | No Content-Security-Policy header. Inline handlers and CDN scripts all allowed without restriction. |
| M-35 | `.dockerignore` | Missing exclusions for `.codex`, `references/`, `docs/`, `backups/`. Unnecessary files copied into Docker build context. |
| M-36 | `src/commands/*.rs` (191 occurrences in 15 files) | `println!` used for CLI output but some code paths are also reachable from web/daemon context, where stdout is not captured by tracing. |
| M-37 | `docker-compose.yml:22-24` | Hardcoded host-specific paths (`/mnt/storage/plex`, `/mnt/decypharr/realdebrid`). Needs `# CHANGEME:` markers. |

---

## LOW Severity

| # | File | Finding |
|---|------|---------|
| L-01 | `src/library_scanner.rs:42` | `filter_map(\|e\| e.ok())` silently swallows permission errors during walkdir. No warning logged. |
| L-02 | `src/source_scanner.rs` | `parse_filename` can return `Some` with empty `parsed_title`. Callers guard against it but it's a leaky abstraction. |
| L-03 | `src/linker.rs` | `truncate_filename_to_limit` has no final size assertion after two-pass truncation. Works in practice but fragile. |
| L-04 | `src/repair.rs:1232-1238` | Rollback on DB failure re-creates a dead symlink. By design for consistency, but confusing. |
| L-05 | `src/auto_acquire.rs:725` | Missing `submitted_at` falls back to `Utc::now()`, extending timeout window on corrupt data. |
| L-06 | `src/auto_acquire.rs:428` | `unreachable!` in dry-run path panics entire process on bug. A `warn!` + `continue` would be safer. |
| L-07 | `src/config.rs` | No maximum cap on `matching.metadata_concurrency`. Value of 10,000 exhausts file descriptors. |
| L-08 | `src/cache.rs:~291` | `RdFile.bytes` is `i64` cast to `u64` without bounds check. Negative values wrap to huge numbers. |
| L-09 | `src/models.rs` | `LinkRecord.created_at` and `updated_at` marked `#[allow(dead_code)]` — populated from DB but never consumed. |
| L-10 | `src/api/http.rs:106` | `build_client` `unwrap_or_else` silently falls back to default client with no timeouts on builder failure. |
| L-11 | `src/api/tvdb.rs:14` | Negative cache sentinel is a magic JSON string constant. Fragile but tested. |
| L-12 | `src/api/radarr.rs` | Client only has `get_system_status` — no data-fetching methods. Stub client. |
| L-13 | `src/api/prowlarr.rs` | `title_satisfies_query` normalization is ASCII-focused. May over-reject non-Latin anime titles. |
| L-14 | `src/api/dmm.rs:13` | Hardcoded DMM auth salt. If server-side salt changes, all DMM functionality breaks silently. |
| L-15 | `src/anime_identity.rs:301-312` | Multi-entry TVDB IDs (long-running anime like Naruto) fall through to `None` in absolute resolution. Feature silently doesn't work for most complex anime. |
| L-16 | `src/anime_scanner.rs:320-360` | Title length penalty in scoring could theoretically deprioritize correct long anime titles. Unlikely due to large bonus offsets. |
| L-17 | Systemic | API keys/tokens could leak through request header logging at DEBUG/TRACE level. Audit tracing config. |
| L-18 | `src/web/handlers.rs:1729` | Backup label from form input not validated. If used in filename, special chars could cause issues. |
| L-19 | `src/web/handlers.rs:1009,1427` | Some handlers use `Query<HashMap<String, String>>` instead of typed structs. Inconsistent with other handlers. |
| L-20 | `src/web/handlers.rs:570-577` | Database error messages returned directly to browser. Could expose SQLite internals. |
| L-21 | `src/web/ui/base.html` + theme-switcher.js | Theme switcher JS loaded but never invoked — no toggle button in any template. Dead code. |
| L-22 | `src/web/ui/base.html:83` | Hardcoded version string `v0.3.0-beta.1` in footer. Will become stale. Inject from `Cargo.toml` at build time. |
| L-23 | Across 37 files | 1523 `unwrap()` calls. Many in test code, but production-path ones need targeted audit. |
| L-24 | `src/db.rs`, `src/repair.rs`, `src/models.rs` | 50+ `#[allow(dead_code)]` annotations. Items marked "planned for future use" should have issue tracker refs or be removed for RC. |
| L-25 | `.github/workflows/release.yml` | Release workflow triggers on tag push without requiring CI to pass first. Could publish broken builds. |
| L-26 | `README.md`, `main.rs` CLI about | Says "Real-Debrid - Plex" but project now supports Emby and Jellyfin equally. |

---

## Praise — What's Already Done Right

These patterns demonstrate production-quality engineering:

- **Atomic symlink replacement** in `linker.rs` via temp file + `rename()` — textbook approach
- **Path health system** with FUSE-aware error classification (`ENOTCONN`, timeout detection). `blocks_destructive_ops()` prevents data loss when mounts are unhealthy
- **Transactional migrations** with idempotent DDL and crash-recovery safety
- **Secret resolution chain** (`env:` / `secretfile:` / `.env`) with permission enforcement
- **Confirmation tokens + delete caps** in cleanup/prune — excellent defensive coding
- **Channel-based rate limiter** with per-host `OnceLock` singletons
- **WAL mode + busy_timeout** for concurrent CLI/daemon/web access to SQLite
- **Parameterized SQL** throughout — no user-facing SQL injection vectors
- **Self-healing backfill** for orphaned on-disk symlinks after crash
- **Comprehensive config validation** with structured `ValidationReport`
- **Retry-After header parsing** with exponential backoff on 429/5xx
- **Plex refresh batch coalescing** with configurable caps and abort guards
- **Anime identity graph** with proper NFC normalization and CJK support
- **Askama auto-escaping** prevents XSS across all templates — no `|safe` or `{% autoescape false %}` found
- **CSRF protection** on all browser mutation endpoints with constant-time token comparison
- **Same-origin browser mutation guard** correctly distinguishes browser vs programmatic API clients
- **Panic recovery** in background web tasks (note: disabled by `panic = "abort"` in release — see M-28)
- **Path traversal protection** on backup restore and cleanup report paths — canonicalization + `starts_with` validation
- **Config validation** prevents exposing web UI remotely without authentication
- **406 tests across 41 files** — strong unit test coverage for this project stage
- **CI pipeline** runs test, clippy, fmt, release-smoke — solid baseline
- **Release workflow** handles cross-compilation (amd64/arm64), Docker multi-platform, SHA256 checksums

---

## Test Coverage Summary

| Area | Test Modules | Test Count | Assessment |
|------|-------------|------------|------------|
| CLI parsing | `main.rs` | 10 | Good |
| Config loading/validation | `config.rs` | 36 | Excellent |
| Database operations | `db.rs` | ~30+ | Good |
| Matcher logic | `matcher.rs` | 20 | Good |
| Linker/symlink ops | `linker.rs` | 19 | Good |
| Source scanner/parsing | `source_scanner.rs` | 29 | Good |
| Cleanup audit | `cleanup_audit.rs` | 42 | Excellent |
| Auto-acquire | `auto_acquire.rs` | 31 | Excellent |
| Web handlers | `web/handlers.rs` | 11 | Adequate |
| Web API | `web/api/mod.rs` | ~15 | Adequate |
| Web auth/security | `web/mod.rs` | 9 | Adequate |
| Templates | `web/templates.rs` | 11 | Good |
| API clients (all) | Multiple | ~30 | Good |
| Media servers | `media_servers/` | ~17 | Good |
| Backup | `backup.rs` | 7 | Adequate |
| Repair | `repair.rs` | 34 | Excellent |
| **Total** | **41 files** | **~406** | **Strong for RC** |

No integration test directory (`tests/`) exists — all tests are in-module unit tests.

---

## V1.0 RC Verdict: **NOT READY** — but close

### What's needed:

**Must fix — RC Blockers (9 items):**
1. B-06: Add `/.secrets/` to `.gitignore` (5 seconds — prevents credential leak)
2. B-01: Add shape validation to matcher fast path (small code change)
3. B-02: Reorder guards in `titles_match` (2 lines)
4. B-03: Fix or remove `get_stats_by_library` (remove dead code or add migration)
5. B-04: Use `path_to_db_text()` in `insert_link_in_tx` (1 line)
6. B-05: Add status checks to 8 TMDB/TVDB endpoints (pattern exists in codebase — copy from Sonarr client)
7. B-07: Migrate `serde_yaml` to `serde_yml` (API-compatible drop-in)
8. B-08: Upgrade `reqwest` from 0.11 to 0.12
9. B-09: Confine Plex DB path input — reject `..`, require `.db` extension or use allow-list

**Should fix before RC (priority HIGH items):**
1. H-12: Constant-time session cookie comparison (1 line)
2. H-13: Remove blanket `#[allow(...)]` on `mod web`
3. H-14: Graceful shutdown for daemon + web server (Docker data integrity)
4. H-01: Atomic repair with temp+rename
5. H-04: `SourceConfig.media_type` as enum
6. H-07: Validate paths are absolute
7. H-08: Replace `VACUUM` with incremental or remove
8. H-15: Add `HEALTHCHECK` to Dockerfile
9. H-17: Fix Docker config web auth defaults
10. H-18: Wire `log_level` config or remove the field
11. M-13: Validate `daemon.interval_minutes > 0`
12. M-28: Resolve `panic = "abort"` vs `catch_unwind` conflict

**Findings tally:**

| Severity | Count |
|----------|-------|
| CRITICAL (RC Blocker) | 9 |
| HIGH | 18 |
| MEDIUM | 37 |
| LOW | 26 |
| **Total** | **90** |

### After fixing the 9 RC blockers and the top HIGH items, the codebase is at approximately **v0.8-v0.9 quality**. The architecture is solid, the safety patterns are mature, and the remaining issues are hardening rather than fundamental design problems. The 406 tests, defensive filesystem operations, and layered auth system show this is a well-engineered project that needs a focused hardening pass to reach RC.

---

*Generated by Claude Opus 4.6 — 6 of 6 audit agents completed*
