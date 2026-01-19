# Replay Simplification – Progress Report

**Date:** 2026-01-19

## Spec

- Original proposal/spec: [proposals/replay-simplification.md](replay-simplification.md)

## Where we are

Phase 1 (“validate the model in isolation”) is implemented as a **test-only harness** that exercises the commands-vs-history replay contract without touching production scheduling/replay behavior.

- Harness module: [src/runtime/replay_engine_simplified.rs](../src/runtime/replay_engine_simplified.rs)
- Wired under `#[cfg(test)]` via runtime mod so it does not affect prod execution.

### What Phase 1 now covers

**1) Commands vs schedule-events determinism (FIFO)**

- Orchestrator calls `schedule_*()` APIs which **emit actions**.
- When history contains a schedule event (e.g. `ActivityScheduled`, `TimerCreated`, …), the harness:
  - consumes the next emitted action,
  - checks the payload matches,
  - binds the emitted “token” to the schedule `event_id`.

**2) Completion delivery via `source_event_id` + progress-gated polling**

- Completion events (`ActivityCompleted/Failed`, `TimerFired`, `SubOrchestrationCompleted/Failed`) are plugged into an open map keyed by the schedule id.
- The orchestrator future is polled only when there’s a chance to make progress (initial poll, and after delivering a completion/external/systemcall).

**3) Futures (not handles)**

- The harness `schedule_*()` calls return explicit types that `impl Future`:
  - activity/sub-orch: `Future<Output = Result<String, String>>`
  - timer: `Future<Output = ()>`
  - external: `Future<Output = String>`
  - system call: `Future<Output = String>`

This matches the “expected a Future” ergonomics needed for Phase 2, and makes the harness useful for validating `async` composition.

**4) External events deliver to the first subscription (by name) deterministically**

- Each `ExternalSubscribed { name }` is assigned a stable per-name `subscription_index` in history order.
- Each `ExternalEvent { name, data }` appends to an arrival list.
- An external future resolves when `arrivals[name][subscription_index]` exists.

This guarantees “deliver to the first external subscription for this name” and prevents poll order from stealing earlier deliveries.

**5) Completion-kind validation (type safety at replay-level)**

- The harness records `schedule_id -> schedule_kind` when schedule events are matched.
- If a completion is applied to a schedule id of the wrong type (e.g. `TimerFired` for an `ActivityScheduled`), the harness returns nondeterminism.

**6) Terminal-history behavior aligned to current runtime dispatcher**

The current orchestration dispatcher treats these as **terminal histories** and acks without invoking user code:

- `OrchestrationCompleted`
- `OrchestrationFailed`
- `OrchestrationContinuedAsNew` (under the dispatcher’s “terminal without CAN start message” rule)

To match that, the harness short-circuits at the start of evaluation if any of those events are present. The in-loop match arm is explicitly `unreachable!()`.

Important note: this differs from the proposal’s event-handling table, which originally labeled `OrchestrationCancelRequested` as “terminal (forced)”. In the current runtime implementation, `OrchestrationCancelRequested` is **not treated as terminal by the dispatcher**; it continues processing and cancellation is surfaced via `TurnResult::Cancelled`.

## What’s been validated via unit tests

Phase 1 includes unit tests that assert:

- schedule mismatch ⇒ nondeterminism error
- completion without schedule ⇒ nondeterminism error
- completion kind mismatch (activity vs timer vs sub-orch) ⇒ nondeterminism error
- end-of-history returns newly emitted actions
- external events deliver in subscription order (not poll order)
- `OrchestrationCancelRequested` is not treated as terminal in the harness

Run locally (targeted):

- `cargo test replay_engine_simplified --lib`

## Not implemented yet (planned Phase 2+)

## Phase 2 prerequisite: production `ReplayEngine` interface parity (swappable)

Before Phase 2 starts, the simplified implementation needs to be **drop-in swappable** behind the exact production replay engine interface used by the orchestration dispatcher today. That means mirroring both the **API surface** and the key **behavioral contracts**.

### Current production interface (must be preserved)

Defined in [src/runtime/replay_engine.rs](../src/runtime/replay_engine.rs):

**Types**

- `pub enum TurnResult`:
  - `Continue`
  - `Completed(String)`
  - `Failed(ErrorDetails)`
  - `ContinueAsNew { input: String, version: Option<String> }`
  - `Cancelled(String)`

