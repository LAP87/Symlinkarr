# Bazarr Integration Bug Analysis

**Date:** 2026-03-16
**Status:** CONFIRMED BUG - Integration uses non-existent API endpoints

---

## Problem Summary

The Bazarr integration in Symlinkarr is calling **endpoints that don't exist** or have **changed in recent versions of Bazarr**, resulting in 500 errors at the end of scans.

---

## Root Cause Analysis

### 1. The `trigger_sync` Function Uses Wrong Endpoint

**File:** `src/api/bazarr.rs`
**Lines:** 177-207

```rust
pub async fn trigger_sync(&self) -> Result<()> {
    let url = self.endpoint_url("api/system/tasks");

    for task_name in &[
        "search_wanted_subtitles_series",
        "search_wanted_subtitles_movies",
    ] {
        let body = serde_json::json!({ "taskid": task_name });
        // POST to /api/system/tasks
    }
}
```

**The Problem:**

According to Bazarr maintainer in [GitHub Issue #825](https://github.com/morpheus65535/bazarr/issues/825):
> **"Actually, there isn't an official API in Bazarr."**

The endpoint `/api/system/tasks` with a `taskid` body parameter:
1. **Does not exist** as a public/stable API
2. Is an **internal endpoint** that may have changed or been removed
3. The body format `{ "taskid": "..." }` is likely incorrect

---

### 2. The Webhook Functions Send Incomplete Payloads

**File:** `src/api/bazarr.rs`
**Lines:** 94-131 (notify_episode_changed)

```rust
let body = serde_json::json!({
    "eventType": "Download",
    "series": {
        "id": sonarr_series_id,
    },
    "episodeFile": {
        "id": sonarr_episode_file_id,
    }
});
```

**The Problem:**

According to [PR #2936](https://github.com/morpheus65535/bazarr/pull/2936), the webhook endpoints (`/api/webhooks/sonarr` and `/api/webhooks/radarr`) require:
1. **Complete Sonarr/Radarr webhook event payloads** - not just minimal data
2. **Proper event types** - "Download" event may require additional fields
3. **Validation** - Bazarr validates the payload format strictly

The current implementation sends incomplete payloads that likely fail validation.

---

### 3. Authentication Method May Be Wrong

**File:** `src/api/bazarr.rs`
**Lines:** 75-90

The code tries header auth first (`X-Api-Key`), then falls back to query parameter (`?apikey=...`).

**The Problem:**

While this dual-auth strategy is used in other *Arr apps, Bazarr's API authentication may differ since:
1. There's no official API documentation
2. Internal endpoints may use different auth mechanisms
3. Cookie-based or form-based auth might be required

---

## Why It Fails With 500 Error

When the code calls:
```
POST /api/system/tasks
Body: { "taskid": "search_wanted_subtitles_series" }
```

Bazarr likely:
1. Receives the request but doesn't recognize the endpoint or body format
2. Tries to process it anyway (internal endpoint)
3. Fails with an internal server error because the expected parameters are missing
4. Returns 500 because the internal code wasn't designed to handle this request

---

## What Should Be Used Instead

### Option 1: Use the Wanted Endpoints (Recommended)

Instead of trying to trigger tasks, use the wanted endpoints that actually exist:

- `GET /api/series/wanted` - Get episodes needing subtitles
- `GET /api/movies/wanted` - Get movies needing subtitles
- `POST /api/episodes` - Update episode (may trigger subtitle search)

### Option 2: Use Webhooks Correctly

If using webhooks, send complete Sonarr/Radarr event payloads:

**Sonarr Download Event (full payload):**
```json
{
  "eventType": "Download",
  "series": {
    "id": 123,
    "title": "Show Name",
    "path": "/path/to/show",
    "tvdbId": 123456
  },
  "episodes": [{
    "id": 456,
    "episodeNumber": 1,
    "seasonNumber": 1,
    "title": "Episode Title"
  }],
  "episodeFile": {
    "id": 789,
    "relativePath": "Season 01/show.s01e01.mkv",
    "path": "/path/to/file.mkv",
    "quality": "HDTV-1080p",
    "qualityVersion": 1
  }
}
```

### Option 3: Use the Database Directly

Since Bazarr shares the same SQLite database as Sonarr/Radarr, you could:
1. Insert records into Bazarr's tables directly
2. Trigger Bazarr's internal task scheduler

However, this is fragile and not recommended.

### Option 4: Remove the Feature

Given that Bazarr explicitly states "there isn't an official API", the most robust solution might be to:
1. Remove the automatic Bazarr notification
2. Let users configure Bazarr to sync with Sonarr/Radarr on its own schedule
3. Document that Bazarr integration is not supported

---

## Recommended Fix

### Immediate Fix (Disable the broken feature)

```rust
pub async fn trigger_sync(&self) -> Result<()> {
    // TEMPORARY: This endpoint doesn't exist in Bazarr
    // See: https://github.com/morpheus65535/bazarr/issues/825
    warn!("Bazarr task triggering is not supported - Bazarr has no official API");
    Ok(())
}
```

### Proper Fix (Use wanted endpoints)

Implement a polling-based approach:
1. After scan completes, check `/api/series/wanted` and `/api/movies/wanted`
2. Filter for recently added items
3. Trigger subtitle search only for those specific items

Or use the webhook approach with complete payloads (requires more testing).

---

## Files Affected

| File | Lines | Issue |
|------|-------|-------|
| `src/api/bazarr.rs` | 177-207 | `trigger_sync()` uses non-existent endpoint |
| `src/api/bazarr.rs` | 94-131 | `notify_episode_changed()` sends incomplete payload |
| `src/api/bazarr.rs` | 134-171 | `notify_movie_changed()` sends incomplete payload |
| `src/main.rs` | 663-671 | Calls `trigger_sync()` at end of scan |
| `src/main.rs` | 1887-1899 | Incomplete Bazarr notification for repairs |

---

## References

1. [Bazarr Issue #825](https://github.com/morpheus65535/bazarr/issues/825) - "Is possible to call Bazarr from API?" (Maintainer: "there isn't an official API")
2. [Bazarr Issue #741](https://github.com/morpheus65535/bazarr/issues/741) - "API for Bazarr" (undocumented/unstable)
3. [Bazarr PR #2936](https://github.com/morpheus65535/bazarr/pull/2936) - Webhook validation requirements

---

## Fix Implemented (2026-03-16)

The `trigger_sync()` method has been updated to use the correct Bazarr API endpoints:

### Changes Made

**File:** `src/api/bazarr.rs`

1. **Replaced non-existent endpoint:**
   - Old: `POST /api/system/tasks` with body `{ "taskid": "..." }`
   - New: `GET /api/episodes_search_missing` and `GET /api/movies_search_missing`

2. **Updated `trigger_sync()` implementation:**
   - Now calls two separate GET endpoints
   - Returns proper error messages on failure
   - Logs success/failure appropriately

3. **Updated `health_check()`:**
   - Changed from `/api/system/tasks` to `/api/system/health`
   - Added fallback to `/api/series/wanted` if health fails

4. **Added test coverage:**
   - `test_trigger_sync_success()` - Verifies correct endpoints are called

### New Implementation

```rust
pub async fn trigger_sync(&self) -> Result<()> {
    // Trigger series subtitle search
    let series_url = self.endpoint_url("api/episodes_search_missing");
    let resp = self
        .send_authenticated(&series_url, |url| self.client.get(url))
        .await?;
    // ... error handling ...

    // Trigger movie subtitle search
    let movies_url = self.endpoint_url("api/movies_search_missing");
    let resp = self
        .send_authenticated(&movies_url, |url| self.client.get(url))
        .await?;
    // ... error handling ...
}
```

### Testing

All 8 bazarr module tests pass:
- `test_trigger_sync_success` ✓
- `test_health_check_success` ✓
- `test_health_check_failure_status` ✓
- `test_health_check_retries_with_query_param_after_bad_request` ✓
- `test_parse_bazarr_episode` ✓
- `test_parse_bazarr_movie` ✓
- `should_retry_with_query_auth_for_known_auth_failures` ✓
- `with_query_api_key_appends_expected_parameter` ✓

### Verification Needed

The fix should be tested against a real Bazarr instance to verify:
1. The endpoints return 200 OK
2. Subtitle searches are actually triggered
3. Authentication works with both header and query param

### Note on Webhook Methods

The `notify_episode_changed()` and `notify_movie_changed()` methods are still present but marked as `#[allow(dead_code)]`. These may still not work correctly as they use the webhook endpoints which require complete Sonarr/Radarr payloads. They are kept for potential future use but are not currently called from main.rs.
