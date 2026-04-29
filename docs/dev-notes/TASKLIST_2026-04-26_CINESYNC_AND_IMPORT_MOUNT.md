# Tasklist: CineSync Inspiration + Provider Source Import

Date: 2026-04-26
Status: implementation started for `symlinkarr import`; remaining items still planned

## Context

This tasklist captures two related follow-up tracks:

1. Ideas worth borrowing from the CineSync source review.
2. A new provider-mount mass-import/bootstrap tool for users who already have a large RD/provider mount but do not yet have corresponding Arr/media-library folders.

Primary product stance remains unchanged:

- Normal Symlinkarr is conservative and treats the Arr/media-library side as the source of truth.
- The new import flow should be a separate, explicit bootstrap workflow.
- Build core features first. UI/UX, docs, wiki, and help expansion come after the implementation is stable.

## CineSync Ideas Worth Borrowing

### 1. Multi-Version Link Mode

Goal:
Allow multiple versions of the same movie/episode slot to coexist when explicitly enabled.

Initial behavior:

- Opt-in only.
- Default fallback policy: keep all live versions.
- Start with movies first. TV/anime can follow after behavior is proven.
- Do not change current single-canonical-link behavior unless enabled.

Possible config shape:

```yaml
linking:
  multi_version:
    enabled: false
    label_template: "{quality}"
    duplicate_policy: "keep_all"
    primary_policy: "highest_quality"
    cleanup_dead_versions: true
```

Implementation tasks:

- [x] Add config model for opt-in `symlink.multi_version`.
- [x] Add richer config validation if/when multi-version gets more knobs than a boolean: no extra validation needed for the current boolean-only surface.
- [x] Extend source parsing or match metadata with enough version labels: quality, edition, HDR/DV, codec.
- [x] Teach destination reduction to keep compatible duplicate slots when multi-version is enabled.
- [x] Add naming support for version labels, e.g. `Movie (2024) - 2160p-<hash>.mkv`.
- [x] Ensure existing DB link records can represent multiple target paths for the same media slot.
- [x] Update dead-link sweep so one dead version does not remove other live versions.
- [x] Prevent repair from replacing a dead version with a source already active for the same media item.
- [x] Add focused tests for movie multi-version create, skip, update, cleanup, and dead-link handling.

Deferred:

- [ ] Evaluate Plex/Jellyfin episode version behavior before enabling TV/anime multi-version.
- [ ] Consider `promote_best_alive` later, but default should remain `keep_all`.

### 2. Provider-Level Repair State

Goal:
Borrow the useful idea of a provider/RD repair queue without turning Symlinkarr into a full all-in-one media organizer.

Potential scope:

- Track provider items that repeatedly fail link/readiness checks.
- Surface them as repair candidates.
- Keep this separate from normal symlink repair, which already handles dead target/source drift.

Implementation tasks:

- [x] Define provider failure reasons and DB representation.
- [x] Record source-readiness and link/check failures against provider/source identity when available.
- [x] Add CLI/report output for provider repair candidates.
- [x] Decide whether auto-acquire should be allowed to repair provider-level failures.

Deferred:

- [ ] UI queue for provider repair.
- [ ] Any destructive provider-side actions. These remain opt-in and out of scope for first pass.

Decision:

- First pass is report-only. Provider-level candidates must not automatically trigger auto-acquire, DMM, Decypharr, RD deletion, or any other provider-side side effect.
- A later UI/queue can offer explicit operator actions once the candidate report has proven useful.

### 3. Release Metadata Enrichment

Goal:
Improve future version labels and upgrade decisions by parsing more release metadata.

Implementation tasks:

- [x] Extend `SourceItem` with optional fields for source quality, edition, HDR/DV, and codec where feasible.
- [x] Keep parser changes low-risk and covered by fixture tests.
- [x] Use enriched metadata in multi-version label generation.

Deferred:

- [ ] Full CineSync-style broad parser parity.
- [ ] Sports/F1/daily-show specialization unless a clear Symlinkarr workflow needs it.

## Provider Source Mass Import

### Product Intent

