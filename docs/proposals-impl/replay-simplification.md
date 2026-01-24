# Proposal: Replay Simplification (Commands/Events Model)

**Date:** 2026-01-19
**Status:** Implemented

## Summary
This proposal simplifies Duroxide replay by moving to a **commands (actions) vs. events (history)** model.

Conceptually this is similar to Temporal’s replay contract and Durable Task-style systems:

- Orchestration code calls `schedule_*()` APIs which **emit actions** (commands).
- The replay engine processes persisted **history events** and enforces determinism by checking that the **sequence of emitted actions matches the sequence of schedule events** already in history.
- If orchestration emits actions beyond history, those actions are returned to the dispatcher as **work to execute** (and later persisted as schedule events).
- Completion events are applied in **FIFO history order** by plugging results into an **open futures** map.
- The engine **polls only when there is a chance to make progress**, primarily after plugging a completion (or delivering other new info like external/cancel).

This can “slide in” by keeping the existing runtime boundary (`ReplayEngine` input/output) stable and replacing only the internals.

## Relationship to earlier proposal
An earlier proposal, [standard-async-futures.md](standard-async-futures.md), explores enabling standard Rust async/await support by moving validation to schedule-time and using FIFO gating in `Future::poll()`.

This replay-simplification proposal aims to solve the same core problems (standard `async {}` composition and deterministic concurrency) but does so by moving the “hard parts” into a single replay loop that processes history events, rather than making each future implementation responsible for history scanning.

### Coverage notes
- This model addresses the main problems identified in the earlier proposal:
   - reducing/centralizing replay complexity (less logic inside each `poll()`),
   - enabling `async {}` blocks that await durable operations,
   - making deterministic racing/joining feasible via workflow-safe combinators.
- One concrete mechanism from the earlier proposal is still required here: a **dehydration mechanism** to prevent Drop-based cancellation from firing when the orchestration is merely suspending and the runtime drops the orchestration future at the end of an evaluation.

## Goals
- Make determinism checks explicit and auditable: “emitted action sequence must match schedule event sequence”.
- Make replay driven by the history event stream.
- Preserve FIFO completion semantics.
- Preserve physical activity cancellation via `Drop`, but ensure replay progress does not depend on `Drop`.
- Avoid public API changes unless unavoidable.
- No backward compatibility with old histories.

## Non-goals
- Migration for existing stored histories.
- Renaming user-facing API types (e.g., `Action` -> `Command`) unless required.

## Terms
- **Action**: emitted by orchestration code; represents a command request (activity, timer, sub-orchestration, external subscription, etc.).
- **Schedule event**: history event that represents an accepted command (e.g., `ActivityScheduled`, `TimerCreated`).
- **Completion event**: history event that represents a result for a schedule (e.g., `ActivityCompleted`, `TimerFired`).
- **Open future**: entry keyed by schedule ID tracking whether its completion is available.
- **Quiescent**: replay cannot advance further without new history.

## EventKind inventory and handling table
This table enumerates all current history event variants in `EventKind` and how the replay-simplification engine treats them.

Legend:
- **Category**: Lifecycle / Schedule / Completion / Terminal / Special
- **Replay handling**: what the engine must do when this event is encountered in history order.
- **Determinism check**: whether it must match an emitted action (and what fields are compared).

