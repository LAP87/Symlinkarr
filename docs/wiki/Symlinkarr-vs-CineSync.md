# Symlinkarr vs CineSync

Short version:

- Use CineSync if you want a broader all-in-one media organizer.
- Use Symlinkarr if you want a conservative repair, cleanup, and symlink layer that fits into an existing Arr/RD/media-server stack.

This page is intentionally scope-focused. It is not a feature-by-feature benchmark.

## Product Shape

Symlinkarr assumes you already have a media stack:

- RD/provider mount
- Sonarr/Radarr-style library folders, usually ID-tagged
- Plex, Jellyfin, or Emby if you want media-server refresh
- optional DMM/Decypharr/Prowlarr/Bazarr/Tautulli around the edges

Symlinkarr's job is to keep the symlink layer explainable and repairable.

CineSync appears broader from the local source review: it includes more organizer-style layout logic, app/service packaging, WebDAV/mount workflow pieces, and larger media-management surface area.

## Where Symlinkarr Is Deliberately Narrower

Symlinkarr does not try to own the whole media workflow.

It focuses on:

- matching source files to existing library intent
- writing and repairing symlinks
- auditing dead, stale, duplicate, or misplaced links
- avoiding destructive provider-side changes
- producing reports before risky cleanup
- keeping media-server refresh optional and observable

That conservative boundary is a feature, not a missing all-in-one layer.

## Where Symlinkarr Is Expanding

The new [`symlinkarr import`](Provider-Source-Import.md) command is the exception to the normal conservative scan flow.

It exists for bootstrap/recovery cases:

- user already has a full provider/RD mount
- library folders do not exist yet
- old Symlinkarr DB is gone after a hardware rebuild
- user wants a fast first pass, then daemon/scan can reconcile later

Even there, import remains filesystem-conservative:

- it writes symlinks
- it writes audit reports
- it can be aggressive about bootstrap decisions
- it still refuses to overwrite real files
- it still does not remove RD/provider/Usenet content

## Practical Recommendation

If someone asks "how is Symlinkarr different from CineSync?", the most honest answer is:

Symlinkarr is for people who want a repairable symlink layer in an existing stack. CineSync is for people who want a broader media organizer that owns more of the workflow.
