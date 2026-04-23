# Tasklist — Operator Loops — 2026-04-23

Source: current repo state after the observability stop-point decision, wiki/task-page split, and the latest dashboard/operator UX passes.

This note replaces `TASKLIST_2026-04-22_POST_RC11.md` as the active horizon.
The new center of gravity is reducing operator decision cost.

## Primary Track

### 1. Compress the web UI into clear operator loops

- Goal: make Symlinkarr feel like a small number of closed operator jobs instead of a collection of feature pages
- Constraint: prefer reuse of the current disclosure/wiki/help-link patterns over redesigning the whole UI

#### Progress

- [x] First pass on result/transition surfaces:
  - move snapshots, artifact receipts, and large review tables behind disclosures
  - keep meaning, next step, and primary drilldowns visible
- [ ] Define the page taxonomy explicitly: hub / result / inspection / setup-recovery
- [ ] Standardize result pages on the same closure contract:
  - outcome
  - meaning
  - next step
  - advanced detail
  - exact drilldown or wiki link
- [ ] Make `Advanced` semantics stricter and consistent:
  - hide low-frequency diagnostics, caveats, filters, exports, and raw detail by default
  - keep action-changing information visible
- [ ] Audit secondary pages for residual dead ends or duplicated explanation, especially:
  - `scan_result`
  - `cleanup_result`
  - `repair_result`
  - `backup_result`
  - `discover_content`
  - `anime_remediation_result`
- [ ] Audit dashboard/triage drilldowns so every attention item lands on a page with an obvious next move

### 2. Hold observability at the current v1.0 stop point

- Goal: avoid turning runtime clarity into dashboard noise
- Constraint: reopen only if real operator feedback says the current heartbeat + provenance model is still not enough

#### Progress

- [x] Land live dashboard fragments, scan origin provenance, daemon cadence from daemon-origin runs, and DB-backed heartbeat
- [x] Expose daemon heartbeat/cadence through the web UI and `/api/v1/status`
- [x] Adopt the observability stop-point thesis for v1.0
- [ ] Reopen only if live operator review still reports runtime ambiguity

### 3. Keep the anime override track deliberately narrow

- Goal: preserve the usefulness of local anime overrides without drifting into a generic remap engine
- Constraint: no explicit episode/season remap design unless real operator evidence proves query-title overrides are insufficient

#### Progress

- [x] Save/delete local anime search overrides from `/scan`
- [x] Feed overrides into anime auto-acquire before anime-lists hints
- [x] Validate override saves against real tagged anime folders
- [x] Preserve failed form drafts and show actual local effect
- [ ] Collect real operator feedback before considering a second override slice

### 4. Use review automation at logical checkpoints

- Goal: treat PR-side deep review as a cheap extra audit pass
- Constraint: use it after meaningful batches, not after every tiny CSS/docs edit

#### Progress

- [x] Re-trigger `@codex review` on PR #32 after the rc.11 follow-up slices
- [ ] Triage any new actionable bot feedback once it appears

## Deferred / Not Main Track

### A. Streaming guard expansion

- Leave dormant unless a future release-upgrader or more aggressive mutation workflow creates a real playback race worth solving

### B. Deeper queue or daemon provenance

- Only reopen if actual operator testing says heartbeat + provenance still leaves meaningful ambiguity

### C. Broad new feature expansion

- Do not widen the product surface without first proving where the new feature fits inside the operator-loop model

### D. More large-file decomposition as a project

- Only do targeted splits when a real feature, test, or review burden justifies them