| EventKind | Category | Replay handling | Determinism check |
|---|---|---|---|
| `OrchestrationStarted { name, version, input, parent_instance, parent_id }` | Lifecycle | Must be the first event for an execution; establish metadata and create/pin the orchestration future | Not an action; history sanity only |
| `OrchestrationCompleted { output }` | Terminal | Forced exit based on history; stop evaluation | Optional: verify workflow terminal agrees (future work); initially “history authoritative” |
| `OrchestrationFailed { details }` | Terminal | Forced exit based on history; stop evaluation | Optional: verify workflow terminal agrees (future work); initially “history authoritative” |
| `OrchestrationContinuedAsNew { input }` | Terminal | Forced exit based on history; stop evaluation | Optional: verify workflow terminal agrees (future work); initially “history authoritative” |
| `OrchestrationCancelRequested { reason }` | Terminal (forced) | Forced exit based on history; stop evaluation (emergency cancel can be inserted asynchronously) | No action match required; history authoritative |
| `ActivityScheduled { name, input }` | Schedule | Consume the next emitted action and validate it; open an entry keyed by `event_id` | Must match emitted `Action::CallActivity` (name/input equality) |
| `ActivityCompleted { result }` | Completion | Plug result into open entry keyed by `source_event_id`; then allow one poll | Requires open entry exists; else nondeterminism |
| `ActivityFailed { details }` | Completion | Plug failure into open entry keyed by `source_event_id`; then allow one poll | Requires open entry exists; else nondeterminism |
| `TimerCreated { fire_at_ms }` | Schedule | Consume the next emitted action and validate it; open an entry keyed by `event_id` | Must match emitted `Action::CreateTimer` (fire_at_ms equality) |
| `TimerFired { fire_at_ms }` | Completion | Plug result into open entry keyed by `source_event_id`; then allow one poll | Requires open entry exists; else nondeterminism |
| `ExternalSubscribed { name }` | Schedule | Consume the next emitted action and validate it; open an entry keyed by `event_id` (if correlation needed) | Must match emitted “subscribe” action (name equality) |
| `ExternalEvent { name, data }` | Completion (special) | Deliver into a deterministic inbox; allow one poll | If an `ExternalSubscribed` discipline is enforced, validate a subscription exists; otherwise treat as deliverable-by-name |
| `OrchestrationChained { name, instance, input }` | Schedule | Consume next emitted action and validate it; treat as fire-and-forget scheduling | Must match emitted `Action::StartOrchestrationDetached` (name/instance/input equality) |
| `SubOrchestrationScheduled { name, instance, input }` | Schedule | Consume next emitted action and validate it; open entry keyed by `event_id` | Must match emitted `Action::StartSubOrchestration` (name/instance/input equality) |
| `SubOrchestrationCompleted { result }` | Completion | Plug result into open entry keyed by `source_event_id`; then allow one poll | Requires open entry exists; else nondeterminism |
| `SubOrchestrationFailed { details }` | Completion | Plug failure into open entry keyed by `source_event_id`; then allow one poll | Requires open entry exists; else nondeterminism |
| `SystemCall { op, value }` | Special (schedule+completion) | Treat as a deterministic side effect recorded in history; deliver `value` to the orchestration and allow one poll if needed | Must match an emitted “system call” action (op equality); `value` is the recorded result |

Notes:
- **Schedule IDs**: schedule events are keyed by their `event_id`. Completion events refer back via `source_event_id`.
- **Order**: schedule events are consumed in strict history order; the workflow must emit an identical sequence.

## Determinism Rules
Strict, Temporal-style:

1) **Schedule determinism (primary)**
   - Each schedule event in history must have a corresponding emitted action at the same sequence position.
   - Emitted action must match (type + payload fields). Mismatch => nondeterminism.

2) **Completion consistency**
   - A completion event must reference an already replay-validated schedule (an entry exists in `open`).
   - Completion without an `open` entry => nondeterminism/corrupt history.

3) **New commands beyond history**
   - After processing all schedule events present in history, any remaining emitted actions are **new commands**.
   - These are returned as `actions_to_take` for the dispatcher.

## Cancellation Semantics
Two distinct concepts:

1) **Replay-level behavior (logical)**
   - Replay must not depend on Rust `Drop` timing.
   - Completions can be plugged even if their futures are never awaited.
   - This prevents stalls even for `select2()` variants that do not drop/cancel losers.

2) **Host-level behavior (physical)**
   - Dropping a durable future can request cancellation of in-flight work (best-effort).
   - This is used for resource usage and cleanup only.

