# Symlinkarr

Symlinkarr is an intelligent, ecosystem-aware symlink manager designed to synchronize a Real-Debrid mount (via Zurg, Decypharr, Riven, etc.) with a media library (Plex, Jellyfin, Kodi).

It is designed to be the "final link" in a modern *arr-stack, ensuring that content found on Real-Debrid is perfectly mapped to your library using industry-standard metadata IDs.

## Vision & Goals
- **Docker-First:** Distributed as a lightweight Docker container for easy integration into existing Compose stacks.
- **GitHub Hosted:** Open-source project intended for community contribution.
- **Metadata-Driven:** Uses `{tvdb-XXXXX}` or `{tmdb-XXXXX}` tags (as recommended by TRaSH Guides) to ensure 100% accuracy.
- **Ecosystem Aware:** Detects and respects existing naming conventions from Sonarr, Radarr, Prowlarr, and Riven.
- **Educational:** Proactively advises users to follow [TRaSH Guides](https://trash-guides.info/) for optimal directory structures and naming conventions.

## Core Features
- **Alias Matching:** Fetches all known international aliases for content from TMDB/TVDB APIs to maximize hit rates on Real-Debrid.
- **Automated Precision:** Eliminates "false positives" (e.g., matching "ER" with "Taskmaster") by using strict ID-based logic and regex anchoring.
- **Symlink Standardization:** Automatically renames symlinks to a clean, standardized format (e.g., `Season 01/Series Name - S01E01 - Episode Title.mkv`) regardless of the messy source filename on Real-Debrid.
- **State Management:** Uses a local SQLite database to track linked content, manage deletions, and handle "dead" links.
- **Cache-Aware Acquisition:** Can search via Prowlarr first and fall back to public Debrid Media Manager cache data before sending chosen content to Decypharr.

## Cleanup Workflow (New)

Symlinkarr now supports a **two-step cleanup workflow** designed for safe removal of bad legacy symlinks (including anime mislinks from subgroup-tagged releases landing in unrelated shows).

### Why two-step?

The workflow is intentionally conservative:

1. `audit` only inspects and writes a JSON report.
2. `prune` can preview that report.
3. `prune --apply` performs deletions only for high-confidence findings.

This avoids accidental deletion of valid links.

### Commands

Run an anime-focused audit and write report to default backup path:

```bash
symlinkarr cleanup audit --scope anime
```

Write report to a custom path:

```bash
symlinkarr cleanup audit --scope anime --out backups/cleanup-audit-manual.json
```

Preview what would be removed:

```bash
symlinkarr cleanup prune --report backups/cleanup-audit-anime-YYYYMMDD-HHMMSS.json
```

Apply removal:

```bash
symlinkarr cleanup prune --report backups/cleanup-audit-anime-YYYYMMDD-HHMMSS.json --apply
```

Legacy dead-link cleanup remains available:

```bash
symlinkarr cleanup
# or
symlinkarr cleanup dead
```

### What audit flags

Each finding has `severity`, `confidence`, and one or more reasons:

- `broken_source`: symlink target does not exist.
- `parser_title_mismatch`: parsed source title does not match library title/aliases with token-boundary checks.
- `arr_untracked`: Sonarr has no matching tracked file for the detected episode.
- `episode_out_of_range`: parsed episode exceeds known metadata range.
- `duplicate_episode_slot`: multiple links for the same episode slot.
- `season_count_anomaly`: season count deviates strongly from expected count.
- `non_rd_source_path`: symlink target is outside configured RD source roots.

### Subgroup-tag / false-positive protection

The cleanup audit uses token-based title matching to avoid substring traps.

Example:
- `show` does **not** match `showgroup`.
- `jujutsu kaisen` **does** match `jujutsu kaisen 03`.

This is specifically to reduce anime release-group collisions in unrelated series folders.

### Safety guarantees

- `prune --apply` only removes filesystem entries that are verified symlinks.
- If backup is enabled, Symlinkarr creates a safety snapshot before applying prune.
- Warnings are reported but not auto-removed unless they escalate with stronger signals.
- Detailed runbook and report semantics: `docs/CLEANUP_AUDIT.md`.

## RD Cache Management

Symlinkarr maintains a local SQLite cache of Real-Debrid torrent metadata to avoid redundant API calls and speed up source scanning. For accounts with many torrents (10k+), the cache sync is rate-limited to prevent 429 errors.

### How it works

During a normal `symlinkarr scan`, the cache sync caps per-torrent API calls to a strict maximum of **150 fetches per cycle** to avoid triggering Real-Debrid's anti-flooding timeouts on massive libraries. Any torrents beyond this cap are saved temporarily without detailed file info.

If cache coverage (torrents with complete file info) is below **80%** of your actively downloaded torrents, the scanner automatically falls back to a **filesystem walk** of your RD mount. This guarantees that your full library is still scanned correctly at lightning speed while the API catches its breath.

In daemon mode, the cache fills incrementally (150 torrents per scan cycle) until it hits the 80% coverage mark.

### Commands

Check current cache coverage:

```bash
symlinkarr cache status
```

Build the full cache without the 150-fetch cap (ideal as a nightly cron job during off-peak hours):

```bash
symlinkarr cache build
```

Example cron entry (nightly at 03:00):

```bash
0 3 * * * /usr/local/bin/symlinkarr cache build
```

### Cache coverage threshold

The scanner utilizes cached data only when **>= 80%** of downloaded torrents have file info. Below that threshold, a lightning-fast filesystem walk is used automatically. This ensures scans always see all your content, even before the cache is fully hydrated.

## Strict Matching Mode

Symlinkarr now supports explicit matching policy in config:

```yaml
matching:
  mode: "strict"            # strict | balanced | aggressive
  metadata_mode: "full"     # full | cache_only | off
```

Default is `strict`.

Metadata lookup behavior:

- `full`: use cache first, then API if cache is missing.
- `cache_only`: use cache only, never make new metadata API calls.
- `off`: skip metadata entirely (folder-title matching + fallback episode naming).

In strict mode:

- matching is deterministic (one best candidate per source),
- ambiguous near-ties are rejected,
- destination conflicts keep only the highest-confidence candidate,
- token-boundary rules reduce false positives like `show` vs `showgroup`.

## Requirements
- A directory structure following TRaSH Guides (folders tagged with TVDB/TMDB IDs).
- A Real-Debrid mount (Zurg, Decypharr, etc.).
- API keys for TMDB/TVDB.

## Configuration

- `config.yaml`: host-oriented local config.
- `config.docker.yaml`: Compose/container config with persistent DB/backup paths under `/app/data`.
- `config.example.yaml`: sanitized template for new installations.
- `.env.example`: homelab-friendly environment variable template for `env:VAR` secrets.

Secrets should be provided via `env:VAR` or `secretfile:/path/to/file`.

Public Debrid Media Manager can be configured as an optional cached-content fallback:

```yaml
dmm:
  url: "https://debridmediamanager.com"
  only_trusted: true
  max_search_results: 3
  max_torrent_results: 10
```

Symlinkarr treats DMM as a discovery/catalog provider only. Execution still goes through Decypharr and your own Real-Debrid account.

For local host runs, Symlinkarr now autoloads `.env` and `.env.local` from:

1. the config file directory
2. the current working directory

Existing shell-exported variables still win over values from `.env`.

When `security.enforce_secure_permissions: true` is enabled, Symlinkarr validates runtime-sensitive file permissions for:

- `secretfile:` inputs
- the SQLite database path
- the backup directory

This is enforced by `symlinkarr config validate`.

For TMDB, Symlinkarr supports either:

- `api.tmdb_api_key`
- or `api.tmdb_read_access_token` (preferred for cleaner HTTP auth)

For the local checked-in setup, secret references point at `secrets/*`, which should stay untracked.

Docker Compose now expects:

- `./config.docker.yaml` mounted to `/app/config/config.yaml`
- `./secrets` mounted read-only to `/app/secrets`
- `./data` mounted to `/app/data`

When running the binary without `--config`, Symlinkarr searches:

1. `SYMLINKARR_CONFIG` if set
2. `./config.yaml`
3. `/app/config/config.yaml`

Machine-readable validation is available via:

```bash
symlinkarr config validate --output json
symlinkarr doctor --output json
```

Auto-acquire provider order today is:

1. Prowlarr search
2. public DMM cached fallback
3. Decypharr add/poll/relink

Planning docs for continued hardening live in:

- `docs/PRODUCT_SPEC.md`
- `docs/DESIGN_COUNCIL.md`
- `docs/API_SPEC.md`

---
