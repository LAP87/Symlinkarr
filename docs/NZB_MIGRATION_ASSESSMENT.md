# NZB / Usenet Migration Assessment

**Question:** if the stack moves off Real-Debrid + Decypharr and onto SABnzbd/NZBGet + indexers
driven by Sonarr/Radarr, does Symlinkarr still work? What breaks, what becomes pointless, what
must be built?

**Method:** every claim below was checked against the source tree at commit `a8416ae`
(branch `feature/review-fixes-and-ui`). File:line citations are to that tree. No files were
modified. A handful of external references are listed at the end and were *not* fetched from
this machine — treat them as pointers, not verified facts.

---

## Verdict

Symlinkarr does not crash on usenet — the scan/match/link core is genuinely storage-agnostic
(`SourceConfig` is name + absolute path + a parser hint, src/config.rs:297-305; `scan_source` is
a plain `WalkDir` filtered by video extension, src/source_scanner.rs:140-167) and the answer
depends entirely on *where you point `sources`*. If you migrate to a usenet **mount**
(NzbDAV / AltMount-style: SAB-compatible API to the arrs, content exposed over rclone/FUSE), the
product transfers almost verbatim and the migration is a five-line YAML edit — content is still
remote, revocable and multi-copy, so dead-link repair, the FUSE/`PathHealth` guards and the
readiness gate all keep their original justification. If you migrate to **classic
SAB/NZBGet-to-local-disk with arr-managed imports**, Symlinkarr keeps running but three of its
five pillars (dead-link repair, mount-health safety, auto-acquire) lose their trigger and its
write path collides with the arrs, which now own the library filesystem — you are left with a
tool whose only defensible remaining job, multi-library fan-out over a hybrid shelf, is the one
thing it does not currently implement (`destination_key` has no library dimension,
src/matcher/scoring.rs:54-63).

The honest headline is therefore **fork-dependent**: *config migration* for the mount and hybrid
cases, *deliberate repositioning plus two real safety fixes* for the local-disk case. And the
failure mode on a naive local-disk migration is not a clean no-op — it is **silent garbage
accumulation**: duplicate entries beside every Sonarr-imported file, dangling links that nothing
cleans up (the dead-link sweep is gated behind `search_missing`, which defaults false —
src/commands/scan.rs:285, src/config.rs:337-339, config.example.yaml:26), and — most likely of
all — 100 % dangling links from day one because symlink targets are absolute paths from
Symlinkarr's own container namespace with no remapping layer (src/utils.rs:35).

---

## The fork that decides everything

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

## What works unchanged

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

### Corrections to the "works unchanged" list

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

## What needs code changes

### 1. The Decypharr readiness gate can blanket-skip every link — but only in one specific fork

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

### 2. Ownership collision with Sonarr/Radarr

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

### 3. Moved ≠ deleted, in the dead-link sweep

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

### 4. No source/library overlap validation — and the obvious place to put it is dead code

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

### 5. No file-stability gating, and no sample/extras/min-size filter anywhere

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

### 6. Cutover hazard in the cleanup audit — real, but smaller than it looks

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

### 7. Auto-acquire cannot reach usenet indexers

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

### 8. Hardlinks are architecturally excluded

Zero hits for `hard_link|hardlink|HardLink` across `src/`. The only write primitive is temp-symlink
+ `renameat2(RENAME_NOREPLACE|RENAME_EXCHANGE)` (src/utils.rs:16-134), `verify_link_target`
asserts `is_symlink` (src/linker.rs:209-215), and at least five guards refuse to touch
non-symlinks (src/linker.rs:531-548, :617-624, :753-757; src/cleanup_audit.rs:523-525;
src/cleanup_audit/prune.rs:255-267). See "Recommended architecture" for why this is probably the
wrong thing to build.

---

## What becomes pointless

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

## Practical failure modes on day one

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

## Recommended architecture

### If you are moving to a usenet mount (fork A)

Do nothing structural. Point `sources` at the mount, blank the debrid/Decypharr/DMM keys, leave
`search_missing: false`. Keep every piece of the `PathHealth`/ENOTCONN machinery — it is
load-bearing again. The only real loss is auto-acquire, which those projects replace by design
because they present themselves to Sonarr/Radarr as a SABnzbd-compatible download client.

### If you are hedging (fork B — hybrid debrid + usenet) — the recommended path

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

### If you are going to local disk (fork C)

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

### Hardlink vs symlink

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

## How to try it today

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

## Workplan

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

## Where the honest answer is "this tool has less to do"

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

## Appendix: objections considered and rejected

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

## External references (pointers only — not fetched from this machine)

- SABnzbd switches, incl. Direct Unpack and "Deobfuscate final filenames" — https://sabnzbd.org/wiki/configuration/4.5/switches
- Sonarr Completed Download Handling — https://wiki.servarr.com/sonarr/settings#completed-download-handling
- TRaSH Guides, hardlinks and instant moves — https://trash-guides.info/File-and-Folder-Structure/Hardlinks-and-Instant-Moves/
- NzbDAV (usenet-as-a-mount, SAB-compatible API) — https://github.com/nzbdav-dev/nzbdav
- AltMount — https://github.com/javi11/altmount
