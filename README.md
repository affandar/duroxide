## rust-dtf

Deterministic task orchestration in Rust, inspired by Durable Task.

What you can build with this (inspired by .NET Durable Task/Durable Functions patterns)
- Function chaining: model a multi-step process as sequential awaits where each step depends on prior results.
- Fan-out/fan-in: schedule many activities in parallel and deterministically aggregate results.
- Human interaction (external events): wait for out-of-band approvals, callbacks, or webhooks and then resume.
- Durable timers and deadlines: sleep for minutes/hours/days without holding threads; resume exactly-once after timeouts.
- Saga-style compensation: on failure, run compensating actions to roll back prior steps.

These scenarios mirror the officially documented Durable Task/Durable Functions application patterns and are enabled here by deterministic replay, correlation IDs, durable timers, and external event handling.

Getting started samples
- See `tests/e2e_samples.rs` for end-to-end usage patterns (hello world, control flow, loops, error handling, and system activities). It's the best starting point to learn the API by example.

What it is
- Deterministic orchestration core with correlated event IDs and replay safety
- Message-driven runtime built on Tokio: activity worker pool, timer worker, external event router, and a provider-backed work queue
- Storage-agnostic via `HistoryStore` (in-memory and filesystem providers today)

How it works (brief)
- The orchestrator runs turn-by-turn. Each turn it is polled once, may schedule actions, then the runtime waits for completions.
- Every operation has a correlation id. Scheduling is recorded as history events (e.g., `ActivityScheduled`) and completions are matched by id (e.g., `ActivityCompleted`).
- The runtime dispatches actions to workers over channels and consumes a provider-backed work queue (`WorkItem`) for completions and external events. It appends new events to history and advances the next turn.
- Logging is replay-safe via `tracing` and the `ctx.trace_*` helpers (implemented as a deterministic system activity). We do not persist trace events in history.
- Providers enforce a history cap (default 1024; tests use a smaller cap). If an append would exceed the cap, they return an error; the runtime fails the run to preserve determinism (no truncation).

Key types
- `OrchestrationContext`: schedules work (`schedule_activity`, `schedule_timer`, `schedule_wait`, `schedule_sub_orchestration`, `schedule_orchestration`) and exposes `trace_*`, `continue_as_new`.
- `DurableFuture`: returned by `schedule_*`; use `into_activity()`, `into_timer()`, `into_event()`, `into_sub_orchestration()` (and `_typed` variants) to await.
- `Event`/`Action`: immutable history entries and host-side actions, including `ContinueAsNew`.
- `HistoryStore`: persistence abstraction (`InMemoryHistoryStore`, `FsHistoryStore`).
- `OrchestrationRegistry` / `ActivityRegistry`: register orchestrations/activities in-memory.

Project layout
- `src/lib.rs` — orchestration primitives and single-turn executor
- `src/runtime/` — runtime, registries, workers, and polling engine
- `src/providers/` — in-memory and filesystem history stores
- `tests/` — unit and e2e tests (see `e2e_samples.rs` to learn by example)

Install (when published)
```toml
[dependencies]
rust-dtf = "0.1"
```

Hello world (activities + runtime)
```rust
use std::sync::Arc;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
use rust_dtf::runtime::{self};
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::providers::fs::FsHistoryStore;

# #[tokio::main]
# async fn main() {
let store = std::sync::Arc::new(FsHistoryStore::new("./data", true));
let activities = ActivityRegistry::builder()
    .register("Hello", |name: String| async move { Ok(format!("Hello, {name}!")) })
    .build();
let orch = |ctx: OrchestrationContext, name: String| async move {
    ctx.trace_info("hello started");
    let res = ctx.schedule_activity("Hello", name).into_activity().await.unwrap();
    Ok::<_, String>(res)
};
let orchestrations = OrchestrationRegistry::builder().register("HelloWorld", orch).build();
let rt = runtime::Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;
let h = rt.clone().start_orchestration("inst-hello-1", "HelloWorld", "Rust").await.unwrap();
let (_history, output) = h.await.unwrap();
assert_eq!(output.unwrap(), "Hello, Rust!");
rt.shutdown().await;
# }
```

Parallel fan-out (DTF-style greetings)
```rust
use futures::future::join;
async fn fanout(ctx: OrchestrationContext) -> Vec<String> {
    let g1 = ctx.schedule_activity("Greetings", "Gabbar").into_activity();
    let g2 = ctx.schedule_activity("Greetings", "Samba").into_activity();
    let (r1, r2) = join(g1, g2).await;
    vec![r1.unwrap(), r2.unwrap()]
}
```

Control flow + timers + externals
```rust
use futures::future::select;
use rust_dtf::DurableOutput;
async fn control(ctx: OrchestrationContext) -> String {
    let race = select(ctx.schedule_timer(10).into_timer(), ctx.schedule_wait("Evt").into_event());
    match race.await {
        // event wins
        Either::Right((data, _left_rest)) => data,
        // timer wins then wait for event
        Either::Left((_timer, right_rest)) => right_rest.await,
    }
}
```

Error handling (Result<String, String>)
```rust
async fn comp_sample(ctx: OrchestrationContext) -> String {
    match ctx.schedule_activity("Fragile", "bad").into_activity().await {
        Ok(v) => v,
        Err(e) => {
            ctx.trace_warn(format!("fragile failed error={e}"));
            ctx.schedule_activity("Recover", "").into_activity().await.unwrap()
        }
    }
}
```

ContinueAsNew and multi-execution
- Use `ctx.continue_as_new(new_input)` to end the current execution and immediately start a fresh execution with the provided input.
- Providers keep all executions’ histories (e.g., filesystem stores `instance/{execution_id}.jsonl`).
- The initial `start_orchestration` handle resolves with an empty success when `ContinueAsNew` occurs; the latest execution can be observed via status APIs.

Status and control-plane
- `Runtime::get_orchestration_status(instance)` -> Running | Completed { output } | Failed { error } | NotFound
- Filesystem provider exposes execution-aware methods (`list_executions`, `read_with_execution`, etc.) for diagnostics.

Local development
- Build: `cargo build`
- Test everything: `cargo test --all -- --nocapture`
- Run a specific test: `cargo test --test e2e_samples dtf_legacy_gabbar_greetings_fs -- --nocapture`

Notes
- Import as `rust_dtf` in Rust source.
- Timers are real time (Tokio sleep). External events are via `Runtime::raise_event`.
- Unknown-instance messages are logged and dropped. Providers persist history only (queues are in-memory runtime components).
- Logging is replay-safe by treating it as a system activity via `ctx.trace_*` helpers; logs are emitted through tracing at completion time (not persisted as history events).

Provenance
- This codebase was generated entirely by AI with guidance from a human
