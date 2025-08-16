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
- Message-driven runtime built on Tokio (activity worker pool, timer worker, external event router)
- Storage-agnostic via `HistoryStore` (in-memory, filesystem today)

How it works (brief)
- The orchestrator runs turn-by-turn. Each turn it is polled once, may schedule actions, then the host blocks waiting for completions.
- Every operation has a correlation id. Scheduling is recorded as history events (e.g., `ActivityScheduled`), and completions are matched by id (e.g., `ActivityCompleted`).
- The runtime dispatches actions to workers over channels. Workers send completions back; the runtime appends them to history and starts the next turn.
- Logging is replay-safe by treating it as a system activity via `ctx.trace_*` helpers. Logs are emitted on completion and also persisted as `TraceEmitted` events.
- Providers enforce a history cap (1024 here). If an append would exceed the cap, they return an error; the runtime fails the run to preserve determinism (no truncation).

Key types
- `OrchestrationContext`: schedules work (`schedule_activity`, `schedule_timer`, `schedule_wait`) and exposes `trace_info/warn/error/debug`.
- `DurableFuture`: returned by `schedule_*`; use `into_activity()`, `into_timer()`, `into_event()` for typed awaits.
- `Event`/`Action`: immutable history entries and host-side actions.
- `HistoryStore`: persistence abstraction (`InMemoryHistoryStore`, `FsHistoryStore`).

Project layout
- `src/lib.rs` — orchestration primitives and single-turn executor
- `src/runtime/` — runtime, activity registry/worker, timer worker
- `src/providers/` — in-memory and filesystem history stores
- `tests/` — unit and e2e tests

Install (when published)
```toml
[dependencies]
rust-dtf = "0.1"
```

Hello world (activities + runtime)
```rust
use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{Runtime, activity::ActivityRegistry};

async fn hello_orch(ctx: OrchestrationContext) -> String {
    ctx.trace_info("hello started");
    let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
    ctx.trace_info(format!("hello result={res}"));
    res
}

# async fn run() {
let registry = ActivityRegistry::builder()
    .register_result("Hello", |name: String| async move { Ok(format!("Hello, {name}!")) })
    .build();

let rt = Runtime::start(Arc::new(registry)).await;
let h = rt.clone().spawn_instance_to_completion("inst-hello-1", hello_orch).await;
let (_history, output) = h.await.unwrap();
assert_eq!(output, "Hello, Rust!");
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

Local development
- Build: `cargo build`
- Test everything: `cargo test --all -- --nocapture`
- Run a specific test: `cargo test --test e2e_samples dtf_legacy_gabbar_greetings_fs -- --nocapture`

Notes
- Import as `rust_dtf` in Rust source.
- Timers are real time (Tokio sleep). External events are via `Runtime::raise_event`.
- Unknown-instance messages are logged and dropped. Providers persist history only (queues are in-memory runtime components).

Provenance
- This codebase was generated entirely by AI with guidance from a human
