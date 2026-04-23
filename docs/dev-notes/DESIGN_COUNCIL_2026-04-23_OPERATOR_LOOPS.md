# Design Council — Operator Loops, Not More Surface — 2026-04-23

Source: code exploration after the observability stop-point, dashboard live-state pass, wiki split, and recent UI streamlining.

## Question

What should Symlinkarr focus on next:

- more observability
- more product breadth
- more internal refactoring
- or a deeper UX simplification pass

## Thesis

The next credible win is not more raw capability.

It is to make Symlinkarr feel like a small number of closed operator loops and hide everything else by default.

In other words:

**the app should optimize for decision clarity, not surface area**

## Snapshot

The codebase no longer points to one obvious technical emergency:

- the long large-file split pass is mostly complete
- the dashboard/state model is now materially more trustworthy
- help-link coverage exists and the wiki has already been split into task pages
- restore/bootstrap/no-config and several risky runtime paths now have meaningful tests

That changes the problem.

The biggest remaining drag is no longer "where is the code."
It is increasingly "how much UI does the operator have to parse before knowing what to do next."

## Findings

### 1. The app now has enough capability that UX clutter matters more than feature gaps

Symlinkarr already supports:

- dashboard triage
- scan/history/detail explainability
- cleanup audit and prune preview
- repair/dead-link workflows
- backup/restore and recovery paths
- config/doctor
- discover/queue
- anime remediation and narrow anime overrides

That is a serious product surface for a local-first operator tool.

The risk is no longer "too little functionality."
The risk is exposing too much of it at once.

### 2. The wiki split changed what the UI should try to explain inline

The web UI now has narrower wiki destinations such as:

- `Dashboard-and-Daily-Operations`
- `Scan-History-and-Why-Not-Signals`
- `Cleanup-Audit-and-Prune-Preview`
- `Backup-and-Restore`
- `Configuration-and-Doctor`

That means pages no longer need to be half control panel and half miniature manual.

The UI should now prefer:

- a short local explanation of meaning
- a concrete next step
- a link to the exact wiki page for the deeper model

Not dense inline explanation for every branch.

### 3. "Advanced" exists, but its contract is still too soft

Symlinkarr already uses an advanced/disclosure pattern, which is directionally correct.

What is still missing is a stronger product rule for what belongs behind it.

Recommended rule:

`Advanced` should hide things that do not change the operator's immediate next action:

- low-frequency diagnostics
- secondary telemetry
- edge-case guidance
- full raw detail tables
- export/filter controls that are not needed for first-pass triage

If something changes the next action, it probably should not be advanced.

### 4. The missing primitive is a shared "operator closure" contract

The strongest pages now already trend toward the same shape:

- what happened
- what it means
- what to do next
- where to drill deeper

But the pattern is not yet strong enough to feel like a product law.

Symlinkarr should standardize a result/transition contract:

1. Outcome
2. Meaning
3. Next step
4. Advanced detail
5. Exact drilldown or wiki escape hatch

This is the simplest way to make many pages feel coherent without redesigning the whole app.

### 5. The right mental model is five operator loops

The app is easiest to reason about if it is presented as a small set of repeatable jobs:

1. Daily triage
   - `Dashboard`
   - `Status`

2. Diagnose why something did not match or link
   - `Scan`
   - `Scan History`
   - `Scan Run`

3. Repair broken state
   - `Dead Links`
   - `Repair Result`
   - `Cleanup`
   - `Prune Preview`

4. Recover runtime/config state
   - `Backup`
   - `Backup Result`
   - `Config`
   - `Doctor`
   - `No Config`

5. Handle anime edge cases
   - `Scan` override controls
   - `Anime Remediation`
   - `Anime Remediation Result`

This model is better than thinking in terms of "all available pages."

It gives the product a cleaner shape:

- fewer top-level mental buckets
- less repeated explanation
- more obvious page-to-page progression

## What This Means For The Next Track

The next main track should be:

**operator-loop compression**

That means:

1. tighten pages that still leave the operator between states
2. formalize what is default-visible versus advanced
3. reduce duplicated explanation where the wiki now carries the deeper guide
4. make every major page answer the same small set of questions quickly

## Recommended Implementation Order

### Tier 1

1. Formalize page classes
   - hub page
   - result page
   - inspection page
   - setup/recovery page

2. Standardize result pages around the closure contract
   - outcome
   - meaning
   - next step
   - advanced detail
   - drilldown/wiki link

3. Tighten advanced-mode semantics
   - default-open only for action-changing information
   - push secondary tables and caveats down

4. Remove explanatory duplication that no longer earns its screen space
   - especially once a focused wiki page already exists

### Tier 2

5. Audit dashboard drilldowns and "needs attention" landings
   - every attention card should land on a page with an obvious next move

6. Re-evaluate product breadth only after the operator loops feel closed
   - anime override slice 2
   - deeper queue provenance
   - other feature candidates

## What Should Not Become The Main Track

The council explicitly recommends against making any of these the primary project right now:

- more telemetry by default
- streaming-guard expansion as a headline feature
- more theme churn
- a new abstraction/refactor marathon
- broad feature expansion without a clear operator-loop fit

## Bottom Line

Symlinkarr does not currently need more pages, more telemetry, or more surface.

It needs a stricter product grammar.

The best next work is to make the app feel like a small set of closed, trustworthy operator loops where the user sees:

- the current state
- the meaning of that state
- the best next step

and only then the deeper machinery.