### Dehydration mechanism (required)
Because the runtime may drop the pinned orchestration future when an evaluation reaches a suspension point, any in-scope durable futures will also be dropped as a consequence. If `Drop` is used to request cancellation, this would incorrectly cancel in-flight work during normal suspension.

To prevent that, the runtime must provide a **dehydration guard**:

- The orchestration evaluation sets a flag (e.g., `ctx.dehydrating = true`) immediately before it relinquishes control back to the dispatcher (i.e., when the workflow is not complete but must wait for future history).
- Durable future `Drop` handlers must check this flag and become a no-op when `dehydrating == true`.
- The flag must be cleared (`dehydrating = false`) at the beginning of the next evaluation before polling resumes.

This keeps cancellation semantics correct:
- **User drop** (e.g., loser branch dropped by deterministic combinator or user control flow): `dehydrating == false` => cancellation can be recorded.
- **Runtime drop due to suspension**: `dehydrating == true` => no cancellation is recorded.

## Async blocks and workflow-safe combinators
One motivation for this replay simplification is enabling orchestrations to express deterministic concurrency using Rust `async { ... }` blocks that await durable operations.

### Design intent
- Orchestrations will use **Duroxide-provided combinators** (not `tokio::select!` / `tokio::join!`) to race/join async blocks in a replay-safe way.
- With this replay model, the only source of progress is history-delivered information (completions/external/cancel). The engine only polls when such information is delivered, making scheduling deterministic.

### Planned implementation approach
- `select` combinator: delegate to `futures::select_biased`.
   - Rationale: deterministic tie-breaking via left-biased polling order.
   - Requirement: operand order must be stable across replays (no data-dependent reordering of branches).
- `join` combinator: delegate to `futures::join`.
   - Rationale: deterministic completion after both branches resolve.

### Cancellation semantics interaction
- For `select` that cancels the loser: Duroxide should deterministically mark the loser branch as cancelled at the orchestration level (to avoid FIFO stalls) and may additionally trigger physical cancellation (best-effort) via `Drop` / provider cancellation.
- For `select2` variants that do not cancel/drop the loser: replay must still progress; loser completion events can be plugged even if never awaited.

## Pseudo Algorithm (Single Workflow-Task Evaluation)

### Inputs
- `working_history = baseline_history + staged_completions` (already filtered to the current execution)
- `handler` + orchestration metadata

### Outputs
- `actions_to_take: Vec<Action>` (new commands beyond history)
- `cancelled_activity_ids: Vec<u64>` (physical cancels)
- `TurnResult` or nondeterminism error

### Core state
- `emitted_actions: VecDeque<Action>` — actions emitted by workflow polls, not yet matched to schedule events.
- `open: HashMap<u64, OpenState>` — keyed by schedule `event_id`.
- `must_poll: bool` — set only when there is new information that can advance the workflow.

### Algorithm