- `pub struct ReplayEngine` with internal state:
  - instance/execution metadata
  - `baseline_history`, `history_delta`
  - `pending_actions: Vec<Action>`
  - `cancelled_activity_ids: Vec<u64>`
  - `abort_error: Option<ErrorDetails>`

**Constructor / stages**

- `ReplayEngine::new(instance: String, execution_id: u64, baseline_history: Vec<Event>) -> ReplayEngine`

- Stage 1 (message → completion events):
  - `prep_completions(&mut self, messages: Vec<WorkItem>)`

- Stage 2 (run one orchestration turn):
  - `execute_orchestration(&mut self,
      handler: Arc<dyn OrchestrationHandler>,
      input: String,
      orchestration_name: String,
      orchestration_version: String,
      worker_id: &str,
    ) -> TurnResult`

**Getters used by the dispatcher / atomic commit path**

- `history_delta(&self) -> &[Event]`
- `pending_actions(&self) -> &[Action]`
- `cancelled_activity_ids(&self) -> &[u64]`
- `made_progress(&self) -> bool`
- `final_history(&self) -> Vec<Event)`

### Behavioral contracts to match (not just signatures)

The swap must preserve the following runtime semantics (as implemented today):

**A) Two-stage evaluation contract**

- `prep_completions()` is responsible for converting `WorkItem` messages into history events in `history_delta`, including:
  - execution_id filtering for completions
  - duplicate detection (baseline and staged-delta)
  - completion-kind mismatch checks (activity vs timer vs sub-orch)
  - special casing:
    - external raised is only materialized if a matching `ExternalSubscribed` exists
    - cancellation request is materialized as `OrchestrationCancelRequested` only when not already terminal and not already cancelled
  - setting `abort_error` for system-level failures / detected nondeterminism

**B) Turn execution contract**

- `execute_orchestration()` must:
  - fail fast if `abort_error` is set
  - build working history as `baseline_history + history_delta`
  - invoke orchestration via `run_turn_with_status_and_cancellations(...)`
  - translate nondeterminism flags/panics into `TurnResult::Failed(Configuration::Nondeterminism)`
  - append *new events* emitted by the orchestration into `history_delta`
  - set `pending_actions` from orchestration decisions
  - set `cancelled_activity_ids`
  - enforce decision precedence:
    - `OrchestrationCancelRequested` in history ⇒ `TurnResult::Cancelled(reason)`
    - `Action::ContinueAsNew` in decisions ⇒ `TurnResult::ContinueAsNew { .. }`
    - user output ⇒ `Completed` / `Failed(Application)`
    - otherwise ⇒ `Continue`

**C) Dispatcher expectations**

- The orchestration dispatcher already treats terminal histories as “ack without processing”, so the swappable engine must remain compatible with that flow (i.e. dispatcher can keep using the same `ReplayEngine` calls only for non-terminal cases).

### What it takes to make the simplified engine truly swappable

Phase 1’s harness (`evaluate_simplified(...)`) is intentionally minimal and does not yet model the full production replay-engine state machine. To become swappable:

1) Introduce an internal implementation boundary (preferred):
   - keep `pub struct ReplayEngine` + `TurnResult` + public methods unchanged
   - move current logic into `impl ReplayEngineImpl` (or similar)
   - add a new impl `CommandsVsHistoryImpl` and select between them (feature flag, cfg, or internal toggle)

2) Ensure the new impl supports the same two-stage workflow:
   - Stage 1: `WorkItem` → `history_delta` + `abort_error` (same filtering/dup rules)
   - Stage 2: `baseline_history + history_delta` + orchestrator invocation → `history_delta` + `pending_actions` + `TurnResult`

3) Maintain all output shaping used by atomic commit:
   - `history_delta()` must contain exactly the events that must be persisted this turn
   - `pending_actions()` must contain the work to enqueue
   - `cancelled_activity_ids()` must remain compatible with provider lock-stealing cancellation

This prerequisite is complete when the simplified implementation can be swapped in without changing the orchestration dispatcher’s call sites.

### Phase 2: Swap the real replay engine internals

- Implement the commands-vs-history replay loop inside the production replay engine (`src/runtime/replay_engine.rs`) while keeping the external boundary stable.
- Ensure `TurnResult` behavior matches current semantics:
  - `OrchestrationCancelRequested` ⇒ `TurnResult::Cancelled(reason)`
  - `ContinueAsNew` handling should match dispatcher rules (including the “CAN without start message” terminal shortcut).

