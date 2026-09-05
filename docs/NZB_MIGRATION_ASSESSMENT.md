# NZB / Usenet Migration Assessment — Hybrid via Decypharr v2

**Status:** decision made. This document is now the integration plan for making Symlinkarr
usenet-aware on the Decypharr mount. The earlier three-way analysis (usenet mount / hybrid /
classic SAB-to-local-disk) is preserved verbatim in **Appendix A** because much of its evidence
still applies; it is no longer the framing.

**Method.** Symlinkarr claims were checked against the working tree at `86b3c0d`
(branch `feature/review-fixes-and-ui`, plus uncommitted edits; the only local diff in
src/linker.rs is a 5-line insertion at :1034+, so cited linker lines are stable). Decypharr
claims were checked against a local clone of github.com/sirrobot01/decypharr at tag `v2.5`
(commit `0dd1cbb`, 2026-08-11) **and** the upstream `beta` branch (HEAD `1f7a62e`, 2026-09-04),
the repo docs, and live reads of the owner's own instance on 2026-09-04. No file under `src/` was
modified.

**Labels used below.**

| Label | Meaning |
|---|---|
| **VERIFIED** | Read in primary source (Decypharr Go code / docs at the cited path, Symlinkarr Rust at the cited line) or observed live on the owner's instance. |
| **INFERRED** | Reasoned from verified facts; not directly confirmed. |
| **MUST-TEST** | Cannot be settled from source; the owner must run the listed check. Every MUST-TEST item is collected in [Verify on your own instance](#verify-on-your-own-instance). |

Where an earlier recon note and the adversarial verification disagree, the verification wins
and the recon claim is not repeated here.

---

## Decision: hybrid via Decypharr v2 usenet

**Settled.** The Decypharr mount stays. Acquisition moves toward usenet. Decypharr itself is the
usenet client: since v2.0 (2026-04-09) it parses NZBs, streams articles over NNTP, exposes the
result on the same mount tree as debrid content, and presents a SABnzbd-compatible API to
Sonarr/Radarr (VERIFIED — release notes for v2.0; `pkg/server/server.go:123`
`routes["/sabnzbd"] = sb.Routes()`; `pkg/manager/entry.go:16-19` group constants incl. `nzbs`).

**Why this collapses the old fork.** The original document's fork A (usenet mount) and fork B
(hybrid) were distinct because a usenet mount was assumed to be a *second* product (NzbDAV,
AltMount) beside Decypharr. With Decypharr serving both protocols, A and B are the same path: one
mount, one `sources[]` entry, one WebDAV probe endpoint, one queue API. Fork C
(SAB/NZBGet-to-local-disk with arr-managed imports) is not being pursued; its analysis — the
ownership collision, the presentation-tree topology, hardlinks — is retained in Appendix A only
because parts of it (path namespace, sweep gating, sample filtering, unattended repair) are
still live risks and are re-framed below.

**Why the trigger is still real.** Measured on the owner's library: ~56 % of 130,224 symlinks are
broken (94 % of links whose names match WEB-DL/WEBRip/AMZN/YTS/RARBG, 29 % of the rest), all with
targets under `__all__/`. Debrid content still vanishes; dead-link detection and repair keep their
original justification. The usenet half of the hybrid exists to replace what vanishes.

**What the owner actually runs (VERIFIED, live 2026-09-04).**

| Fact | Value | Evidence |
|---|---|---|
| Decypharr version | `v2.5.1`, channel `beta` — **not a tagged release** | `GET /version`; `.github/workflows/beta-docker.yml` builds every push to `beta` as latest-tag+PATCH+1 (`v2.5` → `v2.5.1`); `auth_token_only` in `GET /api/config` exists only on the beta branch |
| Mount type | `external_rclone`, `dir_cache_time: 5m` | `GET /api/config` `.mount` |
| Library root (`mount.mount_path`) | `/mnt/decypharr/realdebrid` | `GET /api/config`; `ls` shows `__all__ __bad__ nzbs realdebrid torrents version.txt`; `version.txt` = `v2.5.1-beta` |
| `folder_naming` | `original_no_ext` | `GET /api/config` |
| Usenet providers | **none** (`config.usenet` = `{}`) | `GET /api/config` |
| Debrid clients | `realdebrid` only | `GET /api/config` |
| Arrs known to Decypharr | 6 | `GET /api/arrs` |
| Decypharr repair | enabled, `source=arr`, 04:00, `auto_repair=true`, arrs sonarr/radarr/sonarr-anime | `GET /api/config` `.repair` |
| `default_download_action` | `symlink` | `GET /api/config` |
| Entries today | `__all__` = `torrents` = `realdebrid` = 12,081; `nzbs/` empty; `__bad__` 562 | live `ls`/browse |

The beta build matters: several v2.5 behaviours (NZB parse timing, SAB auth, STRM action, repair
internals) differ on beta, and "v2.5.1" does not pin a commit — the image is rebuilt on every
push to `beta`. See MUST-TEST item 1.

---

## Verdict

With Decypharr v2 as the mount, Symlinkarr's scan/match/link/repair core carries over
**without a topology change**: NZB items land in the same `__all__` group at the same absolute
path shape (`{mount_path}/__all__/{folder}/{file}`) as debrid items, the WebDAV readiness probe
already composes exactly the path Decypharr's router serves, `GET /api/torrents` already lists
NZB entries alongside torrents with every field `DecypharrTorrent` reads, and NZB items are never
visible on the mount while partially parsed. The whole usenet half is a `sources[]` no-op from
the scanner's point of view.

What must be built splits into three tiers:

1. **Already broken today, independent of usenet.** `symlinkarr repair --trigger` posts to
   `POST /api/repair`, which Decypharr removed in v2.3 (2026-05-14); on the owner's build it
   404s. Its request body never matched Decypharr's JSON keys in any version. And the dead-link
   sweep — the one subsystem the 56 %-broken library needs most — only runs inside `scan` when
   `daemon.search_missing` is true (default false). Fix both before touching usenet.
2. **Needed before usenet content flows.** Point `sources[].path` at exactly one view
   (`/mnt/decypharr/realdebrid/__all__` — the on-disk configs already say this; the live
   datapoint disagrees, see MUST-TEST 2); add `protocol` to `DecypharrTorrent`; decide the
   readiness-probe timeout against real NNTP first-byte latency; add a parent-directory fallback
   to the filename parser for obfuscated raw posts.
3. **Only if Symlinkarr acquires usenet itself.** Prowlarr protocol switch, `nzbURLs`
   submission, UUID-keyed tracking, protocol-scoped queue guards, protocol-aware ranking. The
   recommendation below is to **defer this** and let Sonarr/Radarr acquire via Decypharr's SAB
   client, so tier 3 is a follow-up, not a prerequisite.

What is now moot: the local-disk topology work (presentation tree, source/library overlap,
moved-vs-deleted inode tracking, hardlinks), the "retire the FUSE/`PathHealth` machinery" list,
and the fork-C runbook. DMM stays moot for the usenet half (it is a debrid-cache hash oracle) but
remains valid for the torrent half if that is ever re-enabled.

**Recommendation on acquisition.** Let Sonarr/Radarr do usenet acquisition through Decypharr's
`/sabnzbd` client, and keep Symlinkarr to scan/match/link/repair plus a one-POST-per-item
"ask the arr to search" handoff when a dead link has no replacement copy on the mount. Reasons:
(a) the arrs already implement search → grab → failed-download handling → blocklist → retry for
usenet end-to-end; (b) Decypharr's own repair sweep (enabled on this instance) already deletes
broken files, blocklists them in the arr and re-searches (`pkg/manager/repair_sweep.go:639-686,
763-801` at v2.5; `pkg/repair/` on beta); (c) every native-acquire piece in Symlinkarr is
torrent-shaped in exactly four places and none of them is architectural, so the option stays
open and cheap to build later; (d) the SAB shim and `/api/add` give the arr-driven and
Symlinkarr-driven paths the same identity space (`nzo_id` = `info_hash` = one UUID), so nothing
is lost by deferring. Build native usenet acquisition only if the arr path proves too slow to
backfill 70,000 broken links.

---

## What works, what must be built, what is moot

| Subsystem | Under hybrid Decypharr v2 | Label | Evidence |
|---|---|---|---|
| Source walk over `__all__` | Works. NZB items appear in `__all__` with no protocol filter; also in `nzbs/`; **not** under `realdebrid/`. `__bad__` is pruned by the walk. | VERIFIED | `pkg/manager/entry.go:215-243` (`case EntryAllFolder` iterates `ForEachMeta`, no `Protocol`/`Bad` filter), `:244-296` (`nzbs`/`torrents` filter `meta.Protocol`), `:321-349` (provider branch filters `meta.Provider == group`; NZB `ActiveProvider = "usenet"`); Symlinkarr `src/source_scanner.rs:91-92, :161` |
| Path shape | Same for both protocols: `{mount_path}/__all__/{folder}/{file}`; NZB folders are flat (no season subdirs). | VERIFIED | `pkg/manager/entry.go:62-64` `GetTorrentMountPath`; `pkg/usenet/parser/rar.go:181-219` (RAR members exposed under `path.Base`) |
| Folder name for an NZB item | `path.Clean(RemoveExtension(OriginalFilename))` under the owner's `original_no_ext`; `OriginalFilename` = `.nzb` filename minus `.nzb`, invalid chars stripped. With Sonarr/Radarr that is the release title. `RemoveExtension` only strips known media suffixes, so `.x264-GRP` survives. | VERIFIED (rule) / MUST-TEST (observed name) | `pkg/storage/types.go:668-686`; `pkg/manager/usenet.go:39-41`; `internal/utils/regex.go:43-57`; `pkg/usenet/parser/misc.go:80-90`. **Beta caveat:** at submit time `Name`/`OriginalFilename` are the *raw* filename (extension not yet stripped); the parsed name is set later by `processNZBJob` (`git diff v2.5 origin/beta -- pkg/manager/usenet.go`). |
| File names inside an NZB item | RAR members: internal filename (basename). Raw posts: subject-derived `BaseName + ext` — **obfuscated subjects yield obfuscated file names**. | VERIFIED (RAR) / INFERRED (raw) | `pkg/usenet/parser/rar.go:191-219`; `parser.go:763-782` |
| Partially assembled files visible to the scanner | **Never.** `AddNewNZB` writes only to the `queue` store; mount/WebDAV/share listings read only the `entries` store; the entry reaches `entries` solely in `processAction`, after the full parse, a sampled availability gate and a 512-byte head-signature check. Nothing is ever assembled on the mount — files are virtual NNTP streams; sizes are fixed at parse time. | VERIFIED | `pkg/manager/usenet.go:63`; `pkg/storage/storage.go:17`; `pkg/storage/entry.go:136-149`; `pkg/manager/processor.go:313-343`; `pkg/usenet/usenet.go:516-577`; `pkg/usenet/verify.go:20` |
| … but a visible file can still fail mid-read | Only ~1 % of segments were STAT-probed at import (`import_availability_sample_percent=1` live) and only the first 512 bytes were read. Missing middle segments surface at read time or in a later repair sweep. | VERIFIED (gate depth) / MUST-TEST (FUSE behaviour) | `pkg/usenet/usenet.go:578-606`; live config |
| WebDAV readiness probe | Works unchanged. From a source rooted at `__all__` it probes `GET /webdav/__all__/<folder>/<file>` with `Range: bytes=0-0`, which is exactly Decypharr's `/{group}/{torrent}/{file}` route. For an NZB this forces an NNTP article fetch; the 2,500 ms default timeout maps a slow first byte to `Unreadable` → link **skipped** (not bypassed). | VERIFIED (composition) / MUST-TEST (latency) | `src/linker.rs:97-123, :125-174`; `src/api/decypharr.rs:191-256`; `src/config/defaults.rs:317-319`; `pkg/server/webdav/handler.go:66-70` |
| WebDAV probe auth | The probe sends `Authorization: Bearer` (`src/api/decypharr.rs:235-237`); the WebDAV handler only honours Basic auth via `config.VerifyAuth` when `use_auth && enable_webdav_auth`. If the owner turns WebDAV auth on, every probe returns 401 → `Unreadable` → every link skipped. | VERIFIED | `pkg/server/webdav/handler.go:156-164` |
| `GET /api/torrents` | Still live, same contract v2.0→beta; lists the **queue** store for both protocols (`ProtocolAll`); each item carries `"protocol": "torrent"\|"nzb"` (the string is `nzb`, **not** `usenet`; `usenet` is the `active_provider` value). Symlinkarr deserialises every field it uses; it lacks only `protocol`. `limit` > 100 is clamped to 20. | VERIFIED | `pkg/server/api.go:287-384`; `internal/config/config.go:27-29`; `pkg/storage/types.go:43-96`; `src/api/decypharr.rs:68-101` |
| NZB lifecycle as seen in `/api/torrents` (beta) | submit → `status=queued, state=downloading, progress=0, name=<raw filename>, size=0, active_provider=""` → async parse → `status=downloading, name=<parsed>, size` → `status=downloaded, progress=1` → post-action → `state=pausedUP, is_complete=true`. Failure: `state=error, status=error, last_error`. **With `action=none` the entry is deleted from the queue immediately after completion** — it vanishes from `/api/torrents` and SAB history. Failed entries with `progress==0` are auto-purged after `remove_stalled_after`. | VERIFIED (source) / MUST-TEST (no NZB has ever existed on this instance) | `git diff v2.5 origin/beta -- pkg/manager/usenet.go`; `pkg/manager/downloader.go:121-125`; `pkg/manager/queue.go:145-164`; `pkg/storage/types.go:401-423` |
| `is_failed()` / `is_complete` semantics | Read real fields with unchanged vocab: state `downloading\|pausedDL\|pausedUP\|error`, status `queued\|downloading\|downloaded\|error`. | VERIFIED | `pkg/storage/types.go:28-31`; `pkg/debrid/types/status.go:6-11`; `src/api/decypharr.rs:104-123` |
| `POST /api/add` (Bearer) | Still accepts `urls`/`arr`/`action` as Symlinkarr sends them. Also accepts `nzbURLs` (newline-separated, fetched server-side with a User-Agent) and multipart `nzbFiles`. Response is always HTTP 200 with `[{id,status,error}]`; per-item failure is `status:"error"`. For an **NZB** `id` = the UUID that becomes `info_hash`/`nzo_id`. For a **torrent** `id` is a throwaway UUID, **not** the infohash. An `.nzb` URL sent via `urls` fails bencode parsing. | VERIFIED | `pkg/server/api.go:37-208`; `pkg/manager/queue.go:30-55, :68-82`; `internal/utils/magnet.go:74-81` |
| `usenet not configured` | Every NZB submission on this instance today returns `status:"error"` because no provider is configured. | VERIFIED | `pkg/manager/usenet.go:17-20`; live `config.usenet = {}` |
| `action=strm` | Removed on beta (STRM is now a separate reconciler, `POST /api/strm/regenerate`); `action=strm` falls to the symlink branch. Symlinkarr sends `none`, so unaffected. | VERIFIED | `git diff v2.5 origin/beta -- internal/config/config.go pkg/manager/downloader.go` |
| Duplicate NZB submissions | Storage is keyed by `InfoHash` (fresh UUID per submit) with a plain overwrite; no name-based dedup found → duplicates likely create separate entries. Beta adds a "hearsay" layer (`NZBClaimedIncomplete`/`ReportNZB`) that was not traced. Symlinkarr's pre-submit token match is the only guard. | INFERRED / MUST-TEST | `pkg/storage/entry.go` `AddOrUpdate`/`AddQueue`; `pkg/manager/usenet.go` |
| `POST /api/repair`, `GET /api/repair/jobs` | **Removed in v2.3** (commit `3fc8e4d`, 2026-05-11). `symlinkarr repair --trigger` bails with `Decypharr repair error 404`. Replacement: `POST /api/repair/run` (JSON `{ignore_last_checked, force, auto_repair, unrestrict_link, verify_content, protocol: ""\|all\|both\|torrent\|nzb}` → `200 {"run_id"}`, `409` if a sweep is running, `503` if service nil); `GET /api/repair/status` (`{enabled,next_run_at,active_run,last_run,health_counts}`); `GET /api/repair/runs[/{id}]`; per-item `POST /api/repair/recheck/media {arr, media_id, fix}` and `POST /api/repair/health/{name}/check`. No per-arr targeting on `/run`. | VERIFIED | per-tag `git grep` of `pkg/server/routes.go`; `pkg/server/api.go:669-751`; `src/commands/repair.rs:237`; `src/api/decypharr.rs:284-322` |
| Symlinkarr's `RepairRequest` body | Serialises `arr_name`/`media_ids`/`auto_process`; Decypharr (v1.1.6–v2.2) read `arr`/`mediaIds`/`autoProcess`. No field ever matched, so on v2.0–2.2 the trigger ran a **detect-only sweep over all arrs** regardless of `--arr`. | VERIFIED | `src/api/decypharr.rs:40-47`; v2.0 `pkg/server/server.go:59-70`, `pkg/server/api.go:215-247` |
| `GET /api/browse/{group}` | Returns a paginated object `{entries,total,page,limit,total_pages,current_dir,parent_dir,current_kind}`; Symlinkarr's `browse_group` expects a bare array and cannot parse it. Dead code (no callers); `test_parse_browse_entry` encodes the wrong shape. | VERIFIED | `pkg/server/api_browse.go:21-44, :163-235`; `src/api/decypharr.rs:260-280, :474-501` |
| `GET /api/arrs` | Unchanged; `DecypharrArr{name, host}` parses. | VERIFIED | `pkg/arr/arr.go:34-42` (beta) |
| Bearer auth on `/api/*` | Unchanged (`Bearer`/`Token` prefix); 401 JSON when auth on and token missing; 503 JSON until the setup wizard completes. | VERIFIED | `pkg/server/auth.go:47-77`; `pkg/server/middlewares.go:13-59, :75-105` |
| Mount refresh after import | Decypharr issues rclone RC `vfs/forget` + `vfs/refresh` for `refresh_dirs`, default `["__all__"]` only. With `dir_cache_time=5m`, `nzbs/`, `realdebrid/` and `__bad__/` can lag up to 5 min; `__all__` should not. Decypharr's own symlink action polls the mount up to 30 min for files to appear — primary-source confirmation that mount visibility lags catalog visibility under rclone. | VERIFIED (mechanism) / MUST-TEST (external rclone honours RC) | `pkg/mount/external/manager.go:52-54`; `internal/rclone/rclone.go:95-125`; `pkg/manager/mount.go:47-56`; `pkg/manager/downloader.go:35-37, :232-272` |
| `__all__` contains Bad items | Yes — `__all__` does not filter `meta.Bad`; a broken item is listed in both `__all__` and `__bad__`. NZB entries are never flagged Bad (the two `Bad=true` setters are torrent-only paths); broken NZBs are deleted by the repair sweep instead, so their folders disappear. | VERIFIED | `pkg/manager/entry.go:215-243, :297-320`; `pkg/manager/fixer.go:175`; `pkg/manager/link/service.go:210`; `pkg/manager/session.go:560-579` |
| Multi-season / EntryItem merge | Multi-season split creates queue entries only, no extra mount folders. Entries computing the same folder name (same release as torrent *and* NZB) are merged into one folder with the union of files. | VERIFIED / INFERRED (cross-protocol collision untested) | `pkg/manager/downloader.go:88-105`; `pkg/storage/entry.go:209-247` |
| Prowlarr | Hard-pinned to `indexerIds=-2` (torrent only). `protocol` is deserialised but never read; `best_url()` already falls back to `download_url`. `-1` = all usenet is Prowlarr convention, not verified here. | VERIFIED (Symlinkarr) / MUST-TEST (Prowlarr semantics) | `src/api/prowlarr.rs:43-48, :61-64, :102` |
| Idempotency key | `acquisition_jobs.request_key` is `UNIQUE` and derived from `media:{id}` / `episode:{id}:{s}:{e}` / `symlink:{path}` — never from a hash. `info_hash` is a nullable tracking column. No schema change needed for usenet. | VERIFIED | `src/auto_acquire.rs:76-106`; `src/db/migrations.rs:482-501`; `src/auto_acquire/queue.rs:82` |
| Queue guards | `Failing` blocks the whole arr category on any failed incomplete entry; `Capacity` counts every incomplete entry against `max_in_flight` (default 3). Both are protocol-blind. Once the arrs push NZBs into the same categories, an NZB failure stalls Symlinkarr's torrent acquisition and NZBs eat the debrid cap. | VERIFIED | `src/auto_acquire.rs:934-972`; `src/auto_acquire/queue.rs:300-310` |
| `release_completed` (entry vanished from queue) | Step 1 (`rd_torrents` by hash) can never match an NZB UUID. Step 2 checks `source.path.join(release_title)` — correct for a source rooted at `__all__`, dead for a source rooted at the mount root (items live one group deeper), and for NZBs it compares against the Prowlarr title rather than Decypharr's `Entry.Name`. | VERIFIED | `src/auto_acquire.rs:1053-1076`; `src/db/cache.rs:168-178` |
| `__bad__` quarantine handling | Walk prunes `__bad__` (`is_quarantine_dir`). Fine for NZBs (never Bad). | VERIFIED | `src/source_scanner.rs:91-92, :161` |
| DMM | Debrid-cache infohash oracle; no usenet analogue. Bypass for the usenet path. | VERIFIED | `src/api/dmm.rs:38-43`; `src/auto_acquire/dmm.rs:281-283` |
| FUSE/`PathHealth`/ENOTCONN machinery, repair candidate scorer | **Load-bearing. Keep.** The mount is still remote, revocable and many-copies. | VERIFIED | `src/utils.rs:238-345`; `src/repair.rs:632-700` |

---

## The Decypharr v2 usenet surface

### Build identity (beta, not v2.5)

- **VERIFIED.** `GET /version` → `{"version":"v2.5.1","channel":...}`. No tag `v2.5.1` exists;
  `.github/workflows/beta-docker.yml` derives it from the latest tag with PATCH+1 and sets
  `CHANNEL=beta`. `auth_token_only` appears in the owner's `GET /api/config` and exists only on
  the `beta` branch. The owner runs beta.
- **VERIFIED.** `origin/beta` is 298 files / +33k lines ahead of `v2.5` and changes: NZB
  ingestion (async parse, staged file, initial `status=queued`), SAB auth (API token accepted as
  `ma_password`), STRM (action removed), repair (moved to `pkg/repair`, `skip_nzb_repair`
  removed), new routes `/api/arr/reacquire*`, `/api/arr/index*`, `/api/arr/bindings`,
  `/api/strm/regenerate`, a `/stream` mount.
- **MUST-TEST 1.** Record the running image digest / build date. "v2.5.1" does not pin a commit.

### Mount layout (VERIFIED, live + source)

```
/mnt/decypharr/realdebrid/          <- mount.mount_path (fuse.rclone, ro)
├── __all__/      every entry, both protocols, Bad included   <- Symlinkarr source (configs)
├── __bad__/      entries with Bad=true (torrent-only path)   <- pruned by the walk
├── nzbs/         protocol == "nzb" view
├── torrents/     protocol == "torrent" view
├── realdebrid/   provider view (meta.Provider == "realdebrid"); NZBs are NOT here
└── version.txt
```

- `realdebrid/__all__` no longer exists (`ls` → ENOENT); `realdebrid/` is a provider group whose
  children are items.
- No category (`sonarr/`, `radarr/`) folders exist on the mount. The `/mnt/decypharr/sonarr` tree
  in `docs/guides/usenet/sabnzbd.mdx` describes `SavePath = {download_folder}/{category}` (where
  Decypharr writes *its own* symlinks), not the mount. Stale docs.
- `/api/browse/` reports `kind: system|provider|virtual` per root entry; virtual folders are
  user-named and cannot collide with the five reserved names.
- Timing: DFS (not used here) resolves in ~1 s; external rclone depends on RC refresh of
  `__all__` and `dir_cache_time=5m` for everything else.

### `GET /api/torrents` — the in-flight ledger (VERIFIED)

Owner's live sample keys: `action, active_provider, added_on, bad, bytes, category, completed_at,
content_path, created_at, files, info_hash, is_complete, magnet, mount_path, name,
original_filename, progress, protocol, providers, save_path, seeders, size, speed, state, status,
updated_at` — this matches `storage.Entry`'s non-omitempty JSON tags exactly, which independently
confirms the beta build serialises the same struct.

- NZB rows will carry `protocol: "nzb"`, `active_provider: "usenet"` (after parse),
  `info_hash: "<uuid-v4>"`, `category: <arr name>`.
- **Arr-driven NZBs (via `/sabnzbd`) and Symlinkarr-driven NZBs (via `/api/add`) land in the
  same list under the same category** — so Symlinkarr's `max_in_flight` counts the arrs' usenet
  activity too.
- Absent from the list: completed entries submitted with `action=none` (deleted on completion),
  failed entries older than `remove_stalled_after`, and — on the qBittorrent shim
  `/api/v2/torrents/info` — all NZBs (it filters `ProtocolTorrent`).
- `MUST-TEST 5`: none of this has been observed live; no NZB has ever existed on this instance.

### `POST /api/add` — NZB submission without the SAB shim (VERIFIED)

```
POST {url}/api/add            Authorization: Bearer <api_token>      multipart/form-data
  nzbURLs      newline-separated NZB download URLs (fetched server-side, plain GET + User-Agent)
  nzbFiles     one or more .nzb files
  arr          Decypharr Arr name  -> Entry.Category and SavePath
  action       symlink | download | none        (strm removed on beta)
  callbackUrl  optional; payload {hash,name,status,event,category,debrid:"usenet",content_path,error?,message}
-> 200 [ {"id":"<uuid>","status":"success"|"error","error":"...","arr":{...incl. token...},"magnet":...} ]
```

- Same client, same auth, same response type Symlinkarr already parses (`ImportRequest`
  tags `id`/`status`/`error` confirmed at `pkg/manager/queue.go:30-49`).
- Capture `id` **only for NZB submissions**. For torrents it is a random UUID unrelated to the
  infohash (`NewTorrentRequest`, `pkg/manager/queue.go:51-55`).
- The response echoes the Arr including its API `token` — do not log it.
- NZB URL must be reachable from the Decypharr process with no auth beyond what is in the URL
  (Prowlarr proxy links embed `apikey=`). MUST-TEST 9.

### `/sabnzbd/api` — what the arrs will use (VERIFIED)

- Mounted at `{url_base}/sabnzbd`, single endpoint `GET|POST /sabnzbd/api`, dispatched on
  `mode=`. Live: `mode=version` → `{"version":"4.5.0"}`; `mode=queue`, `history`, `get_config`
  200; `/api?mode=` 404.
- Modes: `queue`, `history`, `config`/`get_config`, `status`/`fullstatus`, `addurl`, `addfile`,
  `version`, `get_cats`, `get_scripts`, `get_files`. Delete is `mode=queue&name=delete&value=<id>[,..]`
  (or `value=failed`), also under `mode=history`. `pause`/`resume` are no-ops. Bare
  `mode=delete` (as the docs table says) → 404. `output=` is ignored; everything is JSON.
- `addfile`: multipart field **`name`**, POST only. `addurl`: URLs in the **query-string**
  `name` param, POST only. Optional `action=`. Category comes from `cat=`; auth lookup uses
  `category=`; send both.
- Success: `200 {"status":true,"nzo_ids":[...]}`; total failure: **HTTP 500**
  `{"status":false,"error":...}` (real SAB returns 200). On beta an unparseable NZB is
  *accepted* and fails later (async parse).
- `mode=queue` lists only `protocol=nzb && state=downloading`; slot status `Downloading`
  (`Queued` while `status=queued`), `Completed`, `Failed`. `mode=history` = `pausedUP` +
  `error` entries; `limit` is ignored. `get_files` 404s until the item is in main storage.
- **Auth is not the Bearer token.** The router is outside `authMiddleware`. `ma_username`/
  `ma_password` are tried as (Arr host URL, Arr API key), else (UI username, UI password) when
  `use_auth` is on. **Beta additionally accepts Decypharr's API token as `ma_password`.** With
  `use_auth` off nothing is required — and `mode=get_config` then returns NNTP provider
  credentials in plaintext to anyone who can reach the port. Turn `use_auth` on before adding a
  provider.
- SAB `nzo_id` == `/api/torrents` `info_hash` == `/api/add` `id` for the same NZB.
- A SAB delete calls `Queue().Delete(id, true, nil)` on beta — it **removes the download/symlink
  folder** `{download_folder}/{category}/{name}` for that item.
- Recommendation: Symlinkarr should never call `/sabnzbd`; the arrs use it, Symlinkarr uses
  `/api/add` + `/api/torrents`.

### Repair API (VERIFIED — already broken)

See the table above. Concretely for Symlinkarr:

- `src/api/decypharr.rs:284-322 trigger_repair` → must target `POST /api/repair/run`
  (global sweep, optional `protocol`) and poll `GET /api/repair/status` or
  `GET /api/repair/runs/{id}`. Per-item equivalents are `POST /api/repair/recheck/media
  {arr, media_id, fix}` / `POST /api/repair/health/{name}/check`.
- `get_repair_jobs` and `browse_group` are dead code with removed/wrong shapes: delete.
- Decypharr's own sweep already probes NZB files (STAT sample + head signature), deletes
  broken ones, blocklists in the arr and re-searches; when every file in an entry is broken and
  the arr call succeeded, the entry is deleted — **its folder disappears from `__all__`**, which
  Symlinkarr's dead-link sweep then sees as a vanished target.

### Visibility timing (VERIFIED)

```
submit ─► queue store (invisible on mount; /api/torrents status=queued|downloading)
       ─► async parse + 1% STAT sample + 512-byte head check
       ─► processAction: entries store + RefreshEntries + rclone vfs/refresh __all__
              ▲ item now visible in __all__/ and nzbs/ ; SAB still says "Downloading"
       ─► post-action (symlink | download | none)
       ─► state=pausedUP / is_complete  (SAB "Completed"; with action=none: deleted from queue)
```

Symlinkarr can never link a half-parsed NZB. It *can* link a fully-visible NZB whose unsampled
segments are missing; that is what the readiness probe and the repair sweep are for.

### Verify on your own instance

Every item below is something the source could not settle. Run these before relying on the
corresponding claim. `$D` = `http://127.0.0.1:8282`, `$T` = the Decypharr API token Symlinkarr
uses.

| # | What to establish | Command / check | Why it matters |
|---|---|---|---|
| 1 | Which beta commit is running | `docker inspect <decypharr> --format '{{.Image}} {{.Created}} {{json .Config.Labels}}'` and note any `org.opencontainers.image.revision`; otherwise record the image digest and the container's create date. | Beta rebuilds on every push under the same "v2.5.1"; the async-parse and token-auth behaviours above are keyed to `origin/beta@1f7a62e`. |
| 2 | Which path Symlinkarr actually scans | `symlinkarr config` (prints effective config) and `symlinkarr doctor`; look for a failing `source:<name>:layout` check. The on-disk configs (`config.yaml:27`, `config.local.yaml:25`, `config.docker.yaml:25`) say `/mnt/decypharr/realdebrid/__all__`; the live datapoint supplied during recon said the root. | Root-pointed source = every item seen 3–4 times (`__all__`, `torrents`/`nzbs`, `realdebrid/`) and `release_completed`'s mount check goes dead. Must be `__all__`. |
| 3 | Auth flags | `curl -s $D/api/config \| jq 'paths(..) as $p \| select($p[-1] \| tostring \| test("use_auth\|webdav_auth\|auth_token_only")) \| {($p\|join(".")): getpath($p)}'` — if `use_auth`/`enable_webdav_auth` do not surface here, read Settings → Auth in the UI. | `enable_webdav_auth=true` makes Symlinkarr's Bearer-only probe 401 → every link skipped. `use_auth=false` leaves `/sabnzbd/api?mode=get_config` (NNTP creds) open. |
| 4 | SAB history shape | `curl -s "$D/sabnzbd/api?mode=history&output=json"` — expect `version` and `paused` keys inside `history`. | Source (v2.5 and beta) always emits them; the recon transcript showed only `slots`. If they are genuinely absent, the binary is not built from any commit that was read. |
| 5 | Real NZB lifecycle | After configuring one NNTP provider: `curl -s -H "Authorization: Bearer $T" -F 'nzbURLs=<one NZB URL>' -F 'arr=radarr' -F 'action=symlink' $D/api/add`, take `id`, then loop `curl -s -H "Authorization: Bearer $T" "$D/api/torrents?search=<id>" \| jq '.torrents[] \| {protocol,status,state,progress,is_complete,name,size,active_provider,last_error}'` every 2 s. Then `ls /mnt/decypharr/realdebrid/__all__/ \| grep -i <name>` and the same for `nzbs/`; time both. | Confirms `protocol:"nzb"`, the beta `queued → downloading → downloaded → pausedUP` sequence, the actual folder name under `original_no_ext`, and the rclone-refresh lag between `__all__` and `nzbs/`. |
| 6 | Readiness-probe first-byte latency | For the NZB above: `curl -s -o /dev/null -r 0-0 -w 'connect=%{time_connect} ttfb=%{time_starttransfer} total=%{time_total}\n' "$D/webdav/__all__/<folder>/<file>"` (add `-u user:pass` if WebDAV auth is on). Repeat 5×, cold and warm. | Compare against `symlink.source_probe_timeout_ms` (2500). Above it, links are skipped as `source_unreadable_before_link`. |
| 7 | Read behaviour on missing segments | Pick an old NZB with known-missing articles; `dd if=/mnt/decypharr/realdebrid/__all__/<folder>/<file> bs=1M skip=<mid> count=1 of=/dev/null`; observe EIO vs short read vs hang. | Decides whether the repair sweep or Plex playback is the first thing to notice a 99 %-unsampled bad item. |
| 8 | Duplicate submission | Submit the same NZB URL twice via `/api/add`; check whether `/api/torrents` shows two rows and whether `__all__` shows one merged folder. | Symlinkarr's pre-submit token match is the only double-grab guard if Decypharr does not dedup. |
| 9 | Prowlarr semantics and reachability | `curl -s "http://<prowlarr>/api/v1/search?query=<title>&indexerIds=-1&apikey=<key>" \| jq '.[0] \| {protocol,downloadUrl}'`; then from *inside the Decypharr container* `curl -sI '<that downloadUrl>'` → expect 200. | Confirms `-1` = usenet, the `protocol` string, and that Decypharr can fetch Prowlarr's proxied NZB links unauthenticated. Only needed if Symlinkarr acquires usenet itself. |
| 10 | Arr refresh with `cat=` only | With Sonarr's SAB client configured against Decypharr, grab one episode; in Decypharr logs confirm a `RefreshMonitoredDownloads` POST to Sonarr after completion. | `authenticate()` may register the arr under an empty name when only `cat=` is sent, in which case Decypharr cannot trigger the arr refresh. |
| 11 | External rclone honours Decypharr RC refresh | After item 5, compare `ls __all__` (should show the new folder within seconds) with `ls nzbs/` (may lag up to `dir_cache_time`). Check rclone logs for `vfs/refresh`. | Symlinkarr scans `__all__` only, so lag elsewhere is cosmetic — unless RC auth fails and `__all__` also lags. |
| 12 | Whether Decypharr's repair sweep and Symlinkarr's sweep fight | Run `symlinkarr repair auto --dry-run` (or the plan output) the morning after a 04:00 Decypharr sweep; count links whose targets Decypharr removed overnight. | Establishes the steady-state dead-link volume the arr-handoff (workplan step 9) must absorb. |

---

## Retained live risks (re-framed for hybrid)

These come from the original analysis and still stand under the hybrid path. They are
re-framed, not dropped.

**Absolute symlink targets, no path-remap layer (src/utils.rs:35).** VERIFIED: the link target
is the source path exactly as Symlinkarr's process sees it; there is no relative-link mode and no
`path_map`. Under hybrid the risk is *dormant*, not gone: every consumer (Plex, Sonarr, Radarr,
Symlinkarr) mounts `/mnt/decypharr/realdebrid` at the same absolute path, and NZB items live under
the same root, so nothing changes on day one. It becomes live the moment any consumer is moved to
Decypharr's new NFSv4/SMB share (which exposes the identical tree at whatever path the client
chooses) or a container is given a different mount point. Mitigation ordering: a `doctor` check
that the configured source root is the same path Plex/the arrs report for their root folders
(cheap, uses the arr clients already in the binary), then a `path_map`/relative-link mode
(medium) — workplan step 8.

