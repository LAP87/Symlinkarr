# Design Council — Remaining Work After UI/Wiki Pass — 2026-04-22

Source: code exploration after the wiki split/publish pass, dashboard activity work, needs-attention layer, and recent UI streamlining.

## Question

What is actually left to tackle now that the long structural split pass is mostly done and the web UI is materially more coherent?

This council is intentionally narrower than the earlier product-horizon debate.
It focuses on what the codebase now says is still unfinished, uneven, or worth pushing next.

## Snapshot

The repo is in a better state than the older councils assumed:

- the large-file decomposition pass is mostly done
- explainability is in place for scan/matcher/auto-acquire skip reasons
- the dashboard now has a `Needs Attention` layer and a polling `Live Activity` feed
- the wiki has been split into operator-task pages and the main web UI now points at those narrower destinations
- restore/bootstrap/no-config flows are covered by meaningful tests

That means the center of gravity has shifted.

The next useful work is not "keep splitting files until every module is tiny."
The next useful work is to close the remaining operator workflow gaps.

## Findings

### 1. Observability is better, but still shallow where operators most need it

The dashboard is now useful, but the live/feed layer is still intentionally narrow:

- it tracks active and recent web-triggered `scan`, `cleanup audit`, and `repair`
- it does not yet surface daemon-cycle context
- it does not expose queue/job-level auto-acquire events in the same way
- it does not show richer per-phase failure context without jumping to other pages

This matters because the current dashboard can answer:

- "is something running?"
- "did something recently fail?"

But it still cannot fully answer:

- "what is the daemon doing right now?"
- "which part of auto-acquire is stuck or degrading?"
- "what changed in the system state since the last run?"

Council view:

- the next observability work should deepen the operator feed before attempting raw live logs
- the best shape is not a noisy log tail; it is clearer system-state deltas and queue/job drilldowns

### 2. Streaming safety exists, but only as a partial feature

Tautulli-based active-stream protection already exists in repair.

That is important because it changes the framing of "streaming guard" from a speculative feature into a partially landed one.

Current state:

- repair can query Tautulli and skip actively streamed files
- status can report Tautulli health / stream count
- the web/dashboard does not yet expose this protection clearly as a first-class operator signal
- cleanup/apply and acquisition/relink do not yet share the same protection model

Council view:

- this is now one of the strongest next-step candidates because the integration foundation already exists
- the right next move is expansion and surfacing, not greenfield invention

### 3. Anime tooling is strong, but still stops short of the last-mile operator fix

The codebase has:

- anime identity handling
- anime remediation planning
- a web UI for legacy anime cleanup

What it still lacks is the deliberate override layer for the ugly edge cases that external mappings will never fully solve.

That missing layer is now more visible because the rest of the anime workflow is no longer the bottleneck.

Council view:

- anime override remains the best candidate for a differentiated product feature
- it should be introduced as a small, explicit, auditable operator tool, not as a fuzzy global rule engine

### 4. The main UI is more coherent, but there are still operator dead ends

The major surfaces now have focused wiki links, but some secondary/transitional pages still act like thin endpoints instead of full operator surfaces.

Current examples:

- `backup_result`
- `cleanup_result`
- `repair_result`
- `scan_result`
- `discover_content`
- `anime_remediation_result`

These are not catastrophic UX failures, but they are still places where the operator can land with less guidance than the stronger primary pages now provide.

Council view:

- do not redesign these pages broadly
- do tighten them as "finish the operator loop" surfaces: outcome, meaning, next step, relevant link

### 5. Pure structural refactoring now has sharply lower marginal value

There are still sizeable modules left:

- `cleanup_audit.rs`
- `repair.rs`
- `main.rs`
- `auto_acquire.rs`
- `config.rs`
- `web/templates.rs`
- `matcher.rs`

But the repo no longer has the same obvious monolith problem it had before the recent pass.

The remaining large files should now be split only when one of these is true:

- upcoming feature work is blocked by local complexity
- review cost is still too high in that specific module
- testability clearly improves from a targeted extraction

Council view:

- file size alone is no longer enough reason
- the app should now spend more energy on operator value than on module cosmetology

## What Should Not Become The Main Track

The council explicitly recommends against making any of these the new headline project right now:

- more theme proliferation
- another long "split everything left" refactor marathon
- raw log streaming as the first observability answer
- broad new connector work with no operator workflow attached
- a generic abstraction push just because the codebase is cleaner now

## Recommended Order

### Tier 1

1. Deepen operator observability
   - extend the dashboard/status model so daemon-origin work and auto-acquire job pressure become legible without bouncing between pages
   - prefer queue/job drilldowns, failure context, and state deltas over log noise

2. Expand and surface streaming guard
   - treat the current repair/Tautulli integration as phase one
   - expose the protection clearly in web/dashboard
   - extend the same guard model to destructive cleanup/apply paths and relevant relink/acquire paths where it makes operational sense

3. Finish the remaining operator dead-end pages
   - tighten result/transition surfaces so they always answer:
     - what happened
     - why it matters
     - what the operator should do next

### Tier 2

4. Build the first anime override slice
   - small override store
   - explicit scope
   - clear precedence over external mappings
   - auditability in UI/reporting

5. Only then do targeted structural cleanup where new work actually needs it
   - especially `auto_acquire.rs`, `repair.rs`, or `main.rs` if the above work pushes too much logic into them

## Recommended Thesis For The Next Sessions

Symlinkarr should now optimize for:

**operator confidence under real runtime conditions**

That means the best next work is the work that reduces:

- uncertainty during background activity
- fear around destructive automation
- ambiguity in edge-case media identity handling
- context switching between UI, docs, and terminal

## Bottom Line

The repo is not running out of things to do.

It is running out of reasons to keep treating internal decomposition as the main event.

The most credible next steps are now:

1. better runtime observability
2. stronger streaming-aware safety
3. tighter outcome/next-step UX
4. anime override as the next truly differentiated feature
