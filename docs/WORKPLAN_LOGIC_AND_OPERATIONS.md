# Workplan: logic, operation lifecycle, and operator functions

Status: approved for serial implementation  
Baseline: `main` at `b9e33b8` (`v1.1.0-rc.8`)  
Created: 2026-07-26  
Execution model: one implementation agent at a time, followed by root review and full gates  

## Objective

Close the concrete correctness gaps found in the rc.8 Graphify/source audit, then add
the missing operator functions without creating parallel state machines.

The final result should:

1. make acquisition retries and restart recovery internally consistent;
2. prevent conflicting scan/repair/backfill/cleanup operations across web, scheduler,
   daemon, and standalone CLI processes;
3. expose the existing backfill domain flow through a bounded preview-first web/API
   workflow and, where safe, the scheduler;
4. preserve acquisition provenance and add safe per-job controls;
5. remove the `config.rs <-> models.rs` import cycle without a broad config rewrite.

No release, image publish, Compose edit, or production restart is part of this workplan.
Those remain separate approval gates.

## Non-negotiable implementation rules

- Work serially in the phase order below. Do not start phase N+1 before phase N has
  passed its targeted tests and the full repository gates.
- Preserve the existing public CLI and JSON behavior unless a phase explicitly adds
  fields or endpoints.
- Do not pin, downgrade, or replace dependencies. Prefer the existing Rust standard
  library, Tokio, SQLx, and current project helpers. A dependency change requires a
  demonstrated blocker and root-agent approval.
- Use additive SQLite migrations. Existing rc.8 databases must migrate in place.
- Do not reset or rewrite unrelated dirty work. Every agent must inspect the current
  diff first and accommodate changes already present.
- Do not commit, tag, release, push, or deploy from an implementation agent.
- Every behavior change gets a regression test at the same time.
- After each accepted phase, run `rtk graphify update .` so the graph follows the code.
- Keep operator errors explicit. A rejected operation must say what is running, where
  it came from, and when it started.

## Serial dependency map

```text
Phase 1: acquisition retry state
    |
    v
Phase 2: shared operation registry and exclusivity
    |
    +------------------------------+
    |                              |
    v                              v
Phase 3: backfill web/scheduler    Phase 4 prerequisites
    |                              |
    +--------------+---------------+
                   v
Phase 4: provenance and per-job controls
                   |
                   v
Phase 5: config/models domain-type cleanup
```

Phase 3 depends on phase 2 because backfill must join the same exclusive-operation
contract as scan and repair. Phase 4 follows phase 3 so provenance covers every
request origin, including web and scheduled backfill, without a second migration pass.
Phase 5 is deliberately last because it is structural cleanup, not a correctness gate.

## Shared phase gate

Each implementation agent must provide:

- files changed and why;
- migrations and backward-compatibility notes;
- tests added, including the failure path they prove;
- exact targeted commands run;
- remaining uncertainty or explicitly deferred work.

The root agent then runs:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --all-targets --locked
rtk cargo clippy --all-targets --all-features --locked -- -D warnings
rtk git diff --check
```

If dependency metadata changes despite the no-dependency-change rule, also run:

```bash
rtk cargo audit
rtk cargo build --release --locked
```

No later phase starts while any gate is red.

---

## Phase 1: make acquisition retry and restart state coherent

Status: `DONE`  
Priority: P1 correctness  
Primary owner files:

- `src/auto_acquire.rs`
- `src/auto_acquire/queue.rs`
- `src/auto_acquire/tests.rs`
- `src/db.rs`
- `src/db/acquisition_jobs.rs`
- `src/db/migrations.rs`
- `src/db/types.rs`
- `src/db/tests.rs`

### Problem to solve

`get_manageable_acquisition_jobs()` currently applies `attempts < MAX_JOB_ATTEMPTS`
to active `downloading` and `relinking` rows as well as retryable terminal rows.
An active fifth-attempt job can therefore disappear from restart recovery.

The in-memory `SubmittedAcquire.attempts` value is copied before the database update
increments it. Failure and relink backoff consequently use the previous attempt.
When an existing Decypharr item is reused, submission attempts intentionally do not
increase, but completed-unlinked backoff also uses that unchanged counter. Repeated
relink failures can stay on the minimum delay indefinitely.

### Chosen model

Keep the existing `attempts` column as the submission/search attempt count for
backward compatibility. Add one new counter:

```text
attempts         = provider/submission attempts
relink_attempts  = completed-download relink cycles
```

Do not rename `attempts`; that would expand the migration and every API unnecessarily.

### Required state behavior

```text
Queued/search path
    |
    +-- no candidate / provider error
    |      increment attempts
    |      calculate backoff from the effective incremented value
    |      stop automatic pickup at MAX_JOB_ATTEMPTS
    |
    +-- new Decypharr submission
    |      increment attempts
    |      reset relink_attempts to 0
    |      status = Downloading
    |
    +-- existing Decypharr item reused
           do not increment submission attempts
           status = Downloading