**Dead-link sweep gated behind `search_missing`, default false (src/commands/scan.rs:285;
src/config.rs:337-339).** VERIFIED. Under hybrid this is the *most* important risk, not the
least: the owner's library is 56 % broken, Decypharr's own 04:00 sweep will keep deleting
entries whose files fail, and the scan-time sweep that would mark those links dead is off unless
acquisition is on. `symlinkarr repair auto` exists as the deliberate path, but the daemon does not
sweep by default. Decouple — workplan step 2.

**Parsing reads the file stem only, no parent-directory fallback (src/source_scanner.rs:251,
:437).** VERIFIED. Now attached to a concrete Decypharr behaviour: raw (non-RAR) posts with
obfuscated subjects surface as obfuscated *file* names inside a correctly-named *folder*
(INFERRED from `parser.go:763-782`; MUST-TEST 5). RAR-packed releases keep their internal
names. Workplan step 6.

**Scheduled `RepairAuto` is not classified destructive (src/scheduler.rs:72-74, :678-687).**
VERIFIED. Unchanged under hybrid; more exposed once repair has a second source of churn
(Decypharr deleting NZB entries). Workplan step 10.

**No sample/extras/min-size filter (src/source_scanner.rs:151-165; src/repair.rs:648-695).**
VERIFIED. NZB releases carry samples as often as torrents do. Workplan step 10.

