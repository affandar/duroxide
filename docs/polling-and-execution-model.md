# DTF Polling and Execution Model

## Overview

The Durable Task Framework (DTF) implements a unique execution model that doesn't rely on a traditional async runtime continuously polling futures. Instead, it uses a **replay-based, message-driven architecture** where orchestrations progress through discrete turns triggered by completion messages.

## Key Concepts

### No Continuous Polling

Unlike typical async Rust applications where a tokio runtime continuously polls futures until completion, DTF:
- Polls futures **once per turn** when encountered
- Each completion message triggers a new turn
- The orchestration is **replayed from the beginning** each turn
- Progress is entirely event-driven, not poll-driven

### The Role of `poll_once`

The `poll_once` function creates a minimal polling context:

```rust
fn poll_once<F: Future>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();  // A waker that does nothing
    let mut cx = Context::from_waker(&w);
    let mut pinned = unsafe { Pin::new_unchecked(fut) };
    pinned.as_mut().poll(&mut cx)
}
```

This function:
- Creates a fake waker context that performs no wake operations
- Polls the future exactly once
- For activities: triggers scheduling if not already scheduled
- For awaited futures: returns `Pending` or `Ready` based on history

## Execution Flow Examples

### Scenario 1: Sequential Activities (Awaited)

Consider this orchestration:

```rust
async fn my_orchestration(ctx: OrchestrationContext) -> String {
    let a1 = ctx.schedule_activity("Activity1", "input1").await;
    let a2 = ctx.schedule_activity("Activity2", "input2").await;
    format!("{} {}", a1, a2)
}
```

#### Turn 0: Initial Execution
1. `run_single_execution` called with empty `completion_messages`
2. Orchestration function starts executing
3. First `schedule_activity("Activity1", ...)` creates a `DurableFuture`
4. The `.await` causes `poll_once` on this future:
   - Checks history for `ActivityCompleted/Failed` - not found
   - Adds `Event` with `EventKind::ActivityScheduled` to history
   - Records `Action::CallActivity` in pending actions
   - Returns `Poll::Pending`
5. Orchestration execution stops at this await point
6. Turn result: `TurnResult::Continue`
7. `dispatch_call_activity` enqueues `WorkItem::ActivityExecute` to Worker queue

#### Worker Processing
- Worker dispatcher picks up `ActivityExecute`
- Executes the actual activity function
- Enqueues `WorkItem::ActivityCompleted` to Orchestrator queue

#### Turn 1: Activity1 Completion
1. Orchestrator dispatcher picks up `ActivityCompleted`
2. Calls `handle_completion_item` → `run_single_execution` with completion message
3. `prep_completions` adds it to the completion map
4. **Orchestration replays from the beginning**
5. First `schedule_activity("Activity1", ...).await`:
   - `poll` checks history, finds `ActivityCompleted`
   - Returns `Poll::Ready(DurableOutput::Activity(result))`
6. Execution continues to `schedule_activity("Activity2", ...)`
7. Same process: adds to history, returns `Pending`
8. Turn result: `TurnResult::Continue`
9. Activity2 is dispatched

#### Turn 2: Activity2 Completion
1. Similar to Turn 1, but now both activities complete
2. Orchestration function returns the formatted string
3. Turn result: `TurnResult::Completed(output)`

### Scenario 2: Fire-and-Forget Activities

```rust
async fn my_orchestration(ctx: OrchestrationContext) -> String {
    // Schedule but don't await
    let f1 = ctx.schedule_activity("Activity1", "input1");
    let f2 = ctx.schedule_activity("Activity2", "input2");
    
    // Use poll_once to trigger scheduling
    poll_once(&mut f1);
    poll_once(&mut f2);
    
    "Started activities".to_string()
}
```

#### Single Turn Execution
1. Both `schedule_activity` calls create `DurableFuture` objects
2. `poll_once` manually polls each future once:
   - Adds `ActivityScheduled` events to history
   - Records `CallActivity` actions
   - Returns `Poll::Pending` (ignored since not awaited)
