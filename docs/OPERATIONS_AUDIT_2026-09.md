# Operations audit — September 2026

**Scope.** Every external endpoint Symlinkarr calls, its own HTTP surface, CLI/docs drift,
and the logic around them — checked against the *running* stack (all services live-probed
with the real credentials on 2026-09-04) and against the *real* library on disk, not just
against unit tests. Fixes that came out of it are in commits `c0ed98e` and `2d823e2`.

## 1. Endpoint validity — everything Symlinkarr talks to

| Service | Version found | API Symlinkarr uses | Result |
|---|---|---|---|
| Sonarr (×2) | 4.1.1.824 | `/api/v3/{series,episode,episodefile/{id},wanted/missing,wanted/cutoff}` | ✅ current |
| Radarr | 6.5.1.2032 | `/api/v3/movie` | ✅ v3 is still the stable API in Radarr 6 |
| Prowlarr | 10.0.0.33866 | `/api/v1/search` | ✅ current |
| Tautulli | v2.18.1 | `/api/v2` | ✅ |
| Bazarr | — | `api/webhooks/{sonarr,radarr}`, `api/system/{tasks,health}` | ✅ 200 |
| Real-Debrid | premium until 2026-12-30 | `rest/1.0/torrents`, `/torrents/info/{id}`, `/torrents/addMagnet` | ✅ (does **not** use the removed `instantAvailability`) |
| TMDB | — | v3 with `api_key` or bearer read-token | ✅ |
| TVDB | — | `api4.thetvdb.com/v4/login` | ✅ |
| DMM | — | `/api/search/title`, `/api/torrents/{path}` (problem-key auth) | ✅ reachable |
| Decypharr | **2.5.x** (image `cy01/blackhole:beta`, rolling) | `/api/{browse,add,torrents,arrs}` ✅ — `POST /api/repair` + `GET /api/repair/jobs` **removed in 2.3 → 404** | ⚠️ `repair trigger` had been broken since May; **fixed** (`66296bc`: `/api/repair/run` + `/api/repair/status`) |
| Plex / Emby / Jellyfin | — | `/library/sections`, `/System/Info` | ❌ connection refused — only in `config.test-web.yaml` (stale `localhost` entries); **prod has no media server configured** |

Symlinkarr's own surface: 36 HTML routes + 4 htmx partials + `/api/v1/{cleanup/audit,report/anime-remediation}` + the nested scheduler API all return 200 on the fresh build; every `/static/js/*` asset serves; CSP is `script-src 'self'` (only `style-src` keeps `'unsafe-inline'`, for the injected theme variables). `doctor` and `status` are green. CLI subcommands match `docs/CLI_MANUAL.md` (the "missing" cleanup flags live on `cleanup prune`/`remediate-anime`).

**The gap:** `doctor` probes paths, database and config — **not a single external service**. The Status page's "✅ configured" badges mean *present in config*, not *reachable*. A config pointing at a dead Plex passes both.

## 2. What changed externally, and whether the code noticed

| Change | Effect on Symlinkarr | State |
|---|---|---|
| **Real-Debrid keyword filter (≈2026-05-10)** — cached torrents whose names carry WEB-DL/WEBRip/AMZN/NF/CR/YTS/RARBG are refused ("removed … due to copyright infringement") | Measured on the live library (3,000-link random sample): **94 %** of keyword-matching links and **29 %** of the rest are broken → ≈72,600 of 130,224 symlinks. Prod DB agrees: **48,161 active / 68,585 dead**, acquire queue **0**. Detection works (targets vanish, Decypharr quarantines them in `__bad__`, 562 today). Healing does not: any alternative WEB release is filtered too, so RD-based repair is futile for this class | Detected, not healed → see recommendations and `docs/NZB_MIGRATION_ASSESSMENT.md` |
| **Decypharr 2.0 (2026-04-09), running 2.5.1** — hybrid debrid+usenet, SAB emulation at `/sabnzbd/api` (`mode=version` → 4.5.0; `queue`/`history`/`get_config` live), new mount layout `__all__/ torrents/ nzbs/ <provider>/ __bad__/`, `/api/torrents` items now carry `protocol`, `bad`, `providers`, `content_path` | Our client is v1-shaped and compatible for browse/add/torrents/arrs (unknown fields ignored), but the repair endpoints it called were removed in 2.3 (2026-05) — `symlinkarr repair trigger` returned 404 on the live instance until it was ported to `/api/repair/run` + `/api/repair/status`. `DecypharrTorrent` now carries `protocol` ("torrent" / "nzb"). Both configs point at `__all__`, so the mirrors (`torrents/`, `realdebrid/` — 12,081 entries each, identical) are not walked. `config.usenet` is empty: no usenet providers configured yet | Compatible; usenet plan in the NZB doc |
| **FFmpeg 9** — `ffprobe --version` exits 1 | `doctor` reported a working ffprobe as "version check failed" | **Fixed** (`-version`) |
| Radarr 6 / Prowlarr 10 / Sonarr 4.1 | API paths unchanged; new response fields are ignored by serde | OK |
| Decypharr API `instantAvailability` change (2024) | not used | n/a |