### Dehydration guard (required by spec)

- Add a runtime-level “dehydrating” guard so suspension-driven drops do not trigger physical cancellation.
- Update durable future `Drop` cancellation paths to consult that flag.

### Deterministic combinators

- Implement/validate workflow-safe combinators (`ctx.join`, `ctx.select*`) under the new model.
- Confirm that replay does not rely on loser-drop behavior for progress.

### Coverage expansion

- Extend tests to cover more realistic orchestration patterns (scenario tests) that use:
  - external events + multiple subscriptions
  - continue-as-new chains
  - cancellation paths
  - single-threaded runtime constraints

## Next steps (recommended order)

1) Add a small “parity matrix” doc section comparing proposal semantics vs current runtime semantics, especially around cancellation and terminal histories.
2) Start Phase 2 implementation in the production replay engine behind a feature flag or internal toggle (if helpful), and port Phase-1 tests into `src/runtime/replay_engine_tests.rs`-style coverage.
3) Implement dehydration guard and verify cancellation tests in `tests/scenarios/single_thread.rs` still pass.
4) Run provider validation tests (and/or `cargo nt`) once the production engine behavior changes.
---

## Phase 2 Implementation Plan: Modal Switch Approach

**Checkpoint commit:** `555c5751653ac770a94d3962f743b7a2d7f6ad58`

> **Note:** We are going to try this path out. If it gets too complex or creates too much churn, we can revert to the checkpoint commit, change approach, and try again.

### Why a modal switch?

The Phase 1 harness validated the commands-vs-history replay logic, but it uses its own `SimCtx` and `SimFuture` types. Production uses `OrchestrationContext` and `DurableFuture`, which work differently:

| Aspect | Current (Legacy) Model | Simplified Model |
|--------|------------------------|------------------|
| `schedule_*()` | Mutates `ctx.history` (appends schedule event) | Emits action to buffer (no history mutation) |
| `DurableFuture::poll()` | Scans history for completion event | Checks results map for delivered completion |
| Replay engine | `run_turn_with_status_and_cancellations` | Event-processing loop (validated in Phase 1) |

Swapping `execute_orchestration()` alone is **not sufficient** — the orchestration context and durable futures must also participate in the new model.

### Approach: Add a replay mode to `CtxInner`

```rust
enum ReplayMode {
    Legacy,      // schedule_*() mutates history, futures scan history
    Simplified,  // schedule_*() emits actions, futures check results map
}
```

### Implementation steps

1. **Add mode and emit buffer to `CtxInner`**
   - New field: `replay_mode: ReplayMode`
   - New field: `emitted_actions: Vec<Action>` (or similar buffer)
   - New field: `results: HashMap<u64, CompletionResult>` (for delivering completions)

2. **Make `schedule_*()` mode-aware**
   - When `Legacy`: current behavior (append schedule event to history)
   - When `Simplified`: emit action to buffer, return token-based future

3. **Make `DurableFuture::poll()` mode-aware**
   - When `Legacy`: current behavior (scan history)
   - When `Simplified`: check results map

4. **Swap `execute_orchestration()` internals**
   - When `Simplified`: use the event-processing loop from Phase 1 harness
   - Validate emitted actions against history schedule events
   - Plug completions into results map and re-poll

5. **Feature flag or runtime toggle**
   - Allow legacy mode by default
   - Enable simplified mode for testing/gradual rollout

### What stays the same (API boundary)

The caller in `execution.rs` continues to use:
- `ReplayEngine::new(instance, execution_id, baseline_history)`
- `turn.prep_completions(messages)`
- `turn.execute_orchestration(handler, input, ...)`
- `turn.history_delta()`, `turn.pending_actions()`, `turn.cancelled_activity_ids()`

These do not change. Only the internal machinery changes.

### Risk mitigation

- Feature flag allows fallback to legacy mode
- Checkpoint commit allows full revert if approach is too invasive
- Incremental testing: enable simplified mode in specific test files first

---
## Change history (recent commits)

- Phase 1 harness added (test-only)
- Futures returned by `schedule_*()` in the harness (not handles)
- Deterministic external delivery to earliest subscription
- Completion-kind validation
- Terminal-history behavior aligned to dispatcher; terminal match arm marked `unreachable!`
- Copilot instructions updated to prefer `cargo nt` when available
