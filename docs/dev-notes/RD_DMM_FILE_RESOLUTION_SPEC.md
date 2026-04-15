# RD/DMM File Resolution Spec

## Summary

Symlinkarr ska flytta tyngdpunkten från title/alias-matchning till filnivå-upplösning mot
Real-Debrid och DMM, mer likt hur Stremio/Torrentio arbetar:

1. identifiera rätt mediaobjekt
2. identifiera rätt torrent
3. identifiera rätt fil i torrenten
4. först därefter skapa eller uppdatera symlink

Målet är inte att kopiera Torrentios implementation. Målet är att låna samma starka princip:
exakt identitet och filindex vinner över fuzzy titelmatchning.

## Problem

Nuvarande pipeline är stark på bibliotekssidan men svag på source-sidan.

- `LibraryItem` har metadata-ID
- `SourceItem` har i praktiken parsad titel, år, säsong, avsnitt och path
- när source-path inte innehåller ett explicit `tmdb-*` eller `tvdb-*` faller Symlinkarr tillbaka till alias/titelmatchning
- detta ger systematiska fel när filmer och serier delar namn eller prefix
- multi-file torrents saknar en stark intern representation av "rätt fil"

Konsekvenser:

- fel film/serie-kopplingar
- trasiga eller saknade säsonger/episoder
- onödiga relinks
- Plex upplevs sämre än Stremio trots att innehållet finns i RD

## Product Goal

Bygg en RD/DMM-facing resolver som hittar rätt fil 99,9% av tiden för:

- vanliga filmer
- vanliga TV-avsnitt
- kompletta säsongspaket
- anime med absolutnummer eller scenenumrering

Detta ska mätas som:

- `correct_file_selected_rate >= 99.9%` på en kuraterad regressionssuite
- `wrong_media_type_rate = 0`
- `wrong_episode_rate < 0.1%`
- `dead_link_rate` ska fortsätta minska, inte öka

## Non-Goals

- ersätta Plex, Arr-stack eller Decypharr
- bygga ett Stremio-addon
- lösa all metadatahämtning via AI
- eliminera all fallback-matchning dag ett

## External References

- Torrentio lookup:
  - `addon/addon.js`
  - `addon/lib/repository.js`
  - `addon/lib/streamInfo.js`
- Stremio addon contract:
  - movie ID = `tt...`
  - series ID = `tt...:season:episode`
  - `fileIdx` väljer fil i torrenten

Relevanta länkar:

