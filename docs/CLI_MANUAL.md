# Symlinkarr CLI Manual

This manual reflects the current CLI in `main`.

For inline help, use:

```bash
symlinkarr --help
symlinkarr <command> --help
symlinkarr <command> <subcommand> --help
```

For the wiki source index and page drafts, see [GITHUB_WIKI_FEATURES.md](GITHUB_WIKI_FEATURES.md) and [wiki/Home.md](wiki/Home.md).

## Global Usage

```bash
symlinkarr [--config /path/to/config.yaml] <command> [options]
```

Global options:

- `-c, --config <CONFIG>`: explicit config path
- `-h, --help`: command help
- `-V, --version`: version

If `--config` is omitted, Symlinkarr searches:

1. `SYMLINKARR_CONFIG`
2. `./config.yaml`
3. `/app/config/config.yaml`

Quick sanity check:

```bash
symlinkarr --version
```

## Metadata Cache Policy

Symlinkarr keeps TMDB/TVDB metadata cached for a long time.

- the default cache lifetime is long on purpose
- if one title looks stale, refresh that cache entry instead of lowering the global TTL
- short TTLs mostly mean slower scans and more API calls

## Current v1.0 Notes

Symlinkarr v1.0 is focused on the core library loop:

- scan source mounts
- create and repair symlinks
- preview cleanup before deleting or quarantining anything
- keep backups and restore paths usable
- run the private web UI when you do not want to use the CLI

Known limit: anime specials without good anime-lists hints may still need manual search terms.

## Command Reference

### `scan`

Run the full scan -> match -> link pipeline.

```bash
symlinkarr scan [--dry-run] [--search-missing] [--library <LIBRARY>] [--output text|json]
```

Examples:

```bash
symlinkarr scan --dry-run
symlinkarr scan --library Anime --search-missing
symlinkarr scan --output json
```

### `status`

Show database stats and optional health checks.

```bash
symlinkarr status [--health] [--output text|json]
```

Examples:

```bash
symlinkarr status
symlinkarr status --health
symlinkarr status --health --output json
```

When configured, `status --health` checks Plex, Emby, and Jellyfin separately. One, many, or none of them can be active for media-server refresh after link changes.
No media server is required. If none are configured, Symlinkarr still works normally; health output simply reports those integrations as not configured and skips refresh.
`status --health --output json` also includes a top-level `refresh_backends` array so scripts can see which refresh backends are active.
Use `status --health` for a quick health summary. Use `doctor` when you need deeper checks for the DB, writable paths, backup dir, and library/source roots before a write run.

### `queue`

Inspect and manage persistent auto-acquire jobs.

```bash
symlinkarr queue list [--status <STATUS>] [--limit <LIMIT>]
symlinkarr queue retry [--scope all|blocked|no-result|failed|completed-unlinked]
```

`queue list --status` accepts:

- `queued`
- `downloading`
- `relinking`
- `blocked`
- `no-result`
- `failed`
- `completed-unlinked`
- `completed-linked`

Examples:

```bash
symlinkarr queue list
symlinkarr queue list --status blocked --limit 100
symlinkarr queue retry --scope no-result
```

### `daemon`

Run continuous scan cycles. If `web.enabled: true` is set in config, the web UI is also started in the background.

```bash
symlinkarr daemon
```

If `daemon.vacuum_enabled: true` is configured, the daemon may run one full SQLite `VACUUM` per day at or after `daemon.vacuum_hour_local`. Keep that window outside normal usage hours. Symlinkarr runs that vacuum through a dedicated maintenance connection so the normal async pool is not pinned for the whole operation.

### `web`

Run only the web UI, without starting the daemon loop.

```bash
symlinkarr web [--port <PORT>]
```

Examples:

```bash
symlinkarr web
symlinkarr web --port 9999
```

Default bind address and port come from `config.web.bind_address` and `config.web.port`.

Example config:

```yaml
web:
  enabled: true
  bind_address: "127.0.0.1"
  allow_remote: false
  port: 8726
  username: ""
  password: ""
  api_key: ""
```

Notes:

- Loopback is the safe default for host installs.
- For Docker or another explicitly exposed setup, set `bind_address: "0.0.0.0"` and `allow_remote: true`.
- Think in three modes:
  `local-only` = loopback bind and no remote exposure.
  `remote UI` = remote bind plus Basic auth for the built-in UI.
  `scripts/API` = optional API key in addition to Basic auth for scripts.
