# Design Council — Product Horizon — 2026-04-19

Source: post-RC hardening, explainability pass, theme/system polish, web no-config flow, and startup/restore/bootstrap test expansion.

## Question

Symlinkarr is now beyond "it works" and into "what should it become?"

The council debated three forward tracks:

1. deeper integration with the surrounding stack
2. broader product development inside Symlinkarr itself
3. stronger operator UI/UX

We also debated the inverse question:

4. where should we deliberately stop

## Current Position

Symlinkarr is strongest when it behaves like an integrity-first operator tool for a Real-Debrid-backed media stack.

It is weaker when it tries to look like:

- a general-purpose media manager
- a replacement for Sonarr/Radarr/Prowlarr
- a theme zoo with no product narrative
- a plugin platform before the core operator workflows are fully coherent

That distinction matters. The app now has enough foundation that the next work should mostly be judged by one question:

**Does this reduce operator uncertainty or operator effort in real workflows?**

If yes, it is likely on-strategy.
If no, it is probably drift.

## Debate

### Position A — Go Deeper On Integrations

Argument:

- Symlinkarr already talks to Real-Debrid, Decypharr, DMM, Prowlarr, Plex, Emby, Jellyfin, Bazarr, Tautulli, TMDB, and TVDB.
- The app becomes more valuable when it understands the actual operating conditions of the stack, not just filenames and symlinks.
- "Integrity-first" gets stronger when the app knows refresh state, queue state, streaming state, and identity mismatches across systems.

Best forms of deeper integration:

- Tautulli-backed streaming guard for repair/cleanup/acquire
- richer media-server refresh and invalidation feedback
- stronger anime identity/operator override paths
- more explainable cross-system state in dashboard/API

Weak forms of deeper integration:

- adding many more connectors just because we can
- making integrations configurable in dozens of tiny flags before the workflow is clear
- abstracting everything into traits before one more concrete product need forces the shape

Council view:

- deeper integration is good when it sharpens safety, observability, or automation quality
- deeper integration is bad when it only increases surface area

### Position B — Build More Product Inside Symlinkarr

Argument:

- The strongest remaining opportunities are not low-level anymore; they are workflow-level.
- Symlinkarr already has the ingredients for a real operator product:
  - integrity and backup/restore
  - explainability
  - web dashboard
  - anime remediation
  - acquisition/repair loops

The most compelling product expansions are:

- anime override workflow
- operator-facing incident/triage views
- run history and action history that explain system behavior
- saved remediation plans and safer guided cleanup flows

Risk:

- product breadth can quickly collapse into "a worse clone of the *arr suite"
- every new workflow adds docs burden, UI burden, and regression risk

Council view:

- Symlinkarr should grow as a focused operator product
- it should not grow into a broad "do-everything media platform"

### Position C — Push UI/UX Harder

Argument:

- the backend now knows a lot more than the UI shows cleanly
- operator trust comes from fast comprehension, not from raw capability
- the next big win may come from making the app legible under stress, not from adding new engines

Strong UI/UX directions:

- "what needs attention now?" dashboard
- live activity or near-live run feed
- clearer drilldowns from symptom → cause → suggested action
- onboarding that explains first-run, restore, and stack expectations cleanly
- fewer dead ends and fewer "read docs to understand system state" moments

Weak UI/UX directions:

- visual churn without workflow gain
- more and more themes as a substitute for product direction
- large redesigns before the operator journey is settled

Council view:

- UI/UX should be treated as operator comprehension work, not as decoration work

### Position D — Stop Expanding For A While

Argument:

- the app already covers a lot of ground
- every new integration and workflow compounds maintenance cost
- there is real value in declaring certain frontiers "not now"

This position argues for pausing feature breadth and staying on:

- reliability
- test quality
- workflow clarity
- selective polish

Council view:

- this position is directionally correct, but too conservative if taken literally
- the app should keep moving, but only inside a narrow product thesis

## Council Conclusion

Symlinkarr should continue.

It should **not** stop.

But it should stop trying to prove itself by sheer breadth or by endless internal hygiene.

The right move now is:

1. deeper integration where it improves operational safety or observability
2. product work where it turns raw capability into operator workflow
3. UI/UX work where it shortens the path from "something is wrong" to "I know why and what to do"

