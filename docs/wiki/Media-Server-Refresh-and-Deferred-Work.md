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

Scan history stores per-server refresh telemetry, including the target paths Symlinkarr asked each backend to refresh. Text output also prints a short per-server target sample when refresh work is emitted.

## When Refresh Can Still Be Noisy

Symlinkarr can coordinate its own refresh phases, but it cannot control what Plex, Emby, or Jellyfin do after they notice changed folders.

If multiple media servers are configured, one Symlinkarr run can request refresh work from more than one server. Those servers may then run their own file probes or media analysis. If their folder watchers or scheduled scans are also active, disk load can stack outside Symlinkarr.

For large imports, repairs, or cleanup runs, consider:

- previewing first
- running one heavy operation at a time
- relying on your media servers' scheduled scans if you already have them tuned
- keeping stream probing optional unless routing rules need it

## Refresh Modes

Each media-server backend can choose how Symlinkarr handles refresh requests:

- `immediate`: request refresh during the Symlinkarr run. This is the default.
- `deferred`: queue refresh targets, but do not contact the media server during the mutation run.
- `disabled`: do not queue or send refresh work.

Example:

```yaml
plex:
  refresh_enabled: true
  refresh_mode: deferred
```

When `refresh_mode: deferred` is used, drain queued work manually:

```bash
symlinkarr refresh drain
```

This is useful when Plex, Jellyfin, or Emby already have folder watchers or scheduled scans and you want Symlinkarr to avoid adding more immediate probe pressure.

Import is a special bootstrap path. It writes links and tells you the next step, but it does not force every media server to scan immediately. Use your scheduled scans or folder watchers, run `symlinkarr scan` when you want Symlinkarr to check imported links, or drain deferred refresh work when you are ready.

## What the Status Page Is Good For

Use `Status` to understand:

- whether the system looks generally healthy
- how much tracked dead-link pressure exists
- whether refresh backlog is building up
- whether media-server integrations are behaving as expected

## Related Pages

- dashboard backlog: [Dashboard and Daily Operations](Dashboard-and-Daily-Operations.md)
- scan detail for refresh counters in one run: [Scan History and Skip Reasons](Scan-History-and-Why-Not-Signals.md)