**WalkDir errors silently discarded (src/source_scanner.rs:153).** VERIFIED. With rclone
dir-cache lag and NNTP-backed reads, a walk error is the first symptom of a stale or half-refreshed
directory. Workplan step 4.

**`__bad__` is the only pruned group.** VERIFIED. Harmless while the source is `__all__`; a
root-pointed source (MUST-TEST 2) is the one layout the scanner cannot de-duplicate.

---

## Workplan

Ordered by what unblocks the owner soonest. **PREREQ** = must land before usenet content is
relied on; **PREREQ-if-acquire** = prerequisite only if Symlinkarr's own acquisition stays on
(`daemon.search_missing: true`); **FOLLOW-UP** = after the hybrid is live. Effort is engineering
effort.

| # | What | Why | Effort | Files | Gate |
|---|---|---|---|---|---|
| 1 | **Preflight on the live instance.** Pin `sources[].path` to `/mnt/decypharr/realdebrid/__all__` (never the root); record the beta image digest; read `use_auth` / `enable_webdav_auth` / `auth_token_only`; turn `use_auth` on before adding an NNTP provider (SAB `get_config` leaks provider creds otherwise). Run MUST-TEST 1–4. | Root-pointed source multiplies every item ×3–4 and kills `release_completed`'s mount check; WebDAV auth on = every link skipped; "v2.5.1" pins nothing. | small (config + runbook) | config.yaml:27, config.local.yaml:25, config.docker.yaml:25; `src/commands/doctor.rs:346-374` already flags the root layout | PREREQ |
| 2 | **Decouple the dead-link sweep from `search_missing`.** New `daemon.sweep_dead_links` (default true), keep `search_missing` for acquisition only; surface the sweep result in the scan summary. | The hybrid exists to replace vanished content; detection is the trigger for everything downstream, and it is off by default. 56 % of links are already dead. | small | `src/commands/scan.rs:283-294`; `src/config.rs:337-339`; `src/config/defaults.rs`; config.example.yaml:26; `src/web/ui/config.html` | PREREQ |
| 3 | **Fix `repair --trigger` onto the v2.3+ repair API.** Replace `POST /api/repair` with `POST /api/repair/run` (`{protocol, verify_content, auto_repair}`; handle 409/503), poll `GET /api/repair/status` / `/api/repair/runs/{id}`; drop `RepairRequest`/`RepairJob`/`get_repair_jobs`; delete `browse_group` and its bare-array test or rewrite against `BrowseResponse`. | Broken today (404) regardless of usenet; its body never matched any Decypharr version. Decypharr's sweep is the thing that will re-search NZBs, so Symlinkarr needs a working handle on it. | small–medium | `src/api/decypharr.rs:29-47, :260-344`; `src/commands/repair.rs:231-239`; tests `src/api/decypharr.rs:474-501` | PREREQ |
| 4 | **Foundation: `protocol` on `DecypharrTorrent`** (`#[serde(default)] pub protocol: String`, helper `is_nzb()`), update the three full-struct fixtures; **surface WalkDir errors** (count + log) in the scan walk and repair catalog. | Every protocol-aware decision needs the field; the walk-error count is the only diagnostic for rclone-lag and NNTP-read problems. | small | `src/api/decypharr.rs:68-101`; `src/auto_acquire/tests.rs:128-143, :152-167, :185-200`; `src/source_scanner.rs:153, :166`; `src/repair.rs:650` | PREREQ |
| 5 | **Readiness probe for NZB-backed files.** Measure (MUST-TEST 6); make `source_probe_timeout_ms` per-source; send Basic auth when `enable_webdav_auth` is on (new optional `decypharr.webdav_user/password`), or add a `doctor` check that fails loudly on 401; consider skipping the probe for `nzbs`-view items since Decypharr already head-verified them at import. | 2,500 ms vs NNTP first byte decides whether every NZB link is skipped as `source_unreadable_before_link`; a 401 skips everything. | small (code) + MUST-TEST | `src/linker.rs:125-174`; `src/api/decypharr.rs:191-256`; `src/config.rs:360-365`; `src/config/defaults.rs:317-319` | PREREQ |
| 6 | **Parent-directory fallback in the parser.** When the file stem yields no usable title (or looks obfuscated: hex/random, no year/SxxEyy), parse the parent folder name (which under `original_no_ext` is the release name). Report "parsed from folder" and count unparseable stems instead of emitting garbage `SourceItem`s. | Obfuscated raw posts surface as obfuscated file names in a correctly-named folder; today the junk stem flows into matching as noise. | medium | `src/source_scanner.rs:251, :437`; `src/models.rs:79-102`; tests | PREREQ (before the first obfuscated post) |
| 7 | **Protocol-scope the queue guards and define in-flight accounting.** `Failing`: only block on same-protocol failures (an NZB failure must not stall torrent acquisition and vice versa). `Capacity`: `max_in_flight` counts torrents only; new `max_in_flight_nzb` (or exempt NZBs). Treat "vanished from `/api/torrents`" as *complete-or-failed, check the mount* — with `action=none` every completed entry is deleted immediately, and failed ones are purged after `remove_stalled_after`; arr-driven NZBs share the same list and category. Shorter `completion_timeout_minutes` for NZBs (parse+verify, not a download). | The arrs will now push NZBs into the same categories Symlinkarr polls; protocol-blind guards turn one bad NZB into a 10-minute category block and let NZBs consume the debrid cap. | medium | `src/auto_acquire.rs:934-972, :1023-1076`; `src/auto_acquire/queue.rs:294-310`; `src/config.rs:387-421`; `src/config/defaults.rs:329-339`; tests `src/auto_acquire/tests.rs:127-167` | PREREQ-if-acquire, else FOLLOW-UP |
| 8 | **Path-remap gap.** (a) `doctor`: compare each `sources[].path` / `libraries[].path` with the root folders Sonarr/Radarr report (`SonarrSeries.path`, `RadarrMovie.path` already deserialised) and warn on prefix mismatch. (b) `symlink.link_style: absolute\|relative` or `symlink.path_map: [{from,to}]` applied before `symlink()` and in `verify_link_target`/`target_ok`. | Dormant under hybrid because every consumer shares one mount path; becomes live the day Plex is pointed at Decypharr's NFS/SMB share or a container mount moves. | (a) small, (b) medium | `src/utils.rs:16-49, :138-146`; `src/linker.rs:209-230, :928-931`; `src/commands/doctor.rs`; `src/api/sonarr.rs:20`; `src/api/radarr.rs:17-23` | FOLLOW-UP (a soon, b when a second namespace appears) |
| 9 | **Arr re-search handoff (the recommended acquisition path).** When the sweep marks a link dead and the repair scorer finds no replacement copy on the mount, POST `EpisodeSearch` / `MoviesSearch` to Sonarr/Radarr for that media id (first write endpoint on the arr clients; rate-limit; dedupe by `request_key`). Sonarr/Radarr then grab via Decypharr's `/sabnzbd` client. Document configuring Decypharr as a SAB client in the arrs (URL base `/sabnzbd`, username = arr host URL, password = arr API key, or `ma_password` = Decypharr token on beta). | Reuses the arrs' full usenet pipeline instead of rebuilding it; Decypharr's own sweep already does the same for entries *it* still holds, but not for links Symlinkarr authored to since-deleted entries. | medium | `src/api/sonarr.rs:143-269`; `src/api/radarr.rs:41-64`; `src/repair.rs`; `src/commands/repair.rs`; `src/scheduler.rs`; docs | FOLLOW-UP (first after go-live) |
| 10 | **Carried-over safety items.** Reclassify `RepairAuto` as destructive / opt-in; sample/extras/min-size filter in walk + repair catalog with a size field on `SourceItem`; health-gate the cleanup audit and rename `non_rd_source_path`; bring `cleanup dead` to the prune path's guard level. | Unchanged hazards; repair now has two sources of churn (debrid takedowns and Decypharr deleting broken NZB entries), so unattended repointing is more exposed. | medium | `src/scheduler.rs:72-74, :678-687`; `src/source_scanner.rs:151-165`; `src/repair.rs:648-695`; `src/repair/scoring.rs:160-168`; `src/models.rs:79-102`; `src/cleanup_audit.rs:300-306, :617-622`; `src/commands/cleanup.rs:147-180` | FOLLOW-UP |
| 11 | **Identity when there is no infohash (only with step 12).** Keep `request_key` as-is (VERIFIED media/episode/symlink-keyed). Reinterpret `acquisition_jobs.info_hash` as `provider_id`: btih for torrents (from the magnet, as today), the `/api/add` `id` UUID for NZBs. **Never** capture the `/api/add` `id` for torrents (it is a throwaway UUID). `find_matching_torrent`'s hash branch then works unchanged for both; the token fallback must compare against Decypharr's `Entry.Name` (the .nzb filename) rather than the Prowlarr title, so store `Entry.Name` from the first `/api/torrents` hit. `release_completed`: skip the `rd_torrents` step for NZBs; resolve the mount path as `{source}/{folder}` using the `original_no_ext` rule. | Without a stable id Symlinkarr cannot re-find its own NZB submission, and the RD-cache step can never match a UUID. | small | `src/auto_acquire.rs:1053-1076, :1238-1321`; `src/auto_acquire/queue.rs:424-451`; `src/db/migrations.rs:482-501`; `src/web/handlers/tests.rs:412, :506, :606` | FOLLOW-UP (bundled with 12) |
| 12 | **Native usenet acquisition in Symlinkarr (optional).** Prowlarr: replace the `-2` pin with a config-driven protocol selector (`prowlarr.protocols: [usenet, torrent]`, `indexerIds` -1/-2/omitted) and read `protocol` into `DownloadCandidate`; new `DecypharrClient::add_nzb_urls` sending `nzbURLs`; dispatch on candidate protocol in `submit_request`; protocol-aware ranking (age/grabs for usenet, configured protocol priority before seeders, remove the 0–200 seeder bonus for usenet in anime scoring); bypass DMM when the request is usenet-only; per-arr protocol preference. Requires steps 4, 7, 11 and MUST-TEST 8–9. | Only if the arr handoff (step 9) is too slow for the backfill volume. Every piece is local; nothing architectural. | large | `src/api/prowlarr.rs:88-119, :160-190`; `src/api/decypharr.rs:347-399`; `src/auto_acquire.rs:700-770, :909-932`; `src/auto_acquire/queue.rs:399-451`; `src/auto_acquire/anime.rs:99-103, :196-217, :441-443`; `src/config.rs:455-463` | FOLLOW-UP (optional) |
| 13 | **Hygiene and docs.** Retire DMM for the usenet path (doc as torrent-only); fix `src/cache.rs:509` and any prose that describes the v1 `{mount}/{debrid}/__all__` layout; describe the v2 root, `nzbs/`, the beta caveat and the SAB client settings in README/wiki/CLI manual; make `realdebrid` optional in `/api/v1/health`. | Stops the next operator from re-deriving all of this. | small–medium | `src/cache.rs:509`; config.example.yaml; README.md; docs/wiki; docs/CLI_MANUAL.md; `src/web/api/misc.rs:222-226` | FOLLOW-UP |

