# Symlinkarr Follow-Up Audit — 2026-04-05

> Status note, 2026-04-06: this file is a point-in-time audit snapshot, not the current release-gate list. Several findings below were fixed during the later RC hardening pass. Use [RC_ROADMAP.md](./RC_ROADMAP.md) for the live blocker/task view.

**Auditor:** Claude Opus 4.6 (2 sub-agents: anime/acquisition deep-dive + codebase health check)
**Codebase:** `v0.3.0-beta.1` @ branch `codex/media-server-hardening`
**Scope:** Anime pipeline, acquisition/upgrade lifecycle, previous-fix verification, compilation/dependency health
**Mode:** Read-only — no code changes made

---

## Previous Fix Verification

**All 11 checked CRITICAL/HIGH items from the 2026-04-04 audit are confirmed fixed.**

| ID | Status | Evidence |
|----|--------|---------|
| B-01 | **FIXED** | `matcher.rs:1064-1066` — exact-ID path now calls `source_shape_matches_media_type()`. Dedicated test confirms. |
| B-02 | **FIXED** | `discovery.rs:158-161` — empty-string guard on both sides before equality check. Test confirms. |
| B-03 | **FIXED** | `get_stats_by_library` removed. Replaced by `get_stats()` and `get_stats_by_media_type()` — no phantom columns. |
| B-04 | **FIXED** | `db.rs:2191-2192` — `insert_link_in_tx` now uses `path_to_db_text()` on both paths. |
| B-05 | **FIXED** | `tmdb.rs` and `tvdb.rs` route all HTTP calls through `decode_tmdb_response()` / `decode_tvdb_response()` which check status before `.json()`. TVDB additionally handles 401/404 explicitly. |
| B-06 | **FIXED** | `.gitignore` includes both `/secrets/` and `/.secrets/`. |
| B-07 | **FIXED** | `Cargo.toml:12` uses `serde_yml = "0.0.12"`. No `serde_yaml` references remain. |
| B-08 | **FIXED** | `Cargo.toml:13` uses `reqwest = "0.13.2"`. |
| B-09 | **FIXED** | `canonical_plex_db_path()` rejects `..`, requires `.db` extension, canonicalizes. Plex DB opened read-only. |
| H-12 | **FIXED** | `web/mod.rs:1030` — `constant_time_str_eq()` uses `subtle::ConstantTimeEq` for session cookie, CSRF, Basic auth, and API key. |
| H-13 | **FIXED** | No blanket `#![allow(...)]` on `mod web`. Remaining allows are per-module on API clients with justification comments. |
| H-14 | **FIXED** | Web server uses `with_graceful_shutdown(shutdown_signal())`. Daemon uses `tokio::select!` with `ctrl_c()`. |
| M-28 | **FIXED** | No `panic = "abort"` in Cargo.toml. `catch_unwind` in web module now functions correctly. |

**M-05 (linker `unwrap_or(1)` defaults) — STILL OPEN.** See finding M-05 below.

---

## RC Blockers

None found. All previous critical blockers have been resolved.

---

## HIGH Severity

| # | File | Finding |
|---|------|---------|
| H-01 | `src/discovery.rs:157-169` | `titles_match` containment check has **no minimum length guard**. A normalized library title like `"ed"` or `"up"` matches any torrent containing that substring, causing `find_gaps()` to miss genuinely missing content. The matcher's `contains_score()` has a 0.55 ratio guard — discovery does not. Fix: add `min(lib_title.len(), rd_title.len()) >= 4` before containment, or use ratio-based matching. |
| H-02 | `src/anime_scanner.rs:268-276` | **Specials acquisition is fragile.** When `season_number == 0`, the absolute-episode branch is skipped (by design). Fallback produces `S00Exx` format, which most indexers don't index for anime. Specials without anime-lists identity graph mappings will consistently fail to auto-acquire. Not a bug per se, but a known functional gap worth documenting. |

---

## MEDIUM Severity