This is an aggressive bootstrap/import tool for users who started with DMM/RD/provider mounts and have a pile of content that is not yet represented by Arr-created folders.

It should answer:

> I have a provider folder full of files. Create ID-tagged library targets quickly so the normal Symlinkarr daemon can take over afterward.

It should also cover recovery and migration workflows:

- Fresh install recovery: no Symlinkarr DB exists, but RD/DMM/provider state still contains the user's media.
- Hardware migration: new server/container/disk where old local symlinks and Symlinkarr DB are unavailable.
- Provider-root rebuild: point at the whole provider mount and recreate ID-tagged Movies/TV/Anime library targets.
- Arr-less bootstrap: user started in DMM/RD first and wants to create an Arr-like tagged library surface afterward.

This is not the normal conservative scan flow.

Normal flow:

- Existing Arr/media-library folders are source of truth.
- Symlinkarr matches provider files into those folders.

Import flow:

- Provider/source folder is the initial input.
- Symlinkarr identifies files best-effort.
- Symlinkarr creates tagged library folders and optionally first symlinks.
- Normal scan/daemon later refines, repairs, and relinks.

### Command Shape

Proposed command:

```bash
symlinkarr import \
  --source /mnt/rd/movies \
  --destination /library/movies \
  --content-type movie \
  --mode safe
```

Power-user routing command:

```bash
symlinkarr import \
  --source /mnt/rd \
  --rules /config/import-rules.xml \
  --content-type auto \
  --mode aggressive
```

Example `import-rules.xml`:

```xml
<importRules>
  <destinations>
    <movies default="/library/movies">
      <route resolution="2160p" to="/library/movies-4k" />
      <route quality="remux" to="/library/movies-remux" />
      <route hdr="dv" to="/library/movies-dolby-vision" />
      <route codec="hevc" to="/library/movies-hevc" />
    </movies>

    <tv default="/library/tv">
      <route resolution="2160p" to="/library/tv-4k" />
      <route sourcePathContains="/kids/" to="/library/kids-tv" />
    </tv>

    <anime default="/library/anime">
      <route resolution="2160p" to="/library/anime-4k" />
      <route audio="ja" subtitles="en" to="/library/anime-subbed" />
    </anime>
  </destinations>
</importRules>
```

Content types:

- `movie`
- `tv`
- `anime`
- `auto` for broad provider-root imports that should classify candidates into movie/TV/anime destinations.

Modes:

- `preview`: scan + report only, no writes, no prompt.
- `safe`: high-confidence imports only, no destination conflicts.
- `aggressive`: best-effort bootstrap, lower confidence threshold, fewer blockers.

Current implementation slice:

- [x] Add config-free `symlinkarr import` CLI entrypoint.
- [x] Detect missing/file/direct item/multi-item folder/broad provider-root source shapes.
- [x] Detect direct-item vs top-level folders, including single season-folder handling.
- [x] Extract common explicit IDs from path names: `tmdb`, `tvdb`, `imdb`.
- [x] Build JSON/text reports with source shape, destinations, candidate decisions, target paths, and summary counts.
- [x] Implement live Unix symlink writes for `safe` and `aggressive`.
- [x] Make `safe` import explicit-ID candidates only and refuse existing targets.
- [x] Make `aggressive` import unresolved candidates and replace existing symlink targets.
- [x] Add Y/N confirmation for `aggressive` unless `--yes` is passed.
- [x] Require `--yes` for write-capable import modes when stdin is non-interactive.
- [x] Keep non-symlink target overwrite blocked even in `aggressive`.
- [x] Add actual `import-rules.xml` default destination and top-down route matching for fast filename/path metadata.
- [x] Use existing Symlinkarr config/db when present, while keeping import runnable without config.
- [x] Add cache-only metadata lookup for unresolved imports from existing `api_cache` rows.
- [x] Add explicit `--lookup-mode remote` and `--max-lookups` for bounded TMDB search lookup.
- [x] Keep remote lookup conservative: only accept one exact normalized title match, with year match when available.
- [x] Cache successful remote import resolutions as minimal metadata entries for later cache-first runs.
- [x] Add first `probe`/`strict` metadata support via low-probesize `ffprobe` stream reads.
- [x] Add native `mediainfo --Output=JSON` probing path for users who prefer mediainfo.
- [x] Let rules match probed resolution, codec, HDR, audio language, and subtitle language before falling back to filename/path hints.
- [x] Include `ffprobe` and `mediainfo` in default Docker images and surface optional availability in `doctor`.
- [x] Backfill DB link records for import-created/import-updated symlinks when a Symlinkarr DB is available.
- [x] Write a JSON audit report for every import run, with `--report-path` available for explicit placement.
- [x] Add broad import summary counters for movie/TV/anime/unknown content.
- [x] Add post-import handoff messages to reports/text output.

