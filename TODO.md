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
   - Negative tests: mismatched event order should panic during replay.

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
