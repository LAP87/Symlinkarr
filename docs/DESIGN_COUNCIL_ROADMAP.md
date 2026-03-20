# Design Council Roadmap

Date: 2026-03-20

## Goal

Make Symlinkarr substantially better on three axes at once:

1. Higher precision
2. Lower scan / relink / refresh cost
3. Fewer broken or surprising links over time

The core conclusion from this council is that Symlinkarr should stop treating parsed titles and filesystem walks as primary truth. Better systems solve identity first, file selection second, and only then render paths.

## Current Weaknesses

1. Library-side identity is strong, but source-side identity is still weak.
2. Filename parsing is doing too much work that should belong to a resolver or evidence layer.
3. Full or broad scans still happen too often relative to the amount of changed data.
4. Cleanup and restore are safer now, but canonicality and provenance are still too implicit.
5. Anime remains the hardest case because aired-order, absolute-order and split-cour mappings are not first-class enough.

## Design Principles

1. Identity beats title similarity.
2. File identity beats torrent title.
3. Local state beats repeated remote lookups.
4. Event-driven deltas beat broad rescans.
5. Cleanup, relink and acquire should use different confidence thresholds.
6. Discovery is not truth.
7. Provenance must be explicit and queryable.

## Free / Open Levers We Should Exploit Harder

### Identity and Mapping

