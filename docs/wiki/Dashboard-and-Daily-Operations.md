# Dashboard and Daily Operations

Use this page when the dashboard is your starting point and you want to know what deserves attention first.

## What the Dashboard Is For

- show whether the system looks healthy right now
- surface active work and the latest completed outcomes
- highlight the next operator action when something is wrong

The dashboard is not the place for deep diagnostics. It should tell you where to go next.

## Main Panels

- `Needs Attention`: the most important operator problems right now
- `Live Activity`: active or recently completed background work
- `Latest Run`: the newest scan summary
- backlog and history panels: secondary context when you want pressure or trend information

## Normal Daily Loop

1. Open the dashboard.
2. Check `Needs Attention`.
3. If clear, check `Latest Run`.
4. If needed, open `Scan`, `Dead Links`, or `Cleanup` from the linked action.

## When to Leave the Dashboard

- a scan failed: go to [Scan, History, and Why-Not Signals](Scan-History-and-Why-Not-Signals.md)
- dead links are accumulating: go to [Repair and Dead Links](Repair-and-Dead-Links.md)
- cleanup blockers or legacy drift are building up: go to [Cleanup, Audit, and Prune Preview](Cleanup-Audit-and-Prune-Preview.md)
- refresh backlog is growing: go to [Media Server Refresh and Deferred Work](Media-Server-Refresh-and-Deferred-Work.md)

## What "Healthy" Really Means

Healthy does not mean "nothing interesting happened."

It means:

- no current high-priority blocker is surfaced
- the latest background outcomes do not imply immediate operator intervention
- dead links and blocked queue work are not dominating the system