```rust
// 0) Sanity
assert history contains OrchestrationStarted as first event, else corrupted history

// 1) Init
pin workflow future (created once)
emitted_actions = []
open = {}
actions_to_take = []
must_poll = true

// Dehydration guard
ctx.dehydrating = false

// 2) Process history in order
for event in working_history:

  // Forced exit semantics (cancel/terminal can appear at any point)
  if event is OrchestrationCancelRequested:
     return TurnResult::Cancelled(reason) (history authoritative)
  if event is OrchestrationCompleted/Failed/ContinuedAsNew:
     return TurnResult::{Completed/Failed/ContinueAsNew} (history authoritative)

  if must_poll:
     poll_once(future)
     emitted_actions.extend(ctx.drain_emitted_actions())
     cancelled_activity_ids.extend(ctx.drain_cancelled_activity_ids())
     if future Ready and history has not yet reached a terminal event:
         // nondeterminism unless explicitly allowed; define policy
     must_poll = false

  match event.kind:

    OrchestrationStarted:
      // already validated; no-op

    // Schedule events: must match emitted actions in order
    ActivityScheduled/TimerCreated/ExternalSubscribed/OrchestrationChained/SubOrchestrationScheduled:
      action = emitted_actions.pop_front() else nondeterminism("history schedule but no emitted action")
      if !action.matches_schedule_event(event): nondeterminism("schedule mismatch")
      open[schedule_id=event.event_id] = OpenState { result: None, cancelled: false } if absent
      // optional: bind handle -> schedule_id in ctx

    // Completion events: must reference an open schedule
    ActivityCompleted/ActivityFailed/TimerFired/SubOrchestrationCompleted/SubOrchestrationFailed:
      entry = open.get_mut(source_schedule_id=event.source_event_id) else nondeterminism("completion without schedule")
      if entry.result is None: entry.result = Some(event.payload)
      must_poll = true

    ExternalEvent:
      ctx.deliver_external(name, data)
      must_poll = true

    SystemCall:
      action = emitted_actions.pop_front() else nondeterminism("systemcall in history but no emitted action")
      if !action.matches_system_call(op): nondeterminism("systemcall op mismatch")
      ctx.deliver_system_call_result(op, value)
      must_poll = true

    _:
      ignore

// 3) History drained; finalize any pending poll
if must_poll:
   poll_once(future)
   emitted_actions.extend(...)
   must_poll = false

// 4) Remaining emitted actions are new commands beyond history
actions_to_take.extend(emitted_actions)

// If we are returning Continue (i.e., waiting for future history), mark dehydration before
// dropping the orchestration future so Drop-based cancellation does not trigger.
if returning TurnResult::Continue:
   ctx.dehydrating = true

return TurnResult::Continue + actions_to_take
```

### Notes on “new commands beyond history”
The above returns `actions_to_take` at end-of-history. Equivalent behavior can be implemented as “stop processing schedule checks as soon as we detect the history has no more schedule events”, but the end-of-history approach keeps the loop simple.

## How this slides in with minimal changes

### Keep runtime boundary stable
Keep the existing runtime interface and call site shape:
- `ReplayEngine::new(instance, execution_id, baseline_history)`
- `ReplayEngine::prep_completions(messages)`
- `ReplayEngine::execute_orchestration(...) -> TurnResult`
- getters:
  - `history_delta()`
  - `pending_actions()` (new model: these are “actions_to_take”)
  - `cancelled_activity_ids()`

This preserves the caller in `src/runtime/execution.rs`.

### Internal change only: swap engine implementation
Replace only the internals of `ReplayEngine::execute_orchestration`:
- Today it delegates to `run_turn_with_status_and_cancellations` (poll-once + history-mutating schedule semantics).
- New model: use the event-processing evaluator above.

### Avoid public API changes by adding an internal “emit-actions” mode
To keep orchestration author code unchanged:
- Introduce an internal mode where `schedule_*()` emits actions rather than appending schedule events into `ctx.history`.
- In step 1 (below), we can test the new replay engine without changing schedule by using a minimal harness.

## Implementation plan (phased)

### Phase 1: Add a duplicate replay engine + unit tests (no schedule changes)
Goal: validate the new model in isolation by feeding:
- fixed histories (vectors of `Event`)
- minimal orchestration functions
and asserting the evaluator returns the expected emitted actions / nondeterminism.

Approach:
1) Create a parallel implementation (e.g., `ReplayEngineSimplified`) in a new module close to the current engine.
2) Keep it test-only / non-integrated initially.
3) Build a tight, minimal test harness:
   - A “dummy orchestration context” that can emit actions into a buffer.
   - A “dummy durable future” / minimal await handle that reads readiness from an injected map.
   - Keep all test harness pieces local to the new module (or a sibling `mod tests { ... }`) so they can be deleted later.
4) Unit tests should be table-driven:
   - input: history + orchestrator behavior
   - output: `actions_to_take`, terminal result, and nondeterminism errors.

