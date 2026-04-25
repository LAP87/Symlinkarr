# Media Server Refresh

Use this page for `Status`, dashboard refresh backlog, and media-server behavior.

## What This Covers

After Symlinkarr changes links, Plex, Emby, and Jellyfin still need to learn about those path changes.

This page explains:

- refresh batching
- combining nearby paths
- safety limits
- refresh backlog

## Why Refresh Work Is Not Always Immediate

Symlinkarr avoids hammering media servers.

That means refresh work may be:

- batched
- capped
- skipped
- delayed
- aborted when safety limits are triggered

## What the Status Page Is Good For

Use `Status` to understand:

- whether the system looks generally healthy
- how much tracked dead-link pressure exists
- whether refresh backlog is building up
- whether media-server integrations are behaving as expected

## Related Pages

- dashboard backlog: [Dashboard and Daily Operations](Dashboard-and-Daily-Operations.md)
- scan detail for refresh counters in one run: [Scan History and Skip Reasons](Scan-History-and-Why-Not-Signals.md)
