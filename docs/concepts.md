# Core Concepts

This document explains the fundamental concepts you need to understand to use Duroxide effectively.

## What is Deterministic Orchestration?

Deterministic orchestration ensures that given the same input and history, an orchestration will always produce the same output. This is achieved through:

- **Replay-based execution**: Your code is re-executed from the beginning on each turn
- **Event sourcing**: All state changes are recorded as events in an append-only log
- **Correlation IDs**: Every operation has a unique ID to match requests with responses

## Key Terms

### Orchestration
An orchestration is a long-running workflow defined as an async Rust function. It coordinates multiple activities, handles errors, and can run for extended periods.

```rust
async fn my_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Your workflow logic here
}
```

### Activity
Activities are the building blocks of orchestrations - stateless functions that perform actual work:

```rust
|input: String| async move {
    // Do something with external resources
    Ok(format!("Processed: {}", input))
}
```

### Turn
A turn is a single execution of your orchestration function from start to finish (or until it awaits an incomplete operation). Each completion message triggers a new turn.

### History
The append-only log of all events that have occurred in an orchestration instance. This includes:
- `OrchestrationStarted`
- `ActivityScheduled` / `ActivityCompleted` / `ActivityFailed`
- `TimerCreated` / `TimerFired`
- `ExternalSubscribed` / `ExternalEvent`
- `OrchestrationCompleted` / `OrchestrationFailed`

### Replay
The process of re-executing an orchestration function against its history to reconstruct state. During replay:
- Completed operations return their recorded results immediately
- New operations get scheduled
- The orchestration proceeds until hitting an incomplete operation

## Execution Model

Duroxide uses a **message-driven, turn-based execution model**:

1. **Initial Start**: An orchestration begins when a start message arrives
2. **Scheduling**: The orchestration schedules activities, timers, or waits for events
3. **Suspension**: When awaiting an incomplete operation, the turn ends
4. **Completion**: When a scheduled operation completes, a new turn begins
5. **Replay**: The orchestration replays from the beginning with the updated history
6. **Progress**: Execution continues past previously completed operations

## Determinism Rules

To maintain determinism, orchestrations must follow these rules:

### ✅ DO:
- Use `ctx.system_now_ms()` for current time
- Use `ctx.system_new_guid()` for GUIDs
- Use activities for all I/O operations
- Use `ctx.trace_*` for logging
- Handle all error cases explicitly

### ❌ DON'T:
- Use `SystemTime::now()` or `Instant::now()`
- Generate random numbers directly
- Access files, network, or databases directly
- Use `println!` or standard logging
- Rely on global mutable state

## Durability Guarantees

Duroxide provides several durability guarantees:

1. **At-least-once execution**: Every scheduled operation will execute at least once
2. **Exactly-once effects**: History deduplication ensures effects happen exactly once
3. **Crash recovery**: Orchestrations resume from their last checkpoint after crashes
4. **Progress preservation**: All completed operations are preserved in history

## Correlation and Ordering

Every operation in Duroxide has a correlation ID:

```rust
// When you schedule an activity
let future = ctx.schedule_activity("ProcessItem", "data");
// Internally, this gets ID 1

// In history:
ActivityScheduled { id: 1, name: "ProcessItem", input: "data" }
// Later:
ActivityCompleted { id: 1, result: "processed" }
```

This enables:
- **Deterministic matching**: Completions are matched by ID, not order
- **Race handling**: `select2` and `select` work correctly even with racing operations
- **Replay stability**: The same IDs are used during replay

## State Management

Duroxide doesn't maintain in-memory state between turns. Instead:

1. **State in history**: All state is derived from the event history
2. **Input/output passing**: Pass state between activities via their inputs/outputs
3. **Orchestration context**: Use the orchestration input and activity results
4. **ContinueAsNew**: Reset history while passing new state as input

Example:
```rust
async fn stateful_orchestration(ctx: OrchestrationContext, state_json: String) -> Result<String, String> {
    let mut state: MyState = serde_json::from_str(&state_json)?;
    
    // Process with current state
    let result = ctx.schedule_activity("Process", &state.current_item)
        .into_activity().await?;
    
    // Update state
    state.processed_count += 1;
    state.last_result = result;
    
    if state.processed_count < state.total_items {
        // Continue with new state
        ctx.continue_as_new(serde_json::to_string(&state)?);
        unreachable!() // Never reached
    } else {
        Ok(format!("Processed {} items", state.processed_count))
    }
}
```

## Error Handling

Errors in Duroxide are explicit and part of the orchestration flow:

```rust
// Activities return Result<String, String>
match ctx.schedule_activity("MayFail", "input").into_activity().await {
    Ok(result) => {
        // Success path
    }
    Err(error) => {
        // Error path - can retry, compensate, or fail
        ctx.trace_error(format!("Activity failed: {}", error));
        // Try compensation
        ctx.schedule_activity("Compensate", "").into_activity().await?
    }
}
```

## Next Steps

Now that you understand the core concepts:

1. Read about [Common Patterns](patterns.md)
2. Explore the [examples/](../examples/) directory
3. Review the [Architecture](architecture.md) for deeper understanding
4. Check the [API Reference](api.md) for specific methods
