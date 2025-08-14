## rust-dtf

Mini deterministic task framework core for durable-style orchestrations.

What it is
- Deterministic orchestration core inspired by Durable Task.
- Composable orchestrator futures with stable, correlated history and deterministic replay.
- Message-driven runtime with Tokio worker pools for activities and timers, plus an external event API.

Install (when published)
```toml
[dependencies]
rust-dtf = "0.1"
```

Key concepts
- History is append-only: `ActivityScheduled/Completed`, `TimerCreated/Fired`, `ExternalSubscribed/Event` (each has an id).
- Orchestrator code uses `schedule_*` to compose and `into_*` to await typed results.
- Runtime decouples orchestration from execution via channels (activities, timers, externals).

Local development
- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`

Project layout
- `src/lib.rs` — orchestration primitives (`OrchestrationContext`, `DurableFuture`, events/actions)
- `src/runtime/` — message-driven runtime, activity registry/worker, timer worker
- `tests/e2e_tests.rs` — end-to-end tests (races, ordering, sequential, ignore-before-start)

Quick start: define an orchestrator
```rust
use futures::future::{select, Either};
use rust_dtf::{OrchestrationContext, DurableOutput};

async fn orchestrator(ctx: OrchestrationContext) -> String {
    let left = select(ctx.schedule_activity("A", "1"), ctx.schedule_timer(5));
    let race = select(left, ctx.schedule_wait("Go"));

    let a = match race.await {
        Either::Left((Either::Left((DurableOutput::Activity(a), t_rest)), e_rest)) => {
            let _ = futures::future::join(t_rest, e_rest).await; a
        }
        Either::Left((Either::Right((DurableOutput::Timer, a_rest)), e_rest)) => {
            let (a_out, _e_out) = futures::future::join(a_rest, e_rest).await;
            match a_out { DurableOutput::Activity(v) => v, _ => unreachable!() }
        }
        Either::Right((DurableOutput::External(_), left_rest)) => {
            match left_rest.await {
                Either::Left((a_out, t_rest)) => { let _ = t_rest.await; match a_out { DurableOutput::Activity(v) => v, _ => unreachable!() } }
                Either::Right((t_out, a_rest)) => { match t_out { DurableOutput::Timer => (), _ => unreachable!() }; match a_rest.await { DurableOutput::Activity(v) => v, _ => unreachable!() } }
            }
        }
        _ => unreachable!(),
    };

    let b = ctx.schedule_activity("B", a.clone()).into_activity().await;
    format!("a={a}, b={b}")
}
```

Run with the runtime and register activities
```rust
use std::sync::Arc;
use rust_dtf::runtime::{Runtime, activity::ActivityRegistry};

# async fn example() {
let registry = ActivityRegistry::builder()
    .register("A", |input: String| async move { input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string() })
    .register("B", |input: String| async move { format!("{input}!") })
    .build();

let rt = Runtime::start(Arc::new(registry)).await;

// Option 1: spawn
let h = rt.clone().spawn_instance_to_completion("inst-1", orchestrator).await;
rt.raise_event("inst-1", "Go", "ok").await;
let (history, output) = h.await.unwrap();

// Option 2: run inline
// let (history, output) = rt.clone().run_instance_to_completion("inst-1", orchestrator).await;

println!("history={:#?}, output={}", history, output);
rt.shutdown().await;
# }
```

Deterministic replay
```rust
use rust_dtf::run_turn;
let (hist_after, _actions, out) = run_turn(history.clone(), orchestrator);
assert!(out.is_some());
```

Notes
- Crate name is hyphenated: `rust-dtf`. In Rust code, import it as `rust_dtf`.
- Real-time timers use Tokio sleeps. Externals are delivered via `raise_event`.
- Messages for unknown instances are ignored with a warning; buffering-before-start can be added via provider.