## Recommended Thesis

Symlinkarr should become:

**the operator control plane for symlink-backed media libraries**

Not:

- a torrent/discovery replacement
- a broad media organizer
- a theme playground
- an abstract integration framework

That thesis implies three product pillars:

### 1. Explainability

The app should answer:

- why did this not match?
- why did this not link?
- why did this not acquire?
- why was this blocked?
- what changed since the last run?

### 2. Recovery And Safety

The app should make it hard to break a working library and easy to recover from mistakes.

This includes:

- restore/bootstrap/no-config flows
- safer cleanup/remediation
- streaming-aware guards
- guided recovery views

### 3. Operator Workflow

The app should help an operator move through:

- detect
- understand
- decide
- act
- verify

without dropping into logs, SQLite, or scattered docs unless they choose to.

## What To Build Next

### Tier 1 — Strong Recommendation

#### A. Operator Activity / Live Run Feed

Why:

- biggest UX gap after explainability
- high operator value
- helps web users understand scan/repair/cleanup/auto-acquire in motion

Recommended shape:

- start with polling or HTMX refresh, not WebSockets first
- show current job, recent events, current phase, latest warnings, and final outcome
- do not start with full raw log streaming

Goal:

- "what is it doing right now?"
- "did it stall, fail, or finish?"

#### B. Anime Override Workflow

Why:

- the anime identity/remediation system is already one of the app's most differentiated assets
- current architecture is strong enough to justify operator tooling on top
- this is a real product feature, not random breadth

Recommended shape:

- small override store with explicit scope and auditability
- UI for finding a broken group and applying a deliberate override
- clear precedence rules over external mappings

Goal:

- handle the last ugly 5% that automated mapping will never solve cleanly

#### C. Streaming Guard Via Tautulli

Why:

- very aligned with integrity-first positioning
- easy to explain to users
- protects against the most emotionally expensive kind of automation failure

Recommended shape:

- gate destructive repair/cleanup/relink actions when a path is actively playing
- expose the block reason clearly in CLI/web
- allow explicit override when the operator really wants it

Goal:

- make automation feel safe enough to leave on

### Tier 2 — Good, But After Tier 1

#### D. Dashboard "Needs Attention" Layer

Examples:

- dead links rising
- repeated no-result auto-acquire runs
- blocked anime remediation groups
- refresh backlogs
- stale acquisition jobs

This is higher-value than more generic charts.

#### E. Media Server Adapter Trait

This is still a good architectural direction, but it is not a product win by itself.

Do it when one of these becomes true:

- a fourth serious media-server integration is added
- duplication between Plex/Emby/Jellyfin becomes painful again
- a concrete feature needs a cleaner adapter boundary

Not before.

### Tier 3 — Optional / Later

#### F. Full Log Viewer

Only worth it once the live activity feed exists and proves insufficient.

#### G. More Integration Breadth

Examples:

- Kodi
- Stash
- additional providers

These are not wrong, but they are not the best next bets.

## Where To Stop

The council recommends saying "no" to the following for now:

### 1. More Theme Expansion As A Main Track

The current theme work is enough for now.
Themes should no longer be a primary roadmap item unless they solve a legibility problem.

### 2. Plugin/Marketplace Thinking

Too early.
The app still benefits more from tight, opinionated product flows than from extensibility layers.

### 3. Broad Connector Collection

Do not turn the roadmap into "support every adjacent app".
Each integration should have a direct operator story.

### 4. Another Long Internal Refactor Marathon

The large structural split pass has paid off.
Future refactors should now be attached to a concrete feature, bug, or risk.

### 5. Generic Analytics Dashboarding

Avoid building vanity dashboards.
Prefer actionable state over decorative observability.

## Recommended Execution Order

1. Operator activity / live run feed
2. Dashboard "needs attention" summaries
3. Anime override workflow
4. Tautulli-backed streaming guard
5. MediaServerAdapter trait only if one of the concrete triggers occurs

## Decision

Symlinkarr should keep moving.

But it should move as a sharper product, not a broader one.

The next phase should be about making the system:

- easier to trust
- easier to understand
- safer to automate
- faster to operate

That is enough ambition for the next leg.
