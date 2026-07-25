# Design Council: CineSync-Inspired Features + Provider Source Import - 2026-04-26

Source: CineSync source review, Symlinkarr implementation comparison, and follow-up product discussion around DMM/RD-first users, recovery, and import workflows.

## No-Churn Council Refresh - 2026-04-29

Constraint:
This pass intentionally avoids adding more safety ceremony. Symlinkarr already keeps the important hard lines: no provider deletion, no real-file overwrite, reports for import, and explicit operator intent for write paths. The question here is only whether the current feature set is useful, understandable, and not missing obvious operator needs.

### Current Product Decisions

- Web UI is the operator window to Symlinkarr, so feature parity with CLI is expected.
- Web Import should preview first, then allow apply from the same page.
- Web Import defaults to safe behavior.
- Web `Force` is the user-facing label for CLI aggressive/bootstrap behavior.
- No text-confirm is required for Web Apply. A clear checkbox + apply button is enough for now.
- Import JSON reports are sufficient for v1.1-RC; full run-history integration can wait.
- Anime-Lists should be reused where it helps anime import, but it should remain cached and TTL-based rather than fetched on every import run.

### What Still Looks Worth Doing

- Add anime import reuse of the existing anime parser and Anime-Lists identity graph where it improves ID selection or title hints without slowing the common bootstrap path.
- Consider a small "copy CLI command" affordance in the Web Import preview so users can reproduce the exact run outside the browser.
- Consider showing report path + post-import daemon/scan handoff more prominently after Web Apply.
- Keep `aggressive` as the CLI compatibility name for now, but document Web `Force` as the friendlier UX label.
- If users ask for it, add import report listing/detail later rather than full run-history now.

### Things To Avoid For v1.1-RC

- Do not add provider-side destructive actions to provider repair.
- Do not turn provider repair candidates into automatic acquisition actions.
- Do not build full CineSync-style broad parser parity unless a concrete Symlinkarr workflow needs it.
- Do not add low-confidence automatic matching beyond the explicit Force/bootstrap path.
- Do not make every import run refresh Anime-Lists from the network.
- Do not block Web Apply behind typed confirmation unless real-world use shows misclicks are a problem.

### Missed Or Easy-To-Forget Items

- Web Apply reports should stay discoverable: current target is `backup.path/import-reports/`.
- The Force helper text should explain "tries to include low-confidence or unresolved candidates", not imply filesystem danger.
- Docs should consistently say Web Import is preview + apply, not read-only.
- Import should remain a bootstrap/recovery tool, not a general all-in-one organizer.
- Provider repair report-only mode is distinct from dead-link repair; docs and UI should keep those concepts separate.

## Real Council Refresh - 2026-04-29

Participants:

- Product/UX reviewer
- Operator workflow reviewer
- Implementation maintainer
- Docs/support reviewer

Constraint:
No safety-churning. Findings below are about clarity, operator usefulness, correctness, and support load.

### Fix-Now Findings

1. Web Import preview is not semantically clean enough.
   The Web form posts `mode=safe`, so preview is really a safe-plan preview, not the same as CLI `--mode preview`. This can be acceptable if the label says "Safe preview", but the better product model is:
   - preview builds a read-only plan for the currently selected Web intent
   - apply uses safe by default or Force when checked
   - the summary must make planned writes obvious

2. Import summary can understate actionable work.
   Safe/Force planning can produce candidates with `action=create` while `decision=preview`, but the summary may still show zero `would_create`. This is confusing in both CLI and Web. Add planned create/update counts or count actionable pre-apply rows more clearly.

3. Provider repair candidate count is a sample count.
   Current report output can present a limited sample as if it were the total backlog. Rename/count it as a sample or add a real grouped total later.

4. Config examples drifted.
   Docker config should mirror `refresh_mode` comments from the main example. Multi-version also needs a commented example line and a small operator-facing note.

### Actions Taken

- Web Import now labels the plan as safe or force so the preview matches the selected Web intent.
- Import summaries now count planned create/update actions before apply, so preview does not hide planned writes.
- Provider repair text output now says "Sampled candidates" instead of implying a total backlog count.
- Docker config now includes `refresh_mode` examples for Plex, Emby, and Jellyfin.
- Config examples now include `symlink.multi_version: false`.
- README, wiki home, wiki source index, Provider Source Import, Repair and Dead Links, Media Server Refresh, and CLI docs were updated for import, Force, reports, provider repair, and multi-version operator guidance.

### Docs/Support Findings

1. Add a "Rebuild from RD/provider mount" recipe.
   It should cover: restore/create config, run import preview, apply, run `symlinkarr scan` or leave daemon running, then handle media-server refresh/watchers.

2. Add import to README common commands and wiki home/common flows.
   Users asking "I already have RD/DMM content" should find import without reading dev notes.