3. Orchestration continues and returns immediately
4. Turn result: `TurnResult::Completed("Started activities")`
5. Both activities are dispatched but orchestration completes without waiting

## The Replay Engine

### How Replay Works

Every turn starts from the beginning of the orchestration function:

1. **History Determines State**: The history contains all events from previous turns
2. **Deterministic Execution**: When replaying:
   - Completed activities return their results immediately
   - Pending activities return `Pending`
   - New activities get scheduled
3. **Progress Point**: Execution proceeds until hitting a `Pending` future or completion

### Why Replay?

Replay provides several benefits:
- **Durability**: State is reconstructed from history, not kept in memory
- **Determinism**: Same history always produces same execution
- **Versioning**: Can handle code changes between executions
- **Recovery**: Can resume from any point by replaying history

## The Completion Map

The completion map ensures deterministic ordering of completions:

```rust
pub struct CompletionMap {
    ordered: Vec<CompletionEntry>,
    consumed_indices: HashSet<usize>,
}
```

### Purpose
- Provides deterministic ordering of completions within a turn
- Ensures completions are consumed in the same order during replay
- Critical for operations like `select` that race multiple futures

### How It Works
1. Completions are added to the map in arrival order
2. During replay, futures check if their completion is "ready"
3. A completion is ready if all earlier completions have been consumed
4. This ensures deterministic execution even with racing operations

## Message-Driven Architecture

### The Dispatcher Loop

```rust
loop {
    if let Some((item, token)) = history_store.dequeue_peek_lock(QueueKind::Orchestrator).await {
        match item {
            WorkItem::StartOrchestration { ... } => {
                // Start new orchestration
            }
            WorkItem::ActivityCompleted { ... } => {
                // Trigger new turn with completion
                handle_completion_item(...)
            }
            // ... other completion types
        }
    }
}
```

### Progress Through Messages

Orchestrations only progress when:
1. Initial start message arrives
2. Completion messages arrive (activity, timer, external event, etc.)
3. Each message triggers a new turn
4. No active polling - everything is event-driven

## System Architecture

The DTF system is essentially a **state machine** where:

- **State** = History (persistent event log)
- **Transitions** = Completion messages
- **Transition Function** = Orchestration replay
- **Output** = New events and actions

```
History + Completion Message → Replay → New History + Actions
```

## Key Insights

1. **No Traditional Async Runtime**: The orchestration dispatcher and completion messages drive all progress, not a tokio runtime polling futures.

2. **Single-Pass Polling**: Futures are polled exactly once per turn when encountered, not continuously.

3. **Replay-Based State**: State is reconstructed by replaying history, not maintained in memory.

4. **Deterministic Execution**: Same history + same completions = same result, always.

5. **Event-Driven Progress**: Orchestrations are reactive - they only execute in response to events.

## Comparison with Traditional Async

| Traditional Async | DTF Model |
|------------------|-----------|
| Continuous polling by runtime | Single poll per turn |
| Futures stay in memory | Futures recreated each turn |
| State in memory | State in history |
| Progress through polling | Progress through messages |
| Non-deterministic ordering | Deterministic via completion map |

## Benefits of This Model

1. **Durability**: Orchestrations survive process crashes
2. **Scalability**: No memory overhead for waiting orchestrations
3. **Determinism**: Reproducible execution for debugging
4. **Versioning**: Can evolve orchestration code between turns
5. **Distribution**: Can execute turns on different machines

## Conclusion

The DTF polling model is fundamentally different from traditional async Rust. Instead of relying on continuous polling by a runtime, it uses a message-driven, replay-based approach where each completion message triggers a discrete turn of execution. This provides durability, determinism, and scalability at the cost of replay overhead - a worthwhile tradeoff for durable orchestrations.