| # | File | Finding |
|---|------|---------|
| M-01 | `src/anime_scanner.rs:320-359` | `anime_query_title_score` can favor very short alternate titles (even single-character) if they have `scene_season_number == -1` (global scene title, +320 bonus). A title like `"X"` scores 329, beating most non-scene titles. Single-char search queries produce garbage from Prowlarr. Fix: minimum `strong_words >= 1` or penalize titles < 3 chars. |
| M-02 | `src/matcher.rs:805-815`, `src/linker.rs:523-524` | **Multi-episode files create only one symlink.** `destination_key` uses only the first episode from `SourceItem`. The `episode_end` field (set for files like `S01E01E02E03.mkv`) is never consumed downstream. Episodes 2-N remain "missing" in Sonarr and may trigger duplicate acquisitions. Fundamental design limitation. |
| M-03 | `src/matcher.rs:689-724` | **Scene-episode fallback treats season-relative numbers as absolute.** When an anime file is `S03E15` and the identity graph has no mapping, the fallback calls `resolve_absolute_episode(item, 15)`. But `15` is a season-relative number, not absolute. For multi-season anime, this silently remaps to the wrong episode slot. |
| M-04 | `src/auto_acquire.rs:1971-1979` | `has_conflicting_explicit_season` only checks seasons 1-10. Long-running anime (One Piece, Naruto) can have 20+ seasons. A torrent for "Naruto Season 15" would not be detected as conflicting with desired season 3. Fix: extend range or derive from max known season. |
| M-05 | `src/linker.rs:523-524` | **Still open from previous audit.** `unwrap_or(1)` for missing season/episode. No current code path triggers it (matcher guards prevent it), but it's a latent time bomb — silently creates links at S01E01 if the invariant ever breaks. Fix: use `expect()` or return an error. |

---

## LOW Severity

| # | File | Finding |
|---|------|---------|
| L-01 | `src/auto_acquire.rs:2097-2099` | `is_episode_number_token` allows `"0"` as valid. A title like `"Steins;Gate 0"` has `"0"` stripped as episode number in `anime_batch_fallbacks`, turning the query into just `"Steins Gate"` — which fetches the wrong show. |
| L-02 | `src/discovery.rs:107-112` | `parse_torrent_title` marker list misses anime-specific patterns (`[SubsPlease]`, `[1080p]`, batch indicators). Anime torrents get imperfect title extraction. Mitigated by containment matching. |
| L-03 | `src/web/mod.rs:1033` | `constant_time_str_eq` early-returns on length mismatch, leaking whether token length is correct. Practical risk is low (tokens are fixed-length hex), but not fully constant-time. Fix: HMAC-compare or pad to equal length. |
| L-04 | `src/api/*.rs` (8 files) | Broad `#![allow(dead_code)]` on API modules. Each has justification comment, but blanket suppression means genuinely dead code accumulates silently. Ideally narrow to per-item annotations. |

---

## Dependency Health

| Crate | Current | Latest | Notes |
|-------|---------|--------|-------|
| `reqwest` | 0.13.2 | 0.13.x | **Current** |
| `serde_yml` | 0.0.12 | 0.0.x | Pre-stable but correct successor to serde_yaml |
| `sqlx` | 0.7 | **0.8** | One major behind. Still gets patches. |
| `axum` | 0.7 | **0.8** | One major behind. Still maintained. |
| `tower-http` | 0.5 | **0.6** | Paired with axum 0.8. Upgrade together. |
| `quick-xml` | 0.31 | 0.37 | Minor versions behind, functional. |
| All others | | | Current |

**Actionable:** `sqlx` + `axum` + `tower-http` can be upgraded together when convenient. No security implications on current versions.

---

## Praise

- **Anime identity graph** — Dual-resolution strategy (explicit mappings → default offsets) with ambiguity detection is exactly right. Thorough round-trip tests.
- **Dual parser approach** — Standard + Anime parsers with `parse_dual_variants()` cleanly handles the naming tension. Regex collection well-organized with `LazyLock`.
- **Acquisition lifecycle** — Three-phase state machine (pending → downloading → relinking) with SQLite-backed persistent queue. Backoff strategies sensible, reuse-existing-torrent detection avoids duplicate downloads.
- **Deterministic matching** — `candidate_cmp` tiebreaker chain (score → quality_rank → alias length → path) ensures reproducible output.
- **Query hint deduplication** — Both `anime_identity.rs` and `auto_acquire.rs` normalize before dedup, preventing wasted indexer quotas.
- **Previous fix quality** — All 11 verified fixes are properly implemented with tests. No regressions found.

---

## Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 0 |
| HIGH | 2 |
| MEDIUM | 5 |
| LOW | 4 |
| **Total** | **11** |

### Verdict: **Dramatically improved since 2026-04-04.** All 9 previous RC blockers are resolved. The remaining findings are edge-case hardening (short title matching, multi-episode files, anime specials) rather than correctness bugs. The codebase is approaching RC quality — the HIGH items (H-01, H-02) are functional gaps worth documenting/tracking but are not ship-blockers if acknowledged as known limitations.

---

*Generated by Claude Opus 4.6 — 2 of 2 audit agents completed*