Still planned:

- [x] Add remote metadata lookup for unresolved candidates when explicitly enabled.
- [x] Add TVDB-native remote search for TV/anime imports when TVDB credentials are configured.
- [ ] Enrich probe metadata beyond resolution/codec/HDR/audio/subtitle languages:
  - [x] Dolby Vision profile labels from `ffprobe` side data and `mediainfo` HDR profile data, e.g. `dv-p8`.
  - [ ] Edition metadata beyond current path/title rule matching.
- [x] Prompt before writes for both `safe` and `aggressive` unless `--yes` is passed.
- [ ] Add deeper TV/anime target shaping if folder symlink import is not enough for common servers.
- [x] Add Web UI and docs/wiki after CLI behavior stabilizes.

Interactive behavior:

- `preview` never writes.
- `safe` and `aggressive` run scan + preview first.
- If writes are available and terminal is interactive, ask `Continue and apply these changes? [y/N]`.
- `aggressive` uses a louder warning.
- `--yes` skips prompts for automation.
- Non-interactive runs without `--yes` must fail before writing.

Example prompt:

```text
Aggressive import preview

Source:       /mnt/rd/movies
Destination:  /library/movies
Content type: movie

Candidates:   842
Will create:  811 folders
Will link:    811 files
Ambiguous:    24
Skipped:      7

This mode may create incorrect folders if provider filenames are ambiguous.
Continue and apply these changes? [y/N]
```

### Safety Boundaries

Even aggressive mode should keep a few hard guards:

- [x] Never overwrite real files.
- [x] Never remove provider/RD/Usenet content.
- [x] Prefer an explicit destination root: `auto` imports now require `--destination`, per-type destinations, or `--rules`.
- [x] Warn loudly when destination is already populated.
- [x] Write a report for every run.
- [x] Make all writes auditable.
- [ ] Make writes replayable from a saved report, if we decide replay is worth the surface area.

Allowed in aggressive mode:

- [x] Create ID-tagged targets from best-effort matches where IDs are available.
- [x] Create initial symlinks quickly.
- [ ] Accept lower-confidence single best matches.
- [x] Leave imperfect results for normal daemon/scan cleanup.

### Import Implementation Plan

Phase 1: CLI and report-only pipeline

- [x] Add `import` CLI command.
- [x] Add `ImportMode` enum: `preview`, `safe`, `aggressive`.
- [x] Define import report schema before write behavior.
- [x] Define first confidence/provenance fields for imported candidates:
  - `confidence`: high, medium, low, ambiguous.
  - `resolution_source`: explicit_id, cache, tmdb_lookup, tvdb_lookup, unresolved.
  - `mode_decision`: created, skipped, linked/would-create/would-update, needs_review, failed_lookup via report decision/action/reason.
- [ ] Add richer provenance later if needed:
  - `selected_title`, `alternates`.
  - arr_hint, plex_hint, anime_lists resolution sources.
- [x] Decide whether imported folders/links should get an `origin=import` DB marker: no marker for the first pass; reports plus normal DB link records are enough unless richer provenance becomes useful later.
- [x] Add content-type selection and required source/destination validation.
- [x] Support single-destination scoped imports:
  - `--destination /library/movies --content-type movie`
  - `--destination /library/tv --content-type tv`
  - `--destination /library/anime --content-type anime`
