# Scan History and Skip Reasons

Use this page for the `Scan`, `Scan History`, and `Scan Run Detail` views.

## What These Pages Answer

- Did the last scan work?
- What changed?
- Why did expected links not get created?
- Was the problem matching, linking, refresh protection, or auto-acquire?

## The Three Main Pages

- `Scan`: current trigger point and latest run summary
- `Scan History`: compare past runs and spot regressions
- `Scan Run Detail`: inspect one run more closely, including grouped skip reasons

## Reading "Why Not"

The "why not" sections explain why an item did not move forward.

Common groups:

- `Matcher`: title, metadata, or ambiguity issues
- `Linking`: source missing, already correct, unsafe path, or other link-stage blocks
- `Auto-Acquire`: no result, blocked queue, failed submission, or later completion state
- media-server refresh: refresh work was capped, skipped, delayed, or aborted for safety

## Practical Steps

1. Start on `Scan`.
2. If the latest run looks wrong, open `Scan History`.
3. Open the specific run detail.
4. Check grouped skip reasons first.
5. Only expand detailed samples if grouped reasons are not enough.

## Manual Anime Search Overrides

The `Scan` page now also has an advanced-only `Anime Search Overrides` section.

Use it when:

- anime auto-acquire keeps searching with the wrong scene title
- AniDB/TVDB mapping is not the real problem, but the search title is
- you want a local, explicit correction before queue retries keep failing

The first version is intentionally narrow:

- you can save a preferred title for one anime media id
- you can add extra search hints, one per line
- the override is local and auditable
- it affects anime auto-acquire query building before the normal `anime-lists` hints are added

Use the tagged folder suffix such as `tvdb-12345` or `tmdb-67890` as the media id.

## When a Scan Looks "Quiet"

A scan can finish successfully and still leave work undone.

That usually means:

- there were no safe matches
- the right files were already linked
- auto-acquire did not find trustworthy results
- media-server refresh was limited for safety

## Related Pages

- dead links after scans: [Repair and Dead Links](Repair-and-Dead-Links.md)
- cleanup issues revealed by scans: [Cleanup, Audit, and Prune Preview](Cleanup-Audit-and-Prune-Preview.md)
- queue behavior after scan runs: [Discover and Queue](Discover-and-Queue.md)
