# Symlinkarr

![Rust](https://img.shields.io/badge/Rust-CLI-orange?logo=rust)
![Docker](https://img.shields.io/badge/Docker-ready-2496ED?logo=docker&logoColor=white)
![SQLite](https://img.shields.io/badge/State-SQLite-003B57?logo=sqlite&logoColor=white)
![Plex](https://img.shields.io/badge/Plex-supported-E5A00D)
![Emby](https://img.shields.io/badge/Emby-supported-52B54B)
![Jellyfin](https://img.shields.io/badge/Jellyfin-supported-7A5AF8)

Symlinkarr turns Real-Debrid-backed media into a clean local library.

It scans your source mount, matches files to ID-tagged movie and series folders, writes stable symlinks, and keeps state in SQLite. It works with plain folders or alongside Plex, Emby, and Jellyfin.

If your current stack looks like "RD mount + Sonarr/Radarr + a messy library full of stale or misplaced links", this is the layer meant to make that library deterministic again.

## What It Does

- scans RD-backed mounts and local library folders
- matches against `{tvdb-*}` and `{tmdb-*}` tagged folders
- creates and updates symlinks deterministically
- repairs dead links and finds missing content
- audits bad, stale, or misplaced links before cleanup
- supports Plex, Emby, and Jellyfin refresh after mutations
- caps and guards media refresh pressure so large mutations do not blindly stampede the media server

No media server is required.

## Integrations

- Real-Debrid-backed mounts such as Zurg and Decypharr
- Sonarr and Radarr
- Prowlarr
- Bazarr
- Tautulli
- TMDB and TVDB
- Debrid Media Manager
- Plex, Emby, and Jellyfin, optionally and together

## How To Run It

README examples use `symlinkarr ...` as the neutral command form.

- release binary install: run `./symlinkarr ...` or `symlinkarr ...` if it is on your `PATH`
- source checkout: run `cargo run -- ...`
- Docker: mainly intended for long-running daemon mode via `docker compose up -d`

Example:

```bash
symlinkarr scan --dry-run
cargo run -- scan --dry-run
```

## Quick Start

### Install

Download a release tarball from [GitHub Releases](https://github.com/LAP87/Symlinkarr/releases), or build locally with Cargo, or run with Docker.

Release binary example:

```bash
tar -xzf symlinkarr-<version>-linux-amd64.tar.gz
cd symlinkarr-<version>-linux-amd64
./symlinkarr --help
```

### Configure

Start from [config.example.yaml](config.example.yaml).

Minimum:
- one or more library paths
- one or more source paths
- a writable SQLite `db_path`
- TMDB and TVDB credentials if you want full metadata matching

From source:

```bash
cargo run -- config validate --output json
cargo run -- doctor --output json
```

From a release binary:

```bash
./symlinkarr config validate --output json
./symlinkarr doctor --output json
```

### Run

From source:

```bash
cargo run -- scan --dry-run
cargo run -- scan
cargo run -- web
```

From a release binary:

```bash
./symlinkarr scan --dry-run
./symlinkarr scan
./symlinkarr web
```

Web UI default: `http://127.0.0.1:8726`

## Security Modes

Symlinkarr follows a local-first security model similar to how many people already run *arr apps:

- `local-only`: bind to `127.0.0.1`, keep `allow_remote: false`, and use the UI/API from the same host or through your own tunnel/reverse proxy.
- `remote operator`: bind beyond loopback, set `allow_remote: true`, and configure `web.username` + `web.password`.
- `scripted operator`: optionally add `web.api_key` for non-browser automation clients that should use `Authorization: Bearer ...` or `X-API-Key`.

Practical rules:

- `local-only` is a trusted mode: no built-in auth is required, and browser mutation guards stay off by default
- `web.username` + `web.password` protect the HTML UI and API with HTTP Basic auth
- `web.api_key` protects JSON API clients via `Authorization: Bearer ...` or `X-API-Key`
- when the UI is remotely exposed, browser form mutations use an issued same-origin session cookie plus a server-rendered CSRF token
- remote exposure now requires `web.username` + `web.password`; API key alone is not enough for the built-in HTML UI

Recommended default:

- keep the built-in UI loopback-only unless you have a specific reason to expose it

## Metadata Cache Policy

Symlinkarr keeps TMDB/TVDB metadata cached for a long time on purpose.

- metadata is mostly stable, while API timeouts and rate limits are expensive
- the right fix for stale metadata is targeted refresh/invalidation when a specific title looks wrong
- short global TTLs mainly create avoidable cache churn

Useful commands:

- `symlinkarr cache invalidate tmdb:12345`
- `symlinkarr cache invalidate anime-lists`
- `symlinkarr cache clear`

## Backup Policy

Symlinkarr backups preserve two layers on purpose:

- a JSON manifest of active symlinks for normal restore flows
- a sibling SQLite snapshot from `backup create`, so operators also have a real database recovery artifact

Current-format manifests are integrity-checked during `backup list` and `backup restore`, so corrupted or tampered backups fail loudly instead of half-restoring.
Restore also stays confined to the configured `backup.path`, so a mistyped absolute path or symlink escape cannot make the restore flow read arbitrary files outside the backup directory.

## Common Commands

```bash
symlinkarr scan --dry-run
symlinkarr scan --library Anime --search-missing
symlinkarr status --health
symlinkarr status --health --output json
symlinkarr cleanup audit --scope anime
symlinkarr cleanup prune --report <REPORT.json>
symlinkarr cleanup remediate-anime --plex-db "<PLEX_DB_PATH>"
symlinkarr repair auto --dry-run
symlinkarr discover list
symlinkarr cache status
symlinkarr cache invalidate tmdb:12345
symlinkarr web
```

If you are running from a source checkout, prepend `cargo run --` to the same commands.

## Why People Use It

- keep a Real-Debrid-backed library usable without hand-sorting files
- keep Sonarr/Radarr-style ID-tagged folders clean
- detect and repair bad symlinks before Plex, Emby, or Jellyfin drift too far
- clean up legacy GemLink or early-Symlinkarr mistakes with preview-first workflows

## Known RC Limits

- anime specials without usable anime-lists numbering hints may still need manual search terms, because many indexers are weak at `S00Exx`-style anime queries

## Docs

- [GitHub Wiki](https://github.com/LAP87/Symlinkarr/wiki)
- [Wiki source: feature guide](docs/GITHUB_WIKI_FEATURES.md)
- [Product scope](docs/PRODUCT_SCOPE.md)
- [CLI manual](docs/CLI_MANUAL.md)
- [API schema](docs/API_SCHEMA.md)
- [RC roadmap](docs/RC_ROADMAP.md)
- [Changelog](docs/CHANGELOG.md)
- [WSL development setup](docs/DEV_SETUP_WSL.md)
