# Symlinkarr

![Rust](https://img.shields.io/badge/Rust-CLI-orange?logo=rust)
![Docker](https://img.shields.io/badge/Docker-ready-2496ED?logo=docker&logoColor=white)
![SQLite](https://img.shields.io/badge/State-SQLite-003B57?logo=sqlite&logoColor=white)
![Web UI](https://img.shields.io/badge/Web-UI-0F766E)
![Plex](https://img.shields.io/badge/Media-Plex-EBAF00)

Symlinkarr is the last-mile library layer for Real-Debrid-backed media setups.

It scans your source mount, matches files to ID-tagged library folders, writes clean symlinks, and keeps operational state in SQLite. It is built for people already running some mix of `Real-Debrid`, `Zurg`/`Decypharr`, `Sonarr`, `Radarr`, `Prowlarr`, and a media library on Plex, Emby, Jellyfin, Kodi, or plain folders.

## What It Solves

- deterministic matching against `{tvdb-*}` and `{tmdb-*}` library folders
- clean symlink creation and update
- dead, stale, or misplaced link detection
- repair and reacquire workflows
- safer cleanup with preview/apply guardrails
- anime-specific reporting and remediation planning
- optional post-mutation invalidation for Plex, Emby, and Jellyfin

No media server is required. Symlinkarr still works as a scan/match/link/cleanup tool with only a source mount and library folders.

## Integrations

Symlinkarr can talk to:

- Real-Debrid-backed mounts such as Zurg and Decypharr
- Sonarr and Radarr
- Prowlarr
- Bazarr
- Tautulli
- TMDB and TVDB
- Debrid Media Manager
- Plex, Emby, and Jellyfin

Plex, Emby, and Jellyfin can now all be enabled together for guarded post-mutation refresh fan-out.

## Quick Start

### 1. Install Symlinkarr

You can run Symlinkarr in three supported ways:

- download a release tarball from [GitHub Releases](https://github.com/LAP87/Symlinkarr/releases) and run the `symlinkarr` binary directly
- build locally with `cargo`
- run it with Docker

Example for a release binary:

```bash
tar -xzf symlinkarr-<version>-linux-amd64.tar.gz
cd symlinkarr-<version>-linux-amd64
./symlinkarr --help
```

### 2. Prepare a config

Start from [config.example.yaml](config.example.yaml).

You need, at minimum:

- one or more library paths
- one or more source paths
- a writable SQLite `db_path`
- TMDB and TVDB credentials if you want full metadata matching

### 3. Validate first

```bash
cargo run -- config validate --output json
cargo run -- doctor --output json
```

### 4. Preview a scan

```bash
cargo run -- scan --dry-run
```

### 5. Run it for real

```bash
cargo run -- scan
```

### 6. Start the web UI

```bash
cargo run -- web
```

Default URL:

```text
http://127.0.0.1:8726
```

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
symlinkarr web
```

## Docker

```bash
docker compose up -d
```

Use [config.docker.yaml](config.docker.yaml) as the container-oriented config template.

## Windows 11 Development

Native Windows runtime is not supported. Use `WSL2` or a Linux container.

Quick start:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev sqlite3 git curl
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
git clone <YOUR-REPO-URL> ~/apps/Symlinkarr
cd ~/apps/Symlinkarr
cargo test --quiet
cargo run -- web
```

Full setup: [docs/DEV_SETUP_WSL.md](docs/DEV_SETUP_WSL.md)

## Current Product Status

Today, Symlinkarr has:

- real Plex, Emby, and Jellyfin invalidation adapters
- multi-backend refresh fan-out
- persisted scan telemetry and per-backend refresh history
- guarded cleanup and anime remediation preview/apply flows
- web UI and JSON API for the main operator workflows

What remains before a real `1.0 RC` is tracked here:

- [docs/RC_ROADMAP.md](docs/RC_ROADMAP.md)

## Docs

- [GitHub Wiki](https://github.com/LAP87/Symlinkarr/wiki)
- [CLI manual](docs/CLI_MANUAL.md)
- [API schema](docs/API_SCHEMA.md)
- [RC roadmap](docs/RC_ROADMAP.md)
- [Media-server adapter plan](docs/MEDIA_SERVER_ADAPTER_PLAN.md)
- [Anime lists integration spec](docs/ANIME_LISTS_INTEGRATION_SPEC.md)
- [RD/DMM resolution spec](docs/RD_DMM_FILE_RESOLUTION_SPEC.md)
- [Changelog](docs/CHANGELOG.md)