Example tests:
- schedule mismatch => nondeterminism
- completion without schedule => nondeterminism
- end-of-history => returns new actions
- FIFO completion plugging drives action emission predictably

### Phase 2: “Bring the hammer down” (swap schedule semantics + swap engine)
Goal: integrate the model end-to-end and **remove all legacy replay code**.

#### Step 1: Port all tests to simplified_* APIs
- Convert all existing e2e/integration tests to use `simplified_schedule_*()` methods
- Run full test suite with `use_simplified_replay: true`
- Skip tests that have hard blockers and document them

#### Step 2: Remove legacy replay infrastructure
The following must be **completely removed** (no backward compatibility):

**Legacy APIs to remove from `src/lib.rs`:**
- `DurableFuture` struct and all methods (`into_activity()`, `into_timer()`, `into_event()`, `into_sub_orchestration()`)
- `schedule_activity()`, `schedule_timer()`, `schedule_wait()`, `schedule_sub_orchestration()` (non-simplified versions)
- `run_turn()`, `run_turn_with()`, `run_turn_with_status()`, `run_turn_with_status_and_cancellations()`
- `poll_once()` function
- `TurnResult` type (legacy variant)
- Legacy fields in `CtxInner`: `history`, `next_event_id`, `claimed_scheduling_events`, `cursor`, `pending_external_events`, etc.
- `ReplayMode` enum (becomes unnecessary when only one mode exists)

**Legacy code in `src/futures.rs`:**
- Entire `DurableFuture` `Future` implementation with history scanning/cursor logic
- `AggregateDurableFuture`, `SelectFuture`, `JoinFuture` (legacy versions)
- All cursor-based replay logic

**Legacy code in `src/runtime/replay_engine.rs`:**
- `execute_orchestration()` legacy mode branch
- All cursor/history-scanning logic
- `use_simplified_mode` flag (becomes default/only mode)

**Test harnesses to remove:**
- `src/runtime/replay_engine_simplified.rs` (test-only harness, no longer needed)

#### Step 3: Rename simplified_* to become THE API
- `simplified_schedule_activity()` → `schedule_activity()`
- `simplified_schedule_timer()` → `schedule_timer()`
- `simplified_schedule_wait()` → `schedule_wait()`
- `simplified_schedule_sub_orchestration()` → `schedule_sub_orchestration()`
- `simplified_schedule_sub_orchestration_with_id()` → `schedule_sub_orchestration_with_id()`
- `simplified_schedule_orchestration()` → `schedule_orchestration()`
- `simplified_join()`, `simplified_join2()`, `simplified_join3()` → `join()`, `join2()`, `join3()`
- `simplified_select2()`, `simplified_select3()` → `select2()`, `select3()`
- `trace_*_simplified()` → `trace_*()` (or integrate into existing trace API)
- Remove `use_simplified_replay` from `RuntimeOptions` (always on)

#### Step 4: Update documentation
- Update `docs/ORCHESTRATION-GUIDE.md` with new API
- Update all examples in `examples/`
- Update `docs/durable-futures-internals.md` to reflect new architecture
- Update `docs/replay-engine.md` if exists

#### Migration report
At the end of Phase 2, generate a report documenting:
- Tests successfully ported
- Tests skipped with reasons
- Tests disabled (no longer relevant)
- Any semantic changes or behavior differences discovered

## Open questions
- External events: require prior `ExternalSubscribed` or allow “deliver by name” without subscription?
- Terminal consistency: should we require workflow terminal to match terminal events in history, or is “history authoritative” sufficient initially?
- SystemCall: confirm how user-facing API emits/awaits system calls so replay can match and deliver values.
- Combinators: confirm the exact surface API for workflow-safe `select`/`join` over `async { ... }` blocks and the operand ordering rules required for determinism.
- Dehydration guard: define exact flag lifetime and where it lives (likely on `CtxInner`), ensuring it is set before orchestration future drop on suspension and cleared before polling on the next evaluation.