Things this plan deliberately does **not** include: a hardlink backend, a presentation-tree
topology, source/library overlap validation, moved-vs-deleted inode tracking, or deleting the
FUSE/`PathHealth` machinery. All of those were fork-C items (Appendix A) and are moot while the
Decypharr mount is the only source.

---

## Appendix A — the original three-way analysis (retained, superseded as framing)

> **Read this as history.** Everything below was written before the decision and checked
> against commit `a8416ae`. It analysed three targets — A: a usenet *mount* product beside
> Decypharr, B: hybrid, C: classic SAB/NZBGet-to-local-disk with arr-managed imports. Under the
> decision above, A and B collapse into "Decypharr v2 serves both protocols", and C is not
> being pursued. The sections are kept because their evidence (path namespace, sweep gating,
> sample filtering, unattended repair, cleanup-audit guards, parser behaviour) is still correct
> and is cited from the main plan. Items that only make sense for fork C — ownership collision,
> presentation tree, overlap validation, moved-vs-deleted, hardlinks — are **moot** for the
> hybrid and should not be scheduled. Where this appendix says "fork B" it means the path now
> chosen; where it says "the sweep is off by default" that is workplan step 2.

### The fork that decides everything

Settle this before writing a line of code. It determines whether most of the workplan is needed
at all.

| Target | What `sources` points at | Symlinkarr's role | Verdict |
|---|---|---|---|
| **A. Usenet mount** (NzbDAV, AltMount, InfiniDysk-style) | the FUSE/rclone mount | unchanged — content is remote, revocable, multi-copy | **Works after a config edit.** Only auto-acquire is lost, and those projects replace it by design. |
| **B. Hybrid** — keep debrid, add usenet | debrid mount **and** local dir | unify two backends into one Plex shelf | **Cheapest path.** Closest to what the code already supports; degrades gracefully into C later. |
| **C. Classic SAB/NZBGet → local disk, arrs import** | SAB complete dir *or* arr roots | collides with the arrs; repair loses its trigger | **Repositioning.** Keep it only as a presentation/fan-out layer, and that layer needs new code. |

Under A, the machinery the assessment below calls "pointless" is immediately valuable again:
`ENOTCONN` → `TransportDisconnected` with a remount hint (src/utils.rs:244-276), the timeout
probe (src/utils.rs:305-345), and the repair candidate scorer that needs many copies of the same
episode (src/repair.rs:632-700). Do **not** delete those during A or B.

---

### What works unchanged