3. Expand Provider Source Import wiki.
   Needed sections:
   - Force in Web equals CLI aggressive/bootstrap behavior.
   - Web report path: `backup.path/import-reports/`.
   - CLI report path default vs `--report-path`.
   - `--folders-only` vs default provider symlink writes.
   - Minimal `import-rules.xml` example, first match wins.
   - Anime-Lists cache behavior and `symlinkarr cache invalidate anime-lists`.

4. Add provider repair visibility to repair/dead-link docs.
   Make clear that provider repair reporting is read-only and distinct from normal dead-link repair.

5. Add a small multi-version operator note.
   Opt-in, movie-first, version labels, dead-link cleanup affects only dead version, TV/anime and `promote_best_alive` deferred.

### Defer

- Full import run-history listing.
- Provider repair UI queue.
- Visual import-rules builder/tester.
- Forced Anime-Lists refresh on every import run.
- Full CineSync parser parity.
- Sports/F1/daily-show specialization.
- Typed Web confirmation for Force.

## Question

What should Symlinkarr build next from the CineSync comparison and the new provider-source import idea, and what gaps should be resolved before implementation?

## Planned Feature Set

### 1. Multi-Version Link Mode

Purpose:
Allow more than one source/version for the same movie or episode slot when explicitly enabled.

Planned shape:

- Opt-in config.
- Movie-first implementation.
- Default fallback behavior: keep all live versions.
- Version labels from parsed metadata such as quality, source, edition, HDR/DV.
- Dead-link cleanup removes or marks only the dead version, not the whole media slot.

Open expansion:

- TV/anime version handling after Plex/Jellyfin behavior is tested.
- Future `promote_best_alive` behavior if users want a canonical fallback symlink.

### 2. Release Metadata Enrichment

Purpose:
Improve version labels, future upgrade decisions, and import confidence.

Planned shape:

- Extend `SourceItem` with optional release metadata.
- Parse quality source, edition, HDR/DV, codec, language/audio where feasible.
- Keep this parser work fixture-driven and incremental.

Open expansion:

- Broader parser parity with CineSync only if it directly improves Symlinkarr workflows.

### 3. Provider-Level Repair State

Purpose:
Track provider/RD-side items that repeatedly fail source readiness, link validation, unrestrict/checklink, or similar provider-level checks.

Planned shape:

- Separate from normal symlink dead-link repair.
- Record provider failure reason when source identity is available.
- Surface repair candidates in CLI/report output first.

Open expansion:

- UI repair queue later.
- Provider-side destructive actions remain separate opt-in work, not first pass.

### 4. Provider Source Import

Purpose:
Bootstrap or rebuild ID-tagged library targets from a provider source folder or full provider mount.

Core use cases:

- DMM/RD-first users with media not yet represented in Arr folders.
- Fresh install recovery with no Symlinkarr DB.
- Hardware/container/disk migration where old local symlinks and DB are unavailable.
- Provider-root rebuild from a full RD mount.
- Arr-less bootstrap into an Arr-like tagged library surface.

Command:

```bash
symlinkarr import \
  --source /mnt/rd/movies \
  --destination /library/movies \
  --content-type movie \
  --mode safe
```

Broad root form:

```bash
symlinkarr import \
  --source /mnt/rd \
  --movie-destination /library/movies \
  --tv-destination /library/tv \
  --anime-destination /library/anime \
  --content-type auto \
  --mode aggressive
```

Power-user routing form:

```bash
symlinkarr import \
  --source /mnt/rd \
  --rules /config/import-rules.xml \
  --content-type auto \
  --mode aggressive
```

Modes:

- `preview`: scan and report only.
- `safe`: write high-confidence, non-conflicting imports.
- `aggressive`: best-effort bootstrap with lower confidence threshold and a louder confirmation prompt.

Required behavior:

- Understand direct item folders versus multi-item top-level folders versus broad provider root.
- Support `import-rules.xml` from the first implementation for users who want resolution/quality/HDR/edition/language/path-based destination routing.
- Use explicit IDs and cache DB first.
- Use online lookup only when needed.
- Include `ffprobe` and `mediainfo` in the default Docker image, but keep media stream probing opt-in through metadata mode and routing needs.
- Ask `Continue and apply these changes? [y/N]` for interactive writes.
- `--yes` allows automation.
- Non-interactive writes without `--yes` must fail.

### 5. Later UI/UX, Docs, Wiki, Help

Purpose:
Only after the core features are stable, expose them cleanly.

Planned shape:

- Import preview page.
- Confidence filters and candidate review.
- Report detail view.
- Aggressive-mode warnings.
- Run-history integration.
- CLI manual and wiki pages.

## Council Roles

### Operator Pragmatist

Goal:
Users should get their library back or bootstrapped with minimal waiting and minimal ceremony.

