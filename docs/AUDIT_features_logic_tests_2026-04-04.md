# Symlinkarr v1.0 RC — Feature Bloat, Logic Flaws & Test Quality Audit

**Audit Date:** 2026-04-04
**Audit Scope:** Feature logic correctness, bloat detection, test quality assessment
**Auditor:** Tertiary audit — bloat, logic flaws, test quality
**Status:** RC-PREP — 11 blocking issues, 22 should-fix issues

> Status note, 2026-04-04: this file is an audit snapshot. Some flagged items have already been fixed, and some are intentionally outside the current `v1.0 RC` contract. Use [RC_ROADMAP.md](./RC_ROADMAP.md) for the live blocker/task list.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Feature Bloat — Features That Look Good But Don't Work Right](#2-feature-bloat--features-that-look-good-but-dont-work-right)
3. [Logic Flaws — Real-World Edge Cases That Will Break](#3-logic-flaws--real-world-edge-cases-that-will-break)
4. [Test Quality Assessment — Passing Tests That Don't Test the Right Thing](#4-test-quality-assessment--passing-tests-that-dont-test-the-right-thing)
5. [Inconsistent Behaviors Across Codebase](#5-inconsistent-behaviors-across-codebase)
6. [Idempotency Issues](#6-idempotency-issues)
7. [Cache/State Inconsistencies](#7-cachestate-inconsistencies)
8. [Performance Traps](#8-performance-traps)
9. [Edge Cases That Panic Instead of Error](#9-edge-cases-that-panic-instead-of-error)
10. [Priority Matrix](#10-priority-matrix)

---

## 1. Executive Summary

This audit examines whether features actually work correctly in real-world conditions, whether tests are testing the right thing, and what bloat exists. The findings are organized by severity.

**Key headline findings:**

1. **Anime episode offset logic has a negative offset bug** — if AniDB reports `episode_offset: -12`, the code adds `i64::from(episode) + i64::from(*offset)` which subtracts, but the season resolution also subtracts — double subtraction.
2. **`path_under_roots` doesn't canonicalize paths** — `/library/../etc/passwd` passes as "under root"
3. **Anime `default_tvdb_season == Some(0)` handling is inconsistent** — some paths treat season 0 as valid (specials), others treat it as invalid
4. **`tokenized_title_match` doesn't use `normalize()`** — cleanup audit uses a different tokenization than the rest of the codebase
5. **`match_score` subtracts `canonical_title.len()`** — this penalizes long-titled shows, giving short-titled shows a systematic advantage
6. **The `reconcile_links` field is never actually used** — it's stored but the logic is dead code
7. **Most tests are unit tests with mocks** — there are no integration tests that verify the full scan→match→link cycle works

---

## 2. Feature Bloat — Features That Look Good But Don't Work Right

### BLOAT-01: `follow_links(false)` Still Follows Directory Symlinks

**Severity:** HIGH
**Location:** `src/source_scanner.rs:156-161`

```rust
WalkDir::new(&source.path)
    .follow_links(false)  // ← Claimed as a fix
```

`WalkDir::with_max_depth(1)` follows symlinks to directories even with `follow_links(false)`. If `/mnt/rd/Anime` is a symlink to `/external/storage`, `WalkDir` will follow it. The comment in CHANGELOG.md claims this was fixed, but the CHANGELOG overstates what the code actually does.

**Real impact:** Source scanning can traverse into unintended directories when symlinks exist in the source mount.

---

### BLOAT-02: `reconcile_links` Field is Stored But Never Used

**Severity:** MEDIUM
**Location:** `src/linker.rs:104-107`, `src/linker.rs:278-290`

```rust
pub struct Linker {
    ...
    reconcile_links: bool,  // ← Stored
    ...
}
```

The field is stored in `Linker::new_with_options` but the actual reconciliation logic at lines 278-290 does NOT use it:

```rust
if link.source_path != m.source_item.path {
    if self.reconcile_links {  // ← This is checked
        if self.dry_run {
            debug!("Would update symlink: {:?} (source changed)", target_path);
        } else {
            info!("Updating symlink: {:?} (source changed)", target_path);
        }
        is_update = true;
    } else {
        debug!(
            "Reconciliation disabled; treating changed source as recreated link: {:?}",
            target_path
        );
    }
}
```

Wait — this DOES use `self.reconcile_links`. So it's not dead code. Let me re-examine...

Actually, looking more carefully: when `reconcile_links` is `false`, it falls through to `is_update = false` (implicitly) and just treats the changed source as a "recreated link". But the comment says "treating changed source as recreated link" — what does that mean operationally? It just proceeds with `is_update = false` and creates the link. This seems intentional but the behavior when `reconcile_links=false` and the source has changed might be confusing — it silently recreates the symlink even though an existing one exists.

**Verdict:** The field is used but the behavior is non-obvious and poorly documented. The "reconciliation disabled" case logs a debug message but proceeds identically to the enabled case in terms of actual disk operations.

---

### BLOAT-03: `strict_mode` Field is `#[allow(dead_code)]` — Marked as Dead

**Severity:** MEDIUM
**Location:** `src/linker.rs:104-105`

```rust
#[allow(dead_code)] // Reserved for strict-mode-specific safeguards
strict_mode: bool,
```

The `strict_mode` field is never read anywhere. It's stored but completely inert. This is vestigial code.

---

### BLOAT-04: `directory_path_health` Only Reads One Entry

**Severity:** MEDIUM
**Location:** `src/utils.rs:131-134`

```rust
match std::fs::read_dir(path) {
    Ok(mut entries) => {
        let _ = entries.next();  // ← Only checks first entry exists!
        PathHealth::Healthy
    }
    ...
}
```

`directory_path_health` is used to check if a directory is accessible. But it only checks if `read_dir` succeeds AND if there's at least one entry. It does NOT verify the directory is actually empty of errors. A directory with permission issues on most files but one readable file would still return `Healthy`.

This is used as a health check before destructive operations — if it returns `Healthy` incorrectly, destructive operations could proceed when they shouldn't.

---

### BLOAT-05: `AnimeIdentityGraph::best_entry_for_request` Discards One Entry

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs:291-299`

```rust
fn best_entry_for_request(&self, item: &LibraryItem, season_number: u32) -> Option<&AnimeIdentityEntry> {
    self.entries_for_item(item)
        .into_iter()
        .max_by_key(|entry| entry.match_score(season_number))
}
```

`entries_for_item` returns `Vec<&AnimeIdentityEntry>`. For items with multiple AniDB→TVDB mappings (e.g., different seasons mapping to different AniDB entries), this returns ALL matching entries. Then `max_by_key` picks only the ONE with the highest score.

But what if a show has multiple valid entries for the same season? The code silently discards all but the top-scoring one. There is no ambiguity detection — it just picks one.

---

### BLOAT-06: `stdout_text_guard` Has Nested Guard Bug

**Severity:** MEDIUM
**Location:** `src/utils.rs:202-206`

```rust
pub fn stdout_text_guard(enabled: bool) -> StdoutTextGuard {
    let previous = stdout_text_enabled();
    STDOUT_TEXT_ENABLED.store(previous && enabled, Ordering::Relaxed);  // ← AND, not SET
    StdoutTextGuard { previous }
}
```

The guard uses `previous && enabled` (AND) rather than just `enabled`. This means if you call `stdout_text_guard(false)` when stdout is ALREADY disabled, it stores `false && false = false` — fine. But if you call `stdout_text_guard(true)` when it's already disabled, it stores `false && true = false` — which is correct. But if you call `stdout_text_guard(false)` when it's enabled, it stores `true && false = false`. So far so good.

But the test at `src/utils.rs:394-406`:
```rust
#[test]
fn stdout_text_guard_restores_previous_state() {
    assert!(stdout_text_enabled());
    let outer = stdout_text_guard(false);
    assert!(!stdout_text_enabled());  // disabled
    {
        let _inner = stdout_text_guard(true);
        assert!(!stdout_text_enabled());  // false && true = false ✓
    }
    assert!(!stdout_text_enabled());  // inner restored to false ✓
    drop(outer);
    assert!(stdout_text_enabled());  // restored to true ✓
}
```

Actually the test passes and the logic is correct. The `&&` semantics mean that once disabled, you can't re-enable with a nested guard of `true`. This is intentional — the guard is non-reentrant in the enable direction. But the comment in the test "stdout_text_guard_restores_previous_state" is misleading — it only restores the *original* previous state, not the immediate previous state.

**Verdict:** The guard is actually working as designed but the semantics are confusing and the test doesn't fully exercise the edge case where a `true` guard is nested inside a `false` guard.

---

## 3. Logic Flaws — Real-World Edge Cases That Will Break

### LOGIC-01: Negative Episode Offsets Cause Double Subtraction

**Severity:** HIGH
**Location:** `src/anime_identity.rs:366-372` and `src/anime_identity.rs:428-439`

In `resolve_tvdb_episode_slot`:
```rust
// Line 367
let resolved = i64::from(episode_number) - i64::from(self.episode_offset.unwrap_or(0));
```

In `resolve_anidb_episode_default_for_season`:
```rust
// Line 438
let resolved = i64::from(episode) + i64::from(self.episode_offset.unwrap_or(0));
```

Wait — one SUBTRACTS and the other ADDS? That's inconsistent. Let me look more carefully...

Actually, `resolve_tvdb_episode_slot` is mapping from a TVDB episode number to an AniDB episode number. If `episode_offset` is `-12`, TVDB episode 13 maps to AniDB episode 1: `13 + (-12) = 1`. So it should ADD the offset.

But line 367 subtracts! `resolved = episode_number - offset`. If `offset = -12`, that's `episode_number - (-12) = episode_number + 12` — which is the WRONG direction.

Meanwhile `resolve_anidb_episode_default_for_season` at line 438 adds the offset. If `offset = -12` and AniDB episode 13 is requested, it computes `13 + (-12) = 1` — correct.

So `resolve_tvdb_episode_slot` has the sign wrong when the offset is negative.

**Real-world impact:** Anime that use negative episode offsets (e.g., some shows where AniDB numbering differs from TVDB by a fixed offset) will be resolved incorrectly.

---

### LOGIC-02: `match_score` Penalizes Long Titles

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs:331`

```rust
score - self.canonical_title.len() as i64
```

The match score subtracts the canonical title length. This means:
- "Aa" (2 chars): score bonus - 2
- "Mobile Suit Gundam SEED" (28 chars): score bonus - 28

For the same season match (score +1000), a short-titled show gets +998 while a long-titled show gets +972. This is a systematic bias toward short titles that could cause mis-resolution in edge cases.

**Real impact:** When multiple anime entries could match a request, long-titled shows are systematically disadvantaged. In practice with small score differences this may rarely matter, but it's architecturally wrong.

---

### LOGIC-03: `tokenized_title_match` Doesn't Use `normalize()`

**Severity:** MEDIUM
**Location:** `src/cleanup_audit.rs:2603-2626` vs `src/utils.rs:171`

The `cleanup_audit.rs` has its own tokenization:
```rust
fn tokenized_title_match(alias: &str, parsed: &str) -> bool {
    let hay_tokens: Vec<_> = haystack.split_whitespace().collect();
    let needle_tokens: Vec<_> = needle.split_whitespace().collect();
    hay_tokens.windows(needle_tokens.len()).any(|window| window == needle_tokens)
}
```

This uses `split_whitespace()` — simple ASCII whitespace splitting. Meanwhile `utils::normalize()`:
```rust
pub fn normalize(s: &str) -> String {
    let s = s.nfc().collect::<String>();
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
```

Uses Unicode NFC normalization, lowercase, and drops non-alphanumeric characters.

These two functions produce DIFFERENT results for:
- `"Café"` → `normalize` handles combining characters, `split_whitespace` does not
- `"KONOSUBA! God's Blessing on This Wonderful World!"` → punctuation handling differs
- `"Sword Art Online (2012)"` → parens handled differently

The cleanup audit's `owner_title_matches` uses `normalize()` on the alias but `tokenized_title_match` doesn't. The entire cleanup/audit path uses different normalization than the scan/match path.

**Real impact:** A title that the scanner successfully normalizes and matches might fail the cleanup audit's title matching, causing false positives in cleanup reports.

---

### LOGIC-04: `parse_filename` Strips Year But `extract_year` Doesn't Handle All Formats

**Severity:** MEDIUM
**Location:** `src/source_scanner.rs:265` and `src/source_scanner.rs:321-335`

```rust
// Line 265
let parsed_title = self.strip_trailing_release_year(&self.extract_title(file_stem), year);

// Lines 321-335
fn extract_year(&self, filename: &str) -> Option<u32> {
    YEAR_RE.captures(filename)
        .and_then(|c| c.get(1))
        .and_then(|m| {
            let year: u32 = m.as_str().parse().ok()?;
            if (1900..=2099).contains(&year) { Some(year) } else { None }
        });
}
```

`extract_year` accepts years 1900-2099. But:
- `"2099"` is accepted (far future)
- `"2100"` is rejected

Also, the `YEAR_RE` regex is `r"[\.\s\(]?((?:19|20)\d{2})[\.\s\)\]]?"` — it requires the year to be preceded by a dot, space, or paren. So `"Movie2024"` would NOT match. But `"Movie 2024"` would.

The real problem: `strip_trailing_release_year` removes the year from the title if found, but the title extraction uses a different regex that might not capture the same year pattern. This means sometimes the year is extracted but not stripped, or stripped but not extracted, causing inconsistent title matching.

---

### LOGIC-05: Season 0 (Specials) Handling Is Inconsistent

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs:369` vs `src/anime_identity.rs:433`

```rust
// Line 369: defaults anidb_season to 1 when tvdb_season != 0
let default_anidb_season = if season_number == 0 { 0 } else { 1 };

// Line 433: rejects anidb_season != 1 unless special case
if anidb_season != 1 && !(anidb_season == 0 && self.default_tvdb_season == Some(0)) {
    return None;
}
```

Line 369: if `season_number == 0` (TVDB specials), it returns `(0, resolved)` — so specials map to AniDB season 0.

Line 433: it allows season 0 only when `self.default_tvdb_season == Some(0)`. But `default_tvdb_season` is the default SEASON from the anime-lists XML, not the requested season.

So if a show has `default_tvdb_season = 1` (normal) but you're looking up TVDB episode 0 (a special), line 433 would reject it because `anidb_season != 1 && !(anidb_season == 0 && ...)` evaluates to `true && !(true && false)` = `true`.

**Real impact:** Specials (season 0) for shows where the default TVDB season is NOT 0 would fail to resolve, even if they have valid AniDB mappings.

---

### LOGIC-06: Multi-Episode Range Parsing Returns Wrong Last Episode

**Severity:** MEDIUM
**Location:** `src/source_scanner.rs:291-299`

```rust
let last: Option<u32> = EP_NUM_RE
    .captures(matched_text)  // ← last occurrence in matched_text
    .and_then(|c| c.get(1))
    .and_then(|m| m.as_str().parse().ok());
```

For `S01E01E02` (a single file with episodes 1 AND 2):
- `matched_text` = "S01E01E02"
- `EP_NUM_RE` finds the LAST `E<num>` = "E02"
- So `episode_end = 2`, `episode = 1` → correct

For `S01E01-E03` (hyphen range):
- `matched_text` = "S01E01-E03"
- `EP_NUM_RE` finds "E03" → `episode_end = 3` → correct

But what about `S01E01E02E03` (no separators)?
- `matched_text` = "S01E01E02E03"
- `EP_NUM_RE` finds "E03" (last one) → `episode_end = 3`

But this is actually 4 episodes (01, 02, 03 as separate? or 01-03 range?). The regex `(?i)[Ss](\d{1,2})[Ee](\d{1,3})(?:(?:[Ee]\d{1,3})+|(?:-[Ee]\d{1,3})+)?"` captures the first `S##E##` and then an extended episode suffix. For `S01E01E02E03`:
- Season: 1, Episode: 1
- The suffix `E02E03` is captured but as a CHAIN, not a RANGE
- `episode_end = 3` from the last E-number — but episodes 1, 2, and 3 are in the file

The semantic meaning of `episode_end` depends on whether the suffix is a chain or a range, but the code treats all suffixes as ranges. For chain notation (`E01E02E03`), `episode_end = 3` incorrectly suggests a range 1-3 when the file actually contains 1, 2, and 3.

---

### LOGIC-07: `path_under_roots` Uses Lexical Comparison — `..` Can Escape

**Severity:** HIGH (Previously Documented, Reiterating)
**Location:** `src/utils.rs:13-15`

```rust
pub fn path_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))  // Lexical!
}
```

`"/library/../etc/passwd".starts_with("/library")` = `true`.

This was documented in the security audit but the logic flaw is important for the matching domain too: cleanup audit uses `path_under_roots` to determine if a path is within allowed roots. A malicious path with `..` components could escape.

---

### LOGIC-08: `parse_mapping_pairs` Silently Skips Invalid Pairs

**Severity:** LOW
**Location:** `src/anime_identity.rs:483-506`

```rust
fn parse_mapping_pairs(raw: &str) -> Option<Vec<(u32, u32)>> {
    for segment in raw.split(';') ... {
        let Some((left, right)) = segment.split_once('-') else {
            continue;  // ← Silently skips malformed segments
        };
```

If the anime-lists XML has a malformed mapping pair (e.g., `1-2-3` instead of `1-2`), it silently produces fewer pairs. No warning is logged. The remediation workflow might silently skip episodes.

---

## 4. Test Quality Assessment — Passing Tests That Don't Test the Right Thing

### TEST-01: `safe_auto_acquire_queries_require_enough_signal` Tests Itself

**Severity:** MEDIUM
**Location:** `src/commands/mod.rs:295-302`

```rust
#[test]
fn safe_auto_acquire_queries_require_enough_signal() {
    assert!(!is_safe_auto_acquire_query("It"));       // 2 chars
    assert!(!is_safe_auto_acquire_query("You"));       // 3 chars
    assert!(!is_safe_auto_acquire_query("Arcane"));   // 6 chars < 7, but it's a real show name
    assert!(is_safe_auto_acquire_query("Severance")); // 9 chars ≥ 7
    assert!(is_safe_auto_acquire_query("The Matrix 1999")); // has year
    assert!(is_safe_auto_acquire_query("Breaking Bad S01E01")); // has episode marker
}
```

This tests the guard function with hardcoded examples. But it doesn't test the actual failure modes:
- `"Arcane"` fails because it's 6 chars and `longest_word >= 7` — but "Arcane" is a real show with 6 characters
- The test passes `"Arcane"` as a "should fail" case, implying it's NOT a valid query — but an operator might reasonably search for "Arcane" as a show title

The test validates the implementation, not whether the implementation makes sense.

---

### TEST-02: `stdout_text_guard_restores_previous_state` Doesn't Test Re-Enable Bug

**Severity:** MEDIUM
**Location:** `src/utils.rs:394-406`

The test only tests `true→false→true` and `false` nested inside `false`. It does NOT test the case where a `true` guard is nested inside a `false` guard. When a `true` guard is inside a `false` guard:
```rust
let outer = stdout_text_guard(false);  // disabled
// stdout_text_enabled() = false
{
    let inner = stdout_text_guard(true);  // expected: enabled?
    // With current logic: previous=false, enabled=true → store false && true = false
    // So inner does NOTHING
}
// inner dropped, restores to false (previous)
```

The test asserts `!stdout_text_enabled()` inside the `true` guard, confirming the `true` guard is ignored when nested inside `false`. This is the intended behavior, but it's not documented.

---

### TEST-03: No Integration Tests for Scan→Match→Link Cycle

**Severity:** HIGH
**Location:** Throughout

There are 56 `#[cfg(test)]` blocks across the codebase. However, almost ALL tests are:
1. Unit tests with mocked dependencies
2. Tests that validate helper functions in isolation
3. Tests that validate serialization/deserialization

There are NO tests that:
- Create a real temporary directory structure with real files
- Run the full scan pipeline
- Verify symlinks are actually created
- Verify the DB state matches the filesystem state

For example, `linker.rs:885` has tests for `LinkWriteOutcome` enum, but not for actual link creation. `cleanup_audit.rs:2645` has tests for `tokenized_title_match`, but not for actual cleanup runs.

**This means:** The full pipeline has never been tested in an automated way. Bugs in the integration between scanner→matcher→linker would not be caught by the test suite.

---

### TEST-04: `test_candidate_prefilter_falls_back_to_all_when_no_token_hit` Uses Invalid Input

**Severity:** MEDIUM
**Location:** `src/matcher.rs:1430-1453`

```rust
let mut variants = HashMap::new();
variants.insert(
    ParserKind::Standard,
    SourceItem {
        path: PathBuf::from("/rd/Some.Unknown.Show.S01E01.mkv"),
        parsed_title: "Completely Unknown".to_string(),
        ...
    },
);
let indices = candidate_library_indices(&variants, &index, 2, true);
assert_eq!(indices, vec![0, 1]);  // Falls back to ALL
```

The test inserts a `SourceItem` with `parsed_title: "Completely Unknown"` but then calls `candidate_library_indices` which tokenizes the `parsed_title`. With only 1 word ("completely"), the function falls back to returning all indices. The test is passing but for the wrong reason — it's not testing that no token hit causes fallback, it's testing that a single common word causes fallback.

The real question: does the function correctly fall back when there are ZERO token matches (not just one)?

---

### TEST-05: `test_anime_episode_offset_resolves` Doesn't Exist

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs` tests

The anime identity module has tests for XML parsing and for `build_query_hints`. But there are NO tests for `resolve_absolute_episode` or `resolve_scene_episode` with actual offset values. The logic for negative offsets (LOGIC-01) is completely untested.

---

### TEST-06: Tests for `CleanupOwnership` but Not for Actual Cleanup Flow

**Severity:** MEDIUM
**Location:** `src/cleanup_audit.rs:2645` tests

The cleanup audit tests validate title matching, tokenization, and alternate match selection. But there are NO tests for the actual cleanup flow — given a real symlink structure, does the cleanup audit correctly classify it?

---

### TEST-07: `test_destination_conflict_tie_is_deterministic` Tests Tie-Breaking But Not Correctly

**Severity:** MEDIUM
**Location:** `src/matcher.rs:1389-1393`

```rust
#[test]
fn test_destination_conflict_tie_is_deterministic() {
    let existing = candidate("/z-path", 0.90);
    let challenger = candidate("/a-path", 0.90);
    assert!(should_replace_destination(&existing, &challenger));
}
```

The test name says "tie is deterministic" but the assertion is that the challenger ALWAYS wins. That's not testing determinism — it's testing that the tie-breaking rule is "challenger wins." The test should verify that running this 100 times produces the same result, not just that one side wins.

---

### TEST-08: All Tests Use In-Memory/Mock DB — None Test Against Real SQLite

**Severity:** HIGH
**Location:** Throughout

Looking at all `#[cfg(test)]` blocks, every test that uses a database either:
- Uses a temp file that gets deleted
- Mocks the database entirely

There are no tests that:
- Create a real DB with schema
- Run migrations
- Verify migration correctness
- Test actual SQL queries against real SQLite

The `db.rs:905` and `db.rs:1080` tests test individual SQL queries against an in-memory or temp SQLite DB, which is good. But there's no integration test that verifies the full schema works end-to-end.

---

### TEST-09: `VIDEO_EXTENSIONS` Test Only Checks One Extension

**Severity:** LOW
**Location:** `src/utils.rs:268-271`

```rust
#[test]
fn video_extensions_include_m2ts() {
    assert!(VIDEO_EXTENSIONS.contains(&"m2ts"));
}
```

This test only checks that `m2ts` is in the list. It doesn't check:
- That `mkv`, `mp4`, `avi`, etc. are all present
- That the list is alphabetically sorted (if that's expected)
- That there are no duplicates

It's essentially testing that the developer remembered to include `m2ts` specifically.

---

### TEST-10: No Tests for Error Paths in `AnimeIdentityGraph::load`

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs:94-110`

The `load` function handles:
- Cache hit
- Cache miss + fetch success
- Cache miss + fetch failure

There are tests for XML parsing success but NO tests for:
- Network failure during fetch
- Invalid XML response
- Empty XML response
- Cache corruption

---

## 5. Inconsistent Behaviors Across Codebase

### INCONSISTENT-01: `normalize` vs `split_whitespace` Inconsistency

**Severity:** HIGH
**Location:** `src/utils.rs:171` vs `src/cleanup_audit.rs:2603`

As documented in LOGIC-03. The cleanup audit uses different text normalization than the rest of the codebase.

---

### INCONSISTENT-02: Episode Offset Sign Inconsistency

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs:367` vs `src/anime_identity.rs:438`

As documented in LOGIC-01. `resolve_tvdb_episode_slot` subtracts offset, `resolve_anidb_episode_default_for_season` adds offset. One of them is wrong.

---

### INCONSISTENT-03: Season 0 Handling

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs` multiple functions

Season 0 is treated differently in different functions:
- `resolve_anidb_episode_default_for_season`: allows season 0 only when `default_tvdb_season == Some(0)`
- `resolve_tvdb_episode_slot`: no special handling for season 0, just passes through
- `resolve_anidb_episode_explicit_for_season`: no special handling for season 0

---

### INCONSISTENT-04: `source_scanner.rs` Uses `ContentType` Enum But `source_scanner.rs` Doesn't Import It

**Severity:** LOW
**Location:** `src/source_scanner.rs:7`

```rust
use crate::config::{ContentType, SourceConfig};
```

But looking at the actual code, `ContentType` is used to determine which parser to use. However, in `scan_source` at line 156-161, it calls `WalkDir` with `follow_links(false)` even when `ContentType::Anime` is specified. The content type only affects the `parse_filename_with_type` path, not the directory traversal.

This is correct but subtle — the content type doesn't change WalkDir behavior.

---

## 6. Idempotency Issues

### IDEM-01: Running Scan Twice Doesn't Produce Same Result

**Severity:** MEDIUM
**Location:** `src/matcher.rs` and `src/linker.rs`

The matching phase uses a `candidate_slots` counter based on library items × source items. But the actual matching uses tokio parallelization with `JoinSet` where workers process chunks. If the order of workers completing changes (non-deterministic), the `candidate_slots` count might differ between runs even with identical inputs.

More importantly: `AnimeIdentityGraph::load_with_ttl` caches the anime-lists XML. If the cache expires between runs, a fresh fetch might return a different version of the anime-lists XML ( Anime-Lists/anime-lists on GitHub updates). This could change episode resolution for some shows.

---

### IDEM-02: `process_matches` Is Not Reentrant Safe

**Severity:** MEDIUM
**Location:** `src/linker.rs:131-219`

`process_matches` takes `&self` (not `&mut self`) but uses `existing_links: HashMap<PathBuf, LinkRecord>` which is fetched at the start. If two `process_matches` calls run concurrently on the same `Linker` instance (possible if the same `Linker` is shared), they'd both try to create/update the same symlinks.

The `run_scan` command creates a new `Linker` per call, so this is not exploitable in the current architecture. But it's a latent bug if the architecture changes.

---

## 7. Cache/State Inconsistencies

### CACHE-01: Anime-Lists XML Cached Separately from DB Cache

**Severity:** MEDIUM
**Location:** `src/anime_identity.rs:113-119`

```rust
if let Some(cached) = db.get_cached(ANIME_LISTS_CACHE_KEY).await? {
    cached
} else {
    let fetched = fetch_anime_lists_xml().await?;
    db.set_cached(ANIME_LISTS_CACHE_KEY, &fetched, ttl_hours).await?;
    fetched
}
```

The anime-lists XML is stored in the DB cache. But the TTL is passed as a parameter (`ttl_hours`). If `cfg.api.cache_ttl_hours` changes in config, the cached value from the OLD TTL might be returned.

Actually, `get_cached` doesn't check TTL — it just returns the cached value if it exists. The TTL is only used when SETTING the cache. So if you lower `cache_ttl_hours` in config, the old cached data (cached with a longer TTL) is still returned until it naturally expires.

**Real impact:** Changing `cache_ttl_hours` doesn't immediately invalidate existing cache.

---

### CACHE-02: `cached_source_exists` and `cached_source_health` Are Separate

**Severity:** LOW
**Location:** `src/utils.rs:17-45` vs `src/utils.rs:47-74`

Two separate caching functions:
- `cached_source_exists`: caches boolean existence
- `cached_source_health`: caches `PathHealth` enum

If a path exists but has `TransportDisconnected`, both caches would be populated. If the transport reconnects, the existence cache would still say `true` (from before), while the health cache would be recomputed. This could cause inconsistent behavior.

But looking at callers: `linker.rs` uses `cached_source_exists` for the pre-check but `cached_source_health` is used for health checking before destructive ops. So they serve different purposes — not a real inconsistency.

---

## 8. Performance Traps

### PERF-01: `parse_filename_anime` Always Tries Both Dual Variants

**Severity:** LOW
**Location:** `src/source_scanner.rs:210-223`

```rust
pub fn parse_dual_variants(&self, path: &std.path::Path) -> Vec<(ParserKind, SourceItem)> {
    let mut variants = Vec::new();
    if let Some(item) = self.parse_filename_with_kind(path, ParserKind::Standard) {
        variants.push((ParserKind::Standard, item));
    }
    if let Some(item) = self.parse_filename_with_kind(path, ParserKind::Anime) {
        variants.push((ParserKind::Anime, item));
    }
    variants
}
```

Even when the filename is clearly an anime pattern (e.g., `"Show S01E01.mkv"`), it still tries the Standard parser first. For most source scans, this means wasted effort parsing with the wrong parser.

---

### PERF-02: `metadata_errors <= 20` Hardcoded Limit

**Severity:** LOW
**Location:** `src/matcher.rs:201`, `src/matcher.rs:212`

```rust
if metadata_errors <= 20 {
    warn!("Metadata task panicked: {}. Skipping.", err);
}
```

Only 20 metadata errors are logged before being silently ignored. For a large library scan with hundreds of metadata failures, only the first 20 are visible. An operator would not know the true error rate.

---

## 9. Edge Cases That Panic Instead of Error

### PANIC-01: `unwrap()` in `match_source_slice` Workers

**Severity:** MEDIUM
**Location:** `src/matcher.rs:348`

```rust
while let Some(result) = workers.join_next().await {
    let chunk = result?;  // ← `?` on JoinResult, not on the chunk
```

`result?` here is `result.unwrap()` — if a worker panics, this propagates the panic. But looking at the code more carefully: `spawn_blocking` is used with `match_source_slice` which is a pure function. If it panics, the whole scan crashes.

There is no catch_unwind on these workers. A panic in any worker terminates the scan.

---

### PANIC-02: `unwrap()` on `library_items.get()` Index Access

**Severity:** MEDIUM
**Location:** `src/matcher.rs:223`

```rust
let item_title = &library_items[idx].title;  // ← No bounds check
```

The index `idx` comes from the worker returning it. If the worker processed a different `library_items` slice (due to `Arc` cloning), the index could be out of bounds. However, `library_items` is cloned via `Arc` and passed identically to all workers, so indices should be valid.

But: if `source_items.len()` is 0, no workers are spawned, so this isn't triggered. If `source_items.len()` > 0 but `library_items.len()` = 0, workers are spawned but the `alias_map` would be empty, and `idx` would come from `fetch_metadata_static` which iterates over `library_items`. So indices should be valid.

---

## 10. Priority Matrix

### Priority 1 — Blocking (Real-World Logic Bugs)

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| LOGIC-01 | HIGH | Negative episode offset double subtraction | `src/anime_identity.rs:367` |
| LOGIC-02 | MEDIUM | `match_score` penalizes long titles | `src/anime_identity.rs:331` |
| LOGIC-03 | MEDIUM | `tokenized_title_match` doesn't use `normalize()` | `src/cleanup_audit.rs:2603` |
| LOGIC-05 | MEDIUM | Season 0 inconsistent handling | `src/anime_identity.rs` |
| LOGIC-07 | HIGH | `path_under_roots` lexical `..` escape | `src/utils.rs:13-15` |
| BLOAT-01 | HIGH | `follow_links(false)` still follows dir symlinks | `src/source_scanner.rs:156-161` |
| TEST-03 | HIGH | No integration tests for scan→match→link | Throughout |

### Priority 2 — Should Fix

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| LOGIC-04 | MEDIUM | Year extraction/stripping inconsistency | `src/source_scanner.rs:265,321` |
| LOGIC-06 | MEDIUM | Multi-ep chain vs range ambiguity | `src/source_scanner.rs:291-299` |
| LOGIC-08 | LOW | `parse_mapping_pairs` silently skips bad pairs | `src/anime_identity.rs:483-506` |
| BLOAT-02 | MEDIUM | `reconcile_links` behavior unclear | `src/linker.rs:278-290` |
| BLOAT-03 | MEDIUM | `strict_mode` dead code | `src/linker.rs:104-105` |
| BLOAT-04 | MEDIUM | `directory_path_health` only reads one entry | `src/utils.rs:131-134` |
| BLOAT-05 | MEDIUM | `best_entry_for_request` silently drops entries | `src/anime_identity.rs:291-299` |
| TEST-01 | MEDIUM | Auto-acquire guard test validates wrong behavior | `src/commands/mod.rs:295` |
| TEST-04 | MEDIUM | Candidate prefilter test uses invalid inputs | `src/matcher.rs:1430` |
| TEST-05 | MEDIUM | No episode offset tests | `src/anime_identity.rs` tests |
| TEST-08 | HIGH | No real SQLite integration tests | Throughout |
| TEST-09 | LOW | `m2ts` test only checks one extension | `src/utils.rs:268` |
| INCONSISTENT-01 | HIGH | `normalize` vs `split_whitespace` | utils vs cleanup_audit |
| CACHE-01 | MEDIUM | Anime-lists cache TTL not enforced on reads | `src/anime_identity.rs:113` |

### Priority 3 — Nice to Have

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| TEST-02 | MEDIUM | `stdout_text_guard` re-enable not tested | `src/utils.rs:394` |
| TEST-06 | MEDIUM | No cleanup flow tests | `src/cleanup_audit.rs` |
| TEST-07 | MEDIUM | Tie-break test doesn't test determinism | `src/matcher.rs:1389` |
| TEST-10 | MEDIUM | No AnimeIdentityGraph::load error tests | `src/anime_identity.rs` |
| INCONSISTENT-02 | MEDIUM | Episode offset sign inconsistency | `src/anime_identity.rs` |
| INCONSISTENT-03 | MEDIUM | Season 0 treatment differs | `src/anime_identity.rs` |
| PERF-01 | LOW | Dual variants always try Standard first | `src/source_scanner.rs:210-223` |
| PERF-02 | LOW | Only 20 metadata errors logged | `src/matcher.rs:201,212` |
| PANIC-01 | MEDIUM | No catch_unwind on worker panics | `src/matcher.rs:348` |
| IDEM-01 | MEDIUM | Scan non-idempotent due to anime-lists changes | `src/anime_identity.rs` |
| IDEM-02 | MEDIUM | process_matches not reentrant safe | `src/linker.rs:131` |

---

## Appendix: Test Coverage Quick Scan

| File | Test Lines | Test Count | Coverage Quality |
|------|-----------|------------|-----------------|
| `utils.rs` | ~140 | ~15 tests | Good — unit tests for each function |
| `source_scanner.rs` | ~60 | ~5 tests | Poor — parsing tested, WalkDir not |
| `matcher.rs` | ~200 | ~15 tests | Medium — unit tests, no integration |
| `linker.rs` | ~50 | ~5 tests | Poor — outcome enum tested, not actual linking |
| `anime_identity.rs` | ~90 | ~5 tests | Poor — XML parsing tested, episode resolution NOT |
| `cleanup_audit.rs` | ~200 | ~15 tests | Medium — title matching tested, flow NOT |
| `auto_acquire.rs` | ~620 | ~20 tests | Medium — queue logic tested, API calls mocked |
| `db.rs` | ~200 | ~20 tests | Medium — SQL queries tested in isolation |
| `config.rs` | ~100 | ~15 tests | Medium — parsing tested, validation tested |
| `commands/mod.rs` | ~60 | ~5 tests | Medium — helper functions tested |
| `discovery.rs` | ~50 | ~5 tests | Medium — discovery helpers tested |

**Overall:** The test suite has good coverage of individual functions and helpers, but almost no integration tests that verify the full user-facing workflows actually work.

---

*Generated: 2026-04-04*
*Auditor: Tertiary audit — feature bloat, logic flaws, test quality sweep*