## 3. Fixed in this pass

- `doctor`: ffprobe/ffmpeg probed with `-version` (FFmpeg 9 exits 1 on `--version`).
- `doctor`: a source pointed at a Decypharr mount **root** is flagged (mirrored views → files seen several times, links split across views). Guard only — current configs are correct.
- Scanner never descends into Decypharr's `__bad__` quarantine.
- Backup listing: `/backup` took **18.6 s** — `list()` read every `*.json` in a 1.5 GB directory (four 140 MB cleanup-audit reports + eighteen 30 MB safety manifests) on every render. Now sniffs the first 4 KB for the manifest marker (foreign files skipped unread) and caches summaries per (path, size, mtime): **2.8 s** first render, **0.05 s** after.
- Decypharr repair client ported to the 2.3+ API (`POST /api/repair/run`, `GET /api/repair/status`; 409 = sweep already running); `--arr` is accepted but ignored since the sweep is global. `protocol` field added to the torrent model.
- GUI overhaul completed across all pages; deps refreshed (`cargo update`, 72 bumps, `cargo audit` clean); version `1.1.0-rc.9`.

## 4. Recommendations (not done — decisions or larger work)

1. **`doctor --services`**: one status call per configured service (`/api/v3/system/status`, `/api/v1/system/status`, `/api/v2?cmd=get_tautulli_info`, `/identity`, `/sabnzbd/api?mode=version`, RD `/user`, TMDB `/configuration`, TVDB `/login`) and make Status say *reachable*, not *configured*. The probe script used for this audit is a drop-in template.
2. **Dead-link policy under the RD filter.** 59 % of the library is dead and the queue is empty. Decide: (a) enable `search_missing` in prod so the daemon sweeps + requests re-acquisition (today it defaults `false`, `src/commands/scan.rs:285`), (b) route re-acquisition to usenet via Decypharr's SAB client (the NZB doc's workplan), (c) a `cleanup` policy that prunes links dead for > N days so Plex stops showing 68k phantom items. A distinct skip-reason for "provider refused (keyword filter)" would make the why-not report explain the stall instead of showing generic no-result.
3. **Use `__bad__` / `bad: true` as an authoritative dead signal.** Decypharr already knows which torrents the provider removed; repair could consult `/api/torrents` (`bad`) or the `__bad__` listing instead of waiting for a filesystem stat to fail.
4. **Safety-snapshot bloat.** Every daemon scan writes a ~30 MB restore point embedding the full symlink list (18 in one day), sharing one rotation pool with pre-destructive snapshots. Stop embedding symlinks (the SQLite snapshot carries them), or write a summary sidecar, and split the pools.
5. **Absolute symlink targets, no remap layer** (`src/utils.rs:35`) — still the biggest day-one hazard for any mount move; a `path_map` is cheap insurance.
6. Remove the stale `plex/emby/jellyfin: localhost` entries from `config.test-web.yaml`, or point them at the real hosts; Emby/Jellyfin appear unused.
7. Housekeeping: `thousands` filter should accept values by value (so `|length|thousands` chains compile), `Len` for `BTreeMap`, third copy of the `plex_db` resolver in `src/commands/cleanup/plan.rs`, `style-src 'unsafe-inline'` (move theme vars to a nonce or a static sheet).

## 5. Release state

`1.1.0-rc.9` is committed on `feature/review-fixes-and-ui` (the version bump sits mid-branch; the tag should point at the branch head, which also carries the `/backup` and Decypharr repair fixes) with fmt, clippy, tests and audit green — the same gates `release.yml` runs. Publishing = pushing tag `v1.1.0-rc.9` (builds linux-amd64/arm64, pushes `ghcr.io/lap87/symlinkarr:rc`, creates the pre-release). Uncommitted, deliberately left alone: `src/linker.rs` (owner's change to stop persisting `skipped` link events) and `AGENTS.md`.
