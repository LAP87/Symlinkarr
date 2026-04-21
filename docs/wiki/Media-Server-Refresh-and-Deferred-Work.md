# Media Server Refresh and Deferred Work

Use this page for `Status`, dashboard refresh backlog context, and downstream media-server behavior.

## What This Covers

After Symlinkarr changes links, Plex, Emby, and Jellyfin still need to learn about those path changes.

This page explains:

- refresh batching
- coalescing
- guardrails
- deferred refresh backlog

## Why Refresh Work Is Not Always Immediate

Symlinkarr intentionally avoids stampeding media servers.

That means refresh work may be:

- batched
- capped
- skipped
- deferred
- aborted when safety limits are triggered

## What the Status Page Is Good For

Use `Status` to understand:

- whether the system looks generally healthy
- how much tracked dead-link pressure exists
- whether refresh backlog is building up
- whether media-server integrations are behaving as expected

## Related Pages

- dashboard triage for live backlog signals: [Dashboard and Daily Operations](Dashboard-and-Daily-Operations.md)
- scan detail for refresh counters in one run: [Scan, History, and Why-Not Signals](Scan-History-and-Why-Not-Signals.md)
