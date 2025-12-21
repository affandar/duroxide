![Banner](duroxide_banner.jpg)
## duroxide

[![Crates.io](https://img.shields.io/crates/v/duroxide.svg)](https://crates.io/crates/duroxide)
[![Documentation](https://docs.rs/duroxide/badge.svg)](https://docs.rs/duroxide)

> Notice: Experimental, not intended for production use yet

Deterministic task orchestration in Rust.

> **[Latest Release: v0.1.5](https://crates.io/crates/duroxide/0.1.5)** — Worker queue visibility control, reduced default long poll timeout.
> See [CHANGELOG.md](CHANGELOG.md#0.1.5---2025-12-18) for release notes.

What you can build with this
- Function chaining: model a multi-step process as sequential awaits where each step depends on prior results.
- Fan-out/fan-in: schedule many activities in parallel and deterministically aggregate results.
- Human interaction (external events): wait for out-of-band approvals, callbacks, or webhooks and then resume.
- Durable timers and deadlines: sleep for minutes/hours/days without holding threads; resume exactly-once after timeouts. **Use `ctx.schedule_timer()` for orchestration-level delays. Activities can sleep/poll internally as part of their work (e.g., provisioning resources).**
- Saga-style compensation: on failure, run compensating actions to roll back prior steps.
- Built-in activity retry: `ctx.schedule_activity_with_retry()` with configurable backoff (exponential, linear, fixed) and per-attempt timeouts.

These patterns are enabled by deterministic replay, correlation IDs, durable timers, and external event handling.

Getting started samples
- **Start here**: See `docs/ORCHESTRATION-GUIDE.md` for complete guide to writing workflows
- **Quick start**: Run `cargo run --example hello_world` to see Duroxide in action
- **Advanced patterns**: Check `tests/e2e_samples.rs` for comprehensive usage patterns
- **Provider implementation**: See `docs/provider-implementation-guide.md` for building custom providers
- **Provider testing**: See `docs/provider-testing-guide.md` for testing custom providers
- **Observability**: See `docs/observability-guide.md` for structured logging and metrics

What it is
- Deterministic orchestration core with correlated event IDs and replay safety
- Message-driven runtime built on Tokio, with two dispatchers:
  - OrchestrationDispatcher (orchestrator queue) - processes workflow turns and durable timers
  - WorkDispatcher (worker queue) - executes activities with automatic lock renewal
- Storage-agnostic via `Provider` trait (SQLite provider with in-memory and file-based modes)
- Configurable polling frequency, lock timeouts, and automatic lock renewal for long-running activities

How it works (brief)
- The orchestrator runs turn-by-turn. Each turn it is polled once, may schedule actions, then the runtime waits for completions.
- Every operation has a correlation id. Scheduling is recorded as history events (e.g., `ActivityScheduled`) and completions are matched by id (e.g., `ActivityCompleted`).
- The runtime appends new events and advances turns; work is routed through provider-backed queues (Orchestrator and Worker) using peek-lock semantics. Timers are handled via orchestrator queue with delayed visibility.
- Deterministic future aggregation: `ctx.select2`, `ctx.select(Vec)`, and `ctx.join(Vec)` resolve by earliest completion index in history (not polling order).
- Logging is replay-safe via `tracing` and the `ctx.trace_*` helpers (implemented as a deterministic system activity). We do not persist trace events in history.
- Providers enforce a history cap (default 1024; tests use a smaller cap). If an append would exceed the cap, they return an error; the runtime fails the run to preserve determinism (no truncation).

Key types
- `OrchestrationContext`: schedules work (`schedule_activity`, `schedule_timer`, `schedule_wait`, `schedule_sub_orchestration`, `schedule_orchestration`) and exposes deterministic `select2/select/join`, `trace_*`, `continue_as_new`.
- `DurableFuture`: returned by `schedule_*`; use `into_activity()`, `into_timer()`, `into_event()`, `into_sub_orchestration()` (and `_typed` variants) to await.
- `Event`/`Action`: immutable history entries and host-side actions, including `ContinueAsNew`.
- `Provider`: persistence + queues abstraction with atomic operations and lock renewal (`SqliteProvider` with in-memory and file-based modes).
- `RuntimeOptions`: configure concurrency, lock timeouts, and lock renewal buffer for long-running activities.
- `OrchestrationRegistry` / `ActivityRegistry`: register orchestrations/activities in-memory.

Project layout
- `src/lib.rs` — orchestration primitives and single-turn executor
- `src/runtime/` — runtime, registries, workers, and polling engine
- `src/providers/` — SQLite history store with transactional support
- `tests/` — unit and e2e tests (see `e2e_samples.rs` to learn by example)

Install (when published)
```toml
[dependencies]
duroxide = { version = "0.1", features = ["sqlite"] }  # With bundled SQLite provider
# OR
duroxide = "0.1"  # Core only, bring your own Provider implementation
```

Hello world (activities + runtime)

> **Note**: This example uses the bundled SQLite provider. Enable the `sqlite` feature in your `Cargo.toml`.
```rust
use std::sync::Arc;
use duroxide::{ActivityContext, Client, ClientError, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use duroxide::runtime::{self};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::providers::sqlite::SqliteProvider;

# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let store = std::sync::Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
let activities = ActivityRegistry::builder()
    .register("Hello", |ctx: ActivityContext, name: String| async move { Ok(format!("Hello, {name}!")) })
    .build();
let orch = |ctx: OrchestrationContext, name: String| async move {
    ctx.trace_info("hello started");
    let res = ctx.schedule_activity("Hello", name).into_activity().await.unwrap();
    Ok::<_, String>(res)
};
let orchestrations = OrchestrationRegistry::builder().register("HelloWorld", orch).build();
let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
let client = Client::new(store);
client.start_orchestration("inst-hello-1", "HelloWorld", "Rust").await?; // Returns Result<(), ClientError>
match client.wait_for_orchestration("inst-hello-1", std::time::Duration::from_secs(5)).await? {
    OrchestrationStatus::Completed { output } => assert_eq!(output, "Hello, Rust!"),
    OrchestrationStatus::Failed { details } => panic!("Failed: {}", details.display_message()),
    _ => panic!("Unexpected status"),
}
rt.shutdown(None).await;  // Graceful shutdown with 1s timeout
# Ok(())
# }
```

Parallel fan-out
```rust
async fn fanout(ctx: OrchestrationContext) -> Vec<String> {
    let f1 = ctx.schedule_activity("Greetings", "Gabbar");
    let f2 = ctx.schedule_activity("Greetings", "Samba");
    let outs = ctx.join(vec![f1, f2]).await; // history-ordered join
    outs
        .into_iter()
        .map(|o| match o {
            duroxide::DurableOutput::Activity(Ok(s)) => s,
            other => panic!("unexpected: {:?}", other),
        })
        .collect()
}
```

Control flow + timers + externals
```rust
use duroxide::DurableOutput;
async fn control(ctx: OrchestrationContext) -> String {
    let a = ctx.schedule_timer(std::time::Duration::from_millis(10));
    let b = ctx.schedule_wait("Evt");
    let (_idx, out) = ctx.select2(a, b).await;
    match out {
        DurableOutput::External(data) => data,
        DurableOutput::Timer => {
            // timer won; fall back to waiting for the event deterministically
            ctx.schedule_wait("Evt").into_event().await
        }
        other => panic!("unexpected: {:?}", other),
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
- Providers keep all executions' histories in the SQLite database with full ACID transactional support.
- The initial `start_orchestration` handle resolves with an empty success when `ContinueAsNew` occurs; the latest execution can be observed via status APIs.

Status and control-plane
- `Client::get_orchestration_status(instance)` -> `Result<OrchestrationStatus, ClientError>` where `OrchestrationStatus` is Running | Completed { output } | Failed { details: ErrorDetails } | NotFound
- `Client::wait_for_orchestration(instance, timeout)` -> Wait for completion with timeout, returns `Result<OrchestrationStatus, ClientError>`
- SQLite provider exposes execution-aware methods (`list_executions`, `read_with_execution`, etc.) for diagnostics.

Error classification
- **Infrastructure** errors: Provider failures, data corruption (retryable by runtime, abort turn)
- **Configuration** errors: Unregistered orchestrations/activities, nondeterminism (require deployment fix, abort turn)
- **Application** errors: Business logic failures (handled by orchestration code, propagate normally)
- Use `ErrorDetails::category()` for metrics, `is_retryable()` for retry logic, `display_message()` for logging

Local development
- Build: `cargo build`
- Test everything: `cargo test --all -- --nocapture`
- Run a specific test: `cargo test --test e2e_samples sample_dtf_legacy_gabbar_greetings -- --nocapture`

Stress testing
- Run stress tests: `./run-stress-tests.sh`
- Run with result tracking: `./run-stress-tests.sh --track` (saves to `stress-test-results.md`)
- Tracked results include commit history, performance metrics, and rolling averages
- See `sqlite-stress/README.md` for details

Runtime Configuration
- Configure lock timeouts, concurrency, and polling via `RuntimeOptions`
- Worker lock renewal automatically enabled for long-running activities (no configuration needed)
- Example: `RuntimeOptions { worker_lock_timeout: Duration::from_secs(300), worker_lock_renewal_buffer: Duration::from_secs(30), ... }`
- Lock renewal happens at `(timeout - buffer)` for timeouts ≥15s, or `0.5 * timeout` for shorter timeouts
- See API docs for `RuntimeOptions` for complete configuration options

Observability
- Enable structured logging: `RuntimeOptions { observability: ObservabilityConfig { log_format: LogFormat::Compact, ... }, ... }`
- Default logs show only orchestration/activity traces at configured level; runtime internals at warn+
- Override with `RUST_LOG` for additional targets (e.g., `RUST_LOG=duroxide::runtime=debug`)
- All logs include `instance_id`, `execution_id`, `orchestration_name`, `activity_name` for correlation
- Optional OpenTelemetry metrics via `observability` feature flag
- Run `cargo run --example with_observability` to see structured logging in action
- Run `cargo run --example metrics_cli` to see observability dashboard
- See `docs/observability-guide.md` for complete guide

Notes
- Import as `duroxide` in Rust source.
- Timers are real time (Tokio sleep). External events are via `Runtime::raise_event`.
- Unknown-instance messages are logged and dropped. Providers persist history only (queues are in-memory runtime components).
- Logging is replay-safe by treating it as a system activity via `ctx.trace_*` helpers; logs are emitted through tracing at completion time (not persisted as history events).