- [x] Support broad provider-root imports with multiple destination roots:
  - `--movie-destination /library/movies`
  - `--tv-destination /library/tv`
  - `--anime-destination /library/anime`
  - `--content-type auto`
- [x] Support full import routing rules from the first implementation via `--rules /config/import-rules.xml`.
- [x] Keep destination CLI flags as the simple path; use `import-rules.xml` for power-user routing and custom library ordering.
- [x] Allow pointing directly at a provider mount root, e.g. `/mnt/rd`, when the user intentionally wants a broad import.
- [x] Detect broad/root imports and warn before writes, but do not block them.
- [x] Scan selected provider folder only.
- [x] Detect source shape before grouping:
  - multi-item containing folder, e.g. `/mnt/rd/movies` with many movie folders/files.
  - broad provider mount root, e.g. `/mnt/rd`, with mixed movies/TV/anime.
  - direct movie item folder, e.g. `/mnt/rd/Dune.2021.2160p`.
  - direct TV/anime show root, e.g. `/mnt/rd/Show.Name.S01` or `/mnt/rd/Show.Name/Season 01`.
- [x] For direct item sources, treat the provided source path as one import candidate instead of assuming every child is a separate title.
- [x] For top-level multi-item sources, group children into separate candidate items.
- [x] For broad provider-root sources, group by torrent/folder/item boundaries before classifying content type, including one-level expansion for common category roots like Movies/Shows/Anime.
- [x] Group candidate files by likely movie/show/anime title enough for first pass: file-only folders with multiple non-episodic videos become multi-item candidates, while sibling episodic files for the same title remain a direct item.
- [x] Produce JSON and human-readable reports with `created`, `linkable`, `ambiguous`, `skipped`, `failed_lookup`.
- [x] Add broad-root summary counters by content type.
- [x] Add interrupted-run behavior: reruns are idempotent for already-correct import symlinks and report skipped targets.

Provider repair reporting note:

- `symlinkarr report` now surfaces a read-only provider repair sample from existing link events such as `source_missing_before_link`, `source_unreadable_before_link`, dead-link detection, and repair failures.
- This is intentionally not an auto-repair queue yet. It gives us operator visibility first, then we can decide whether provider-level failures should feed auto-acquire or a UI queue.

Phase 2: Metadata identification

- [x] Resolve IDs cache-first for explicit IDs, existing Symlinkarr metadata cache rows, and existing TMDB/TVDB external ID cache rows.
- [ ] Add anime-lists/import-resolution cache coverage where useful:
  - anime-lists cache.
  - [x] import-resolution cache, e.g. `import:resolve:<type>:<normalized-title>:<year>`, written from successful remote lookup and reused before broad metadata-cache scans.
- [x] Only perform online lookup when explicit IDs/cache are insufficient.
- [x] Write successful online resolution results back to cache.
- [x] Movie lookup: title + year -> TMDB.
- [x] TV lookup: title -> TVDB/TMDB TV for show-level import.
- [ ] Anime lookup: reuse anime parser and Anime-Lists mapping where useful, but keep bootstrap fast.
- [x] Detect explicit IDs in source path first: `tmdb-123`, `{tmdb-123}`, `tvdb-456`.
- [x] Add confidence classification: high for explicit/cache/accepted remote IDs, low for unresolved candidates.
- [x] Mark exact remote lookup duplicates as `ambiguous`/`needs_review` instead of plain unresolved.
- [ ] Add medium confidence classification once broader fuzzy matching exists.
- [x] Add `--offline` for explicit-ID-only imports.
- [x] Add later flags for lookup behavior:
  - `--refresh-metadata` to bypass stale cache.
- [x] Add `--max-lookups N` as a rate-limit guard for large root imports.
- [x] Define rate-limit behavior for online lookups in root imports: stop at `--max-lookups` and warn how many unresolved candidates remain.

Phase 2b: Media stream probing