| Subsystem | Evidence | Note |
|---|---|---|
| Source abstraction | src/config.rs:297-305 | Name + absolute path + parser hint. No debrid/FUSE/torrent semantics in the type. `/data/usenet/complete` drops in. Only the stale doc comment "Absolute path to the arrow mount root" (src/config.rs:300) needs a rewrite. |
| Filesystem ingestion | src/source_scanner.rs:140-167 | Plain `WalkDir` + video-extension filter. |
| Debrid ingestion bypass | src/commands/scan.rs:609 vs :696-704 | The RD-cache branch is gated on a non-empty token; the empty-token path goes straight to the walk. Leave the token empty and the whole debrid layer is skipped cleanly — no dead code executes. |
| Boot without debrid credentials | src/config.rs:696-704 | `validate_runtime_settings` errors only on empty `libraries`/`sources`. A missing RD token is at most a warning under `security.require_secret_provider`. The binary boots, scans, links and serves the web UI with no `realdebrid:`, `decypharr:`, `dmm:` or `prowlarr:` block. |
| Release-title parsing, anime absolute-vs-seasonal numbering, pack scoring | src/api/prowlarr.rs, src/auto_acquire/anime.rs | Operates on strings, not magnets. **Caveat below:** it parses the *file stem only*, which is a real usenet hazard. |
| Never writes into a source root | src/utils.rs:16-49, :128-134 | The only FS mutations are the temp-symlink rename inside the library tree and quarantine renames. The reference compose mounts sources `:ro` (docker-compose.yml:25-26). **Partially refuted below** — it does write *into arr library roots*. |
| Dead-link *detection* | src/linker.rs:889-1024, src/repair.rs:308-453 | Still correctly catches operator deletes, disk failure and renames, and is far cheaper on local disk than over FUSE. |
| SQLite link ledger, backup/restore, scheduler, media-server targeted refresh, three-way FS/DB/Plex reconciliation | — | Keyed off library paths and the link table, not the provider. **Two corrections below** (scheduler job count; symlink-only inventories). |
| `__all__` runtime probe-path special case | src/commands/mod.rs:219-233 | Identity function for any other directory name. Harmless. |

#### Corrections to the "works unchanged" list

**The scheduler has eight job kinds, not two, and one of them mutates symlinks unattended.**
`ScheduledEvent` is `Scan, Backup, HousekeepingVacuum, CacheRefresh, CleanupAudit, RepairAuto,
CleanupPruneApply, AnimeRemediationApply` (src/scheduler.rs:31-40). `RepairAuto` is fully wired
and calls `execute_repair_auto` on a timer (src/scheduler.rs:678-687), and repair repoints live
symlinks via `replace_symlink_atomically` (src/repair.rs:923) and removes them
(src/repair.rs:966). `is_destructive()` covers only the two prune-style events
(src/scheduler.rs:72-74), and those two bail as "not wired in this build"
(src/scheduler.rs:688-693). **Consequence:** any "there is a human in the loop typing a
confirmation token" reasoning is wrong for repair. A usenet box with a repair schedule can
silently repoint library links with no operator present.

**The audit and reconciliation inventories are symlink-only, which is a debrid assumption baked
in as provider-neutrality.** The cleanup-audit walk does `if !entry.file_type().is_symlink()
{ continue; }` (src/cleanup_audit.rs:523-525) and `path_compare` applies the same filter
(src/commands/report/path_compare.rs:173). On a debrid box every file in a library root *is* a
symlink, so the filter was free. In a usenet stack the arrs put **real files** in the library and
every one of them is invisible to these subsystems. Concretely: duplicate-slot detection cannot
see the arr's real file — so the duplicate outcome predicted below is undetectable by
Symlinkarr's own duplicate detector; `plex_not_on_fs` compares Plex's index against the symlink
set only, so every arr-imported real file Plex knows about is reported as
present-in-Plex-missing-on-disk, making the reconciliation report noise proportional to library
size. These are not "unchanged" — they are silently mis-calibrated.

**Parsing reads the file stem only; there is no parent-directory fallback.**
`parse_filename` starts with `let file_stem = path.file_stem()?` (src/source_scanner.rs:251) and
the anime parser does the same (src/source_scanner.rs:437). There are zero uses of `.parent()`
anywhere in src/source_scanner.rs. On a debrid mount that is safe, because a torrent preserves the
release-named file inside the release folder. Usenet's signature failure mode is the opposite:
obfuscated final filenames (`a7f3c19b4e.mkv`) inside a correctly-named job folder, where the
title lives only in the parent directory. Worse, title extraction never fails — it returns the
garbage stem, so a `SourceItem` is still produced and the junk flows into matching as noise
rather than being reported as unparseable. **This is a prerequisite on your SAB/NZBGet
configuration** ("Deobfuscate final filenames", or sourcing from arr-renamed files), not
something the code can recover from. It belongs at the top of any migration runbook.

---

### What needs code changes

#### 1. The Decypharr readiness gate can blanket-skip every link — but only in one specific fork

`SourceReadinessGate::from_config` returns `Some(..)` when `symlink.verify_source_readability`
(default `true`, src/config.rs:361-362, src/config/defaults.rs:68) **and** `has_decypharr()`
(`!self.decypharr.url.is_empty()`, src/config.rs:995-997) are both satisfied
(src/linker.rs:73-88). `default_decypharr_url()` returns `"http://localhost:8282"`
(src/config/defaults.rs:321-323), so a config that never mentions Decypharr still constructs the
gate. A connection refusal maps to `WebDavProbeError::Unreadable` (src/api/decypharr.rs:239-241),
`ensure_readable` returns `Err`, and the write is skipped with `source_unreadable_before_link`
(src/linker.rs:555-578). Results are cached per source-*parent* directory
(src/linker.rs:126-131), so one refused connection poisons a whole release folder.

Three qualifications that matter:

- **It does not fire in the hybrid fork.** If Decypharr is still running, a probe of a local
  usenet path returns 404 → `WebDavProbeError::NotFound` (src/api/decypharr.rs:243-245) →
  `saw_not_found` → `SourceReadiness::Ready` (src/linker.rs:145-167, with the explicit comment
  "bypass rather than false-blocking the link"). The usenet half links fine. The blanket skip
  occurs only when **nothing answers** on the configured URL, i.e. pure-usenet with Decypharr
  removed. Conversely, any **non-404** status from whatever is listening on :8282 (401/403/502,
  or an unrelated service) maps to `Unreadable` (src/api/decypharr.rs:246-252) and skips
  *everything*, including the debrid half.
- **The fix is a documented one-line YAML edit, not a code change.** `decypharr.url` ships in
  config.example.yaml:67. Blanking it makes `has_decypharr()` false and the gate returns `None`.
  (`verify_source_readability` and `source_probe_timeout_ms` are the *undocumented* escape
  hatches — neither appears in the sample config.)
- **It is not silent.** The skip emits a `warn!` naming path and reason on every occurrence
  (src/linker.rs:559-562), increments a counted skip reason (src/linker.rs:363-365), and the scan
  CLI prints a "Top skip reasons" block (src/commands/scan.rs:474-483).

Still worth fixing: `default_decypharr_url()` is the only integration that defaults to a
non-empty value (compare `DmmConfig` at src/config/defaults.rs:103-113 and the empty
`realdebrid.api_token`). Change it to `String::new()` and document the two `symlink.*` keys.
**Effort: small.**

#### 2. Ownership collision with Sonarr/Radarr

A "library" is an arr root folder discovered by `\{(tvdb|tmdb)-([0-9]+)\}`
(src/library_scanner.rs:13-14, :28-55). With Completed Download Handling on, the arrs import the
real file into that same folder under their own naming. Symlinkarr then either hits
`regular_file_guard` and skips (src/linker.rs:531-548) or creates a second entry beside the real
file — and `find_existing_equivalent_tv_target` explicitly `continue`s past non-symlink
candidates (src/linker.rs:753-757), so it cannot see the slot is already filled.

The nuance that both an over-optimistic and an over-pessimistic reading get wrong:

- TV naming is **not** hardcoded. `naming_template` defaults to
  `"{title} - S{season:02}E{episode:02} - {episode_title}"` (src/config/defaults.rs:313-315,
  config.example.yaml:31) — Sonarr's own default episode format — and the season directory is
  `format!("Season {:02}", season)` (src/linker.rs:789), Sonarr's default season folder format.
  Only the movie name is hardcoded (`{title} ({year}).{ext}`, src/linker.rs:809-838).
- But `regular_file_guard` fires only on an **exact path collision**, and Sonarr's stock episode
  format appends a quality token that Symlinkarr's default template does not. So in practice the
  paths differ and the guard rarely fires. **The realistic outcome is duplicates, not skips** —
  and per correction above, Symlinkarr's own duplicate detector cannot see them because the arr's
  file is not a symlink.

**Fix: pick a topology.** Recommended: `sources` = the arr-managed library roots (stable, already
renamed, samples already stripped), `libraries` = a separate Symlinkarr-owned presentation tree.
**Effort: medium.** The folder-provisioning half is smaller than it first looks —
`std::fs::create_dir_all(parent)` (src/linker.rs:612-614) creates the *entire* missing ancestor
chain including `<root>/Show Name {tvdb-123}/`, not just the Season dir. The real constraint is
upstream: the matcher needs a `LibraryItem`, and `LibraryScanner` only yields items for
directories that already exist (src/library_scanner.rs:37-51). For a migration of an *existing*
arr-managed library that is a non-issue. For a *fresh* presentation tree it is tens of lines
against clients already in the binary — `SonarrSeries` already deserializes `path` and `tvdb_id`
(src/api/sonarr.rs:20, :24), `RadarrMovie` already deserializes `path`, `tmdb_id` and `year`
(src/api/radarr.rs:17-23), and a live `SonarrClient` is already constructed inside the audit
(src/cleanup_audit.rs:739-751).

#### 3. Moved ≠ deleted, in the dead-link sweep

`PathHealth::Missing` is deliberately excluded from `blocks_destructive_ops()`
(src/utils.rs:255-261) because "a missing path is a legitimate 'gone' signal" — correct under
debrid takedowns, wrong under usenet. The sweep calls `mark_dead_path` then
`std::fs::remove_file` on the library symlink (src/linker.rs:941, :977). Nothing on `SourceItem`
records inode, size or mtime that could distinguish a move from a delete (src/models.rs:79-102).

Two corrections to how this is usually framed:

- **Timing.** Inside `scan` the sweep is gated on `search_missing`, which defaults false
  (src/commands/scan.rs:285-293, src/config.rs:337-339, config.example.yaml:26) — and a usenet
  operator is exactly the person who turns acquisition off. So the *default* usenet configuration
  never sweeps during scans. Links to arr-removed downloads rot in the library with no
  dead-marking and no diagnostic.
- **The proposed fix is wrong on its own.** A grace window or an N-sweep threshold does not help
  when the file was moved **permanently** out of the download dir — it converts a fast failure
  into a slow one. And inode re-association cannot work in the `sources = completed-dir` topology,
  because after the move the file lives in the arr root, which is not a configured source, so
  nothing walks it and no inode is ever re-observed. **This item is subordinate to the topology
  choice in §2, not an independent safety fix.** Recording size/mtime is still worth doing as
  hardening once the topology is right.

Related and cheap: `target_ok` in the sweep is a raw `read_link` string comparison
(src/linker.rs:928-931) rather than `resolve_link_target` (src/utils.rs:138-146), unlike
`verify_link_target` (src/linker.rs:209-230). For links Symlinkarr itself authored this is
provably equivalent — source paths are validated absolute (src/config.rs:943-950), the walk is
rooted there, and `symlink()` is called with that same absolute path (src/utils.rs:35) — so this
is a latent inconsistency, not a live defect. It becomes a defect only in a hardlink port.

#### 4. No source/library overlap validation — and the obvious place to put it is dead code

`validate_paths` checks each path independently for absoluteness and health
(src/config.rs:938-976) and the walk accepts `is_symlink()` entries as source files
(src/source_scanner.rs:154). Impossible on a debrid box (separate mounts); very likely on a
single-disk usenet host where `/data/media` and `/data/usenet` share a parent. A source pointed
at `/data` re-ingests Symlinkarr's own library symlinks, producing symlink-to-symlink chains.

**Important:** `Config::validate()` (src/config.rs:690-693) — the function that calls
`validate_paths` — has exactly two non-test callers: `symlinkarr doctor`
(src/commands/doctor.rs:133) and `symlinkarr config` (src/commands/config.rs:9). Scan, link,
repair, cleanup and the daemon never call it. A guard added there would print a warning to
whoever runs `doctor` and would not stop a single bad scan. The guard must live in
`Config::load`, in `collect_source_items` (src/commands/scan.rs:602), or in the walk itself.
**Effort: small — but target the right file.**

#### 5. No file-stability gating, and no sample/extras/min-size filter anywhere