- `local-only` is trusted mode: no built-in auth is required there.
- `web.username` + `web.password` enable HTTP Basic auth for the bundled HTML UI and JSON API.
- `web.api_key` enables API auth for `Authorization: Bearer ...` or `X-API-Key` clients.
- `web.api_key` alone is not a valid remote-exposure mode for the built-in UI.
- HTML forms require the issued browser session plus a server-rendered CSRF token when the built-in UI is remotely exposed.
- Native Windows is not supported; use WSL2 or a Linux container on Windows 11.
- Plex refresh pacing is configured in `config.yaml` under `plex.refresh_delay_ms`, `plex.refresh_coalesce_threshold`, and `plex.max_refresh_batches_per_run`.
- `plex.abort_refresh_when_capped` is the RC-safe default: if the refresh plan exceeds the per-run cap, Symlinkarr aborts the whole Plex refresh phase instead of queueing only the first batches.
- Emby and Jellyfin refresh is configured under `emby.*` and `jellyfin.*`. `refresh_batch_size`, `max_refresh_batches_per_run`, and `abort_refresh_when_capped` control load, and `fallback_to_library_roots_when_capped` lets Symlinkarr fall back to a few library-root refreshes when too many individual paths changed.
- Concurrent Symlinkarr write runs share one media-server refresh lock. Later runs wait instead of hammering Plex, Emby, or Jellyfin in parallel.

### `cleanup`

Cleanup commands for dead links, audit reports, and prune.

```bash
symlinkarr cleanup [--library <LIBRARY>] [--output text|json]
symlinkarr cleanup dead [--library <LIBRARY>] [--output text|json]
symlinkarr cleanup audit [--scope anime] [--out <PATH>]
symlinkarr cleanup prune --report <REPORT> [--apply] [--include-legacy-anime-roots] [--max-delete <N>] [--confirm-token <TOKEN>] [--gate-mode enforce|relaxed]
symlinkarr cleanup remediate-anime [--library <LIBRARY>] [--output text|json] [--plex-db <PATH>] [--title <FILTER>] [--out <PATH>]
symlinkarr cleanup remediate-anime [--library <LIBRARY>] [--output text|json] --apply --report <REPORT> --confirm-token <TOKEN> [--max-delete <N>] [--gate-mode enforce|relaxed]
```

Examples:

```bash
symlinkarr cleanup
symlinkarr cleanup audit --scope anime
symlinkarr cleanup audit --scope anime --out backups/cleanup-audit-manual.json
symlinkarr cleanup prune --report backups/cleanup-audit-anime-YYYYMMDD-HHMMSS.json
symlinkarr cleanup prune --report backups/cleanup-audit-anime-YYYYMMDD-HHMMSS.json --include-legacy-anime-roots
symlinkarr cleanup prune --report backups/cleanup-audit-anime-YYYYMMDD-HHMMSS.json --apply --confirm-token <TOKEN>
symlinkarr cleanup remediate-anime --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db"
symlinkarr cleanup remediate-anime --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db" --title "Gundam" --out backups/anime-remediation-gundam.json
symlinkarr cleanup remediate-anime --apply --report backups/anime-remediation-gundam.json --confirm-token <TOKEN>
```

Notes:

- `cleanup audit` supports `anime`, `tv`, `movie`, and `all`.
- `cleanup prune` is intentionally two-step. Preview first, then apply.
- `cleanup prune --include-legacy-anime-roots` opt-ins warning-only anime findings where an untagged legacy root coexists with a tagged `{tvdb-*}`/`{tmdb-*}` root. These candidates are quarantined as `foreign`, not deleted.
- prune preview now shows `blocked candidates` when rows were reviewed but held back by trust or policy checks, and `cleanup prune --apply` refuses to run as a no-op when only blocked rows remain.
- successful destructive cleanup can trigger media-server refresh for affected library roots when refresh is configured. Plex, Emby, and Jellyfin can all be refreshed in the same run.
- that refresh step uses the actual changed symlink paths, so prune/anime cleanup no longer refresh every selected library root by default.
- `cleanup remediate-anime` is the safer follow-up for the anime backlog from `report --plex-db ...`. Preview writes a plan JSON with eligible and blocked titles, then apply reuses that exact report plus a confirmation token.
- `cleanup remediate-anime` only auto-handles groups where the legacy roots are foreign-only, the recommended tagged root is DB-tracked, and no non-symlink media files are present under the legacy root. Everything else stays blocked for manual review.
- `cleanup remediate-anime --apply` requires `cleanup.prune.quarantine_foreign=true`, because it quarantines `foreign` legacy symlinks instead of deleting them.
- If you pass `--plex-db`, that exact path must exist. Symlinkarr only falls back to standard local Plex DB paths when no explicit override was supplied.
- Destructive cleanup commands refuse to run when a configured source mount is unhealthy or missing at runtime. Fix the mount first, then re-run the command.