- [x] Include both `ffprobe` (via ffmpeg) and `mediainfo` in the default Docker image.
- [x] Keep import default at `--metadata-mode fast`; do not open/probe media files unless requested or required by rules.
- [x] Support import metadata modes:
  - `fast`: filename/path/release-title hints only.
  - `probe`: use bounded stream probing when routing rules require audio/subtitle/stream metadata.
  - `strict`: audio/subtitle/stream routing rules only match when verified by probing.
- [x] Support probe tool selection:
  - `auto`: prefer `ffprobe`, fallback to `mediainfo`.
  - `ffprobe`.
  - `mediainfo`.
- [x] Probe only when a route uses fields that cannot be resolved from filename/path hints, e.g. audio/subtitles.
- [x] Minimize RD/provider transfer:
  - use small probe/read limits where the tool supports it.
  - never use frame counting, thumbnails, or full-duration analysis.
  - use timeout per file.
  - use low concurrency, default 1 for broad root imports.
  - [x] cache probe results by source identity/path + size/mtime where available.
  - [x] for TV/anime groups, probe a representative file first and route the group from that result when reasonable.
- [x] Treat failed/expensive probes as `probe_failed` and continue according to mode.
- [x] Add doctor checks for `ffprobe` and `mediainfo` availability and versions.

Phase 3: Folder creation

- [x] Create movie folders as `Title (Year) {tmdb-123}` when cache/remote metadata resolves the canonical title/year.
- [x] Create TV/anime folders as `Title (Year) {tvdb-123}` when cache/remote metadata resolves the canonical title/year.
- [x] Resolve destination through `import-rules.xml` when provided.
- [x] Routing rules should support, from the start:
  - content type: movie, tv, anime.
  - resolution: 2160p/4K, 1080p, 720p, 480p, unknown.
  - quality/source: remux, bluray, webdl, webrip, hdtv, dvd, unknown.
  - HDR/video traits: dv, hdr10, hdr10plus, hlg, sdr, unknown.
  - edition: extended, directors-cut, theatrical, unrated, remastered, collectors, unknown.
  - language/audio hints when parsed: original language, dub/sub hints, audio codec/channel if available.
  - source path contains / release title contains.
  - fallback/default destination per content type.
- [x] Route evaluation is top-down, first matching route wins.
- [x] If no route matches, use the content-type default destination.
- [x] Support safe mode only for high-confidence, non-conflicting folders.
- [x] Support aggressive mode for best-effort medium/low confidence when not ambiguous.
- [x] Decide destination conflict policy: skip non-symlink conflicts; aggressive can replace existing symlink targets only.
- [x] Refuse conflicts instead of overwriting non-symlink targets; aggressive may replace existing symlink targets only.
- [x] Add naming rules for unknown year, missing external IDs, and conflicting providers.

Phase 4: Optional initial symlink creation

- [x] Decide whether first implementation defaults to folder creation only.
- [x] Add `--folders-only` and `--create-links` behavior.
- [ ] Use existing linker/naming helpers where possible.
- [x] Ensure DB link records are written consistently for import-created/import-updated symlinks when a DB is available and the candidate has an ID.

Decision:

- Default remains create-links because this is the fast bootstrap path users asked for.
- `--folders-only` creates real ID-tagged folders without provider symlinks. Normal scan/daemon can fill them later.
- `--create-links` is an explicit spelling for the default write behavior and cannot be combined with `--folders-only`.
- [x] Keep destructive cleanup out of import.

Phase 5: Daemon handoff

- [x] Ensure created folders are immediately discoverable by normal `scan`, including import-created top-level folder symlinks with ID tags.
- [x] Ensure normal scan can update/reconcile import-created links.
- [x] Add a post-import summary that tells the user to run normal scan/daemon.

Phase 6: Tests

- [x] Unit tests for source grouping and ID extraction.
- [x] Unit tests for mode/confidence decisions.
- [x] Import dry-run/report fixtures for movie, TV, anime, mixed root, and direct item folder.
- [x] Integration tests for preview-only no-write behavior.
- [x] Integration tests for safe mode writes.
- [x] Integration tests for aggressive prompt behavior.
- [x] Integration tests for non-interactive `--yes` automation.
- [x] Regression tests for refusing to overwrite real files.