Downloading
    |
    +-- still active -> remain resumable regardless of attempts
    +-- failed       -> Failed with submission backoff
    +-- completed    -> Relinking and increment relink_attempts

Relinking
    |
    +-- link found   -> CompletedLinked
    +-- within wait  -> remain resumable regardless of either cap
    +-- timed out    -> CompletedUnlinked with relink backoff
                           |
                           +-- due and relink_attempts below cap -> retry relink cycle
                           +-- cap reached -> remain visible, require manual retry
```

### Database changes

1. Raise `LATEST_SCHEMA_VERSION` from 20 to 21.
2. Add migration v21:
   - `relink_attempts INTEGER NOT NULL DEFAULT 0` on `acquisition_jobs`;
   - preserve all existing rows and indexes;
   - provide the repository-standard down-migration behavior used by migration tests.
3. Extend `AcquisitionJobRecord` and `AcquisitionJobUpdate`.
4. Make increments explicit. The update API must be able to:
   - increment submission attempts;
   - increment relink attempts;
   - reset relink attempts after a genuinely new download;
   - leave both counters unchanged for capacity deferral.
5. Rewrite the manageable-job predicate:
   - `downloading` and `relinking` are always resumable;
   - `queued`, `no_result`, and `failed` obey the submission-attempt cap;
   - `completed_unlinked` obeys the relink-attempt cap and retry timestamp;
   - `blocked` remains retryable according to its timestamp without pretending that
     a queue/provider guard consumed a submission attempt.
6. Batch/manual retry resets both counters.

### Backoff rules

- Compute delay using the count that will be persisted for the current failure.
- Submission failure progression remains `30m, 90m, 180m, 180m...`.
- Completed-unlinked progression remains `5m, 15m, 45m, 120m, 120m...`.
- A reused existing download advances `relink_attempts`, not `attempts`.
- Capacity deferral and provider-pending guards do not consume either attempt budget.

### Required tests

Unit tests:

- backoff helpers for attempts 0 through cap+1;
- effective-attempt calculation for new submission versus reused existing item;
- relink backoff advances on repeated reused-existing cycles.

Database/migration tests:

- v20 database migrates to v21 with `relink_attempts = 0`;
- fresh schema contains the column;
- down/round-trip behavior follows current migration-test conventions;
- `downloading` with `attempts == MAX_JOB_ATTEMPTS` is manageable after restart;
- `relinking` with both counters at the cap is still manageable until it reaches a
  terminal state;
- maxed `failed` is not manageable;
- maxed `completed_unlinked` is not automatically manageable;
- manual retry resets both counters and makes the row manageable;
- capacity-blocked rows remain retryable without consuming attempts.

Flow regression tests:

- second submission failure uses the 90-minute tier, not 30 minutes;
- repeated completed-unlinked reuse progresses 5 -> 15 -> 45 minutes;
- a fifth-attempt active download can be reloaded and completed after a simulated
  process restart.

### Failure modes and operator outcome

| Failure | Handling after phase | Test | Operator visibility |
|---|---|---|---|
| Process exits during fifth download | Active row is reloaded | Required | Status remains Downloading |
| Existing torrent never relinks | Separate capped relink retries | Required | CompletedUnlinked with next retry/cap |
| Provider is temporarily pending | No attempt consumed | Required | Blocked reason retained |
| Manual retry targets exhausted jobs | Both counters reset | Required | Explicit reset count |

### Acceptance criteria

- No active job is excluded only because its submission counter reached five.
- Backoff tiers match persisted counters.
- Automatic relink retries are finite and manually recoverable.
- Existing queue CLI output remains compatible; adding `relink_attempts` to JSON is
  optional in this phase and may be deferred to phase 4.
- Full shared phase gate passes.

Completion evidence:

- additive schema v21 migration and v20 -> v21 -> v20 -> v21 round-trip verified;
- active cap/restart, manual reset, capacity, submission-backoff, and relink-cycle
  regressions added;
- root gate: 862 passed, 1 ignored; fmt, clippy with warnings denied, and diff-check
  passed.

---

## Phase 2: shared persistent operation registry and exclusivity

Status: `IN PROGRESS`  
Priority: P1 concurrency and restart safety  
Depends on: Phase 1 accepted  
Expected owner files/modules:

- `src/db.rs`
- `src/db/migrations.rs`
- `src/db/types.rs`
- new `src/db/operations.rs`
- `src/db/tests.rs`
- new `src/operations.rs` or an equivalently narrow coordinator module
- `src/main.rs`
- `src/commands/daemon.rs`
- `src/scheduler.rs`
- `src/web/mod.rs`
- synchronous mutation handlers under `src/web/handlers/` and `src/web/api/`

### Problem to solve

Web scan/cleanup/repair use one in-memory mutex. Manual scheduler runs use a separate
semaphore. Automatic scheduler ticks use neither. A second CLI process bypasses both.
The current guards therefore prevent only a subset of conflicting operations.

### Chosen architecture

Use one SQLite-backed operation registry as the source of truth. Do not introduce a
new queue service or dependency.

```text
CLI / Web / Scheduler / Daemon
            |
            v
   OperationCoordinator::acquire()
            |
            v
   BEGIN transaction
      recover expired running lease
      INSERT running operation
      unique active lock_key enforces exclusivity
   COMMIT
            |
            v
   execute existing command/domain function
            |
      +-----+------+
      |            |
   success       error/panic/shutdown
      |            |
      v            v
   Succeeded     Failed/Interrupted
      \            /
       update result + finished_at
