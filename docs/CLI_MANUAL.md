# Symlinkarr CLI Manual

This manual reflects the current CLI surface in `main`.

For inline help, use:

```bash
symlinkarr --help
symlinkarr <command> --help
symlinkarr <command> <subcommand> --help
```

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
```

Notes:

- Loopback is the safe default for host installs.
- For Docker or another explicitly exposed setup, set `bind_address: "0.0.0.0"` and `allow_remote: true`.
- Native Windows is not supported; use WSL2 or a Linux container on Windows 11.
- Plex refresh pacing is configured in `config.yaml` under `plex.refresh_delay_ms`, `plex.refresh_coalesce_threshold`, and `plex.max_refresh_batches_per_run`.
- `plex.abort_refresh_when_capped` is the RC-safe default: if the refresh plan exceeds the per-run cap, Symlinkarr aborts the whole Plex refresh phase instead of queueing only the first batches.

### `cleanup`

Cleanup workflows for dead links, audit reports, and prune.

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
- `cleanup remediate-anime` is the guarded follow-up for the correlated anime backlog from `report --plex-db ...`. Preview writes a remediation plan JSON with eligible and blocked titles, then apply reuses that exact report plus a confirmation token.
- `cleanup remediate-anime` only auto-handles groups where the legacy roots are foreign-only, the recommended tagged root is DB-tracked, and no non-symlink media files are present under the legacy root. Everything else stays blocked for manual review.
- `cleanup remediate-anime --apply` requires `cleanup.prune.quarantine_foreign=true`, because the workflow intentionally quarantines `foreign` legacy symlinks instead of deleting them.
- If you pass `--plex-db`, that exact path must exist. Symlinkarr only falls back to standard local Plex DB paths when no explicit override was supplied.
- Destructive cleanup commands refuse to run when a configured source mount is unhealthy or missing at runtime. Fix the mount first, then re-run the command.

### `repair`

Repair dead symlinks or trigger upstream repair workflows.

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

### `discover`

Inspect RD cache content not currently represented in the library.

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
symlinkarr backup restore backups/backup-20260321-010203.json --dry-run
```

Notes:

- `backup restore` now uses the same runtime safety gate as scan/repair/cleanup apply: if configured library roots or source mounts are unhealthy, the restore is refused before any symlink or DB mutation happens.

### `cache`

Manage the Real-Debrid cache layer.

```bash
symlinkarr cache build
symlinkarr cache status
```

Examples:

```bash
symlinkarr cache status
symlinkarr cache build
```

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
- `--full-anime-duplicates` disables the default sample cap for anime duplicate sections so you can export the full mixed-root and Hama-split remediation backlog
- when `--plex-db` is present, the anime section now includes a remediation queue that ranks correlated legacy-root/Hama-split titles by filesystem and DB impact, so you can work the backlog in a sensible order
- `--anime-remediation-tsv` writes that remediation queue as a spreadsheet-friendly TSV file and implicitly lifts the sample cap for the queue export

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