## Later UI/UX, Docs, Wiki, Help

Do after core behavior is stable.

## Later Media Server Refresh / Probe Storm Investigation

Context:

- User observed heavy ZFS RAID activity while multiple media servers were running many `ffprobe` jobs at the same time.
- Likely overlap to investigate: Symlinkarr creates/updates symlinks, then one or more media servers may immediately deep-scan/analyze the same files while their own scheduled scans and folder watchers are also active.
- Do not assume Symlinkarr is the only trigger; verify current refresh behavior against Plex/Jellyfin/Emby watchers, scheduled scans, and any library-refresh calls we make.

Questions to answer:

- [x] When does Symlinkarr actively tell Plex/Jellyfin/Emby to refresh libraries or paths?
- [x] Can those refresh calls stack across all configured media servers after scan/import/repair/link changes?
- Do we need an optional "signal only / deferred refresh" mode where Symlinkarr records changed paths and leaves actual media-server scanning to scheduled scans or folder watchers?
- Should media-server refresh be queued behind Symlinkarr's own scan/link/repair work, rate-limited, or disabled by default for users with aggressive media-server watchers?
- Can we expose per-server toggles: immediate refresh, deferred refresh, manual-only, disabled?
- Can we document the tradeoff clearly: faster media-server visibility vs less disk/probe pressure?

Initial code audit notes:

- Scan calls `invalidate_after_mutation(...)` after link creation/update when media-server refresh is configured: `src/commands/scan.rs`.
- Repair calls `invalidate_after_mutation(...)` after repaired/stale affected paths: `src/commands/repair.rs`.
- Cleanup/prune/remediation calls `invalidate_after_mutation(...)` for affected paths, or `refresh_selected_library_roots(...)` when only selected roots are known: `src/commands/cleanup.rs`.
- `src/media_servers/mod.rs` executes planned refreshes for all configured refresh backends through `FuturesUnordered`, so Plex, Emby, and Jellyfin can be asked to refresh concurrently inside one Symlinkarr refresh phase.
- Symlinkarr already has an inter-process media refresh lock and deferred queue when another Symlinkarr process is refreshing, but that does not protect against the media servers' own folder watchers/scheduled scans running at the same time.
- Plex target selection currently uses library roots, while Emby/Jellyfin can use targeted paths or root fallback depending on cap settings.
- Import currently does not trigger media-server refresh directly; its report tells the user to run normal scan/daemon afterwards.

Planned tasks:

- [x] Audit all Symlinkarr call sites that trigger media-server refresh/analyze behavior.
- [x] Compare current target planning behavior for Plex, Jellyfin, and Emby.
- [x] Add config option(s) for optional/deferred media-server refresh behavior if needed: `refresh_mode: immediate|deferred|disabled`.
- [x] Add manual drain command for deferred refresh work: `symlinkarr refresh drain`.
- [x] Add observability/logging that shows when Symlinkarr requested media-server refreshes and for which paths.
- [x] Document recommended settings for users who already rely on media-server scheduled scans and folder watch.

UI/UX tasks:

- [x] Web UI import preview page.
- [x] Web UI import apply path with safe default and explicit Force checkbox for aggressive/bootstrap behavior.
- [x] Confidence filters and candidate review table.
- [x] Report detail view.
- [x] Clear warning states for aggressive mode.
- [ ] Run-history integration.

Docs/wiki/help tasks:

- [x] CLI manual entry for `import`.
- [x] Wiki page: Provider Source Import.
- [x] Wiki page/update: CineSync comparison and product scope.
- [x] Help text for `preview`, `safe`, and `aggressive`.
- [x] Examples for movies, TV, anime.
- [x] Safety section explaining that Symlinkarr does not mutate RD/Usenet content.

## Suggested Build Order

1. Multi-version data model and movie-only implementation.
2. Multi-version tests and dead-link behavior.
3. Import preview/report pipeline.
4. Import safe mode.
5. Import aggressive mode with interactive prompt.
6. Optional initial symlink creation.
7. Provider-level repair state.
8. UI/UX.
9. Docs/wiki/help.
