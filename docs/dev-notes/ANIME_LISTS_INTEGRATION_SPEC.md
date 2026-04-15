# Anime-Lists Integration Spec

## Goal

Use `Anime-Lists/anime-lists` as an anime-only identity and episode-order crosswalk inside Symlinkarr so that:

- anime matching stops relying only on filename heuristics plus TVDB/TMDB metadata
- anime `search-missing` can generate better queries and reject wrong provider hits
- absolute numbering, split cour, specials, OVAs, and AniDB-style release naming can be resolved to the correct library season/episode more reliably

This is not a general TV/movie feature. It should live behind the anime lane only.

## Why This Matters

Symlinkarr already has stronger library-side identity than source-side identity:

- library folders are scanned from `{tvdb-*}` / `{tmdb-*}` tags in [library_scanner.rs](/home/lenny/apps/Symlinkarr/src/library_scanner.rs#L10)
- anime source parsing uses anime-specific regexes in [source_scanner.rs](/home/lenny/apps/Symlinkarr/src/source_scanner.rs#L31)
- anime missing acquisition is built from Sonarr Anime plus query heuristics in [anime_scanner.rs](/home/lenny/apps/Symlinkarr/src/anime_scanner.rs#L38)
- anime episode normalization in matching currently depends on library metadata seasons, not on an anime-native mapping graph, in [matcher.rs](/home/lenny/apps/Symlinkarr/src/matcher.rs#L644)

That works for simpler cases, but it still leaves real anime-specific failure classes:

- release names use AniDB/main titles while the library is TVDB/TMDB-tagged
- absolute numbering does not align cleanly with TVDB aired order
- specials or OVAs may belong to season `0` or map into aired episodes with offsets
- a provider hit can look title-correct but still be the wrong season/episode after mapping

`Anime-Lists/anime-lists` is relevant because it is exactly a maintained AniDB-rooted mapping layer from AniDB to TVDB/TMDB/IMDb, including explicit episode mapping rules and offsets:

- repository overview and purpose: [Anime-Lists/anime-lists](https://github.com/Anime-Lists/anime-lists)
- default list and XML format: [anime-lists README](https://raw.githubusercontent.com/Anime-Lists/anime-lists/master/README.md)
- Hama explicitly uses those XML mappings for AniDB ID to TVDB/TMDB ID matching and episode mapping: [Hama README](https://raw.githubusercontent.com/ZeroQI/Hama.bundle/master/README.md)

## Non-Goals

- do not make AniDB the universal canonical root for all of Symlinkarr
- do not force Plex/Hama/ASS concepts into non-anime libraries
- do not trust provider titles blindly just because they string-match an AniDB title
- do not block normal anime scans if the crosswalk is missing or stale

## Current Gaps

### 1. Anime Matching Uses Metadata Seasons, Not a Real Anime Crosswalk

Today anime matching uses:

- parsed `season/episode` from anime filenames in [source_scanner.rs](/home/lenny/apps/Symlinkarr/src/source_scanner.rs#L392)
- fallback absolute-to-season mapping against metadata seasons in [matcher.rs](/home/lenny/apps/Symlinkarr/src/matcher.rs#L718)

That is useful, but it only knows what TMDB/TVDB metadata says about the library item. It does not know:

- AniDB main title aliases
- explicit AniDB episode -> TVDB/TMDB episode remaps
- whether a title is effectively movie/special/OVA material in anime-land
- whether a release using absolute numbering maps to season `0`, season `1`, or a remapped aired slot

### 2. Anime Search-Missing Generates Queries Without a Crosswalk

Anime `search-missing` currently:

- gets missing/cutoff episodes from Sonarr Anime
- chooses a query title from Sonarr titles
- uses scene numbering or absolute numbering heuristics in [anime_scanner.rs](/home/lenny/apps/Symlinkarr/src/anime_scanner.rs#L239)
- sends those queries into provider acquisition via [scan.rs](/home/lenny/apps/Symlinkarr/src/commands/scan.rs#L316)

It does not currently use an AniDB-rooted crosswalk to:

- add better alternate queries
- understand when Sonarr season/episode should be translated into AniDB absolute numbering
- validate whether a provider result actually maps back to the requested anime episode

### 3. Provider Ranking for Anime Is Still Too Title-Heavy

Provider search flows today:

- try Prowlarr ranked search first
- then DMM lookup and ranking in [auto_acquire.rs](/home/lenny/apps/Symlinkarr/src/auto_acquire.rs#L894)
- derive DMM search queries from parsed titles in [auto_acquire.rs](/home/lenny/apps/Symlinkarr/src/auto_acquire.rs#L1052)

This is good generic plumbing, but anime validation is still too weak because it lacks a canonical anime mapping graph.

## Proposed Design

Introduce a local anime-only identity graph built from `anime-lists` and make it a hint+validation layer for anime workflows.

### New Core Concept

Add a new internal component:

- `AnimeIdentityGraph`

This graph answers:

- given library item `{tvdb-*}` or `{tmdb-*}`, which AniDB entries correspond to it?
- what is the preferred AniDB-rooted title set for searching?
- how does an AniDB episode or special map to TVDB/TMDB season/episode?
- given a parsed release or provider result, does it map back to the requested library episode?

### Data Sources

Minimum viable import:

- `anime-list.xml`
- optionally `anime-list-full.xml` for broader coverage

Later optional enrichments:

- `animetitles.xml` for more AniDB title aliases
- `anime-movieset-list.xml` for movie/special grouping hints
- `anime-offline-database` or other crosswalks as secondary evidence, not primary truth

### Local Storage

Add anime-only tables in SQLite:

- `anime_identities`
  - `anidb_id`
  - `canonical_name`
  - `tvdb_id`
  - `tmdb_tv_id`
  - `tmdb_movie_ids`
  - `imdb_ids`
  - `default_tvdb_season`
  - `default_tmdb_season`
  - `is_movie_like`
  - `raw_source_hash`
  - `updated_at`
- `anime_episode_mappings`
  - `anidb_id`
  - `provider`
  - `anidb_season`
  - `provider_season`
  - `start`
  - `end`
  - `offset`
  - `raw_mapping_text`
  - `mapping_kind`
- `anime_titles`
  - `anidb_id`
  - `title`
  - `title_kind`
  - `language`

Do not overload the main link tables with anime XML-specific shape. Keep this isolated.

## New Module Plan

Add:

- `src/anime_identity.rs`
  - XML import
  - local graph builder
  - lookup helpers
  - episode mapping resolver

Extend:

- `src/db.rs`
  - anime identity schema + load/store methods
- `src/anime_scanner.rs`
  - query expansion and request verification
- `src/matcher.rs`
  - anime-only source resolution hooks
- `src/auto_acquire.rs`
  - provider result validation / reranking for anime
- `src/commands/scan.rs`
  - telemetry and startup load path for anime graph

## Exact Integration Points

### A. Library-to-AniDB Crosswalk

Input:

- `LibraryItem` already carries `tvdb-*` or `tmdb-*`

Add lookup:

- `AnimeIdentityGraph::find_by_library_item(&LibraryItem) -> Vec<AnimeIdentityEntry>`

Rules:

- if library item is not `ContentType::Anime`, skip immediately
- TVDB library item can match on `tvdb_id`
- TMDB anime series can match on `tmdb_tv_id`
- anime movie/special library items can match on `tmdb_movie_ids` or `imdb_ids`

Output:

- possible AniDB roots
- preferred search aliases
- mapping defaults such as `defaulttvdbseason`

### B. Anime Source Resolution in Matcher

Current anime resolution is here:

- [matcher.rs](/home/lenny/apps/Symlinkarr/src/matcher.rs#L644)

Add anime-graph-aware branch before current fallback:

1. resolve library item to AniDB candidate set
2. inspect parsed anime source item
3. if source looks absolute-numbered or scene-numbered:
   - map through AniDB -> TVDB/TMDB rules first
4. if graph gives a confident target season/episode:
   - use that
5. otherwise:
   - fall back to current metadata-season heuristics

This means:

- `anime-lists` becomes the primary adapter for anime numbering weirdness
- current metadata-season logic remains as fallback, not as the only brain

### C. Search-Missing Query Expansion

Current query generation is here:

- [anime_scanner.rs](/home/lenny/apps/Symlinkarr/src/anime_scanner.rs#L239)

Add:

- `build_anime_query_candidates(...) -> Vec<String>`

For each missing episode request:

1. Start from current Sonarr title/scene/absolute query
2. Resolve library item through `AnimeIdentityGraph`
3. Expand with:
   - AniDB main title
   - AniDB official/alt titles if available
   - TVDB title if different
   - absolute-numbering form if graph says aired `SxxEyy` corresponds to AniDB absolute `N`
   - special/OVA-specific query form if graph maps the requested item into season `0` or standalone entry
4. Deduplicate and score queries

Example class this helps:

- Sonarr wants `Season 02 Episode 03`
- anime-lists says AniDB absolute `27`
- query set can include both:
  - `Show Name S02E03`
  - `Show Name 27`

### D. Provider Result Validation

This is the biggest payoff for `search-missing`.

Current providers:

- Prowlarr
- DMM
- later potentially Torrentio/Zilean-like adapters if you add them

Add anime-only validation stage after provider hit parsing:

- `validate_anime_candidate(request, release_title, anime_graph) -> AnimeCandidateVerdict`

Possible verdicts:

- `ExactEpisodeMatch`
- `SeasonPackContainsRequestedEpisode`
- `AmbiguousAnimeMatch`
- `WrongMappedEpisode`
- `WrongSeries`

Validation flow:

1. Parse provider title with current anime parser
2. Resolve requested library item to AniDB candidates
3. If release title implies absolute numbering, map that to library episode via anime graph
4. If release title implies `SxxEyy`, normalize through anime graph if needed
5. Reject hits that title-match but map to the wrong episode/season
6. Boost hits that map exactly

This should sit inside anime ranking for:

- Prowlarr result ranking
- DMM anime result ranking
- future Torrentio/Zilean adapters

### E. Post-Selection Safety Check Before Linking

Even if provider ranking thinks a hit is good, run a final anime crosswalk validation once the torrent/file has been selected.

That check should answer:

- does the chosen file map to the requested library episode under AniDB->TVDB/TMDB mapping?

If not:

- mark as blocked/ambiguous
- do not submit or do not relink automatically

This is the last line of defense against "right show, wrong episode".

## Search-Missing Benefits by Provider

### DMM

DMM is strongest when IMDb lookup works, but anime is often weaker there than normal TV/movies.

`anime-lists` helps by:

- supplying alternate AniDB/TVDB/TMDB aligned search titles
- explaining when a requested aired episode is actually better searched as an absolute-number release
- validating that the returned torrent corresponds to the intended mapped episode, not just the intended series

### Prowlarr

Prowlarr returns broad scene/indexer hits. Anime crosswalk validation should sharply improve this path because title-only anime hits are often misleading.

### Torrentio / Zilean Style Sources

If Symlinkarr later adds those, the same anime graph remains useful:

- use provider IDs if available
- then use anime graph to verify episode mapping or file choice when numbering is anime-specific

The graph is not provider-specific. It is a canonical anime adapter layer.

## What to Import from anime-lists

Use these fields aggressively:

- `anidbid`
- `tvdbid`
- `tmdbtv`
- `tmdbid`
- `imdbid`
- `defaulttvdbseason`
- `tmdbseason`
- `episodeoffset`
- `mapping-list`

Treat these carefully:

- `tvdbid="movie"` / movie-like entries
- comma-separated `tmdbid` / `imdbid`
- season `0` and specials
- mappings to `0`, which often mean "not a normal aired episode slot"

## Implementation Phases

### Phase 1: Local Graph Import

Build:

- `src/anime_identity.rs`
- DB schema and import path
- startup load path

Acceptance:

- local import of `anime-list.xml`
- lookup by `tvdb_id`, `tmdb_tv_id`, `tmdb_movie_id`, `imdb_id`
- unit tests for mapping parser

### Phase 2: Matcher Hook

Build:

- anime graph lookup from `LibraryItem`
- anime graph mapping before current metadata-season fallback in matcher

Acceptance:

- existing anime matcher tests still pass
- new tests cover:
  - absolute numbering across split seasons
  - specials/OVA remaps
  - movie-like anime entries

### Phase 3: Search-Missing Query Expansion

Build:

- anime query candidate expansion in `anime_scanner.rs`
- anime-only request metadata that carries resolved AniDB context into acquisition

Acceptance:

- query generation tests show expanded absolute/scene/aired forms
- no change to non-anime query generation

### Phase 4: Provider Hit Validation

Build:

- anime candidate verdict type
- anime-aware reranking and rejection in provider ranking for anime requests

Acceptance:

- wrong-episode title matches are rejected
- exact-mapped hits are preferred
- logs explain why a candidate was rejected

## Telemetry to Add

- `anime_graph_entries_loaded`
- `anime_graph_lookup_hits`
- `anime_graph_lookup_misses`
- `anime_graph_episode_remaps_applied`
- `anime_query_expansions_total`
- `anime_candidate_rejected_wrong_mapping_total`
- `anime_candidate_promoted_exact_mapping_total`
- `anime_search_missing_exact_mapping_rate`

## Failure Policy

If anime graph is unavailable or stale:

- do not break normal scans
- log clearly
- fall back to current anime behavior

If anime graph yields multiple plausible mappings:

- treat as ambiguous
- prefer no auto-grab over wrong auto-grab

## Recommendation

This should be built.

Not because Hama uses it, but because `anime-lists` solves a real identity problem that Symlinkarr still has in anime:

- title alias ambiguity
- absolute numbering ambiguity
- special/OVA mapping ambiguity
- provider-result validation ambiguity

The highest-value first implementation is:

1. import `anime-lists`
2. use it for anime `search-missing` query expansion
3. use it for anime provider-hit validation
4. then wire it deeper into anime matching

That ordering gives faster practical payoff than starting with a matcher rewrite.
