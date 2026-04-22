# Design Council — Observability Stop Point — 2026-04-22

Source: post-rc.11 runtime observability pass after dashboard live fragments, scan-origin provenance, daemon heartbeat, and status/API alignment.

## Question

Have we reached a sensible stop point for v1.0 observability work, or should Symlinkarr keep digging toward a richer live runtime model right now?

## What Exists Now

The current branch now gives operators materially better runtime confidence than the earlier dashboard/state model:

- dashboard hero, latest run, and needs-attention are live fragments
- scan history/detail/API preserve run origin
- daemon cadence is derived from daemon-origin scans instead of any latest scan
- daemon heartbeat is persisted in the database
- dashboard and status surface daemon heartbeat, phase, and staleness
- live activity shows:
  - active web-triggered work
  - queue pressure/outcomes
  - recorded scan provenance
  - live daemon heartbeat/phase
- `/api/v1/status` now exposes the same daemon observability contract as the web UI

That is enough to answer the most important operator questions:

- did a scan happen?
- where did it come from?
- is the daemon still alive?
- is it scanning, sleeping, or stale?
- are queue/dead-link conditions worsening?

## Why This Is A Good Stop Point

The current model is still intentionally summary-first.

That matters because Symlinkarr is strongest when it gives the operator:

- high-signal state
- clear next steps
- low cognitive noise

The next layers of observability would be more expensive and noisier:

- queue/job event streams instead of current snapshots
- explicit daemon-cycle phase history
- mutation provenance beyond run origin
- raw or near-raw live logs

Those can be valuable, but they stop being cheap, obvious wins.

## What Deeper Observability Would Actually Mean

If Symlinkarr continues past the current stop point, it should not do so via "more polling everywhere."

It would need at least one of these deliberate product decisions:

1. Event history for daemon/queue transitions
   - persistent job/cycle timeline, not just current state + latest outcome

2. Stronger daemon provenance
   - explicit cycle IDs, cycle start/end markers, failure categories, maybe heartbeat history

3. External-operator contract expansion
   - more API surfaces intended for dashboards, monitoring, or automation

4. True log-adjacent runtime inspection
   - only if summaries stop answering real operator questions

## Council Recommendation

For v1.0, stop here unless one of these happens:

- real operator feedback says the current dashboard/status/feed still leaves runtime ambiguity
- queue behaviour becomes the main source of confusion
- a new feature such as release-upgrader or more autonomous repair makes summary telemetry insufficient

In other words:

**heartbeat + persisted provenance is enough for v1.0 unless real usage proves otherwise**

## Next If We Continue Anyway

If the team chooses to go one step deeper despite that recommendation, the next best target is:

1. queue/job provenance before logs

That means:

- track meaningful queue transitions
- surface why a job moved from queued → blocked / downloading / relinking / failed
- keep it operator-summary shaped, not terminal-tail shaped

## Bottom Line

Symlinkarr now has a credible runtime observability floor.

More observability is still possible, but it is no longer obviously the best use of time by default.
The next step should be justified by actual operator confusion, not by the abstract appeal of richer telemetry.