- [Torrentio repository](https://github.com/TheBeastLT/torrentio-scraper)
- [Stremio addon SDK stream request docs](https://github.com/Stremio/stremio-addon-sdk/blob/f977cc9edbe8b03246574d136a27f4fc797eb06c/docs/api/requests/defineStreamHandler.md)
- [Stremio stream response docs](https://github.com/Stremio/stremio-addon-sdk/blob/f977cc9edbe8b03246574d136a27f4fc797eb06c/docs/api/responses/stream.md)

## Proposed Architecture

### 1. Introduce File-Level Source Identity

`SourceItem` räcker inte längre som enda sanningskälla. Vi behöver en ny, starkare modell:

`ResolvedSourceFile`

Suggested fields:

- `provider`: `rd_cache | rd_mount | dmm`
- `info_hash: Option<String>`
- `torrent_id: Option<String>`
- `file_index: Option<u32>`
- `torrent_title: String`
- `file_path_in_torrent: String`
- `mount_path: Option<PathBuf>`
- `parsed_title: String`
- `parsed_year: Option<u32>`
- `season: Option<u32>`
- `episode: Option<u32>`
- `episode_end: Option<u32>`
- `media_type_hint: Option<Movie|Tv|Anime>`
- `embedded_ids: Vec<MediaId>`
- `size_bytes: Option<u64>`
- `quality: Option<String>`
- `release_group: Option<String>`
- `languages: Vec<String>`
- `confidence: f64`
- `provenance: Vec<ResolutionReason>`

Ny princip:

- en symlink ska peka på en `ResolvedSourceFile`
- matchern ska i första hand välja mellan filobjekt, inte mellan råa titelsträngar

### 2. Add Resolver Stage Before Matcher

Ny pipeline:

1. library scan
2. RD/DMM resolver builds candidate file set
3. exact and hard-filter resolution
4. legacy matcher fallback only for unresolved items
5. linker

Resolvern ska:

- läsa RD cache
- hämta RD torrent info när filindex eller fil-lista saknas
- läsa DMM-resultat för saknat material
- producera filnivåkandidater för ett givet mediaobjekt

### 3. Match by Hard Constraints First

Före all fuzzy ranking ska resolvern tillämpa hårda regler.

Hard rejects:

- movie får aldrig matcha episodisk fil
- TV får aldrig matcha filmfil
- TV-avsnitt måste ha korrekt `season/episode`, eller en verifierbar anime-mappning
- movie-year mismatch över konfigurerbar tolerans rejectas
- specials får inte mappas in i vanliga slots utan explicit metadata-stöd

Hard accepts:

- explicit embedded `tmdb-*`, `tvdb-*`, `imdb-*`
- Arr-provided metadata och explicit episode slot
- RD/DMM candidate already keyed to exact media ID
- torrent file explicitly selected by `file_index` from a known resolver hit

### 4. Store File Selection in DB

Utöka länkstate i DB så att vi kan återanvända exakt upplösning.

Add fields to `links` or sibling table:

- `info_hash`
- `torrent_id`
- `file_index`
- `file_path_in_torrent`
- `resolver_kind`
- `resolver_confidence`
- `resolved_media_type`
- `resolution_version`

Resultat:

- senare scans kan återverifiera exakt samma fil
- cleanup och repair kan avgöra om fel fil eller bara död path är problemet
- multi-file torrents blir förstaklassmedborgare

## Resolution Strategy

### Movie Resolution

1. Explicit media ID in path or metadata
2. RD/DMM candidate already keyed to exact movie ID
3. Exact normalized title + exact/near year
4. Exact title + no episodic markers + largest valid video file
5. Legacy title matcher fallback only if no file-level resolver hit exists

Movie-specific hard filters:

- reject `SxxEyy`
- reject complete-season bundles
- reject files under obvious series folders unless explicit ID override exists

### Series Resolution

1. Exact `media_id + season + episode`
2. Exact `media_id + season + episode range`
3. Complete-season package containing the exact episode file
4. Anime absolute-number mapping
5. Legacy matcher fallback only for unresolved items

Series-specific hard filters:

- exact slot required unless anime mapper says otherwise
- season pack must contain the requested episode as a concrete file
- do not guess "largest file in torrent" for episodic content when file list is available

### Anime Resolution

Anime kräver egen adapter ovanpå samma resolver.

Needed:

- absolute episode mapping
- scene-number mapping
- subgroup-tolerant parsing
- multi-episode range support
- cache of resolved anime numbering decisions

Anime ska inte leva i helt separat arkitektur. Det ska vara samma file resolver med en anime-specific numbering adapter.

## RD/DMM Integration Model

### Real-Debrid

Use:

- torrent cache for broad enumeration
- torrent info endpoint for exact file list
- selected file IDs when known

Expected output per torrent:

- torrent metadata
- canonical file list
- index-stable file identifiers
- mount path mapping

### DMM

DMM ska användas för candidate discovery, inte som slutlig sanning.

DMM responsibilities:

- hitta sannolika cached torrents för saknat material
- returnera candidates med info hash / magnet / title / size
- ge resolvern en kandidatlista att verifiera via RD eller filparsing

Resolvern får aldrig skapa symlink direkt från "DMM titel verkar rätt". DMM är candidate source, inte final arbiter.

## Ranking Model

Efter hard filters används en enkel, deterministisk poängmodell.

Suggested weights:

- exact media ID: `+1000`
- exact season/episode: `+500`
- exact file index from prior resolution: `+400`
- exact year: `+120`
- title exact: `+100`
- title token match: `+40`
- release quality preference: `+10`
- language preference: `+5`

Penalties:

- episodic markers for movie: immediate reject
- year mismatch `> 1`: immediate reject unless explicit ID
- ambiguous near tie: reject, do not guess

## Fallback Policy

Legacy alias matcher ska finnas kvar men endast som sista steg.

Rule:

- if resolver returns any exact or hard-filter-clean candidates, do not invoke broad fuzzy fallback
- if fallback does run, its output must still pass media-type gating
- fallback results should be tagged `resolution_kind = legacy_title_match`

## Observability

Add counters:

- `resolver_candidates_total`
- `resolver_exact_id_hits_total`
- `resolver_file_index_hits_total`
- `resolver_fallback_hits_total`
- `resolver_reject_wrong_media_type_total`
- `resolver_reject_wrong_episode_total`
- `resolver_ambiguous_total`
- `resolver_selected_from_dmm_total`

Add audit surfaces:

- list active links grouped by `resolver_kind`
- list links missing `file_index`
- list movies backed by episodic-looking paths
- list TV links where selected file no longer exists in torrent file list

## Rollout Plan

### Phase 1: Guardrails

Scope:

- keep current matcher
- add hard media-type gating everywhere
- persist more provenance for future cleanup

Success:

- wrong movie-to-episode links stop appearing

### Phase 2: File Resolver Core

Scope:

- add `ResolvedSourceFile`
- build RD file-list loader
- choose exact file inside multi-file torrents

Success:

- TV and season-pack accuracy improves without touching DMM

### Phase 3: DMM Candidate Resolver

Scope:

- integrate DMM as candidate discovery source
- verify each candidate against RD file list before linking

Success:

- missing-season and missing-episode recovery becomes Stremio-like instead of title-guessing

### Phase 4: DB and Repair Integration

Scope:

- persist `info_hash/file_index`
- teach repair and cleanup to use file identity

Success:

- relink and dead-link repair become deterministic

### Phase 5: De-emphasize Legacy Matcher

Scope:

- legacy matcher only for unresolved low-confidence cases
- add config flag to disable broad fallback for strict deployments

Success:

- most links come from file-identity path, not alias scoring

## Cheap-Model Execution Plan

Den här designen är medvetet uppdelad för billigare modeller.

Rules for implementation tickets:

- varje ticket ska röra en modul eller ett tydligt dataflöde
- varje ticket ska ha 2-5 tester
- inga tickets ska kräva samtidig refactor av `matcher`, `linker`, `db` och `repair`
- varje ticket ska kunna granskas lokalt med `cargo test <small-scope>`

Recommended ticket breakdown:

1. Add `source_shape_matches_media_type()` guards everywhere match selection happens
2. Add DB columns for `info_hash/file_index/resolver_kind`
3. Add `ResolvedSourceFile` model and converters from RD cache rows
4. Add RD torrent-info fetcher returning stable file list objects
5. Add exact TV file selector from season packs
6. Add exact movie file selector rejecting episodic bundles
7. Add resolver provenance and confidence recording
8. Add DMM candidate verification path
9. Add CLI/debug command to inspect resolver decisions
10. Add cleanup audit for wrong-media-type active links

This is deliberate. GPT-5.4-mini or similar models can ship those tickets safely if each owns one module and one acceptance boundary.

## Acceptance Tests

Must-have regression fixtures:

- `The Avengers (2012)` must not match `Avengers Assemble S01E01`
- `Ghost in the Shell S.A.C. 2nd GIG Individual Eleven` must not match `Stand Alone Complex S02E01`
- `The Lincoln Lawyer (2011)` movie must not match `The Lincoln Lawyer S02E01`
- exact `tmdb-*` or `tvdb-*` in source path must still override title weirdness
- season packs must pick exact episode file, not largest file
- anime absolute numbering must resolve to correct season/episode

Production checks:

- no active movie link may point to an episodic-looking source path
- no active TV link may lack resolved episode slot unless flagged as special/anime exception

## Open Questions

- vilken minsta metadatauppsättning kan DMM ge oss utan extra network roundtrips?
- ska `file_index` vara provider-specific eller normaliserad intern identifierare?
- ska resolvern leva i `source_scanner.rs`, ny modul `resolver.rs`, eller delas mellan `cache.rs` och `matcher.rs`?
- hur aggressivt ska vi backfilla gamla links med `info_hash/file_index`?

## Recommendation

Implementera inte detta som en stor matcher-rewrite.

Gör det som en resolver-bana vid sidan av nuvarande matcher:

- exact file identity first
- deterministic file selection second
- legacy title matcher last

Det är den minsta vägen till Stremio-lik träffsäkerhet utan att kasta bort nuvarande kodbas.
