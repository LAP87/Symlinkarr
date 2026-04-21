# Symlinkarr Wiki Source Index

This file is the source index for the GitHub wiki.

The actual page drafts live under [wiki/Home.md](wiki/Home.md) and the rest of [docs/wiki/](wiki/).

## Current Source Map

- [Home](wiki/Home.md)
- [Dashboard and Daily Operations](wiki/Dashboard-and-Daily-Operations.md)
- [Scan, History, and Why-Not Signals](wiki/Scan-History-and-Why-Not-Signals.md)
- [Repair and Dead Links](wiki/Repair-and-Dead-Links.md)
- [Cleanup, Audit, and Prune Preview](wiki/Cleanup-Audit-and-Prune-Preview.md)
- [Backup and Restore](wiki/Backup-and-Restore.md)
- [Configuration and Doctor](wiki/Configuration-and-Doctor.md)
- [Discover and Queue](wiki/Discover-and-Queue.md)
- [Anime Remediation](wiki/Anime-Remediation.md)
- [Media Server Refresh and Deferred Work](wiki/Media-Server-Refresh-and-Deferred-Work.md)

## Why This Changed

The older wiki structure was too broad for the current web UI.

It was possible to add a contextual help link in the UI and still land on a page that mixed too many operator intents together. The new source layout is split by operator task instead.

## Broad Pages Should Become Hubs

Broad top-level wiki pages can still exist, but they should route the operator to narrower task pages instead of trying to answer everything in one place.

In practice:

- `Home` should route to the task pages
- `Getting Started` should narrow to install/bootstrap/restore intent
- `User Guide` should stop acting as the catch-all explanation target for advanced operator surfaces
- `Operations and Safety` should stop acting as the catch-all explanation target for cleanup, repair, backup, and doctor

## Related Notes

- wiki coverage audit: [dev-notes/WIKI_COVERAGE_AUDIT_2026-04-21.md](dev-notes/WIKI_COVERAGE_AUDIT_2026-04-21.md)
- web UI charter help-link rules: [dev-notes/WEB_UI_CHARTER.md](dev-notes/WEB_UI_CHARTER.md)
