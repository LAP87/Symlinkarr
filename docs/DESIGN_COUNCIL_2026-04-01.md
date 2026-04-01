# Design Council - 2026-04-01

This document captures a hostile-but-fair design council pass over Symlinkarr after the `v0.3.0-beta.1` release prep.

Inputs used:

- live repo state on `main`
- current [RC roadmap](./RC_ROADMAP.md)
- current README/wiki/API/CLI docs
- external jury passes via Ollama cloud models
- direct codebase inspection of the largest runtime hotspots

## Executive Verdict

Symlinkarr is **not a finished product** and should **not** be recommended broadly to all Real-Debrid users yet.

It is, however, **well past hobby-script territory**. The core product direction is coherent and the current label is best described as:

- **strong power-user beta**
- **real RC-prep**
- **not yet a trustworthy v1.0 RC**

The main problem is **not** that the product has no value. The main problem is that the product still asks the operator to trust too many complicated mutation paths without enough simple, visible answers when something is skipped, blocked, partially applied, or rate-limited.

## Council Synthesis

### What The Council Agreed On

- the core product is real and useful
- the biggest remaining gap is **trust**, not raw feature count
- anime/legacy remediation is still the messiest operational area
- media-server overload and partial-failure handling still need more hardening
- observability is not yet strong enough for broad recommendation
- several core files are now large enough that maintenance risk is rising fast

### What The Council Rejected

Some external jury opinions were deliberately harsher than the final synthesis.

The council does **not** recommend:

- removing the web UI entirely
- removing media-server-free operation
- ripping out multi-server support just because it adds complexity

Those features are already integrated into the product story and are valuable. The problem is not that they exist. The problem is that they still need a simpler, safer operator model.

## Overscope vs Right Scope

### Product Scope

The product is **broad but still coherent**.

The current scope still fits the same core product:

- scan
- match
- link
- repair
- cleanup
- optional media refresh

That is a valid "last-mile library layer" for RD-backed media setups.

### Implementation Scope

The implementation is becoming **too context-heavy in a few hotspots**.

Largest Rust files right now:

- `src/db.rs` ~ 4.1k lines
- `src/cleanup_audit.rs` ~ 3.7k lines
- `src/auto_acquire.rs` ~ 3.1k lines
- `src/web/api/mod.rs` ~ 3.1k lines
- `src/commands/report.rs` ~ 2.6k lines
- `src/config.rs` ~ 2.5k lines
- `src/web/handlers.rs` ~ 2.4k lines

Some of that size is tests, but several of these files still contain **1.5k-2.5k+ lines of runtime logic before tests begin**. That is the real warning sign.

## Not Yet Good Enough For `v1.0 RC`

### 1. Remediation Trust

Current problem:

- too few groups are cleanly auto-eligible
- blocked reasons are still not simple enough
- dirty libraries remain difficult to reason about

Why this blocks RC:

- users will not trust cleanup/remediation if it still feels heuristic-heavy and operator-opaque

### 2. Scan/Link Observability

Current problem:

- roadmap still explicitly calls out missing skip-reason visibility
- too many "why did nothing happen?" cases still require logs or JSON digging

Why this blocks RC:

- a product that silently skips work looks broken, even when it is technically acting safely

### 3. Media-Server Hardening

Current problem:

- multi-backend invalidation is real, but still sensitive to pacing and cap semantics
- overload risk is known from live Plex behavior

Why this blocks RC:

- a linking tool that destabilizes Plex, Emby, or Jellyfin loses operator trust immediately

### 4. Mutation Safety Story

Current problem:

- cleanup, repair, remediation, and refresh are guarded, but the global story is still too complex
- the product still needs a simpler answer to: "what exactly happened, and what do I do now?"

Why this blocks RC:

- recovery needs to feel deterministic, not expert-only

## Where The Architecture Is Good

- the product still has a recognizable core pipeline
- media-server adapters are at least behind a boundary now
- the roadmap is more honest than the average beta project
- there is already substantial real-world validation against dirty live libraries
- the test surface is large and meaningful

## Where The Architecture Is Getting Too Heavy

- scan telemetry and scan history are becoming field-heavy
- DB, cleanup, reporting, and web/API surfaces are carrying too many cross-cutting concerns
- background-job state, mutation workflows, and operator reporting are split across several large files
- anime-specific complexity is still too entangled with general operational trust

## AI-Assisted Development Risk

Symlinkarr is now large enough that **one-model full-context development is unreliable by default**.

The risk is highest in work that crosses:

- `db.rs`
- `commands/scan.rs`
- `cleanup_audit.rs`
- `auto_acquire.rs`
- `web/api/mod.rs`
- `web/handlers.rs`
- `config.rs`

This does **not** mean the project is too large for LLM-assisted work. It means future work should keep using:

- narrow slices
- explicit review prompts
- parallel explorers/jury passes
- stronger module boundaries over time

## Immediate Recommendation

Do **not** spend the next major cycle on more breadth.

Do:

1. finish operator-visible scan/link skip reasons
2. improve blocked-reason summaries and remediation clarity
3. continue real-load hardening of Plex/Emby/Jellyfin refresh behavior
4. start breaking the biggest runtime hotspots into smaller modules

## Suggested Cuts Or Deferrals

Do not add these before RC:

- new acquisition/provider complexity
- broader duplicate-remediation automation outside current safe zones
- new media-server-side features beyond hardening existing ones
- UI cosmetics that do not improve operator trust

## Final Product Call

Symlinkarr is currently:

- **valuable**
- **coherent**
- **already useful in live environments**
- **not yet broad-user safe**

The right next move is not to rethink the whole product.

The right next move is to keep the product shape, stop adding breadth, and finish the trust/observability/hardening work that makes the existing feature set feel deterministic.