Position:

- `import` is not normal conservative scan. It should be fast, direct, and useful for people who know what they are doing.
- Allow full RD/provider root imports.
- Support multiple destinations with `content-type auto`.
- Aggressive mode should apply after a human Y/N prompt without requiring an extra write flag.
- A report is mandatory, but a perfect report should not block import progress.

Concern:
If `import` inherits too much of the existing cautious matcher behavior, it will fail the user who needs it most: someone with a huge provider mount and no local state.

### Safety Engineer

Goal:
Even aggressive workflows should not destroy real data or silently create unrecoverable mess.

Position:

- Never overwrite real files.
- Never delete provider/RD/Usenet content.
- Non-interactive writes require `--yes`.
- Always write a report with enough information to audit and re-run.
- Destination conflicts need explicit policy: skip, quarantine, or suffix. Do not guess silently.

Concern:
The word `aggressive` must mean "accept imperfect metadata", not "ignore filesystem safety."

### Metadata Skeptic

Goal:
Avoid filling a media library with confident-looking wrong IDs.

Position:

- Cache-first is correct, but stale cache and weak title parsing can create false confidence.
- Safe mode needs strict confidence rules:
  - explicit ID in path/name
  - strong title + year
  - unique provider result
  - no competing plausible hit
- Aggressive mode should classify uncertainty in the report even if it proceeds.

Concern:
The import tool should distinguish "best effort" from "verified" in both DB and reports, or later daemon behavior will be hard to explain.

### Integration Realist

Goal:
Use surrounding stack data when available, but do not make import depend on a fully healthy Arr stack.

Position:

- Import should work without Sonarr/Radarr.
- If Arr/Plex data is available, use it as hints.
- Existing Symlinkarr cache, TMDB/TVDB cache, external IDs, and anime-lists cache should be first-class inputs.
- Add `--offline`, `--refresh-metadata`, and `--max-lookups` after the first implementation path is clear.

Concern:
Avoid building a hidden "Arr replacement." Import should create the initial target surface and hand off to normal Symlinkarr/Arr workflows.

### Media Server Advocate

Goal:
Make output friendly for Plex/Jellyfin/Emby.

Position:

- Multi-version should start movie-only because Plex/Jellyfin movie version grouping is clearer than episode versioning.
- Naming must be predictable and media-server-compatible.
- Import should create folders that look like normal Symlinkarr targets, not a special import-only layout.

Concern:
If import creates odd folder names or mixed media roots, cleanup and media-server matching will become harder later.

### Power User Librarian

Goal:
Let users express their own library routing once, so they do not need custom scripts or follow-up questions for every folder layout.

Position:

- `import-rules.xml` should exist from the first implementation, not as a forgotten later enhancement.
- CLI destination flags are fine for simple setups.
- XML rules should handle users who split libraries by 4K, remux, HDR/DV, edition, language, anime, kids, or path conventions.
- Route order should be explicit: top-down, first match wins.

Concern:
If routing starts too small, users will immediately ask for every missing condition. It is better to define the routing vocabulary early.

### Quota Realist

Goal:
Avoid RD/provider quota waste from import metadata probing.

Position:

- Filename/path parsing should remain the default.
- Stream probing should happen only when route rules actually need stream metadata such as audio or subtitles.
- Both `ffprobe` and `mediainfo` can be installed in the default container, but the presence of tools must not make probing default behavior.
- Probe reads need hard limits: timeout, low concurrency, no frame counting, no thumbnails, no full-file analysis.
- Probe results should be cached.
- For grouped TV/anime releases, probe a representative file first rather than every episode.

Concern:
Plex and similar tools can burn RD transfer by reading too much. Symlinkarr import must avoid becoming another quota-heavy scanner.

## Council Debate

### Debate 1: Is `import` too broad for Symlinkarr?

Consensus:
No, as long as it is framed as bootstrap/recovery, not as the normal operating model.

Reasoning:

- Public users are arriving with DMM/RD-first libraries.
- Recovery from lost local state is a real operational need.
- Symlinkarr already understands enough of source scanning, metadata IDs, naming, and link repair to own this better than a separate script.

Boundary:
Normal `scan` remains conservative. `import` is explicitly a different tool.

### Debate 2: Should full provider-root import be allowed?

Consensus:
Yes.

Reasoning:

- Power users should be able to point at `/mnt/rd` when they know what they are doing.
- Blocking root imports would make the feature less useful for recovery and migration.

Required controls:

- Broad/root detection.
- Clear preview summary.
- Multiple destination roots for `movie`, `tv`, and `anime`.
- Strong report output.
- Non-interactive writes gated by `--yes`.

### Debate 3: Should aggressive mode require a write flag?

Consensus:
No separate write flag is required for interactive use.

Preferred behavior:

- Run scan + preview.
- Prompt `Continue and apply these changes? [y/N]`.
- Apply only on explicit `y`.
- In scripts, require `--yes`.

Rationale:
The confirmation prompt is clearer than forcing users to learn `--apply` plus mode semantics.

### Debate 4: Should import create symlinks immediately?

Provisional answer:
Support it, but keep it explicit until the first implementation proves itself.

Options:

- `--folders-only`: create tagged folders, let daemon create links later.
- `--create-links`: create initial symlinks during import.
- Later default can be decided from real usage.

Council leaning:
Start with folder creation and report pipeline first. Add direct link creation once DB/linker integration is tested.

### Debate 5: How should confidence be recorded?

Consensus:
Every import candidate needs a confidence and provenance record.

Suggested fields:

- `confidence`: high, medium, low, ambiguous.
- `resolution_source`: explicit_id, cache, arr_hint, plex_hint, tmdb_lookup, tvdb_lookup, anime_lists.
- `selected_id`.
- `selected_title`.
- `alternates`.
- `mode_decision`: created, skipped, linked, needs_review, failed_lookup.

Reason:
Normal daemon/scan and future UI need to explain why an imported folder exists.

### Debate 6: Should routing rules be full-featured from the start?

Consensus:
Yes.

Reasoning:

- Import is the place where users decide physical library layout.
- Users with RD/DMM-first libraries often have personal rules already: 4K separate, remux separate, anime separate, kids separate, HDR/DV separate.
- A small rules file is cleaner than adding many one-off CLI flags.

First implementation should support:

- content type
- resolution
- quality/source
- HDR/video traits
- edition
- language/audio hints when parsed
- source path contains
- release title contains
- default destination per content type
- top-down first-match-wins evaluation

### Debate 7: Should media probing be built in or rely on host tooling?

Consensus:
Install both `ffprobe` and `mediainfo` in the default Docker image, but keep probing opt-in.

Reasoning:

- Container users should not need to build a custom image just to verify audio/subtitle streams.
- The image size increase is acceptable.
- Default import behavior must still be `fast`, using filename/path hints only.

Probe policy:

- `fast`: no media file probing.
- `probe`: bounded probing when route fields require it.
- `strict`: audio/subtitle/stream rules only match when verified.
- `auto` tool selection prefers `ffprobe` and falls back to `mediainfo`.
- Timeout and concurrency limits are required.
- Probe cache is required.
- Failed probes should not abort the whole import unless strict mode makes that candidate unroutable.

## Missing Pieces To Add To The Tasklist

- [ ] Define import report schema before write behavior.
- [ ] Define confidence/provenance fields for imported candidates.
- [ ] Decide whether imported folders/links should get an `origin=import` DB marker.
- [ ] Decide destination conflict policy: skip, quarantine, suffix, or require explicit flag.
- [ ] Add broad-root summary counters by content type.
- [ ] Add `--folders-only` and `--create-links` decision.
- [ ] Decide whether `safe` should prompt or auto-apply in interactive mode after preview.
- [ ] Add interrupted-run behavior: resume, rerun idempotently, or report partial writes.
- [ ] Add rate-limit behavior for online lookups in root imports.
- [ ] Add naming rules for unknown year, missing external IDs, and conflicting providers.
- [ ] Add import dry-run/report fixtures for movie, TV, anime, mixed root, and direct item folder.
- [ ] Add `import-rules.xml` schema/parser and fixture examples.
- [ ] Add destination-routing tests for resolution, quality, HDR/DV, edition, language/path, and default fallback.
- [ ] Add Docker runtime dependency install for `ffmpeg` and `mediainfo`.
- [ ] Add bounded media-probe helper with timeout, concurrency, and result cache.
- [ ] Add doctor checks for `ffprobe` and `mediainfo`.

## Recommended Build Order

1. Define import report schema and confidence/provenance model.
2. Implement `symlinkarr import --mode preview` with source-shape detection.
3. Add cache-first ID resolution and explicit-ID extraction.
4. Add single-destination folder creation in `safe`.
5. Add broad provider-root import with multiple destinations and `content-type auto`.
6. Add `import-rules.xml` routing.
7. Add aggressive mode prompt and `--yes`.
8. Add optional initial symlink creation.
9. Implement movie-only multi-version.
10. Add release metadata enrichment needed by multi-version labels.
11. Add provider-level repair state.
12. Build UI/UX and docs/wiki/help after CLI behavior is stable.

## Council Conclusion

The strongest near-term product move is `symlinkarr import`.

It converts a limitation into a recovery and onboarding strength:

- normal Symlinkarr remains conservative
- import becomes the rugged bootstrap path
- the daemon/scan loop refines the result afterward

The most important design rule is:

**Aggressive import can be metadata-aggressive, but it must remain filesystem-conservative.**
