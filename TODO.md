## TODO: Durable Task Rust Core – Current Context and Next Steps

### Current context
- Minimal deterministic orchestration core in `src/lib.rs`:
  - Public types: `Event`, `Action`, `OrchestrationContext`, `run_turn`, `Executor::drive_to_completion`.
  - Execution model: single-threaded polling with a no-op `Waker`; logical time advances on `TimerFired` events; GUIDs are deterministic per instance.
  - Futures for orchestration primitives:
    - `ctx.call_activity(name, input)` emits `Action::CallActivity` or consumes matching `Event::ActivityResult` during replay.
    - `ctx.timer(delay_ms)` emits `Action::CreateTimer` or consumes `Event::TimerFired`.
    - `ctx.wait_external(name)` emits `Action::WaitExternal` or consumes `Event::ExternalEvent`.
  - Each primitive uses an internal `scheduled: Cell<bool>` to ensure the action is recorded once per future instance, even if polled multiple times before host runs.
- Sample orchestrator in `src/main.rs`:
  - Calls A, waits a timer, waits external event "Go", then calls B; result is formatted string.
  - `host_execute_actions` simulates a host by appending matching `Event`s to history.
  - Run once to completion, then replay to verify determinism (no new actions, identical output).

### Decisions and constraints
- Focus scope: implement an in-memory provider and an Azure Storage-backed provider; skip tracing/observability for now.
- Keep deterministic replay as the primary invariant; host/provider must only append to history, never mutate existing entries.

### Proposed next steps (prioritized)
0. Refactor to correlated event IDs and buffered completions (DTF-style)
   - Introduce stable correlation IDs for actions/events:
     - Activity: TaskScheduled(id) ↔ TaskCompleted(id)
     - Timer: TimerCreated(id) ↔ TimerFired(id)
     - External: ExternalSubscribed(name, id?) ↔ ExternalRaised(name, seq or id)
   - Context should buffer completions by correlation rather than consuming strictly from the head of history.
   - Futures resolve by matching their correlation, allowing multiple results (from races) to exist without replay corruption.
   - Define deterministic tie-breaking for WhenAny-like races (match history order); leave losing contenders buffered for later awaits.
   - Update unified futures and `run_turn` to use the buffer; keep single-poll-per-turn semantics deterministic [[race refactor]].
   - Convert existing tests; keep `complex_control_flow_orchestration` as a guard against the current failure mode.
1. Extract provider-facing traits
   - Define minimal traits for a host/provider boundary (names suggestive):
     - `HistoryStore` (append-only per instance; read by cursor/index),
     - `TimerScheduler` (persist timers as events to fire at logical time),
     - `ExternalEventRouter` (enqueue external events),
     - Optionally a single `Provider` that exposes these capabilities.
   - Include an `InstanceId` concept and plumb it through `OrchestrationContext`.

2. Implement an in-memory provider
   - Thread-safe append-only history per `InstanceId` (e.g., `Arc<Mutex<HashMap<InstanceId, Vec<Event>>>>`).
   - Deterministic host loop that:
     - Calls `run_turn` with the current instance history,
     - Executes returned `Action`s to append new `Event`s,
     - Repeats until `run_turn` returns `Some(output)`.

3. Refactor `src/main.rs` host
   - Replace `host_execute_actions` with the in-memory provider implementation and a reusable `run_to_completion(instance_id, orchestrator)` helper.

4. Introduce public instance-level API
   - `start_new(orchestrator, input) -> InstanceId`.
   - `raise_event(instance_id, name, data)`.
   - `get_status(instance_id) -> { history_len, last_event_time, output? }`.

5. Tests to lock in replay determinism
   - Activity -> Timer -> ExternalEvent scenario (current sample) via the provider API.
   - Multiple activities and timers, parallel fan-out/fan-in, event ordering edge cases.
   - Negative tests: mismatched correlation should panic during replay; verify buffered race losers don’t corrupt subsequent awaits.

6. Azure Storage provider scaffold
   - Research available Rust crates for Azure Storage (Queues/Tables/Blobs) and viability for Durable semantics.
   - Define a minimal storage schema compatible with append-only history (Tables for history, Queues for work dispatch, or a simplified single store to start).
   - Implement a no-op or stubbed provider behind a feature flag; iterate to real I/O afterward.

7. Repository structure and docs
   - Add a `providers/` module with `in_memory` and `azure_storage` submodules.
   - Expand `README.md` with usage examples and invariants.

### Nice-to-haves / tech debt
- Proper executor integration (wake and re-poll strategy) beyond `poll_once` loop.
- Cancellation and timeouts; activity failure semantics and retries.
- Serialization via `serde` for activity inputs/outputs and events.
- Concurrency controls for fan-out/fan-in workflows.
- Logging (optional; avoid tracing until providers are done).
- Packaging and CI (fmt, clippy, tests).

### Quick run check (today)
- `cargo run` should print a completed result and a successful deterministic replay.

### Open question: schedule_* futures vs ergonomic wrappers
- Summary: We kept both the composable primitives (`schedule_activity`, `schedule_timer`, `schedule_wait`) which return a unified `DurableFuture -> DurableOutput`, and the ergonomic wrappers (`call_activity`, `timer`, `wait_external`) that map directly to concrete outputs (`String`/`()`/`String`).
- Why both:
  - Composability: `schedule_*` enables racing/combining with `select`, `join`, `join3`, etc., since they share a single future/output type.
  - Ergonomics: wrappers avoid repetitive `match` boilerplate when awaiting a single primitive sequentially.
- What to understand deeper:
  - Whether we can design combinators (e.g., `when_any`, `when_all`) that preserve arrival-order deterministically without relying on poll-order bias of `select`.
  - Tradeoffs of typed `DurableOutput` vs forcing a uniform `String` output. Current choice favors type clarity and safety over superficial uniformity.
  - Clear guidance on when to use `schedule_*` (races/fan-out) vs wrappers (simple awaits) to minimize code duplication while staying explicit.
  - Impact on replay determinism: ensure combinators pick earliest completion by history order when needed; wrappers should remain simple pass-throughs.
  - API surface: if we ever drop wrappers, show concise helper patterns to extract `DurableOutput` without noise; otherwise keep both paths documented.
