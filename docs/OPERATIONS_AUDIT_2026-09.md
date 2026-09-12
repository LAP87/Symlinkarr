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

---

## Follow-up pass — 2026-09-12 (UI/UX walk + logic check against the live library)

Every page was loaded against the real test database and every read-only CLI command run;
a dry-run `scan --library Movies` served as the end-to-end matcher/linker exercise. Fixes are
in `7e253e4`, `866b532` and `2825685`.

### Fixed
| Finding | Where | Fix |
|---|---|---|
| **Dry-run wrote to the database** — a dry-run scan inserted 465 link rows (the "correct symlink on disk, no DB record" backfill was not gated) | `src/linker.rs` | gated on `!dry_run`, regression test |
| **Movie filenames doubled the year** ("0.5 mm (2014) (2014).mkv") whenever the folder-derived title was used (metadata lookup miss/404) — 2,552 links in the library, most of them the film's only link | `src/linker.rs` | year no longer appended when the title already ends with `(YYYY)`; movies gained the same existing-equivalent adoption TV had, so the next scan adopts the misnamed links instead of duplicating them |
| **"Queue 71,216" counted finished jobs** — `active_total` summed `no_result` + `failed` + `completed_unlinked` | `src/db/types.rs`, templates, `status` CLI/JSON | split into *in flight* (65,176) and *needs review* (6,040); badges, Status section, CLI panel and JSON updated |
| "Auto-acquire queue is **blocked**" card with 0 blocked (fired on failed jobs); raw numbers in the copy | `src/web/handlers.rs` | title/message reflect what is there; thousands separators via `utils::format_thousands` |
| Dashboard "Streams 0" while the Status page said Tautulli was unavailable; 900 ms page-load timeout | `src/web/handlers.rs`, `dashboard.html` | 2.5 s timeout; badge shows "Streams —" when the guard is unavailable |
| **Discover auto-ran a 464 s pipeline (4 MB response) on every page load**, and a reload started a second concurrent pass | `discover.html`, `src/web/handlers/admin.rs`, `WebState` | runs only after the scope form is submitted ("Build preview" otherwise); single discover slot per process ("already running" notice); placement rows capped at 1,000 |
| Backup rows "7298 fewer than current" unformatted; doctor showed the probe parent instead of the configured source path | `admin.rs`, `doctor.rs` | formatted; configured path shown |
| **Playback guard blocked every page and then showed "Unavailable / 0 streams"** — Tautulli's `get_activity` takes ~15 s on this host (it waits on Plex), so no page-load timeout could succeed | `src/web/handlers.rs`, `WebState` | answers instantly from a cache (last probe + last successful check), refreshes in a background task with a 20 s budget, shows the last good result with its age when a live probe fails; "Streams —" only when nothing is known |

**Proof.** A second dry-run `scan --library Movies` after the fixes: link rows 89,269 → 89,269 (the first run had added 465), created 139 → 125, updated 29 → 4, `already_correct` 1,804 → 2,119 — the misnamed movie links are adopted instead of duplicated.

### Observed, not changed (decisions or larger work)
- **Metadata cache expiry makes the first scan after 30 days slow.** `api.cache_ttl_hours: 720` had lapsed, so the Movies dry-run re-fetched TMDB metadata for all 6,896 items (`cache_hit_ratio=0%`, match phase 674 s of an 11-minute scan). Consider refreshing entries lazily/incrementally, a longer TTL for released films, or surfacing "cache cold" on the scan page.
- **"Why not" is dominated by expected noise on scoped scans**: a Movies scan reports `matcher_media_shape_mismatch = 93,580` (TV files correctly rejected). Classifying shape mismatches as an expected filter, or labelling them "not a movie (TV episode)", would make the top-reasons list actionable.
- **The 2,552 misnamed movie links keep their names.** Adoption prevents duplicates; renaming them to the canonical name needs a small `repair rename` (or a one-off script): for each active movie link whose filename ends `(YYYY) (YYYY)`, `rename` the symlink and update `links.target_path`.
- **Discover is still a synchronous multi-minute request.** The right shape is a background job with progress (like scans), served from a snapshot.
- The 465 rows the earlier dry-run inserted into `data/symlinkarr.db` describe real on-disk links and are accurate; they were left in place.

### Round 2 — 2026-09-12 (decisions taken)
| Item | Done |
|---|---|
| Metadata cache TTL | `api.cache_ttl_hours: 0` = never expires, now the default and set in the shipped configs; stored rows are aligned at startup. Negative lookups keep a 7-day TTL and anime-lists mappings keep their own cap. |
| "Why not" noise | Media-shape mismatches and already-correct links are filtered out of highlights, top-reason lists, groups and the CLI summary; shown as one "N filtered" figure. |
| Doubled-year movie names | `repair normalize-names --apply` run against the library: **3,058 renamed, 261 untracked orphans renamed, 10 duplicates removed**; 24 records had no file at either name (dead) and 3 + 6 on-disk cases keep a canonical file serving a *different* source (two versions — cleanup's call). Prod's database needs the same command once after the upgrade to reconcile its records (the files are already renamed). |
| Discover | Background job with running banner (scope, start, elapsed), last outcome and a kept snapshot; one pass at a time; rows capped at 1,000. |
