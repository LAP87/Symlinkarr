# Tasklist - Post RC.11 - 2026-04-22

Source: current repo state after `v1.0.0-rc.11`, the long structural split pass, dashboard activity work, operator wiki split, and the latest anime-override / daemon-observability follow-up slices.

This note replaces the older "finish the split pass" tasklist as the active horizon.
The new center of gravity is operator confidence under live runtime conditions.

## Primary Track

### 1. Deepen live operator observability on the dashboard

- Goal: make the main dashboard trustworthy without forcing operators to reload or bounce between pages
- Constraint: prefer focused HTMX fragments and state summaries over raw log streaming

#### Progress

- [x] Add polling `Live Activity` feed for scan / cleanup / repair / queue incidents
- [x] Add `Needs Attention` triage for failed work, dead links, queue pressure, playback guard, and deferred refresh backlog
- [x] Surface overdue daemon cadence in dashboard triage and in `Status`
- [x] Make the `Needs Attention` panel itself HTMX-live so the highest-signal dashboard section no longer goes stale after first render
- [ ] Decide whether the next live slice should target dashboard summary cards/header badges or true daemon-origin provenance

### 2. Keep the anime override track narrow and operator-friendly

- Goal: make query-title overrides safe and legible without drifting into a generic remap engine
- Constraint: v1.0 stays query-title/hints only unless real operator evidence proves explicit remapping is needed

#### Progress

- [x] Save/delete local anime search overrides from `/scan`
- [x] Feed overrides into anime auto-acquire before anime-lists hints
- [x] Validate override saves against real tagged anime folders
- [x] Preserve failed form drafts so operators can correct mistakes without retyping
- [x] Show whether saved overrides still resolve locally and summarize their actual effect
- [ ] Collect real operator feedback before considering any explicit season/episode remap design

### 3. Use external review passes as cheap deep-dive checkpoints

- Goal: exploit PR-side review automation when the branch meaningfully changes
- Constraint: use it after logical batches, not after every tiny CSS/test tweak

#### Progress

- [x] Re-trigger `@codex review` on PR #32 after the rc.11 follow-up slices
- [ ] Triage any new actionable review feedback once the bot responds

### 4. Keep remaining UI work tied to real operator friction

- Goal: continue polishing only where pages still feel unclear or stale in practice
- Constraint: no broad redesign or theme churn as a primary track

#### Progress

- [x] Streamline dense pages with disclosures and focused wiki links
- [x] Close the obvious help-link coverage gaps
- [ ] Review the latest local image / compose pass notes and only tighten pages that still leave the operator without a clear next step

## Deferred / Not Main Track

### A. Streaming guard expansion

- Current repair / cleanup / dashboard / status protection is sufficient for now
- Revisit only if real incidents or a future release-upgrader feature make it materially important

### B. Explicit anime remap engine

- Out of scope for v1.0
- Revisit only if real-world operator cases prove query-title overrides are not enough

### C. More large-file decomposition for its own sake

- No longer the headline project
- Only do targeted splits when a concrete feature, test, or review burden justifies them