- [Kometa Anime IDs](https://github.com/Kometa-Team/Anime-IDs)
- [anime-offline-database](https://github.com/manami-project/anime-offline-database)
- [Fribb/anime-lists](https://github.com/Fribb/anime-lists)
- [PlexAniBridge-Mappings](https://github.com/eliasbenb/PlexAniBridge-Mappings)
- [ids.moe](https://ids.moe/)
- [AnimeAPI](https://github.com/nattadasu/animeApi)

These should become a versioned local identity graph, not just ad hoc lookup helpers.

### Resolver / File Selection Prior Art

- [Stremio addon SDK](https://github.com/Stremio/stremio-addon-sdk)
- [Torrentio](https://github.com/TheBeastLT/torrentio-scraper)
- [ShokoServer](https://github.com/ShokoAnime/ShokoServer)
- [Shokofin](https://github.com/ShokoAnime/Shokofin)

The useful lesson is not their transport layer. The useful lesson is their identity model:

1. exact media id
2. exact season / episode or absolute mapping
3. exact file within the release

### Parsing / Ranking Helpers

- [GuessIt](https://guessit-io.github.io/guessit/)
- [Anitopy](https://pypi.org/project/anitopy/)
- [rank-torrent-name](https://github.com/dreulavelle/rank-torrent-name)

These are better viewed as parser / rank inputs inside a resolver pipeline, not as final truth.

### Scan / Refresh / Delta Patterns

- [Autoscan](https://github.com/autobrr/autoscan)
- [Plex Autoscan](https://github.com/l3uddz/plex_autoscan)
- [Plex `.plexmatch`](https://support.plex.tv/articles/plexmatch/)
- [Plex TV naming guidance](https://support.plex.tv/articles/naming-and-organizing-your-tv-show-files/)

These show the right shape:

1. ingest events
2. invalidate narrow state
3. refresh only the changed path

### Discovery, Not Truth

- [bitmagnet](https://github.com/bitmagnet-io/bitmagnet)

Useful for candidate discovery and indexing ideas. Not safe as a canonical identity source.

## What To Stop Doing

1. Stop using title similarity as the main source-side identity mechanism.
2. Stop assuming a DB-tracked path is automatically canonical.
3. Stop treating full scans as the normal answer to small changes.
4. Stop mixing discovery confidence and cleanup confidence.
5. Stop encoding too much volatile metadata into canonical path names.
6. Stop relying on RD cache presence as if it were stable truth.

## Target Architecture

### 1. Evidence Ledger

Create a local evidence model for every candidate file and every linked file.

Each resolved source file should persist at least:

- `media_type`
- `canonical_provider`
- `canonical_id`
- `secondary_ids`
- `season`
- `episode`
- `absolute_episode`
- `info_hash`
- `file_index`
- `filename`
- `video_size`
- `video_hash`
- `source_path`
- `resolver_kind`
- `mapping_source`
- `confidence_identity`
- `confidence_file`
- `confidence_freshness`
- `confidence_corroboration`
- `created_at`
- `last_verified_at`

### 2. Identity Graph

Build a local graph rooted in stable provider ids.

For movies and normal TV:

- TMDB
- TVDB
- IMDb

For anime:

- AniDB as preferred root where available
- crosswalk to AniList, MAL, TVDB, TMDB, IMDb
- explicit mapping provenance

This graph must be versioned and refreshable independently from scans.

### 3. Resolver Pipeline

Replace "best fuzzy match wins" with a staged resolver:

1. Resolve target media identity.
2. Resolve expected episode / season / absolute mapping.
3. Resolve candidate torrents or source groups.
4. Resolve exact file within the torrent or mount.
5. Fail closed when evidence is conflicting.

### 4. Local Persistent Index

Maintain a local persistent index for:

- library folders
- symlink targets
- source files
- torrent file inventory
- last-known matches
- last-known refresh state

Scans should become delta updates over this index, not fresh rediscovery.

### 5. Event-Driven Operations

Default flow should be:

1. receive change signal
2. invalidate a narrow subset of local state
3. resolve only affected targets
4. issue targeted Plex refresh

Broad scans remain as repair tools, not steady-state behavior.

### 6. Quarantine

Introduce a quarantine state for uncertain candidates.

Acquire may tolerate lower confidence than relink.
Relink may tolerate lower confidence than cleanup.
Cleanup should be the strictest action.

## Canonical Naming Direction

Canonical links should stay intentionally boring.

Preferred baseline:

- `Show (Year) {tvdb-12345}/Season 01/Show - S01E01.ext`
- `Movie (Year) {tmdb-12345}/Movie (Year).ext`

Optional episode titles should not be required for canonicality.
If included, they should be derived from stable metadata and never be the reason a link exists.

## Precision Improvements With Highest ROI

1. Persist `info_hash` + `file_index` when available.
2. Add file-level identity to DB and linker state.
3. Add hard media-shape guards everywhere.
4. Add anime-specific identity graph with explicit order mappings.
5. Generate `.plexmatch` when we know the exact show or movie id.

## Performance Improvements With Highest ROI

1. Persistent source index
2. Event-driven scan invalidation
3. Narrow targeted Plex refresh
4. Replay local evidence before network lookups
5. Narrow `search-missing` to explicit target scopes

## Safety Improvements With Highest ROI

1. Canonicality should be explicit, not inferred from "tracked in DB".
2. Cleanup should require reproducible evidence, not just suspicion.
3. Restore should restore provenance, not just paths.
4. Mixed tracked/untracked duplicate slots should remain conservative.
5. All destructive actions should be report-driven with safety snapshots.

## Proposed Backlog

### Epic 1: Resolver Foundations

1. Add `resolved_source_files` table with `info_hash`, `file_index`, provider ids and confidence dimensions.
2. Add `canonicality_state` for active links and duplicate slots.
3. Persist resolver provenance on every link write.

### Epic 2: Local Identity Graph

1. Build import pipeline for Kometa / manami / Fribb / PlexAniBridge data.
2. Normalize to a local `anime_identity_graph`.
3. Add conflict and provenance tracking for every mapping edge.

### Epic 3: File-Level Resolution

1. Introduce `ResolvedSourceFile`.
2. Resolve exact file per torrent when a torrent has multiple candidates.
3. Use `info_hash + file_index` as the strongest available source identity.

### Epic 4: Event-Driven State

1. Add `filesystem_state` or `source_index` table.
2. Ingest Arr or internal events as narrow invalidations.
3. Add `scan-path` and `search-missing --target` style entrypoints.

### Epic 5: Plex Interop

1. Generate `.plexmatch` for exact-ID cases.
2. Prefer targeted refresh over broad section refresh.
3. Add refresh batching per library path.

### Epic 6: Parser Layer Hardening

1. Evaluate GuessIt for TV/movie parsing fallback.
2. Evaluate Anitopy for anime parsing fallback.
3. Keep current parsers as fast-path, but compare and score parser disagreement.

### Epic 7: Cleanup / Repair Evolution

1. Make canonicality explicit in prune planning.
2. Add quarantine for uncertain replacements.
3. Add evidence-aware repair that prefers exact file identity over filename similarity.

## Cheap-Agent Ticket Order

This is the recommended execution order for smaller implementation agents.

1. Add DB schema for `ResolvedSourceFile` and provenance fields.
2. Thread `info_hash` and `file_index` through cache and scanner models.
3. Add narrow `target` filtering to `search-missing` and scan workflows.
4. Add `.plexmatch` generator for exact-ID links.
5. Build identity graph importer for one source first, then expand.
6. Add parser fallback comparison logging.
7. Introduce quarantine and confidence thresholds by action type.

## Acceptance Criteria

1. Movie links never resolve to episodic sources unless an exact id path explicitly says so.
2. Multi-file torrent resolution picks the same file deterministically across rescans.
3. Anime mappings that disagree across sources return `ambiguous`, not an active link.
4. Targeted refresh touches only affected paths.
5. Cleanup cannot delete a canonical tracked link because of a weaker duplicate signal.
6. Restore can explain exactly why and how a link was recreated.
7. A typical incremental scan does less work than a full scan by at least one order of magnitude.

## Open Questions

1. Should anime canonicality root on AniDB or on whichever id family the library folder already carries?
2. Should `.plexmatch` be written eagerly or only for selected libraries?
3. Should file hashes be optional hints or required for high-confidence relink?
4. Should legacy anime stay in a separate compatibility lane long-term?

## Recommended Next Build

If only one major feature gets built next, it should be:

`ResolvedSourceFile + provenance + file-level identity`

That single change improves:

1. precision
2. repair correctness
3. cleanup safety
4. future targeted missing-search
5. future Stremio/Torrentio-style resolution