```

### Operation data model

Migration v22 creates `operation_runs`:

- `id INTEGER PRIMARY KEY`;
- `lock_key TEXT NOT NULL`;
- `kind TEXT NOT NULL`;
- `origin TEXT NOT NULL` (`cli`, `web`, `scheduler`, `daemon`);
- `scope TEXT`;
- `status TEXT NOT NULL` (`running`, `succeeded`, `failed`, `interrupted`);
- `started_at`, `heartbeat_at`, `finished_at`;
- `message`;
- optional `result_json` for typed summaries without schema churn.

Add a partial unique index that permits only one `running` row per `lock_key`.
Initial lock key: `library-operation`.

### Conflict policy

The following acquire `library-operation`:

- scan, including search-missing;
- repair auto/manual;
- backfill preview/apply when it reads a consistency-sensitive snapshot or mutates;
- cleanup audit, prune apply, and anime remediation apply;
- import apply and link-state backfill.

Backup, cache refresh, status/report reads, and media-server refresh remain outside
the lock unless source inspection demonstrates unsafe overlap.

Acquire at top-level entry points, not inside nested domain helpers. Auto-acquire
relink scans invoked by scan/backfill must inherit the existing operation rather than
trying to reacquire the same lock.

### Lease and crash behavior

- Use an atomic transaction plus the partial unique index for acquisition.
- Heartbeat running operations at a fixed interval using existing Tokio primitives.
- Do not add a dependency merely for cancellation.
- Treat a missing heartbeat older than the documented lease timeout as stale on the
  next acquire/startup, mark it `interrupted`, then allow a new operation.
- Normal completion stops heartbeat and records terminal status.
- Panic/error paths must record failure before releasing exclusivity.
- Web shutdown waits for tracked background jobs for a bounded grace period. If the
  grace period expires, abort the task and mark the operation interrupted.
- Daemon mode retains and joins the web task instead of fire-and-forgetting it.

### WebState migration

- Replace concurrency decisions based solely on `BackgroundJobState` with the
  coordinator.
- Typed last outcomes may remain temporarily for rendering, but active state and
  conflicts come from `operation_runs`.
- Store the operation id alongside typed outcomes so the dashboard can correlate the
  generic run with scan/repair/cleanup detail.
- The scheduler run-now semaphore may be removed only after equivalent registry tests
  prove the new lock. Do not leave two authorities that can disagree.

### Required tests

Database tests:

- exactly one concurrent acquire succeeds;
- second acquire returns the active run metadata;
- different processes/connections observe the same lock;
- stale heartbeat is marked interrupted and recoverable;
- fresh heartbeat is not stolen;
- success, error, and interrupted terminal transitions preserve history.

Integration tests:

- web scan blocks manual scheduled scan;
- automatic scheduled scan blocks web repair;
- standalone CLI-style acquire blocks web cleanup apply;
- nested auto-acquire relink scan does not self-deadlock;
- panic/error releases the operation through a recorded terminal state;
- shutdown drains a short task and interrupts an over-grace task.

### Failure modes and operator outcome

| Failure | Handling after phase | Test | Operator visibility |
|---|---|---|---|
| Two mutation entry points race | Unique running lock | Required | HTTP 409/CLI error names active run |
| Process dies holding lock | Heartbeat expiry recovery | Required | Old run marked Interrupted |
| Nested relink scan reacquires | Inherited operation context | Required | No deadlock |
| Web receives Ctrl-C mid-job | Bounded drain, then interrupt | Required | Persistent terminal message |

### Acceptance criteria

- There is one exclusivity authority across web, scheduler, daemon, and CLI.
- Conflict responses include kind, origin, scope, and start time.
- Operation state survives UI restart.
- No normal command path can leave a fresh permanent lock.
- Existing scheduler run history remains intact and correlated where useful.
- Full shared phase gate passes.

---

## Phase 3: preview-first backfill for web/API and bounded scheduling

Status: `PENDING`  
Priority: P1 operator function  
Depends on: Phase 2 accepted  
Expected owner files/modules:

- `src/commands/backfill.rs`
- `src/web/mod.rs`
- `src/web/api/`
- `src/web/handlers/`
- `src/web/templates.rs`
- `templates/`
- `src/scheduler.rs`
- relevant web, handler, scheduler, and backfill tests
- `docs/API_SCHEMA.md`, `docs/CLI_MANUAL.md`, and wiki operator docs

### Reuse before adding

`run_backfill()` and `BackfillSummary` remain the domain implementation. Web and
scheduler adapters must not duplicate Arr enumeration, empty-folder detection,
matching, request caps, or auto-acquire behavior.

If presentation printing is inseparable from computation, extract the smallest typed
domain entry point needed by both CLI and web, then keep CLI rendering as an adapter.

### User flow

```text
Open Backfill
    |
    v