### `repair`

Repair dead symlinks or trigger upstream repair.

```bash
symlinkarr repair [--library <LIBRARY>] scan
symlinkarr repair [--library <LIBRARY>] auto [--dry-run] [--self-heal]
symlinkarr repair [--library <LIBRARY>] trigger [--arr <ARR>]
```

Examples:

```bash
symlinkarr repair scan
symlinkarr repair auto --dry-run
symlinkarr repair auto --self-heal
symlinkarr repair trigger --arr sonarr
```

Notes:

- successful `repair auto` runs can trigger the same media-server refresh for affected library roots when refresh is configured.
- Plex, Emby, and Jellyfin are modeled as separate backends and may now all be enabled together.

### `discover`

Review concrete source-to-target placements for tagged folders that still look empty or underlinked.

```bash
symlinkarr discover [--library <LIBRARY>] [--output text|json] list
symlinkarr discover [--library <LIBRARY>] [--output text|json] add <TORRENT_ID> [--arr sonarr]
```

Examples:

```bash
symlinkarr discover list
symlinkarr discover list --library Movies --output json
symlinkarr discover add XXXXXXXXXXXXX --arr sonarr
```

Notes:

- `discover list` now uses the same match and target-path logic as scan/linking, but keeps the result in preview/report form.
- the output is a placement review: which source file would land in which tagged folder path, plus whether that would be a create, update, or blocked write.
- `discover add` is a manual Decypharr handoff for one RD torrent. It is not the long-term folder-fill path.

### `backup`

Create, inspect, and restore symlink backups.

```bash
symlinkarr backup [--output text|json] create
symlinkarr backup [--output text|json] list
symlinkarr backup [--output text|json] restore <FILE> [--dry-run]
```

Examples:

```bash
symlinkarr backup create
symlinkarr backup list --output json
symlinkarr backup restore backups/symlinkarr-backup-20260321-010203.json --dry-run
```
### `restore`

Restore from a backup archive without needing a config file. Designed for bootstrapping a fresh installation.

```bash
symlinkarr restore <FILE> [--dir <DIR>] [--dry-run] [--list]
```

Examples:

```bash
symlinkarr restore backups/symlinkarr-backup-20260321-010203.json
symlinkarr restore backups/symlinkarr-backup-20260321-010203.json --dry-run
symlinkarr restore backups/symlinkarr-backup-20260321-010203.json --list
symlinkarr restore backups/symlinkarr-backup-20260321-010203.json --dir /app/config
```

Notes:

- runs without a `config.yaml` — the whole point is that a fresh install has none
- restores `config.yaml` and the SQLite snapshot when present in the backup
- standalone/no-config restore only recreates secrets inside the config tree or the standard Docker `/app/secrets` layout; other external secret paths still need to be recreated manually
- `--dry-run` previews what would be restored without writing files
- `--list` shows backup contents (symlink count, timestamps, what snapshots are included)
- `--dir` sets the target directory for restored files (defaults to `/app/config` if it exists, otherwise current directory)
- environment-only secrets are not included in backups and must be added manually
- after restore, edit the config to match your environment, then start normally

### `bootstrap`

Create a starter config and required directories for a fresh install.

```bash
symlinkarr bootstrap [--dir <DIR>] [--list]
```

Examples:

```bash
symlinkarr bootstrap
symlinkarr bootstrap --dir /app/config
symlinkarr bootstrap --list
```

Notes:

- creates a commented starter `config.yaml` and `backups/` directory
- `--list` checks what is missing without creating anything
- `--dir` sets the target directory (defaults to `/app/config` if it exists, otherwise current directory)
- edit the generated config before starting Symlinkarr

Notes:

- `backup restore` now uses the same runtime safety check as scan/repair/cleanup apply: if configured library roots or source mounts are unhealthy, the restore is refused before any symlink or DB write happens.
- `backup restore` only accepts manifests that resolve inside the configured `backup.path`; symlink escapes and arbitrary absolute paths are rejected in both CLI and web flows.
- restore failures now include the backup file path so you can tell which snapshot failed.
- `backup create` now writes `symlinkarr-backup-...json`, a sibling `symlinkarr-backup-....sqlite3` snapshot, and an app-state bundle for the current `config.yaml` plus any `secretfile:` secrets the install can see.
- treat `Symlinkarr Backup` as the main backup to keep. `Restore Point` is the lighter rollback snapshot created around risky runs.
- `backup restore` now restores app-state too when that bundle is present and the current install paths match.
- environment-only secrets still remain outside the backup set, and a truly fresh install still needs config/secrets placed before first startup.
- `backup list` and `backup restore` validate manifest integrity for current-format backups before trusting them.