`scan_source` accepts every `is_file() || is_symlink()` entry whose extension is in
`VIDEO_EXTENSIONS` (src/source_scanner.rs:151-165) — no `filter_entry`, no directory exclusion,
no age or size-settle check. The repair catalog is the same: extension test plus a non-empty
parsed title, no size floor and no directory filter (src/repair.rs:648-695).

- SABnzbd Direct Unpack writes into `_UNPACK_<job>` directories inside the complete folder and
  renames them on finish; NZBGet writes the final `.mkv` while par2 repair may still rewrite it.
- The more common case is missed by both of those: when SAB's incomplete and complete dirs sit on
  **different filesystems** (SSD scratch + storage array is the standard usenet layout), the final
  move is a file-by-file copy, so fully-named `.mkv` files exist half-written for minutes.
- Nothing excludes `Sample/`, `Extras/` or `Featurettes/`. On a debrid `__all__` this mostly
  produced harmless unmatched noise; against a SAB complete dir it produces real links to sample
  files. In the destination-slot contest a sample carrying the full release title ties on score
  and on quality rank, after which the tiebreak falls to alias length and then reverse-lexicographic
  path ordering (src/matcher/scoring.rs:253-266) — there is no size input at all, because
  `SourceItem` has no size field (src/models.rs:79-102).
- In the repair catalog the same gap is sharper: `Movie.2020.1080p.BluRay-GRP.sample.mkv` scores
  exact title (0.35) + exact year (0.15) = 0.50, exactly `MOVIE_THRESHOLD`
  (src/repair.rs:39-41, src/repair/scoring.rs:115-149); a `Show.S01E01...sample.mkv` scores 0.85
  against a 0.75 TV threshold. Size is a **+0.05 bonus, never a precondition**
  (src/repair/scoring.rs:160-168). The real copy outscores a sample only via the +0.10 quality
  and +0.05 size bonuses — a thin margin, and combined with unattended `RepairAuto` (§ correction
  above) that is a silent-wrong-outcome path, not merely a missing-value path.

The counter-argument that a bad link self-heals — the linker re-stats the source immediately
before every write and skips with `source_missing_before_link` if it vanished
(src/linker.rs:321-343), and the next sweep would remove a stale link — **fails under default
config**, because that sweep is off (§3). One cycle of churn becomes a permanent dangling entry.
**Effort: medium. Skip it entirely if you adopt the presentation-tree topology** (arr-managed
files are already stable and already sample-free).

#### 6. Cutover hazard in the cleanup audit — real, but smaller than it looks

`BrokenSource` is computed from a bare `!entry.source_path.exists()` with no health gate
(src/cleanup_audit.rs:300-302), is unconditionally Critical (src/cleanup_audit/classify.rs:488-497)
with confidence 1.0 (:511-513), and High/Critical findings feed `candidate_paths`.
`NonRdSourcePath` — any symlink whose resolved source is not under a configured `sources[].path`
(src/cleanup_audit.rs:617-622) — is High (src/cleanup_audit/classify.rs:500-501), so during a
partial migration whichever half of your links is not listed in `sources` becomes prune-eligible.
The serialized reason string is literally `non_rd_source_path`. `default_max_delete` is 5000
(src/config/defaults.rs:261-263).

But `prune --apply` is fenced by six independent guards and is **restorable**:

| Guard | Evidence |
|---|---|
| Refuses a report already applied (`applied_at` stamp, written back after apply) | src/cleanup_audit/prune.rs:60-65, :293-306 |
| Report age capped by `max_report_age_hours` | src/cleanup_audit/prune.rs:87-94 |
| Confirmation token must match the **freshly recomputed** plan — a stale copy-paste fails | src/cleanup_audit/prune.rs:96-103 |
| Exceeding the delete cap bails rather than truncating | src/cleanup_audit/prune.rs:105-111 |
| Each path re-checked against library roots under `security.enforce_roots` | src/cleanup_audit/prune.rs:129-140 |
| Safety snapshot of every active link + untracked candidates when `backup.enabled` (default true) | src/cleanup_audit/prune.rs:113-124, src/config/defaults.rs:203-205, config.example.yaml:141-142 |
| Source-root health gate before apply | src/cleanup_audit/prune.rs:12-14 |

The residual hazard is precise: the audit *itself* is not health-gated (unlike repair at
src/repair.rs:467 and path_compare at src/commands/report/path_compare.rs:58), and a retired
mount reads as `PathHealth::Missing`, which does **not** block destructive ops
(src/utils.rs:255-261) — so the apply-time gate lets it through too. The fix is to health-gate
the audit and rename the reason string. **Effort: medium.**

There is a blunter exposure worth naming: `symlinkarr cleanup dead` runs the same delete path
with **no confirmation token, no `--apply`, no max-delete cap and no Tautulli active-stream
guard** (src/commands/cleanup.rs:147-180; the Tautulli check exists only on the prune path,
src/commands/cleanup.rs:831-894). It does health-gate and does write a safety snapshot when
backups are on (src/commands/cleanup.rs:157-163), but during a cutover it is a much likelier
accident than the audit+prune sequence.

#### 7. Auto-acquire cannot reach usenet indexers

Prowlarr search is hard-pinned to `indexerIds=-2` with the inline comment `// -2 = all torrent
indexers` (src/api/prowlarr.rs:102), no config override, single call site
(src/auto_acquire.rs:722). Usenet indexers are never queried, so every job walks to `NoResult`.
This is a genuine code change with no config workaround.

Two things are *less* broken than they look, if you ever do want to keep acquisition:

- A `.nzb` URL already flows through unchanged: `best_url()` prefers `magnet_url` and **falls
  back to `download_url`** (src/api/prowlarr.rs:61-64), and `protocol` is already deserialized,
  merely unread (src/api/prowlarr.rs:47-48).
- The hash-free tracking path already exists: `TorrentTracker.info_hash` is `Option<String>` and
  `find_matching_torrent` treats `None` as a first-class case with a category + timestamp + token
  fallback (src/auto_acquire.rs:1291-1320).

What genuinely has nothing to slot into is the **download client**. `DecypharrClient` is a
concrete struct threaded by concrete type; the whole repo contains exactly one trait definition,
`pub trait Len` in src/web/filters.rs:6. The submit surface is a single call taking a URL —
`decypharr.add_content(&[candidate.url], &arr, "none")` (src/auto_acquire/queue.rs:424) — which is
the same shape as SABnzbd's `mode=addurl`, so the port is a new client plus enum dispatch (the
in-repo precedent is `MediaServerKind`, src/media_servers/mod.rs:47-73), not an architecture
rewrite. **But see "What becomes pointless" — you almost certainly should not do this at all.**

#### 8. Hardlinks are architecturally excluded

Zero hits for `hard_link|hardlink|HardLink` across `src/`. The only write primitive is temp-symlink
+ `renameat2(RENAME_NOREPLACE|RENAME_EXCHANGE)` (src/utils.rs:16-134), `verify_link_target`
asserts `is_symlink` (src/linker.rs:209-215), and at least five guards refuse to touch
non-symlinks (src/linker.rs:531-548, :617-624, :753-757; src/cleanup_audit.rs:523-525;
src/cleanup_audit/prune.rs:255-267). See "Recommended architecture" for why this is probably the
wrong thing to build.

---

### What becomes pointless

| Subsystem | Size / evidence | Why |
|---|---|---|
| The whole DMM subsystem | src/api/dmm.rs (~394 lines incl. a reverse-engineered JS hash challenge), src/auto_acquire/dmm.rs (~433 lines), `DmmSearchSession`, `DmmConfig` | A debrid-cache hash oracle with no usenet analogue. `magnet_uri_from_hash` synthesizes `magnet:?xt=urn:btih:{hash}` — meaningless to SAB/NZBGet. Note config.example.yaml:58-59 ships DMM pre-populated with a live public URL. |
| Submit/track half of auto-acquire | src/auto_acquire/queue.rs:424, src/auto_acquire.rs:909-932, :1291-1349 | Posts magnets to Decypharr, asks Decypharr which arrs it knows, scrapes `xt=urn:btih:` to re-discover the submission. |
| Queue-capacity and failing-torrent guards | src/auto_acquire.rs:934-972 | "Clean them up in DMM/Decypharr", "Waiting avoids filling the RD queue" — provider-quota-shaped rationales. Usenet queue depth is bounded by disk and bandwidth; a failed par2 repair should fail one job, not stall a category. |
| `Relinking` / `CompletedUnlinked` states and their backoff budgets | src/db/types.rs:240-249, src/auto_acquire.rs:47-52, :640-673, :1078-1108 | They exist because a debrid mount reports "complete" before FUSE exposes the file. With local disk the file is there the instant the client says done. `run_relink_scans` runs a full library scan on every poll cycle. |
| RD-cache-backed repair catalog | src/repair.rs:724-815, gated at src/commands/repair.rs:300-333 | ~90 lines unreachable once `has_realdebrid()` is false. |
| `repair trigger`, `repair auto --self-heal` | src/commands/repair.rs:231-239, :143-144 | There is no "ask the provider to re-fetch" in usenet. par2 repair and Sonarr's Redownload Failed live below/outside Symlinkarr. |
| `cache build` / `cache status`, `discover add <TORRENT_ID>` | src/commands/cache.rs:12-17, :35-38; src/commands/discover.rs:299-328 | Hard-bail without an RD key / pure RD→magnet→Decypharr handoff. `cache invalidate`, `cache clear` and `discover list` stay useful. |
| `realdebrid:` config section, `rd_torrents` table, `RealDebridClient` | src/config.rs:369-383, src/api/realdebrid.rs:97-210 | Four keys that exist only to paginate RD's `/torrents` API. `add_magnet` is already `#[allow(dead_code)]`. |
| FUSE-disconnect machinery (~330 lines) | src/utils.rs:238-345 | `ENOTCONN=107` → `TransportDisconnected`, the 10 s directory probe timeout, `unreachable_source_roots`. None of these arms can fire on local ext4/xfs/btrfs. **Keep the `IoError` arm** — it correctly aborts a mass dead-mark when a disk is dying. **Do NOT delete any of this under forks A or B.** |
| Decypharr WebDAV readiness gate + `__all__`-aware relative-path arithmetic | src/linker.rs:66-180 | ~115 lines plus two config keys, once the URL default is fixed. |
| `--search-missing` / `daemon.search_missing` re-acquisition pipeline | src/commands/backfill.rs:626-640 | Re-derives "what is missing" from the exact same `monitored && !has_file` flags Sonarr uses, then re-implements search→rank→grab that Sonarr/Radarr already do natively for usenet end-to-end (import, rename, retry, blocklist, quality profiles, cutoff upgrades, failed-download handling). The honest replacement for hundreds of lines is one POST per item. Both arr clients are read-only GETs today (src/api/sonarr.rs:143-269, src/api/radarr.rs:41-64). **Keep** the cross-check half: `load_sonarr_snapshots` / `ArrUntracked` already consumes Sonarr's authoritative answer (src/cleanup_audit.rs:735-864). |

**Correction — repair does *not* become pointless.** The usual argument is that
`filter_repair_candidates_already_active` "drops candidates already linked under the same
media_id, i.e. excludes the only file that exists". That is wrong: the filter is **path-scoped**.
It only considers dropping a candidate when `active_sources.contains(&candidate.path)` — the
*exact* file already backing a live link for that media id (src/repair.rs:1459-1470). The dominant
dead-link cause under usenet is a Sonarr/Radarr rename, quality upgrade or import move: the
replacement file has a **different** path, is not in `active_sources`, and is retained. So repair
keeps a genuine, frequently-firing trigger — *provided the destination of the move is inside a
configured source*, which is exactly what the presentation-tree topology guarantees and what the
`sources = SAB-complete-dir` topology does not. Under the recommended topology, arr-renamed files
(`Show - S01E05 - Title.mkv`) also parse more cleanly than scene names, so the candidate scorer is
arguably *more* reliable here than against a debrid `__all__` mount.

**Correction — acquisition is already off by default and triple-gated**, so "retire the
acquisition surface" is hygiene, not migration work. `daemon.search_missing` defaults false
(src/config.rs:337-339, config.example.yaml:26), the pipeline runs only under
`search_missing && (has_prowlarr() || has_dmm()) && has_decypharr()` (src/commands/scan.rs:310)
with explicit user-facing explanations when the gate fails (src/commands/scan.rs:466-471), the
same guard is repeated at src/commands/backfill.rs:198-203, src/commands/repair.rs:143-144, :232,
src/commands/discover.rs:303 and src/commands/cache.rs:12-17, and the DMM client is only
constructed under `has_dmm()`. `DmmConfig.url` and `ProwlarrConfig.url` both default empty. A
usenet config that omits those blocks disables the whole pipeline automatically.