Choose explicit Arr scope + library or item filter
    |
    v
POST /api/v1/backfill/preview
    |
    +-- validation error -> actionable 400, no writes
    |
    v
Dry-run BackfillSummary
    |
    +-- direct matches / ambiguous / skipped / missing-search count
    |
    v
Explicit Apply confirmation
    |
    +-- relink only ----------------------------+
    |                                           |
    +-- search missing -> show request cap      |
                          reject unsafe scope    |
                                                v
                              shared OperationCoordinator
                                                |
                                                v
                                        background result/status
```

### API contract

Add:

- `POST /api/v1/backfill/preview`;
- `POST /api/v1/backfill/run`;
- `GET /api/v1/backfill/status`.

Request fields:

- required `arr` (`radarr`, `sonarr`, or `sonarr-anime`; `all` is preview-only);
- required `library` or `item` filter for mutating/search-missing runs;
- `search_missing`, default false;
- an explicit confirmation token/boolean for apply;
- optional dry-run-compatible output controls if already used by API conventions.

Responses use typed `BackfillSummary` fields rather than parsing CLI text.

### Safety rules

- Preview is always `dry_run=true` and creates no links, acquisition jobs, or Decypharr
  submissions.
- A mutating run requires explicit scope and confirmation.
- `search_missing=true` must show and obey
  `decypharr.effective_max_requests_per_run()`.
- Reject unbounded `arr=all` search when configured cap is unlimited.
- Use the phase-2 operation coordinator. Do not add another WebState boolean.
- Existing browser session, same-origin, and remote auth protections apply.

### Scheduler support

Add `ScheduledEvent::Backfill` only with strict argument validation:

- explicit non-`all` Arr scope;
- library or item filter;
- `search_missing` defaults false;
- search-missing requires a finite request cap;
- safety backup policy follows other link-mutating scheduler events;
- exported/imported scheduler rules preserve the new event.

If these invariants cannot be expressed cleanly in the current scheduler schema,
ship web/API backfill first and record scheduler backfill as the only permitted
phase split. Do not weaken the safety rules to fit the schema.

### Required tests

Domain tests:

- preview has zero DB/link/acquisition side effects;
- typed summary equals CLI summary inputs;
- explicit scope filters are honored;
- capped search defers excess requests deterministically.

API/handler tests:

- auth and same-origin enforcement;
- invalid/missing scope returns 400;
- preview success renders all summary categories;
- apply without confirmation is rejected;
- unsafe unlimited all-scope search is rejected;
- coordinator conflict returns 409 with active operation details;
- status survives a new WebState over the same database.

Scheduler tests:

- parse/serialize/export/import round trip;
- safe bounded rule validates and executes;
- missing scope, unlimited all-search, and malformed arguments fail closed;
- run-now and automatic tick share the operation lock.

Visual/operator verification:

- Backfill page at desktop and narrow widths;
- loading, empty, success, deferred, ambiguous, conflict, and failure states;
- double-submit produces one operation, not two.

### Acceptance criteria

- Operators can preview before any mutation.
- Web/API and CLI use one backfill implementation.
- No global unlimited search action is exposed.
- Backfill participates in persistent operation history.
- Full shared phase gate passes.

---

## Phase 4: acquisition provenance and safe per-job controls

Status: `PENDING`  
Priority: P2 observability and recovery  
Depends on: Phase 3 accepted  
Expected owner files/modules:

- `src/auto_acquire.rs` and `src/auto_acquire/`
- `src/db/acquisition_jobs.rs`
- `src/db/migrations.rs`
- `src/db/types.rs`
- `src/commands/queue.rs`
- `src/web/api/`
- `src/web/handlers.rs`
- `src/web/templates.rs`
- queue, DB, API, handler, and template tests
- API/CLI/operator documentation

### Data to preserve

Migration v23 adds nullable/backward-compatible fields:

- `request_origin`: `scan`, `backfill`, `repair`, `manual`, `unknown`;
- `reason_code`: stable machine-readable transition/outcome reason;
- `provider`: selected candidate provider such as `prowlarr` or `dmm`;
- `selected_query`: the actual query variant that selected the candidate.

Existing rows read as `unknown`/null. Do not store magnet URLs, credentials, cookies,
or provider payloads.

`AutoAcquireOutcome.reason_code`, provider selection, and the selected query must reach
the persisted job update instead of being reduced to a free-text error.

### Origin semantics

`request_origin` describes the latest domain request that caused the job to be
enqueued or reconsidered. If preserving both first and latest origin is materially
cheap during implementation, use `created_origin` plus `last_request_origin`;
otherwise prefer one explicit latest origin over an ambiguous value.

All call sites must set origin:

- scan and anime scan -> `scan`;
- Arr backfill, including web/scheduler -> `backfill`;
- dead-link repair -> `repair`;
- queue/manual operator actions -> `manual`.

### API and CLI controls

Add:

- `GET /api/v1/queue` with status and bounded limit filters;
- `POST /api/v1/queue/{id}/retry`;
- `POST /api/v1/queue/{id}/cancel` for jobs not handed to Decypharr.

Rules:

- per-job retry is allowed only for retryable terminal states;
- retry resets the relevant counters and clears stale provider/outcome detail while
  preserving origin/history fields needed for audit;
- cancel is allowed for queued/blocked/no-result/failed/completed-unlinked work that
  has no active external download;
- do not pretend to cancel an active Decypharr download. Return conflict with a clear
  explanation; external download deletion is not part of this phase;
- batch CLI retry remains for compatibility;
- add optional CLI `queue retry --id` only if it reuses the same DB method.

Add a terminal `cancelled` status if required. It must be visible, manually retryable,
and excluded from automatic pickup.

### Operator presentation

Queue cards and activity items show:

- origin;
- provider and actual selected query when known;
- stable reason label plus existing human message;
- submission and relink attempts;
- next retry/cap state;
- retry/cancel buttons only when valid for the current state.

### Required tests

- v22 rows migrate with safe defaults;
- all scan/backfill/repair call sites persist the correct origin;
- Prowlarr reject -> DMM select preserves provider, selected query, and reason;
- no secret/magnet material is persisted or rendered;
- per-id retry changes only one job and validates state;
- cancel of queued job works and prevents automatic pickup;
- cancel of downloading/relinking job returns conflict without altering the row;
- repeated retry/cancel requests are idempotent;
- API list limits are clamped and status parsing fails closed;
- browser controls obey auth and same-origin protection;
- existing batch retry and queue JSON remain compatible.

### Failure modes and operator outcome

| Failure | Handling after phase | Test | Operator visibility |
|---|---|---|---|
| Fallback chooses different provider | Persist actual provider/query | Required | Queue explains selection |
| Retry races active worker | Conditional state update | Required | 409 with current status |
| Cancel requested after handoff | Reject, do not delete externally | Required | Clear conflict |
| Old DB row lacks provenance | Safe unknown/null defaults | Required | “Unknown origin”, no crash |

### Acceptance criteria

- Every new acquisition request has a persisted origin.
- Stable reason codes survive restart and are returned by API/CLI.
- One job can be retried or safely cancelled without resetting its status cohort.
- Active external downloads are never falsely reported cancelled.
- Full shared phase gate passes.

---

## Phase 5: remove the config/models domain-type cycle

Status: `PENDING`  
Priority: P3 structural cleanup  
Depends on: Phases 1-4 accepted  
Expected owner files/modules:

- new neutral media/domain module under `src/`
- `src/config.rs`
- `src/models.rs`
- import sites identified by Graphify/compiler
- focused serialization and model tests

### Scope

Move `MediaType` and `ContentType` into one neutral domain module. Update imports and
retain serialization/display/default behavior exactly.

```text
Before:
config.rs --uses--> models::MediaType
models.rs --uses--> config::ContentType

