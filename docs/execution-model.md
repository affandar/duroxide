# Execution Model

## Overview

Duroxide implements a replay-based, message-driven execution model where orchestrations progress through discrete turns triggered by completion messages.

## Key Concepts

### Turn-Based Execution

Orchestrations execute in "turns":

1. **Trigger**: A work item arrives (start, activity completion, timer fired, external event)
2. **Replay**: The orchestration function replays from the beginning with full history
3. **Progress**: Code runs until it awaits something not yet complete
4. **Yield**: The turn ends, new actions are persisted, orchestration waits for next completion

```
Turn 1: Start → schedule Activity1 → await (pending) → yield
Turn 2: Activity1 completes → replay → schedule Activity2 → await (pending) → yield  
Turn 3: Activity2 completes → replay → return result → complete
```

### No Continuous Polling

Unlike typical async Rust applications where tokio continuously polls futures:

- Orchestrations are **polled once per turn** 
- Each completion message triggers a new turn
- The orchestration is **replayed from the beginning** each turn
- Progress is entirely event-driven, not poll-driven

### Token-Based Completion Delivery

When an orchestration schedules work:

1. The `schedule_*` method emits an `Action` and returns a token
2. The returned future polls for a `CompletionResult` keyed by that token
3. The replay engine delivers completions by binding tokens to persisted event IDs
4. On replay, completions are re-delivered to the same tokens

```rust
// Internally, schedule_activity works like:
pub fn schedule_activity(&self, name: &str, input: &str) -> impl Future<...> {
    let token = inner.emit_action(Action::CallActivity { ... });
    
    std::future::poll_fn(move |_cx| {
        if let Some(CompletionResult::ActivityOk(s)) = inner.get_result(token) {
            Poll::Ready(Ok(s))
        } else {
            Poll::Pending
        }
    })
}
```

### Deterministic Replay

The orchestration function must be **deterministic**—given the same history, it always:
- Schedules the same operations in the same order
- Makes the same decisions based on results
- Reaches the same state

This allows the runtime to:
- Reconstruct execution state by replaying with history
- Survive crashes—history is persisted, state is reconstructed

## Example: Sequential Activities

```rust
async fn my_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let a = ctx.schedule_activity("Step1", &input).await?;
    let b = ctx.schedule_activity("Step2", &a).await?;
    Ok(b)
}
```

### Turn 0: Initial Execution

1. Orchestration starts with empty history
2. `schedule_activity("Step1", ...)` emits `Action::CallActivity`, returns future
3. `.await` polls future → no completion yet → `Poll::Pending`
4. Turn ends, runtime persists `ActivityScheduled` event and dispatches work

### Turn 1: Step1 Completion

1. Worker completes Step1, enqueues `ActivityCompleted`
2. Orchestration replays from beginning
3. `schedule_activity("Step1", ...)` → token gets bound to persisted event
4. `.await` polls → completion found → `Poll::Ready(Ok(result))`
5. Execution continues to Step2, same process

### Turn 2: Step2 Completion

1. Worker completes Step2
2. Replay: Step1 await resolves immediately (from history)
3. Step2 await resolves with new completion
4. Orchestration returns, `OrchestrationCompleted` appended

## Composition: Select and Join

### Select (Race)

```rust
let activity = ctx.schedule_activity("SlowTask", "");
let timeout = ctx.schedule_timer(Duration::from_secs(30));

match ctx.select2(activity, timeout).await {
    Either2::First(result) => result,  // Activity won
    Either2::Second(()) => Err("timeout".to_string()),  // Timer won
}
```

`select2` uses `futures::select_biased!` internally—the first future to complete wins, and the loser is dropped.

### Join (Fan-out)

```rust
let f1 = ctx.schedule_activity("Task", "A");
let f2 = ctx.schedule_activity("Task", "B");
let f3 = ctx.schedule_activity("Task", "C");

let results = ctx.join(vec![f1, f2, f3]).await;  // Wait for all 3
```

`join` uses `futures::future::join_all` internally—all futures run concurrently and results are collected in order.

## ReplayEngine Integration

The `ReplayEngine` orchestrates each turn:

1. **Prep completions**: Convert incoming work items to completion results
2. **Execute**: Poll the orchestration function once with deliverable completions
3. **Capture**: Collect emitted actions for the runtime to persist and dispatch

See [replay-engine.md](replay-engine.md) for details.