### `cache`

Manage both cache layers:

- the Real-Debrid torrent/file-info cache used for discovery and faster scans
- the sticky TMDB/TVDB/anime-lists metadata cache used for matching and anime resolution

```bash
symlinkarr cache build
symlinkarr cache status
symlinkarr cache invalidate <KEY>
symlinkarr cache clear
```

Examples:

```bash
symlinkarr cache status
symlinkarr cache build
symlinkarr cache invalidate tmdb:12345
symlinkarr cache invalidate tmdb:tv:
symlinkarr cache invalidate anime-lists
symlinkarr cache clear
```

Notes:

- `cache build` and `cache status` are about the Real-Debrid torrent cache.
- `cache invalidate` is for targeted metadata refresh when a specific title or anime mapping looks stale.
- `cache clear` removes all cached TMDB/TVDB/anime-lists metadata and forces fresh fetches on later lookups.
- `cache invalidate tmdb:tv:` or similar family prefixes invalidate whole metadata families when you need a wider refetch than a single title.
- `cache invalidate tmdb:12345` expands to both TMDB TV/movie metadata plus external-id cache entries for that ID.
- the metadata cache is long-lived by default; prefer `cache invalidate` over lowering the global metadata TTL.

Known anime limit:

- anime specials without usable anime-lists numbering hints may still need manual search terms, because many indexers are weak at `S00Exx`-style anime queries.

### `config`

Validate config parsing, secrets indirection, and referenced paths.

```bash
symlinkarr config validate [--output text|json]
```

Example:

```bash
symlinkarr config validate --output json
```

### `doctor`

Run a preflight health checklist.

```bash
symlinkarr doctor [--output text|json]
```

Example:

```bash
symlinkarr doctor --output json
```

### `report`

Generate a library report, with optional filesystem vs Symlinkarr DB vs Plex DB path drift compare.

```bash
symlinkarr report [--output text|json] [--filter movie|series] [--library <LIBRARY>] [--plex-db <PATH>] [--full-anime-duplicates] [--anime-remediation-tsv <PATH>] [--pretty]
```

Examples:

```bash
symlinkarr report
symlinkarr report --filter movie --output json --pretty
symlinkarr report --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db"
symlinkarr report --filter movie --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db" --output json --pretty
symlinkarr report --library Anime --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db" --full-anime-duplicates --output json --pretty
symlinkarr report --library Anime --plex-db "/var/lib/plex/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db" --anime-remediation-tsv /tmp/anime-remediation.tsv
```

Notes:

- without `--plex-db`, the report still compares actual filesystem symlink paths against active Symlinkarr DB links
- with `--plex-db`, the report adds a path-set compare against Plex-indexed files under the selected library roots
- Plex `deleted_at` is treated as advisory only; the only strong cleanup signal is `Plex deleted + known missing source`, because Plex can mark paths deleted during transient RD-mount outages
- `--full-anime-duplicates` disables the default sample cap for anime duplicate sections so you can export the full mixed-root and Hama-split cleanup backlog
- when `--plex-db` is present, the anime section includes a cleanup queue that ranks legacy-root/Hama-split titles by filesystem and DB impact, so you can work the backlog in a sensible order
- `--anime-remediation-tsv` writes that anime cleanup queue as a spreadsheet-friendly TSV file and lifts the sample cap for the queue export

## JSON-Capable Commands

These top-level commands currently support `--output json`:

- `scan`
- `status`
- `queue`
- `cleanup`
- `discover`
- `backup`
- `config validate`
- `doctor`
- `report`

## Deprecated / Hidden Compatibility Command

There is also a hidden compatibility alias:

```bash
symlinkarr dry-run [--library <LIBRARY>] [--output text|json]
```

Prefer:

```bash
symlinkarr scan --dry-run
```
- Docker users typically do not need `bootstrap`; the image or compose setup already creates directories and mounts config. Use `bootstrap` only for local/bare-metal first-run.
- If `symlinkarr web` is started without a `config.yaml`, it serves a setup page with instructions for `symlinkarr restore` and `symlinkarr bootstrap` instead of refusing to start.
- Docker users: to restore into a fresh container, mount the backup directory and run `docker exec symlinkarr symlinkarr restore /app/backups/<file>.json`, then restart the container.