After:
config.rs ----\
               -> media_domain::{MediaType, ContentType}
models.rs ----/
```

Use temporary internal re-exports only if they reduce diff risk. Remove them before
phase acceptance if no compatibility consumer needs them.

### Explicitly not part of this phase

- Do not split the full `Config` god object.
- Do not change config file syntax or defaults.
- Do not introduce trait-based configuration abstractions.
- Do not reorganize unrelated models.

Those changes have much larger blast radius and no current correctness payoff.

### Required tests

- current YAML/JSON content-type values deserialize unchanged;
- `ContentType::default()` remains TV;
- anime still maps to `MediaType::Tv` where currently expected;
- display/serde values remain stable;
- Graphify reports no `config.rs -> models.rs -> config.rs` import cycle;
- full repository gates pass without new allow attributes.

### Acceptance criteria

- The two-file cycle is gone.
- No user configuration or API representation changes.
- The change is mechanical and reviewable.
- Full shared phase gate passes.

---

## What already exists and must be reused

- SQLite migration runner through schema v20.
- Acquisition queue persistence, state enums, batch retry, and queue status UI.
- Stable auto-acquire reason codes in memory.
- Scan origins and persistent scheduler run history.
- Web background scan/cleanup/repair patterns and panic capture.
- Scheduler validation, safety backups, stale-run recovery, and export/import.
- Typed `BackfillOptions` and `BackfillSummary`.
- Browser session, same-origin, remote-bind, and auth protections.
- Full Rust unit/integration-style test suite with 856 passing tests at baseline.

The workplan extends these primitives. It does not replace them with a new framework.

## NOT in scope

- New metadata/download providers: unrelated to the identified lifecycle gaps.
- Active Decypharr download deletion: cancellation semantics and data-loss risk require
  a separate design.
- Full configuration dependency injection rewrite: too broad for the payoff.
- New Rust dependencies or version pins: current stack already supplies the required
  database and async primitives.
- Release/tag/GHCR/production rollout: separate authorization and verification gate.
- General visual redesign: phase 3 adds only the UI required for a safe operator flow.

## Test coverage map

```text
PHASE 1
  migration -> query eligibility -> transition counter -> backoff -> restart
      [unit]        [DB test]          [flow test]       [flow test]

