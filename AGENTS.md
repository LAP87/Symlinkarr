# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Commands

### Build & Run
```bash
cargo build --release          # Build release binary
cargo run -- <command>         # Run during development
```

### Test & Lint
```bash
cargo test                     # Run all tests (inline #[test] functions)
cargo test <test_name>         # Run a single test by name
cargo test --lib matcher       # Run tests in a specific module
cargo clippy --all-targets --all-features -- -D warnings
```

Tests are inline `#[cfg(test)]` modules within each source file (no separate `tests/` directory). 217 tests across 28 files — most heavily tested modules are `repair.rs`, `source_scanner.rs`, `config.rs`, `cleanup_audit.rs`, `matcher.rs`, and `commands/`.

### Common Operations
```bash
# Scan and link
symlinkarr scan --dry-run         # Preview matches without creating symlinks
symlinkarr scan                   # Full scan → match → link cycle

# Daemon mode
symlinkarr daemon                 # Run continuously

# Cleanup workflows
symlinkarr cleanup audit --scope anime              # Audit suspicious symlinks
symlinkarr cleanup prune --report <path>            # Preview prune impact
symlinkarr cleanup prune --report <path> --apply    # Apply deletions

# Diagnostics
symlinkarr config validate --output json            # Validate config
symlinkarr doctor --output json                     # Health check
symlinkarr status --health                          # Database + service status

# Repair
symlinkarr repair scan            # Find dead symlinks
symlinkarr repair auto            # Auto-replace dead links

# Discovery
symlinkarr discover list          # Show RD content not in your library

# Queue management
symlinkarr queue list             # Show auto-acquire jobs
symlinkarr queue retry            # Reset retryable jobs
```

### Docker
```bash
docker-compose up -d           # Run in container
docker-compose build           # Rebuild image
```

## Architecture

### Pipeline (Data Flow)
1. `library_scanner.rs` walks Plex library folders → extracts `{tvdb-XXXXX}`/`{tmdb-XXXXX}` tags → produces `LibraryItem`s
2. `source_scanner.rs` scans RD mount (filesystem walk or RD API cache via `cache.rs`) → parses filenames → produces `SourceItem`s
3. `matcher.rs` fetches all aliases from TMDB/TVDB → scores source↔library pairs using token-boundary title matching → deterministic best-candidate selection → `MatchResult`s
4. `linker.rs` creates/updates symlinks with naming template; records each link in SQLite via `db.rs`; reconciles dead/missing links
5. In daemon mode, `commands/daemon.rs` polls and repeats the cycle; auto-acquire queue triggers Prowlarr→DMM→Decypharr pipeline