The same applies to the cosmetics: `integration_summary` is a pure function of the `has_*()`
predicates and prints the literal string `"local-only"` when nothing is configured
(src/startup.rs:105-152). Blanking `decypharr.url` fixes the startup banner too. What genuinely
needs code is `/api/v1/health`, which emits `realdebrid` as a non-optional field so automation
sees a permanent "missing" signal (src/web/api/misc.rs:222-226), and roughly six web panels that
render unguarded zeroes (dashboard auto-acquire card, Scan/Status queue grids, per-run acquire
telemetry, Missing Search filter, Anime Search Overrides editor).

---

### Practical failure modes on day one

Assume: SAB writes to `/downloads/complete`, Sonarr/Radarr import into `/tv` and `/movies`, Plex
reads those, Symlinkarr runs in its own container. In roughly descending order of likelihood.

**1. 100 % dangling links from a container path-namespace mismatch.** The link target is the
source path exactly as the Symlinkarr process saw it: `std::os::unix::fs::symlink(source_path,
&temp_path)` (src/utils.rs:35). There is no relative-link mode and no path-rewrite config — zero
hits for `path_map|path_mapping|remote_path|relative_symlink` across `src/` and
config.example.yaml. The debrid convention hides this because everyone mounts the provider at the
identical absolute path in every container; the reference compose literally maps
`/mnt/decypharr/realdebrid` onto itself (docker-compose.yml:25-26). A usenet stack does not follow
that convention — SAB typically exposes `/downloads`, Sonarr sees `/downloads` + `/tv`, and Plex
usually has **no** mount of the download dir at all. Every link then dangles inside Plex while
Symlinkarr reports perfect health, because it validates links in its own namespace
(src/linker.rs:928-931). Reported symptom: *"Symlinkarr says 4,000 links created, Plex shows
4,000 unavailable episodes."*

**2. Duplicate entries beside every arr-imported file.** Deterministic, not a coin flip — see §2
above. Symlinkarr's own duplicate-slot detector cannot see them (src/cleanup_audit.rs:523-525).

**3. Symlinkarr publishes content Sonarr deliberately refused.** Matching is purely
parsed-filename → ID-tagged folder (src/matcher.rs:727, :821-847); nothing at link time consults
the arr's import decision — the Sonarr snapshot is only read much later, by the cleanup audit
(src/cleanup_audit.rs:735-864). A SAB completed dir is a *staging area* that legitimately contains
failed imports, rejected qualities, wrong-language grabs, manual downloads and jobs pending par2
repair. Under debrid the mount **is** the library, so everything on it is by definition wanted.
Under usenet, every reject that parses to a `{tvdb-NNN}` title gets published into Plex within one
scan interval. This is a behavioural inversion, and only the topology change in §2 fixes it.

**4. Plex is told to index links that are then never cleaned up.** Every created/updated link
contributes a refresh path (src/linker.rs:368-370) and the refresh fires whenever
`linked_total > 0` (src/commands/scan.rs:237-250) — before any validity check. With a 60-minute
default interval (config.example.yaml:25) and Sonarr importing continuously, any scan that catches
a file in the window between SAB finishing and Sonarr importing creates a link, tells Plex to index
it, and then — because the dead-link sweep is off by default (§3) — leaves it dangling in Plex's
database permanently.

**5. Permission collisions inside the arr root.** Symlinkarr `create_dir_all`s directories inside
Sonarr's own root folder (src/linker.rs:612-614) and drops temp symlinks there
(src/utils.rs:16-49). There is **no umask, chown or ownership handling anywhere** — the only
`set_permissions` calls are on Symlinkarr's own db/backup/report files (src/db.rs:93, :121;
src/backup.rs:175-238; src/cleanup_audit.rs:156, :192). In the standard container split —
Symlinkarr pinned to `user: "1000:1000"` by the reference compose (docker-compose.yml:9), Sonarr
and SAB under a different PUID/PGID with their own umask — a `Season 03` directory created by
Symlinkarr can be unwritable by Sonarr, and Sonarr's next import into that season fails with a
permission error that looks like a Sonarr bug. This is the one direction in which Symlinkarr *does*
interfere with arr file management.

**6. Half-written files linked mid-copy.** See §5 of "needs code changes". Not self-healing under
defaults.

**7. Whole releases silently missing, with no error.** Obfuscated final filenames → garbage parsed
title → never matched, never reported as unparseable. Prerequisite: SAB "Deobfuscate final
filenames" on, or source from arr-renamed files.

**8. A second, independent cause of `created=0`.** `LibraryScanner` only discovers folders matching
`\{(tvdb|tmdb)-([0-9]+)\}` (src/library_scanner.rs:13-14, :28-55), and neither Sonarr nor Radarr
adds those tags to folder names by default. An operator who builds a fresh arr-managed library
during migration gets zero library items, zero matches and the same symptomless `created=0` —
*after* fixing the Decypharr default. Both belong in the runbook.

**9. Walkdir errors are silently discarded** — no logging, no counter: `.filter_map(|e| e.ok())`
at src/source_scanner.rs:153, and the only feedback is `Found {} media files` at :166. Same pattern
in the repair catalog (src/repair.rs:650). A job directory renamed out from under the walk, or an
EACCES on a directory SAB created with a different umask, produces an error that is dropped on the
floor; the operator sees only a file count that silently moves between runs. This is what makes the
concurrency problems above undiagnosable in practice.

**10. Unattended symlink rewrites.** Scheduled `RepairAuto` executes the full repair pipeline
including symlink replacement (src/scheduler.rs:678-687, src/repair.rs:923) and is not classified
destructive (src/scheduler.rs:72-74). Combined with the sample-promotion gap in §5, a usenet box
with a repair schedule can repoint library links at sample files with no operator present.

**11. Minor, but know about them.** Stale temp symlinks `.<name>.symlinkarr-<pid>-<seq>.tmp`
accumulate in the arr library after a SIGKILL/OOM between creation and install (src/utils.rs:29-36;
the error path removes them at src/utils.rs:129-132, but there is no startup sweep). And
`renameat2` is used with `RENAME_NOREPLACE`/`RENAME_EXCHANGE` and no EINVAL/ENOSYS fallback
(src/utils.rs:52-108) — fine on ext4/xfs/btrfs, not universally supported on FUSE union
filesystems (unRAID `shfs` `/mnt/user`) or older ZFS. Low confidence, untested here, but usenet
migrants are disproportionately the population moving library roots onto single-box array storage.

---

### Recommended architecture

#### If you are moving to a usenet mount (fork A)

Do nothing structural. Point `sources` at the mount, blank the debrid/Decypharr/DMM keys, leave
`search_missing: false`. Keep every piece of the `PathHealth`/ENOTCONN machinery — it is
load-bearing again. The only real loss is auto-acquire, which those projects replace by design
because they present themselves to Sonarr/Radarr as a SABnzbd-compatible download client.

#### If you are hedging (fork B — hybrid debrid + usenet) — the recommended path

This is the strongest near-term argument for keeping the tool, and the case the existing code
handles best. A single Plex shelf unifying a legacy debrid mount with new usenet-imported files is
a real job that neither Sonarr nor Radarr will do for you: the arrs manage one root folder per
quality profile and have no notion of presenting two storage backends as one library. Symlinkarr
already tolerates multiple heterogeneous `sources`, and both sides are just directories to the
scanner.

What to watch:

- The WebDAV gate does **not** blanket-skip the usenet half while Decypharr is up (404 →
  `Ready`, src/linker.rs:145-167). It *will* skip everything if something else answers on :8282
  with a non-404 status.
- List the local half in `sources`, or `NonRdSourcePath` flags it High and prune-eligible
  (src/cleanup_audit.rs:617-622, src/cleanup_audit/classify.rs:500-501).
- Keep the `PathHealth` machinery. Do not delete it during a hybrid.
- Separate moved-from-deleted before you let the sweep run on the usenet half.

The hybrid also survives the debrid side being retired later — it degrades into the
presentation-tree topology rather than needing a second migration.

#### If you are going to local disk (fork C)

`sources` = the arr-managed library roots. `libraries` = a separate Symlinkarr-owned presentation
tree with its own `{tvdb-N}`/`{tmdb-N}` item folders. This sidesteps the ownership collision, the
move-vs-delete hazard, the `_UNPACK_`/half-copy hazard and the publishing-rejected-content
inversion **all at once**, because arr-managed files are stable, renamed, sample-free and
authoritative. `libraries[].depth` is already a per-library knob (src/library_scanner.rs:39,
config.example.yaml:10, :15), so a different nesting is config, not code.

This topology is also the only one under which the tool has a *product* left: multi-library
fan-out. That needs one real change — a library discriminator in the matcher's identity model.
`destination_key` is built from media id + season/episode with no library dimension
(src/matcher/scoring.rs:54-63) and `best_per_source.push(best)` emits exactly one match per source
file (src/matcher.rs:837), so a movie tagged `{tmdb-603}` in both a 1080p and a 4K library
collides and one silently wins. `multi_version` only adds a quality/edition discriminator, is
movies-only and is off by default (src/config.rs:354-356).

#### Hardlink vs symlink

**Do not build the hardlink backend.** Three reasons, in order of decisiveness:

1. **It is impossible on the common usenet host.** A hardlink cannot cross filesystems. The
   standard two-pool usenet build is fast NVMe scratch for `/downloads` and an HDD array for
   `/media`. Nothing in the codebase does a same-filesystem check — the only `dev()` comparison is
   the concurrency guard comparing one path against itself before and after the exchange
   (src/utils.rs:99-101). If a hardlink mode ships, it must fail loudly on an `st_dev` mismatch,
   never silently fall back — otherwise it breaks for exactly the operators with the most disk.
2. **It is unnecessary under the recommended topology.** Hardlinks are the arr convention for
   local files because of atomic moves, no double disk usage and no cross-filesystem links — all
   of which matter when the *arr* owns the file. Under a presentation tree, Symlinkarr is a
   read-only view over files it does not own, and a symlink is the correct primitive.
3. **It is an identity-model rewrite, not a new function.** There is no atomic
   temp+`renameat2(RENAME_EXCHANGE)` trick for hardlinks; a hardlink has no `read_link`, so
   `target_ok` (src/linker.rs:928-931) would be permanently false and churn dead-marks forever
   with no filesystem effect; and five guards that today protect you would refuse to act on
   exactly the files you want managed (src/linker.rs:531-548, :617-624, :753-757;
   src/cleanup_audit.rs:523-525; src/cleanup_audit/prune.rs:255-267).

Build hardlink mode only if you commit to Symlinkarr filling arr root folders — which the analysis
above says you should not.

---

### How to try it today

Nothing here requires a recompile. Every key already exists in `config.example.yaml`.

```yaml
security:
  require_secret_provider: false     # avoids a warning about the now-empty RD token

libraries:
  # Fork C (recommended): a Symlinkarr-owned presentation tree.
  # These directories must already contain {tvdb-N}/{tmdb-N}-tagged item folders,
  # or LibraryScanner finds zero items and nothing links.
  - name: "Movies"
    path: "/data/plex/movies"
    media_type: "movie"
    depth: 1
  - name: "Series"
    path: "/data/plex/series"
    media_type: "tv"
    depth: 1

sources:
  # Point at the ARR-MANAGED ROOTS, not SAB's completed dir.
  # Files here are already imported, renamed, sample-free and stable.
  - name: "Usenet-Movies"
    path: "/data/media/movies"
    media_type: "movie"
  - name: "Usenet-Series"
    path: "/data/media/series"
    media_type: "tv"

db_path: "/app/data/symlinkarr.db"

daemon:
  interval_minutes: 60
  search_missing: false        # MUST stay false: no usenet indexer path exists
                               # (src/api/prowlarr.rs:102, indexerIds=-2)

symlink:
  naming_template: "{title} - S{season:02}E{episode:02} - {episode_title}"
  dry_run: true                # start here; flip to false after a clean report
  verify_source_readability: false   # belt-and-braces; the next line already
                                     # disables the WebDAV gate

# ── The three lines that switch off the entire debrid stack ──
realdebrid:
  api_token: ""                # empty ⇒ has_realdebrid() false ⇒ RD cache branch
                               # skipped entirely (src/commands/scan.rs:609)
dmm:
  url: ""                      # empty ⇒ has_dmm() false
decypharr:
  url: ""                      # empty ⇒ has_decypharr() false ⇒ SourceReadinessGate
                               # returns None (src/linker.rs:73-77) ⇒ links are written

api:
  tmdb_api_key: "env:SYMLINKARR_TMDB_API_KEY"
  tvdb_api_key: "env:SYMLINKARR_TVDB_API_KEY"

backup:
  enabled: true                # keep this — it is what makes cleanup/prune reversible
```

**What to expect**