PHASE 2
  entry point -> atomic acquire -> heartbeat -> command -> terminal result
   [integration]     [DB race]      [time test]  [existing] [DB/API test]
       |
       +-> conflict from another origin [integration]
       +-> stale process recovery       [integration]
       +-> shutdown drain/interrupt     [integration]

PHASE 3
  browser/API -> validation -> dry preview -> confirm -> coordinator -> backfill
      [handler]       [unit]       [no-side-effect] [handler] [integration]
       |
       +-> capped acquisition [domain + handler]
       +-> scheduler rule      [scheduler integration]

PHASE 4
  candidate/origin -> persisted provenance -> API/UI -> retry/cancel CAS
      [domain]             [migration/DB]    [handler]  [race test]

PHASE 5
  type move -> serde/default compatibility -> compile graph -> full regression
    [unit]             [unit]                 [Graphify]       [full suite]
```

No silent failure path is accepted. A phase is incomplete if a production failure
listed above has neither a test nor an operator-visible terminal state.

## Performance constraints

- Operation-registry acquisition must be O(1) with indexed active-lock lookup.
- Heartbeat writes must be bounded to one update per active operation per interval.
- Queue list API must enforce a server-side maximum.
- Backfill preview must reuse the current batched Arr/database queries; no new N+1 loop.
- Provenance columns must not store full provider payloads.
- Phase 5 must not add clone-heavy config wrappers or dynamic dispatch.

## Agent execution protocol

One Terra worker is active at a time.

For each phase:

1. root updates the phase status to `IN PROGRESS`;
2. root dispatches one `gpt-5.6-terra` worker with explicit file ownership;
3. worker inspects the existing diff and implements only that phase;
4. worker runs targeted tests and reports;
5. root reviews the diff and runs the shared phase gate;
6. root fixes or sends focused follow-up to the same worker;
7. after acceptance, root marks the phase `DONE`, updates Graphify, and starts the next
   Terra worker.

This is intentionally sequential. Phases share DB migrations, state enums, WebState,
and scheduler behavior; parallel worktrees would create avoidable conflicts and make
state-machine review harder.

## Completion definition

The workplan is complete when:

- all five phases are marked `DONE`;
- every migration upgrades an rc.8 database in tests;
- full fmt/test/clippy/diff-check gates pass after the combined diff;
- Graphify is current and the config/models cycle is gone;
- docs match the shipped CLI/API/UI behavior;
- repository changes are reviewed but not released or deployed without a new explicit
  instruction.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|---|---|---|---:|---|---|
| CEO Review | `/plan-ceo-review` | Scope and strategy | 0 | Not run | Existing audit already fixed the target scope |
| Codex Review | `/codex review` | Independent second opinion | 0 | Not run | Serial Terra execution requested by user |
| Eng Review | `/plan-eng-review` | Architecture and tests | 1 | Clear for implementation | Serial dependencies locked; failure and coverage map included |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | Deferred | Run against phase-3 implementation, not speculative markup |
| DX Review | `/plan-devex-review` | Developer experience | 0 | Not required | No new developer-facing distribution surface |

**UNRESOLVED:** 0 architectural decisions blocking phase 1.  
**VERDICT:** Engineering plan cleared for serial implementation. Phase 3 receives a
focused design/visual check after code exists.