### Core Modules (`src/`)
- `main.rs` — Slim CLI entry point (~310 lines): `clap` derive types, `main()` dispatch, CLI tests
- `commands/mod.rs` — Shared helpers: `selected_libraries()`, `print_final_summary()`, panel display, cross-cutting utilities
- `commands/scan.rs` — Full scan→match→link cycle, Plex refresh, missing-search auto-acquire
- `commands/status.rs` — Database stats + per-service health checks
- `commands/repair.rs` — Dead symlink repair with self-heal via Prowlarr/Decypharr
- `commands/cleanup.rs` — Dead-link cleanup, audit, and prune workflows
- `commands/daemon.rs` — Continuous polling loop
- `commands/discover.rs` — Gap analysis: RD content not in library
- `commands/queue.rs` — Auto-acquire job inspection and retry
- `commands/backup.rs` — JSON backup/restore with safety snapshots
- `commands/cache.rs` — RD torrent cache build/status
- `commands/config.rs` — Config validation
- `commands/doctor.rs` — Preflight health checklist
- `config.rs` — YAML config parsing; `env:VAR` and `secretfile:` secret indirection; defines `Config`, `ContentType`, `MatchingMode`, `MetadataMode`
- `models.rs` — Shared data types: `MediaType`, `MediaId`, `LibraryItem`, `SourceItem`, `MatchResult`, `LinkRecord`
- `db.rs` — SQLite via `sqlx`; schema for links, scan history, cache, acquisition jobs
- `matcher.rs` — The core matching engine; metadata cache (DB-backed), concurrent alias lookups, strict/balanced/aggressive scoring
- `linker.rs` — Symlink creation/updates with naming templates; dead-link reconciliation
- `source_scanner.rs` — Two parser kinds (`Standard` and `Anime`) with different regex strategies for filename parsing
- `library_scanner.rs` — Walks library dirs, extracts metadata ID tags from folder names
- `cache.rs` — RD torrent metadata cache (avoid filesystem walk when API data available)
- `cleanup_audit.rs` — Two-step cleanup: `CleanupAuditor` inspects symlinks, emits JSON report with `FindingSeverity`/`FindingReason`
- `auto_acquire.rs` — Prowlarr→DMM→Decypharr acquisition pipeline with job state machine (Queued→Downloading→Relinking→Completed)
- `anime_scanner.rs` — Sonarr anime integration: missing episode detection, scene numbering, query building
- `discovery.rs` — Gap analysis: finds RD content not present in library
- `repair.rs` — Dead symlink repair: finds replacements on RD mount or via Prowlarr self-heal
- `backup.rs` — JSON backup/restore of symlink state with safety snapshots before destructive ops
- `utils.rs` — `PathHealth` (FUSE mount detection, transport disconnect), `normalize()` for title comparison, `ProgressLine` for terminal output

### API Clients (`src/api/`)
- `http.rs` — Shared retry-with-backoff HTTP layer (`send_with_retry`); all API clients use this
- `tmdb.rs`, `tvdb.rs` — Metadata lookups (titles, aliases, seasons/episodes)
- `realdebrid.rs` — RD torrent/links API with paginated listing
- `decypharr.rs` — Decypharr integration for auto-acquire queue
- `dmm.rs` — Debrid Media Manager cached-content fallback
- `sonarr.rs`, `radarr.rs` — Arr stack tracking/missing detection
- `prowlarr.rs` — Release search
- `bazarr.rs` — Subtitle sync trigger
- `tautulli.rs` — Plex analytics
- `plex.rs` — Library section refresh

### Web UI (`src/web/`)
Axum-based web interface (port 8726). `WebState` wraps `Arc<Config>` + `Arc<Database>`.
- `mod.rs` — Router setup, CORS, static file serving
- `handlers.rs` — Page handlers (dashboard, cleanup forms, discovery)
- `templates.rs` — Askama HTML templates
- `api/mod.rs` — JSON API endpoints (`/api/status`, `/api/scan`, `/api/repair/auto`, `/api/cleanup/*`, `/api/links`, `/api/doctor`)
- `ui/` — HTML templates; `static/` — CSS with themes

### Key Design Patterns
- **Strict matching mode** (default): Deterministic one-best-candidate; rejects ambiguous near-ties; destination conflicts keep highest-confidence only
- **Two-step cleanup**: `audit` writes JSON report → `prune --apply` removes high-confidence findings; safety snapshot created before apply
- **Token-boundary title matching**: Prevents substring false positives (e.g., `show` vs `showgroup`); critical for anime release-group collisions
- **Dual parsers**: `ParserKind::Standard` (SxxExx) vs `ParserKind::Anime` (`[SubGroup] Title - 03`) selected by `ContentType`
- **Three metadata modes**: `full` (cache then API), `cache_only`, `off` — configured in `matching.metadata_mode`
- **Auto-acquire state machine**: Jobs tracked in SQLite with status transitions and retry backoffs

### Config Structure
- `config.yaml` — Local host config
- `config.docker.yaml` — Container config with `/app` paths
- `config.example.yaml` — Template for new installs
- Secrets via `env:VAR` or `secretfile:/path` (validated when `security.enforce_secure_permissions: true`)
- Config search order: `SYMLINKARR_CONFIG` env → `./config.yaml` → `/app/config/config.yaml`