| Command | Expected |
|---|---|
| startup banner | `stack: local-only` (src/startup.rs:150-152) if no arrs/Plex configured; otherwise the arr/Plex list with no Decypharr entry. |
| `symlinkarr doctor` | Clean, or a loud per-source error if a `sources[].path` does not exist (src/config.rs:952-975). Note this only runs under `doctor`/`config` — not at scan time. |
| `symlinkarr scan --dry-run` | Library items found > 0 **only if** your library dirs carry `{tvdb-N}`/`{tmdb-N}` tags. Matches roughly = your episode count. Zero `source_unreadable_before_link` in the "Top skip reasons" block (src/commands/scan.rs:474-483). |
| `symlinkarr scan` (dry_run off) | Links created. **Verify the target resolves in Plex's namespace, not just Symlinkarr's** — this is failure mode #1. |
| dangling links after an arr rename | Nothing happens by default. The sweep is gated behind `search_missing` (src/commands/scan.rs:285). Run `symlinkarr repair auto` deliberately, and read the plan before applying. |
| `symlinkarr cleanup audit` | Expect `NonRdSourcePath` findings for any link whose source is outside `sources` — High severity, prune-eligible. Do not run `prune --apply` until that list is empty of things you want to keep. |
| `/api/v1/health` | Will report `realdebrid: "missing"` forever (src/web/api/misc.rs:222-226). Cosmetic; needs a code fix to silence. |

**If you must point at SAB's completed dir instead** — accept that you will hit failure modes
2, 3, 5, 6 and 7. At minimum: set `dry_run: true` and leave it there until you have read a full
scan report, and never enable a `repair_auto` schedule.

---

### Original fork-agnostic workplan (superseded by the main Workplan)

Ordered. Effort is engineering effort, not calendar.

| # | Step | Effort | Applies to fork | Key files |
|---|---|---|---|---|
| 0 | **Decide the fork.** Mount vs hybrid vs local-disk. No code. | — | all | Read src/utils.rs:238-345 and src/repair.rs:632-700 to see how much engineering assumes a remote, revocable, many-copies filesystem. |
| 1 | **Runbook + preflight, before any code.** (a) verify SAB/NZBGet filename deobfuscation or source from arr-renamed files; (b) verify container path parity — the path Symlinkarr writes into a symlink must resolve identically inside Plex; (c) verify `{tvdb-N}`/`{tmdb-N}` tags exist on library folders; (d) UID/umask parity between Symlinkarr and the arrs. | small | all | src/source_scanner.rs:251, :437; src/utils.rs:35; src/library_scanner.rs:13-14; src/linker.rs:612-614; docker-compose.yml:9, :25-26 |
| 2 | **`default_decypharr_url()` → `String::new()`**; warn when `verify_source_readability` is on but Decypharr is unreachable; document `verify_source_readability` and `source_probe_timeout_ms` in the sample config. | small | all | src/config/defaults.rs:321-323; src/config.rs:360-365, :995-997; src/linker.rs:73-88; config.example.yaml |
| 3 | **Surface discarded `WalkDir` errors** (count + log). Cheapest possible diagnosability win; everything in "day one" is undiagnosable without it. | small | all | src/source_scanner.rs:153, :166; src/repair.rs:650 |
| 4 | **Add a path-remapping / relative-link mode** (`symlink.link_style: absolute\|relative`, or a `path_map` applied to the target before `symlink()`). | medium | C, and B if namespaces differ | src/utils.rs:16-49; src/linker.rs:209-230, :928-931 |
| 5 | **Implement the presentation-tree topology**, including provisioning `{tvdb-N}`/`{tmdb-N}` item folders from the already-deserialized arr data. Subsumes the `_UNPACK_`/half-copy hazard and the publishing-rejected-content inversion. | medium | C | src/library_scanner.rs:28-55; src/linker.rs:612-614; src/api/sonarr.rs:20, :24; src/api/radarr.rs:17-23; src/cleanup_audit.rs:739-751 |
| 6 | **Source/library overlap validation** — reject configs where any source root contains, equals or is contained by any library root, and skip walk entries resolving under a library root. **Put it in `Config::load` or `collect_source_items`, not `validate_paths`** (that function is `doctor`/`config`-only). | small | B, C | src/config.rs:690-693, :938-976; src/commands/scan.rs:602; src/source_scanner.rs:154; callers at src/commands/doctor.rs:133, src/commands/config.rs:9 |
| 7 | **Sample / extras / min-size exclusion** in both the scan walk and the repair catalog, plus a size field on `SourceItem` fed into the destination-slot tiebreak. Closes the repair sample-promotion path. | medium | all (fixes a latent debrid bug too) | src/source_scanner.rs:151-165; src/repair.rs:648-695; src/repair/scoring.rs:160-168; src/matcher/scoring.rs:253-266; src/models.rs:79-102 |
| 8 | **Health-gate the cleanup audit** (match repair and path_compare), rename `non_rd_source_path` to something provider-neutral, lower `default_max_delete` for the migration window, and bring `cleanup dead` up to the prune path's guard level (cap + Tautulli check). | medium | all | src/cleanup_audit.rs:300-306, :617-622; src/cleanup_audit/classify.rs:488-513; src/config/defaults.rs:261-263; src/commands/cleanup.rs:147-180 vs :831-894; cf. src/repair.rs:467, src/commands/report/path_compare.rs:58 |
| 9 | **Separate moved from deleted** in the sweep — size + mtime on `SourceItem`/`LinkRecord`, plus a grace window. Subordinate to step 5: under the presentation-tree topology it is hardening; under `sources = completed-dir` it is a band-aid on a topological problem. Also switch `target_ok` to `resolve_link_target`. | medium | C | src/linker.rs:919-985; src/models.rs:79-102; src/utils.rs:138-146, :255-261 |
| 10 | **Reclassify `RepairAuto` as destructive** in the scheduler, or gate scheduled repair behind an explicit opt-in. | small | all | src/scheduler.rs:72-74, :678-687; src/repair.rs:923, :966 |
| 11 | **Feature-gate / retire the acquisition surface.** Delete src/api/dmm.rs, src/auto_acquire/dmm.rs, `DmmSearchSession`, `DmmConfig`, `repair trigger`, `discover add`, `cache build`/`cache status`. Make `realdebrid` optional in the health JSON and guard the six unconditional web panels. **Hygiene, not migration work** — the pipeline is already off by default and triple-gated. | medium | all | src/api/dmm.rs; src/auto_acquire/dmm.rs; src/commands/repair.rs:231-239; src/commands/discover.rs:299-328; src/commands/cache.rs:10-53; src/web/api/misc.rs:222-226; src/web/ui/*.html |
| 12 | **Library discriminator in the matcher's identity model** — the actual remaining product. Only if you want multi-library fan-out. | large | C (and B, long-term) | src/matcher/scoring.rs:54-63; src/matcher.rs:73-88, :451-505, :837; src/config.rs:354-356 |
| 13 | **Rewrite bootstrap, README, wiki** around the chosen topology. Drop the false "Real-Debrid API token — Required file" line (verified false: the binary boots, scans, links and serves with no debrid config). | medium | all | src/commands/bootstrap.rs:11-47, :94; README.md; docs/wiki/Home.md; docs/CLI_MANUAL.md; config.example.yaml |
| 14 | **Hardlink backend** — only if you commit to arr-owned import semantics, which the analysis recommends against. Requires a same-filesystem `st_dev` precondition that fails loudly, an inode-based identity model, and relaxation of five non-symlink guards. | large | C, discouraged | src/utils.rs:16-134; src/linker.rs:209-230, :531-548, :617-624, :753-757, :928-931; src/cleanup_audit/prune.rs:255-267 |

---

### Where the honest answer is "this tool has less to do"

In a pure-usenet setup where Sonarr/Radarr import real files into their own root folders, a
symlink manager does not earn its keep, and Symlinkarr specifically earns it least. Its pillars
are: (1) discover files on a provider mount, (2) match them to ID-tagged folders, (3) own symlinks
there, (4) repair those links when the provider yanks content, (5) auto-acquire.

- Pillar 1 becomes trivial — it is a `WalkDir` over local disk.
- Pillar 3 collides with the arrs (src/linker.rs:531-548) unless you move to a separate tree.
- Pillar 4 keeps a *trigger* (arr renames and upgrades — see the repair correction above) but
  loses its *original justification*: a par2-verified file on your ext4 cannot be un-written by an
  upstream takedown, and the many-copies assumption behind the candidate scorer no longer holds.
- Pillar 5 is both blocked (`indexerIds=-2`, magnets, btih) and redundant with what the arrs do
  end-to-end for usenet.

Only pillar 2 has standalone value, and the arrs do it better, because they match against their
own database rather than parsed filenames. Symlinkarr already consumes Sonarr's authoritative
`has_file` answer (src/cleanup_audit.rs:735-864) — its dead-link table is a second, weaker copy of
state Sonarr already holds. There is a positioning tell here: the closest neighbour, CineSync,
earns its keep by *replacing* the arrs' organiser role; Symlinkarr deliberately coexists with arrs
that own the library, which is exactly the position with no room left once the arrs also own the
files.

**Recommendation.** If you are leaving debrid entirely for local disk, do not try to keep
Symlinkarr in its current role. Either move to a usenet mount and keep the product intact, or
repurpose it deliberately as a presentation/fan-out layer (steps 5 and 12) — which is real work
and a real product, but is not the product you have today. If you are hedging, run hybrid: it is
the cheapest path, it is the scenario the existing code is closest to supporting, and it buys time
to see whether usenet takedown pressure follows debrid's.

---

### Objections considered and rejected (original)

Kept here so the reasoning is not re-litigated later.

- *"Repair loses its trigger under usenet."* Rejected. `filter_repair_candidates_already_active`
  is path-scoped, not media-id-scoped (src/repair.rs:1459-1470); arr renames and quality upgrades
  produce exactly the dead-link-with-a-live-replacement case the scorer was built for — provided
  the replacement lands inside a configured source.
- *"The Decypharr default is an undiscoverable trap requiring a code change."* Rejected.
  `decypharr.url` ships at config.example.yaml:67, and blanking it disables the gate
  (src/config.rs:995-997, src/linker.rs:73-77). The skip is also logged, counted and surfaced in
  the CLI and web UI. It remains worth fixing as the one integration that defaults non-empty.
- *"One `cleanup audit` + `prune --apply` deletes 5000 links behind a copy-pasted token."*
  Rejected as stated. The token is recomputed from a fresh plan and a stale paste fails
  (src/cleanup_audit/prune.rs:96-103); the delete cap bails rather than truncating (:105-111); and
  a safety snapshot of every active link is written first when backups are on (:113-124), so the
  deletion is reversible. The residual hazard — the audit itself is not health-gated, and a
  removed mount reads as `Missing`, which does not block — is real and is step 8.
- *"Item folders are never created, so a presentation tree is a large build."* Rejected on the
  filesystem half: `create_dir_all` creates the whole ancestor chain (src/linker.rs:612-614). The
  real constraint is that `LibraryScanner` only discovers existing tagged dirs
  (src/library_scanner.rs:37-51), and provisioning them from data the arr clients already
  deserialize is tens of lines.
- *"Symlinkarr never interferes with arr file management."* Rejected. It `create_dir_all`s inside
  arr root folders with no umask/chown handling (src/linker.rs:612-614; no `umask`/`chown` anywhere
  in `src/`), which can leave a season directory Sonarr cannot write into.
- *"A half-written file linked mid-copy self-heals on the next sweep."* Rejected under default
  config: the sweep is gated on `search_missing`, which is false by default
  (src/commands/scan.rs:285).
- *"`target_ok`'s string comparison is a live bug."* Rejected. Source paths are validated absolute
  (src/config.rs:943-950) and `resolve_link_target` is the identity for absolute targets
  (src/utils.rs:138-141), so the comparison is provably equivalent for every Symlinkarr-authored
  link. Worth aligning anyway, for consistency and for any future hardlink work.


---

## External references

Primary (read for this revision):

- Decypharr source — https://github.com/sirrobot01/decypharr (tag `v2.5`, commit `0dd1cbb`; branch `beta`, HEAD `1f7a62e` on 2026-09-04). Files cited by path throughout.
- Decypharr v2.0 release notes — https://github.com/sirrobot01/decypharr/releases/tag/v2.0
- Decypharr docs sources in-repo: `docs/src/content/docs/reference/api.md`, `guides/usenet/overview.md`, `guides/usenet/sabnzbd.mdx`, `guides/repair.mdx`, `guides/shares/overview.md`, `guides/virtual-folders.md` (note: the SAB page and the API reference are stale in the places called out above; the Go source is authoritative).

Pointers only (not fetched from this machine):

- SABnzbd switches, incl. Direct Unpack and "Deobfuscate final filenames" — https://sabnzbd.org/wiki/configuration/4.5/switches
- Sonarr Completed Download Handling — https://wiki.servarr.com/sonarr/settings#completed-download-handling
- Prowlarr API (`indexerIds` semantics) — https://prowlarr.com/docs/api/
- TRaSH Guides, hardlinks and instant moves — https://trash-guides.info/File-and-Folder-Structure/Hardlinks-and-Instant-Moves/
- NzbDAV — https://github.com/nzbdav-dev/nzbdav ; AltMount — https://github.com/javi11/altmount (the standalone usenet-mount products fork A originally assumed; superseded by Decypharr's own usenet support)
