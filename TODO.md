## Durable Task Rust Core – Status and Next Steps

### Current state (green)
- Deterministic orchestration core (`src/lib.rs`)
  - Correlated history with stable IDs: `ActivityScheduled/Completed`, `TimerCreated/Fired`, `ExternalSubscribed/Event`.
  - Unified `DurableFuture` with composable `schedule_*` APIs and typed adapters: `into_activity()`, `into_timer()`, `into_event()`.
  - Single-poll-per-turn determinism preserved; replay consumes by correlation from history (not head-of-queue).

- Message-driven runtime (`src/runtime/`)
  - `Runtime` with long-lived Tokio worker pools (activity, timer) and a completion router.
  - `ActivityRegistry` to register async activity handlers; `ActivityWorker` dispatches by name.
  - Real-time timers (sleep for requested delay) and `raise_event(instance, name, data)` API.
  - Per-instance inbox; messages to unknown instances are ignored with a warning.
  - Instance execution: `spawn_instance_to_completion` (async task), `run_instance_to_completion` (direct), `drain_instances`, `shutdown`.

- Tests (10/10, fast; real-time delays are a few ms)
  - Deterministic end-to-end orchestration and replay.
  - Races (activity vs timer vs external), staggered completions across turns.
  - Sequential fan-in chain, action-order determinism in first turn.
  - External-only orchestration; event-vs-timer ordering (both ways).
  - External sent before instance start is dropped (ignored) and later event succeeds.

### What we’ve accomplished
- Refactored to DTF-style correlated events and buffered resolution; removed head-of-queue assumptions.
- Simplified API while keeping composability and ergonomics (`schedule_*` + `into_*`).
- Split host into a message-based runtime; workers run independently of instance turns.
- Added spawn/drain lifecycle for running multiple instances concurrently.
- Converted tests to the runtime; added race/order/ignore-before-start cases.

### Proposed next steps (prioritized)
1) Provider model and persistence
   - Define a provider trait set to abstract storage and routing:
     - `HistoryStore` (append/read by instance, durable)
     - `TimerScheduler` (durable, wakes by wall-clock)
     - `ExternalEventRouter` (enqueue/signal to instances)
     - Optional `WorkQueue` abstraction for activities
   - Plug the runtime against these traits; keep the current in-memory implementation as a baseline.

2) File-system provider
   - Simple, append-only per-instance history files with fsync; directory-per-instance for events and work.
   - Basic crash recovery: on restart, read history and resume turns; ensure id adoption remains deterministic.

3) Azure Blob (or Table/Queue) provider
   - Minimal schema for append-only history and durable timers.
   - Map activities to a queue; completions flow back as events.
   - Keep it behind a feature flag initially.

4) Determinism helpers
   - GUID and logical-time determinism utilities to ensure reproducible values under replay.
   - Make available in `OrchestrationContext` for user orchestrations (e.g., `ctx.new_guid()`, `ctx.now_ms()`).

5) Crash recovery tests
   - Kill between scheduling and completion; on resume, ensure no duplicate scheduling and deterministic convergence.
   - Corrupted/out-of-order histories: detect and fail fast or recover by correlation.

6) Observability
   - Proper logging (structured) across core and runtime.
   - Metrics for turns, queue depth, action counts, latency, and failure rates.

7) Visualization
   - Small website to visualize instance history and state transitions (timeline of events, per-turn view).
   - Live view for in-memory provider; pluggable backends later.

8) API polish and docs
   - Public `Runtime`/`ActivityRegistry` docs and examples.
   - Guidance on schedule_* vs into_* usage (races vs sequential awaits).
   - Error handling and cancellation model (timeouts, activity failures, retries).

9) Error handling and tests
   - Activities throw errors, control flow works in orchestrations to handle these

### Explicit TODOs
- Add proper logging.
- Add proper metrics.
- Build a website to visualize execution.
- Formalize a provider model for the state, queues and timers.
- Write a file system based provider.
- Write an Azure Blob based provider.
- Write GUID and time deterministic helper methods.
- Crash recovery tests.


