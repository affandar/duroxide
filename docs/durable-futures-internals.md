# DurableFuture Internals: Scheduling, Polling, and Event Claim System

This document provides a detailed technical explanation of how Duroxide's replay engine works, focusing on `DurableFuture` scheduling, polling, the claim system, and aggregate futures (select/join). It also covers important differences from standard Rust async/Tokio programming.

## Table of Contents

1. [Core Concept: History-Based Execution](#core-concept-history-based-execution)
2. [High-Level Architecture](#high-level-architecture)
3. [The DurableFuture Type](#the-durablefuture-type)
4. [Event Model](#event-model)
5. [The Claim System](#the-claim-system)
6. [Polling and Replay](#polling-and-replay)
7. [Aggregate Futures (Select/Join)](#aggregate-futures-selectjoin)
8. [Runtime Integration](#runtime-integration)
9. [Rust Async Differences](#rust-async-differences)

---

## Core Concept: History-Based Execution

Duroxide orchestrations are **deterministic functions** that execute incrementally across multiple "turns". The key insight: **execution generates history, and history enables replay**.

### The Execution Model

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Orchestration Lifecycle                          │
│                                                                      │
│   ┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐  │
│   │  Turn 1  │────▶│  Turn 2  │────▶│  Turn 3  │────▶│  Turn N  │  │
│   │ (fresh)  │     │ (replay) │     │ (replay) │     │(complete)│  │
│   └──────────┘     └──────────┘     └──────────┘     └──────────┘  │
│        │                │                │                │         │
│        ▼                ▼                ▼                ▼         │
│   [history]        [history]        [history]        [history]      │
│   grows            grows            grows            final          │
└─────────────────────────────────────────────────────────────────────┘
```

**Each turn:**
1. The runtime creates a **fresh orchestration future** with the current history
2. The future is **polled once** - it runs until it awaits something not yet complete
3. The future is **dropped** (dehydrated) - only the history persists
4. When new completions arrive, the process repeats (rehydration)

### Example: A Simple Orchestration

```rust
async fn order_workflow(ctx: OrchestrationContext) -> String {
    let item = ctx.schedule_activity("ValidateOrder", "order-123")
        .into_activity().await.unwrap();
    
    ctx.schedule_timer(Duration::from_secs(60)).into_timer().await;
    
    let result = ctx.schedule_activity("ProcessPayment", &item)
        .into_activity().await.unwrap();
    
    result
}
```

**Turn 1** - First execution, nothing in history:
```
Execute: ValidateOrder scheduled → await → BLOCKED (no completion)
History: [OrchestrationStarted, ActivityScheduled("ValidateOrder")]
Actions: [CallActivity("ValidateOrder")]
State:   DEHYDRATED (future dropped, waiting for activity)
```

**Turn 2** - Activity completed, replay + continue:
```
Execute: ValidateOrder scheduled → await → READY (completion found!)
         Timer scheduled → await → BLOCKED (timer not fired)
History: [OrchestrationStarted, ActivityScheduled, ActivityCompleted, TimerCreated]
Actions: [CreateTimer]
State:   DEHYDRATED (waiting for timer)
```

**Turn 3** - Timer fired, replay + continue:
```
Execute: ValidateOrder → READY (replay)
         Timer → READY (replay)
         ProcessPayment scheduled → await → BLOCKED
History: [..., TimerFired, ActivityScheduled("ProcessPayment")]
Actions: [CallActivity("ProcessPayment")]
State:   DEHYDRATED (waiting for activity)
```

**Turn 4** - Payment completed, replay + finish:
```
Execute: ValidateOrder → READY (replay)
         Timer → READY (replay)
         ProcessPayment → READY (replay)
         Return "success"
History: [..., ActivityCompleted, OrchestrationCompleted]
State:   COMPLETED
```

### Why This Works

The orchestration function is **deterministic** - given the same history, it always:
- Schedules the same operations in the same order
- Makes the same decisions based on results
- Reaches the same state

This allows us to:
- **Drop the future** after each turn (no in-memory state to preserve)
- **Recreate it** with history to resume exactly where we left off
- **Survive crashes** - history is persisted, state is reconstructed

### The Replay Rules

This document covers the rules that make replay work:
1. **Claim System**: Each future claims its corresponding scheduling event from history
2. **FIFO Ordering**: Completions must be consumed in history order
3. **Nondeterminism Detection**: If code tries to schedule differently than history shows, it's caught
4. **Loser Cancellation**: Select2 losers are marked so their completions don't block

---

## High-Level Architecture

Duroxide implements **deterministic replay** for durable orchestrations. The mechanics are:

1. **First Execution**: When orchestration code runs for the first time, each `schedule_*` call creates a new event and records an `Action` for the runtime.
2. **Replay**: When the runtime re-processes work, it replays the same orchestration code with the existing history. The futures "adopt" their corresponding events from history rather than creating new ones.

This is achieved through a **claim system** where each `DurableFuture` claims ownership of specific events in the history.

```
┌─────────────────────────────────────────────────────────────────────┐
│                    OrchestrationContext                              │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  CtxInner (Arc<Mutex<...>>)                                   │  │
│  │  • history: Vec<Event>           // Persisted event log       │  │
│  │  • actions: Vec<Action>          // Pending runtime actions   │  │
│  │  • claimed_scheduling_events: HashSet<u64>  // Claimed IDs    │  │
│  │  • consumed_completions: HashSet<u64>       // FIFO tracking  │  │
│  │  • cancelled_source_ids: HashSet<u64>       // Select losers  │  │
│  │  • next_event_id: u64            // Counter for new events    │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              │
                              │ cloned reference
                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      DurableFuture                                   │
│  • claimed_event_id: Cell<Option<u64>>  // Common field            │
│  • ctx: OrchestrationContext            // Common field            │
│  • kind: Kind                           // Operation-specific data │
│    ├─ Activity { name, input }                                      │
│    ├─ Timer { delay_ms }                                            │
│    ├─ External { name, result }                                     │
│    ├─ SubOrch { name, version, instance, input }                    │
│    └─ System { op, value }                                          │
└─────────────────────────────────────────────────────────────────────┘
```

---

## The DurableFuture Type

`DurableFuture` is a unified future type that wraps different kinds of durable operations. Common fields are stored directly on the struct, while operation-specific data is in the `kind` variant:

```rust
pub struct DurableFuture {
    pub(crate) claimed_event_id: Cell<Option<u64>>,  // Claimed scheduling event ID
    pub(crate) ctx: OrchestrationContext,            // Shared orchestration context
    pub(crate) kind: Kind,                           // Operation-specific data
}

pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
    },
    Timer {
        delay_ms: u64,
    },
    External {
        name: String,
        result: RefCell<Option<String>>,  // Cached result
    },
    SubOrch {
        name: String,
        version: Option<String>,
        instance: RefCell<String>,
        input: String,
    },
    System {
        op: String,
        value: RefCell<Option<String>>,
    },
}
```

### Key Design Choices

1. **Common Fields on Struct**: `claimed_event_id` and `ctx` are stored directly on `DurableFuture`, not inside each `Kind` variant. This eliminates the need to match all variants just to access these common fields.

2. **`Cell<Option<u64>>` for `claimed_event_id`**: Interior mutability allows the future to claim an event ID during polling without requiring `&mut self`.

3. **Cloned `OrchestrationContext`**: Each future holds a clone of the context (which is `Arc<Mutex<CtxInner>>`). This allows multiple futures to exist simultaneously and all access the shared state.

4. **Lazy Event ID Assignment**: Event IDs are not assigned when `schedule_*` is called. They're discovered/created during the first poll.

---

## Event Model

Events are categorized into three types:

### 1. Scheduling Events
Created when an operation is scheduled:
- `ActivityScheduled { event_id, name, input, execution_id }`
- `TimerCreated { event_id, fire_at_ms, execution_id }`
- `ExternalSubscribed { event_id, name }`
- `SubOrchestrationScheduled { event_id, name, instance, input, execution_id }`
- `SystemCall { event_id, op, value, execution_id }` (scheduling + completion combined)

### 2. Completion Events
Created when an operation completes:
- `ActivityCompleted { event_id, source_event_id, result }`
- `ActivityFailed { event_id, source_event_id, details }`
- `TimerFired { event_id, source_event_id, fire_at_ms }`
- `ExternalEvent { event_id, name, data }` (matched by name, not source_event_id)
- `SubOrchestrationCompleted { event_id, source_event_id, result }`
- `SubOrchestrationFailed { event_id, source_event_id, details }`

### 3. Lifecycle Events
- `OrchestrationStarted { event_id, name, version, input, ... }`
- `OrchestrationCompleted { event_id, output }`
- `OrchestrationFailed { event_id, details }`
- `OrchestrationContinuedAsNew { event_id, input }`

### Event Linkage

Scheduling and completion events are linked via `source_event_id`:

```
ActivityScheduled { event_id: 2, name: "Greet", ... }
        │
        │ source_event_id = 2
        ▼
ActivityCompleted { event_id: 5, source_event_id: 2, result: "Hello" }
```

### Example: Simple Activity Flow

Consider this orchestration:

```rust
async fn greet_workflow(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    let greeting = ctx.schedule_activity("Greet", &name).into_activity().await?;
    Ok(greeting)
}
```

**Turn 1 (First Execution)** - History before: `[OrchestrationStarted {...}]`

```
History (before turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, name: "greet_workflow",    │
│                            input: "Alice" }                         │
└────────────────────────────────────────────────────────────────────┘

Execution:
  1. ctx.schedule_activity("Greet", "Alice") creates DurableFuture
  2. .into_activity().await polls the future
  3. Future scans history → no ActivityScheduled found
  4. Creates new event (event_id: 2), records Action::CallActivity
  5. Looks for completion → none found → returns Pending

History (after turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "Greet",                │
│                         input: "Alice", execution_id: 1 }          │
└────────────────────────────────────────────────────────────────────┘

Actions emitted: [CallActivity { scheduling_event_id: 2, name: "Greet", input: "Alice" }]
```

**Turn 2 (After Activity Completes)** - Worker executed activity, completion recorded

```
History (before turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "Greet", ... }          │
│ [3] ActivityCompleted { event_id: 3, source_event_id: 2,           │
│                         result: "Hello, Alice!" }                   │
└────────────────────────────────────────────────────────────────────┘

Execution (REPLAY):
  1. ctx.schedule_activity("Greet", "Alice") creates DurableFuture
  2. .into_activity().await polls the future
  3. Future scans history → finds ActivityScheduled(event_id: 2), CLAIMS it
  4. Looks for completion → finds ActivityCompleted(source_event_id: 2)
  5. FIFO check passes → returns Ready(Ok("Hello, Alice!"))
  6. Orchestration returns Ok("Hello, Alice!")

History (after turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1-3] ... (unchanged)                                              │
│ [4] OrchestrationCompleted { event_id: 4, output: "Hello, Alice!" }│
└────────────────────────────────────────────────────────────────────┘
```

---

## The Claim System

The claim system ensures deterministic replay by tracking which events have been "claimed" by which futures.

### Data Structures

```rust
struct CtxInner {
    // Events claimed by futures (scheduling event IDs)
    claimed_scheduling_events: HashSet<u64>,
    
    // Completion events that have been consumed (FIFO enforcement)
    consumed_completions: HashSet<u64>,
    
    // External events consumed by name (since they're matched by name, not source_event_id)
    consumed_external_events: HashSet<String>,
    
    // Source event IDs whose completions should be auto-skipped (select2 losers)
    cancelled_source_ids: HashSet<u64>,
}
```

The `cancelled_source_ids` set tracks the `source_event_id` values of operations that lost a `select2` race. Their completion events are automatically skipped in FIFO ordering checks, preventing stale completions from blocking subsequent operations.

### Claim Process (During First Poll)

When a `DurableFuture` is polled for the first time:

1. **Check if already claimed**: If `claimed_event_id.get().is_some()`, skip to completion lookup.

2. **Search history for matching scheduling event**:
   - Scan history for the FIRST unclaimed scheduling event of the correct type
   - **Nondeterminism check**: If the first unclaimed event is a DIFFERENT type than expected, this indicates the orchestration code has diverged from its original execution → set `nondeterminism_error`

3. **If found in history (replay)**:
   - Verify it matches (same name, input, etc.)
   - Mark it as claimed: `claimed_scheduling_events.insert(event_id)`
   - Store locally: `claimed_event_id.set(Some(event_id))`

4. **If NOT found (first execution)**:
   - Allocate new event ID: `next_event_id += 1`
   - Create and append the scheduling event to history
   - Record the corresponding `Action` for the runtime
   - Mark as claimed

### Global Ordering Invariant

**Critical**: Scheduling events MUST be claimed in the exact order they appear in history. This enforces determinism:

```rust
// In DurableFuture::poll for Kind::Activity:
for event in &inner.history {
    match event {
        Event::ActivityScheduled { event_id, name: n, input: inp, .. }
            if !inner.claimed_scheduling_events.contains(event_id) =>
        {
            // This MUST be our scheduling event - check match
            if n != name || inp != input {
                inner.nondeterminism_error = Some(format!(
                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{n}','{inp}') but expected ActivityScheduled('{name}','{input}')"
                ));
                return Poll::Pending;
            }
            found_event_id = Some(*event_id);
            break;
        }
        // If next unclaimed event is a DIFFERENT type, that's also nondeterminism
        Event::TimerCreated { event_id, .. }
            if !inner.claimed_scheduling_events.contains(event_id) =>
        {
            inner.nondeterminism_error = Some(format!(
                "nondeterministic: schedule order mismatch: next is TimerCreated but expected ActivityScheduled('{name}','{input}')"
            ));
            return Poll::Pending;
        }
        // ... similar checks for other event types
        _ => {}
    }
}
```

### Example: Nondeterminism Detection

**Original orchestration** (deployed v1):
```rust
async fn workflow_v1(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    ctx.schedule_activity("A", "").into_activity().await?;
    ctx.schedule_activity("B", "").into_activity().await?;
    Ok("done".to_string())
}
```

**Modified orchestration** (deployed v2 - BUGGY):
```rust
async fn workflow_v2(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;  // NEW!
    ctx.schedule_activity("A", "").into_activity().await?;
    ctx.schedule_activity("B", "").into_activity().await?;
    Ok("done".to_string())
}
```

**What happens when v2 replays v1's history?**

```
History (from v1 execution):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "A", input: "" }        │
│ [3] ActivityCompleted { event_id: 3, source_event_id: 2, ... }     │
│ [4] ActivityScheduled { event_id: 4, name: "B", input: "" }        │
│ [5] ActivityCompleted { event_id: 5, source_event_id: 4, ... }     │
│ [6] OrchestrationCompleted { event_id: 6, ... }                    │
└────────────────────────────────────────────────────────────────────┘

v2 Replay Attempt:
  1. ctx.schedule_timer(...) creates Timer DurableFuture
  2. .into_timer().await polls the future
  3. Timer future scans history for TimerCreated
  4. First unclaimed scheduling event is ActivityScheduled("A", "")
  5. → NONDETERMINISM ERROR: "next is ActivityScheduled('A','') but expected TimerCreated"
```

The claim system catches this because the ordering of scheduling events in history doesn't match what the new code is trying to schedule.

### Example: Claim Tracking State

```rust
async fn multi_step(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    let a = ctx.schedule_activity("Step1", "").into_activity().await?;
    let b = ctx.schedule_activity("Step2", &a).into_activity().await?;
    Ok(b)
}
```

**State during replay (after all completions):**

```
History:
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "Step1", input: "" }    │
│ [3] ActivityCompleted { event_id: 3, source_event_id: 2, ... }     │
│ [4] ActivityScheduled { event_id: 4, name: "Step2", input: "..." } │
│ [5] ActivityCompleted { event_id: 5, source_event_id: 4, ... }     │
└────────────────────────────────────────────────────────────────────┘

CtxInner state during replay:

After polling Step1 future:
  claimed_scheduling_events: {2}      // Step1's scheduling event claimed
  consumed_completions: {3}           // Step1's completion consumed
  
After polling Step2 future:
  claimed_scheduling_events: {2, 4}   // Both scheduling events claimed
  consumed_completions: {3, 5}        // Both completions consumed
```

---

## Polling and Replay

### The Turn-Based Execution Model

Duroxide uses a **turn-based execution model**:

1. Runtime fetches work item with current history
2. Runtime calls `run_turn_with_status()` which:
   - Creates `OrchestrationContext` with history
   - Creates orchestrator future and **polls it once**
   - Returns updated history, actions, output (if completed), and any nondeterminism error

```rust
pub fn run_turn_with_status<O, F>(...) -> (Vec<Event>, Vec<Action>, Option<O>, Option<String>) {
    let ctx = OrchestrationContext::new(history, ...);
    let mut fut = Box::pin(orchestrator(ctx.clone()));
    
    match poll_once(fut.as_mut()) {
        Poll::Ready(out) => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let nondet = ctx.inner.lock().unwrap().nondeterminism_error.clone();
            (hist_after, actions, Some(out), nondet)
        }
        Poll::Pending => {
            // Same, but output is None
        }
    }
}
```

### poll_once Implementation

```rust
fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> Poll<F::Output> {
    let w = noop_waker();  // No-op waker - we don't use wake notifications
    let mut cx = Context::from_waker(&w);
    fut.as_mut().poll(&mut cx)
}
```

**Note**: The `noop_waker` is crucial - Duroxide doesn't use Tokio's wake mechanism for driving futures. Instead, the runtime polls orchestrations when work items arrive.

### Completion Lookup (FIFO Enforcement)

After claiming a scheduling event, the future looks for its completion:

```rust
// Find our completion in history
let our_completion = inner.history.iter().find_map(|e| match e {
    Event::ActivityCompleted { event_id, source_event_id, result, .. }
        if *source_event_id == our_event_id =>
    {
        Some((*event_id, Ok(result.clone())))
    }
    Event::ActivityFailed { event_id, source_event_id, details, .. }
        if *source_event_id == our_event_id =>
    {
        Some((*event_id, Err(details.display_message())))
    }
    _ => None,
});

if let Some((completion_event_id, result)) = our_completion {
    // FIFO check: All completions BEFORE ours must be consumed
    if can_consume_completion(&inner.history, &inner.consumed_completions, completion_event_id) {
        inner.consumed_completions.insert(completion_event_id);
        return Poll::Ready(DurableOutput::Activity(result));
    }
}
Poll::Pending
```

### FIFO Completion Consumption

The `can_consume_completion` function enforces that completions are consumed in history order, while accounting for cancelled select2 losers:

```rust
fn can_consume_completion(
    history: &[Event],
    consumed_completions: &HashSet<u64>,
    cancelled_source_ids: &HashSet<u64>,  // Select2 losers to skip
    completion_event_id: u64,
) -> bool {
    history.iter().all(|e| {
        match e {
            Event::ActivityCompleted { event_id, source_event_id, .. }
            | Event::ActivityFailed { event_id, source_event_id, .. }
            | Event::TimerFired { event_id, source_event_id, .. }
            | Event::SubOrchestrationCompleted { event_id, source_event_id, .. }
            | Event::SubOrchestrationFailed { event_id, source_event_id, .. } => {
                // Cancelled completions (select2 losers) don't block - skip them
                if cancelled_source_ids.contains(source_event_id) {
                    return true;
                }
                // Otherwise: all completions before ours must be consumed
                *event_id >= completion_event_id || consumed_completions.contains(event_id)
            }
            Event::ExternalEvent { event_id, .. } => {
                // External events don't have source_event_id - can't be cancelled via select2
                *event_id >= completion_event_id || consumed_completions.contains(event_id)
            }
            _ => true,
        }
    })
}
```

**Select2 Loser Handling**: When `select2` returns with a winner, the loser's `source_event_id` is added to `cancelled_source_ids`. This ensures the loser's eventual completion (e.g., `TimerFired`) doesn't block subsequent operations waiting for their own completions.

---

## Aggregate Futures (Select/Join)

### AggregateDurableFuture

```rust
pub struct AggregateDurableFuture {
    ctx: OrchestrationContext,
    children: Vec<DurableFuture>,
    mode: AggregateMode,  // Select or Join
}

enum AggregateMode {
    Select,  // Return first ready child
    Join,    // Wait for all children
}
```

### Select Semantics (Three-Phase Polling)

The select operation races multiple futures and returns when the first completes. **Critical implementation details**:

1. During replay, we must poll ALL children, not just until we find a winner (to claim scheduling events)
2. When returning a winner, we must mark losers' `source_event_id`s as cancelled (to prevent FIFO blocking)

```rust
AggregateMode::Select => {
    // Phase 1: Poll ALL children to ensure they claim their scheduling events
    let mut ready_results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
    for (i, child) in this.children.iter_mut().enumerate() {
        if let Poll::Ready(output) = Pin::new(child).poll(cx) {
            ready_results[i] = Some(output);
        }
    }

    // Phase 2: Find the first ready child (winner)
    let winner_index = ready_results.iter().position(|r| r.is_some());

    if let Some(winner_idx) = winner_index {
        // Phase 3: Mark all loser source_event_ids as cancelled
        let mut inner = this.ctx.inner.lock().unwrap();
        for (i, child) in this.children.iter().enumerate() {
            if i != winner_idx {
                // Common field: access claimed_event_id directly from DurableFuture
                if let Some(source_id) = child.claimed_event_id.get() {
                    inner.cancelled_source_ids.insert(source_id);
                }
            }
        }
        
        // Return the winner
        let output = ready_results[winner_idx].take().expect("winner is ready");
        return Poll::Ready(AggregateOutput::Select {
            winner_index: winner_idx,
            output,
        });
    }

    Poll::Pending
}
```

**Phase 3 is critical**: Without marking losers as cancelled, their eventual completion events (e.g., `TimerFired` from a timeout that lost to an activity) would block FIFO ordering for subsequent operations. This is analogous to how `tokio::select!` drops the losing futures - we can't drop them (they're already in history), but we mark them as "don't wait for their completions".

### Example: Select with Timeout

```rust
async fn with_timeout(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    let activity = ctx.schedule_activity("SlowTask", "");
    let timeout = ctx.schedule_timer(Duration::from_secs(30));
    
    let (winner, output) = ctx.select2(activity, timeout).await;
    
    match winner {
        0 => match output {
            DurableOutput::Activity(result) => result,
            _ => unreachable!(),
        },
        1 => Err("timeout".to_string()),
        _ => unreachable!(),
    }
}
```

**Turn 1 (First Execution)** - Both futures created, both pending

```
History (before turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
└────────────────────────────────────────────────────────────────────┘

Execution:
  1. schedule_activity("SlowTask") → creates DurableFuture (activity)
  2. schedule_timer(30s) → creates DurableFuture (timer)
  3. select2(activity, timeout).await polls AggregateDurableFuture
  4. Phase 1: Poll activity → no match in history → creates event_id: 2
  5. Phase 1: Poll timer → no match in history → creates event_id: 3
  6. Phase 2: Neither ready → Pending

History (after turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "SlowTask", ... }       │
│ [3] TimerCreated { event_id: 3, fire_at_ms: 1700000030000 }        │
└────────────────────────────────────────────────────────────────────┘

Actions: [CallActivity{...}, CreateTimer{...}]
```

**Turn 2 (Activity Completes First)** - Activity wins the race

```
History (before turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "SlowTask", ... }       │
│ [3] TimerCreated { event_id: 3, fire_at_ms: 1700000030000 }        │
│ [4] ActivityCompleted { event_id: 4, source_event_id: 2,           │
│                         result: "task result" }                     │
└────────────────────────────────────────────────────────────────────┘

Replay:
  1. schedule_activity → creates DurableFuture
  2. schedule_timer → creates DurableFuture
  3. select2.await polls aggregate
  4. Phase 1: Poll activity → claims event_id: 2, finds completion → Ready!
  5. Phase 1: Poll timer → claims event_id: 3, no TimerFired → Pending
  6. Phase 2: Activity (index 0) is ready → winner found
  7. Phase 3: Mark timer (source_event_id: 3) as cancelled
  8. Return (0, Activity(Ok("task result")))

claimed_scheduling_events: {2, 3}  // BOTH claimed even though timer lost
consumed_completions: {4}
cancelled_source_ids: {3}          // Timer's eventual TimerFired won't block FIFO

History (after turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1-4] ... (unchanged)                                              │
│ [5] OrchestrationCompleted { event_id: 5, output: "task result" }  │
└────────────────────────────────────────────────────────────────────┘
```

**Key Point**: Both scheduling events (2 and 3) are claimed even though the timer lost. This is essential - if we didn't claim the timer's event, subsequent operations would see TimerCreated as the "next unclaimed event" and fail with nondeterminism.

### Example: Why Two-Phase Polling is Critical

**Without two-phase polling (buggy):**

```rust
// WRONG implementation (old bug)
for (i, child) in children.iter_mut().enumerate() {
    if let Poll::Ready(output) = Pin::new(child).poll(cx) {
        return Poll::Ready(Select { winner_index: i, output });  // Returns immediately!
    }
}
```

**Scenario:** Activity wins, then orchestration schedules another activity:

```rust
async fn problematic(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    let activity = ctx.schedule_activity("Fast", "");
    let timer = ctx.schedule_timer(Duration::from_secs(10));
    ctx.select2(activity, timer).await;
    
    // Now schedule another activity
    ctx.schedule_activity("Next", "").into_activity().await
}
```

**Turn 2 Replay (with buggy select):**

```
History:
┌────────────────────────────────────────────────────────────────────┐
│ [2] ActivityScheduled { event_id: 2, name: "Fast", ... }           │
│ [3] TimerCreated { event_id: 3, ... }                              │
│ [4] ActivityCompleted { event_id: 4, source_event_id: 2, ... }     │
│ [5] ActivityScheduled { event_id: 5, name: "Next", ... }           │
│ ...                                                                 │
└────────────────────────────────────────────────────────────────────┘

Buggy replay:
  1. select2 polls activity → claims 2, ready!
  2. select2 returns IMMEDIATELY (doesn't poll timer)
  3. Timer's event_id: 3 is NOT claimed!
  4. schedule_activity("Next") polls
  5. Scans for unclaimed scheduling event → finds TimerCreated(3)!
  6. NONDETERMINISM: "next is TimerCreated but expected ActivityScheduled('Next',...)"
```

**With two-phase polling (correct):**

```
Correct replay:
  1. Phase 1: Poll activity → claims 2, ready
  2. Phase 1: Poll timer → claims 3, pending
  3. Phase 2: Return activity's result
  4. schedule_activity("Next") polls
  5. Scans for unclaimed → finds ActivityScheduled("Next") at event_id: 5 ✓
```

### Example: Why Phase 3 (Loser Cancellation) is Critical

Two-phase polling solves the nondeterminism problem, but there's another issue: **stale completions from losers can block FIFO ordering**.

**Scenario:** Multiple `select2` calls with activity winners, then a final timer:

```rust
async fn retry_then_sleep(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    // Two retries with timeouts (activity wins both times)
    for _ in 0..2 {
        let activity = ctx.schedule_activity("Task", "");
        let timeout = ctx.schedule_timer(Duration::from_secs(30));
        ctx.select2(activity, timeout).await;
    }
    
    // Final sleep timer
    ctx.schedule_timer(Duration::from_secs(10)).into_timer().await;
    Ok("done".to_string())
}
```

**Turn 3 History (after both activities complete):**

```
History:
┌────────────────────────────────────────────────────────────────────┐
│ [2] ActivityScheduled { event_id: 2, name: "Task", ... }           │
│ [3] TimerCreated { event_id: 3, fire_at_ms: ... }                  │  ← Retry 1 timeout
│ [4] ActivityCompleted { event_id: 4, source_event_id: 2, ... }     │  ← Activity wins
│ [5] ActivityScheduled { event_id: 5, name: "Task", ... }           │
│ [6] TimerCreated { event_id: 6, fire_at_ms: ... }                  │  ← Retry 2 timeout
│ [7] ActivityCompleted { event_id: 7, source_event_id: 5, ... }     │  ← Activity wins
│ [8] TimerCreated { event_id: 8, fire_at_ms: 10000 }                │  ← Final sleep
│ [9] TimerFired { event_id: 9, source_event_id: 3, ... }            │  ← STALE: retry 1's loser
│ [10] TimerFired { event_id: 10, source_event_id: 6, ... }          │  ← STALE: retry 2's loser
│ [11] TimerFired { event_id: 11, source_event_id: 8, ... }          │  ← Final sleep fires
└────────────────────────────────────────────────────────────────────┘
```

**Without loser cancellation (buggy - orchestration hangs!):**

The final timer (event_id: 8) looks for its completion (event_id: 11). But `can_consume_completion` checks: "Are events 9 and 10 consumed?" They're not - no one is waiting for them! The orchestration hangs forever.

**With loser cancellation (correct):**

When each `select2` returns, the losing timer's `source_event_id` is added to `cancelled_source_ids`:
- After retry 1: `cancelled_source_ids = {3}`
- After retry 2: `cancelled_source_ids = {3, 6}`

Now `can_consume_completion(11)` checks event 9 (`source_event_id: 3` → in cancelled set → skip) and event 10 (`source_event_id: 6` → in cancelled set → skip). The check passes, and the final timer completes!

**This is analogous to Tokio's `select!`**: In `tokio::select!`, losing futures are dropped and their resources released. We can't drop history events, but marking them as cancelled achieves the same semantic - their completions are no longer relevant.

### Join Semantics (Fixed-Point Polling)

Join waits for all children to complete. Due to FIFO completion ordering, we need fixed-point polling:

```rust
AggregateMode::Join => {
    let mut results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
    loop {
        let mut made_progress = false;
        for (i, child) in this.children.iter_mut().enumerate() {
            if results[i].is_some() { continue; }
            if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                results[i] = Some(output);
                made_progress = true;
            }
        }

        if results.iter().all(|r| r.is_some()) {
            // All ready - sort by completion event_id (history order)
            let mut items = /* collect (event_id, index, output) */;
            items.sort_by_key(|(eid, _, _)| *eid);
            return Poll::Ready(AggregateOutput::Join { outputs: ... });
        }

        if !made_progress {
            return Poll::Pending;
        }
        // Loop again - newly consumed completions may unblock others
    }
}
```

**Why fixed-point?** FIFO ordering means child A's completion might be blocked waiting for child B's completion to be consumed first (if B's completion appears earlier in history).

### Example: Join with FIFO Ordering

```rust
async fn fan_out_fan_in(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    let futures = vec![
        ctx.schedule_activity("TaskA", ""),
        ctx.schedule_activity("TaskB", ""),
        ctx.schedule_activity("TaskC", ""),
    ];
    let results = ctx.join(futures).await;
    // results are in completion-order (history order), NOT array order
    Ok(format!("{:?}", results))
}
```

**Turn 1 (First Execution)** - All activities scheduled

```
History (after turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "TaskA", ... }          │
│ [3] ActivityScheduled { event_id: 3, name: "TaskB", ... }          │
│ [4] ActivityScheduled { event_id: 4, name: "TaskC", ... }          │
└────────────────────────────────────────────────────────────────────┘
```

**Turn 2 (All Complete)** - B finished first, then C, then A

```
History (before turn):
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "TaskA", ... }          │
│ [3] ActivityScheduled { event_id: 3, name: "TaskB", ... }          │
│ [4] ActivityScheduled { event_id: 4, name: "TaskC", ... }          │
│ [5] ActivityCompleted { event_id: 5, source_event_id: 3, ... }     │  ← B finished first
│ [6] ActivityCompleted { event_id: 6, source_event_id: 4, ... }     │  ← C finished second
│ [7] ActivityCompleted { event_id: 7, source_event_id: 2, ... }     │  ← A finished last
└────────────────────────────────────────────────────────────────────┘

Join polling (fixed-point):

Iteration 1:
  Poll TaskA → claims 2, finds completion 7
    can_consume_completion(7)? 
      → Is 5 consumed? No → BLOCKED (5 < 7)
  Poll TaskB → claims 3, finds completion 5
    can_consume_completion(5)?
      → All completions before 5 consumed? Yes → Ready! → consume 5
  Poll TaskC → claims 4, finds completion 6
    can_consume_completion(6)?
      → Is 5 consumed? Yes → Ready! → consume 6
  
  made_progress = true, not all done, loop again

Iteration 2:
  Poll TaskA → finds completion 7
    can_consume_completion(7)?
      → Is 5 consumed? Yes
      → Is 6 consumed? Yes
      → Ready! → consume 7
  
  All done! Sort by completion event_id: [(5, B), (6, C), (7, A)]
  Return results in history order: [B_result, C_result, A_result]

consumed_completions: {5, 6, 7}
```

**Why FIFO matters**: The results vector is sorted by completion `event_id`, reflecting the actual completion order preserved in history. This ensures the orchestration observes completions in the same order they actually happened.

### Example: FIFO Blocking

Consider what happens if we didn't enforce FIFO:

```
Without FIFO (buggy):
  Poll TaskA → finds completion 7, consume immediately!
  Poll TaskB → finds completion 5, consume!
  Poll TaskC → finds completion 6, consume!
  
  Results returned in: [A, B, C] order (polling order)
```

This violates history ordering invariants - the orchestration would observe completions in a different order than they actually occurred. This is effectively undefined behavior: the history says "B finished, then C, then A" but the orchestration would see them in poll order [A, B, C]. Any logic that depends on completion order (e.g., "process results in the order they finished") would behave incorrectly.

The FIFO rule ensures completion events are consumed in the same order they appear in history, preserving the temporal semantics of the original execution.

---

## Runtime Integration

### The Orchestration Turn Cycle

```
┌────────────────────────────────────────────────────────────────────┐
│                        Runtime Loop                                 │
└────────────────────────────────────────────────────────────────────┘
        │
        │ 1. Fetch work item (with history)
        ▼
┌────────────────────────────────────────────────────────────────────┐
│  run_turn_with_status(history, orchestrator)                        │
│    • Create OrchestrationContext with history                       │
│    • Box::pin(orchestrator(ctx))                                    │
│    • poll_once(future)                                              │
│      ─────────────────────────────────────────────────────────      │
│      │ DurableFuture polls:                                       │ │
│      │  • Claim scheduling events from history                    │ │
│      │  • OR create new events + record Actions                   │ │
│      │  • Look for completion → return Ready or Pending           │ │
│      ─────────────────────────────────────────────────────────      │
│    • Return (history, actions, output, nondeterminism)              │
└────────────────────────────────────────────────────────────────────┘
        │
        │ 2. Process Actions
        │    • CallActivity → enqueue work item
        │    • CreateTimer → schedule timer
        │    • ContinueAsNew → start new execution
        ▼
┌────────────────────────────────────────────────────────────────────┐
│  Commit to Provider                                                 │
│    • Persist history delta                                          │
│    • Update execution status                                        │
│    • Enqueue activity/timer work items                              │
└────────────────────────────────────────────────────────────────────┘
        │
        │ 3. Activity Worker executes, records completion
        │
        │ 4. Next turn fetches updated history...
        ▼
```

### Example: Complete Multi-Turn Lifecycle

Consider a workflow with retry logic:

```rust
async fn retry_workflow(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    for attempt in 1..=3 {
        let result = ctx.schedule_activity("FlakyTask", "").into_activity().await;
        if result.is_ok() {
            return result;
        }
        if attempt < 3 {
            ctx.schedule_timer(Duration::from_secs(1)).into_timer().await;
        }
    }
    Err("all attempts failed".to_string())
}
```

**Turn 1** - First attempt scheduled

```
Provider State: Instance created, execution_id=1
History: [OrchestrationStarted{1}]

Turn 1 execution:
  - for loop starts, attempt=1
  - schedule_activity("FlakyTask") → creates event_id: 2
  - .await → Pending (no completion yet)

History after: [OrchestrationStarted{1}, ActivityScheduled{2}]
Actions: [CallActivity{scheduling_event_id: 2, name: "FlakyTask"}]
Output: None (pending)
```

**Turn 2** - First attempt fails

```
Worker executed FlakyTask → failed
History: [..., ActivityScheduled{2}, ActivityFailed{3, source: 2}]

Turn 2 execution (REPLAY + NEW):
  - for loop starts, attempt=1 (replayed)
  - schedule_activity → CLAIMS event_id: 2
  - .await → finds ActivityFailed → returns Err("...")
  - result.is_ok() → false
  - attempt < 3 → true, schedule timer
  - schedule_timer → creates event_id: 4
  - .await → Pending

History after: [..., ActivityFailed{3}, TimerCreated{4}]
Actions: [CreateTimer{scheduling_event_id: 4, delay_ms: 1000}]
Output: None
```

**Turn 3** - Timer fires, second attempt scheduled

```
Timer dispatcher fired timer
History: [..., TimerCreated{4}, TimerFired{5, source: 4}]

Turn 3 execution (REPLAY + NEW):
  - attempt=1: claims 2, gets failure, schedules timer
  - timer: CLAIMS event_id: 4, finds TimerFired{5} → Ready
  - loop continues, attempt=2
  - schedule_activity("FlakyTask") → creates event_id: 6
  - .await → Pending

History after: [..., TimerFired{5}, ActivityScheduled{6}]
Actions: [CallActivity{scheduling_event_id: 6}]
```

**Turn 4** - Second attempt succeeds

```
Worker executed FlakyTask → succeeded
History: [..., ActivityScheduled{6}, ActivityCompleted{7, source: 6}]

Turn 4 execution (FULL REPLAY):
  - attempt=1: claims 2, fails, timer claims 4, fires
  - attempt=2: claims 6, finds ActivityCompleted{7}
  - result.is_ok() → true, return Ok("success")

History after: [..., ActivityCompleted{7}, OrchestrationCompleted{8}]
Actions: []
Output: Some(Ok("success"))
```

**Final History:**

```
┌────────────────────────────────────────────────────────────────────┐
│ [1] OrchestrationStarted { event_id: 1, ... }                      │
│ [2] ActivityScheduled { event_id: 2, name: "FlakyTask", ... }      │
│ [3] ActivityFailed { event_id: 3, source_event_id: 2, ... }        │
│ [4] TimerCreated { event_id: 4, fire_at_ms: ... }                  │
│ [5] TimerFired { event_id: 5, source_event_id: 4, ... }            │
│ [6] ActivityScheduled { event_id: 6, name: "FlakyTask", ... }      │
│ [7] ActivityCompleted { event_id: 7, source_event_id: 6, ... }     │
│ [8] OrchestrationCompleted { event_id: 8, output: "success" }      │
└────────────────────────────────────────────────────────────────────┘

Claim tracking (Turn 4):
  claimed_scheduling_events: {2, 4, 6}   // All scheduling events
  consumed_completions: {3, 5, 7}        // All completions
```

---

## Rust Async Differences

### 1. Futures Are Created But May Never Be Awaited

In standard Rust async, creating a future and not awaiting it is a bug. In Duroxide, this is normal:

```rust
// Standard Rust: This is wasteful/buggy
let fut = async_operation();  // Created
// fut is dropped without being awaited

// Duroxide: This is fine - select abandons losers
let activity = ctx.schedule_activity("A", "");
let timer = ctx.schedule_timer(Duration::from_secs(5));
let (winner, output) = ctx.select2(activity, timer).await;
// One of these futures is "abandoned" - completely normal
```

**What happens to abandoned futures?**
- They're dropped when the orchestration turn ends
- Their scheduling events ARE recorded in history
- Their completion events may still arrive (runtime handles stale events)
- On replay, the futures still claim their scheduling events (two-phase polling)

### 2. No Waker/Wake Mechanism

Tokio futures use `Waker` to notify the executor when they're ready to make progress. Duroxide uses a **no-op waker**:

```rust
fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> Poll<F::Output> {
    let w = noop_waker();  // Never actually wakes anything
    let mut cx = Context::from_waker(&w);
    fut.as_mut().poll(&mut cx)
}
```

**Implications**:
- You cannot use `tokio::time::sleep()` in orchestration code
- Progress is driven by the runtime polling, not by wake notifications
- Each "turn" is a single poll; the runtime decides when to poll again

### 3. Interior Mutability Pattern

Standard async Rust typically uses `&mut self` for stateful operations. Duroxide uses `Cell` and `RefCell` for interior mutability:

```rust
pub struct DurableFuture {
    claimed_event_id: Cell<Option<u64>>,  // Mutated during poll
    ctx: OrchestrationContext,            // Contains Arc<Mutex<...>>
    kind: Kind,                           // Operation-specific data
}
```

**Why?** The `Future::poll` signature provides `Pin<&mut Self>`, but the `Pin` contract restricts direct mutation of potentially self-referential data. Since `DurableFuture` is `Unpin` (verified at compile time), we could use `&mut`, but interior mutability simplifies the design when multiple futures share context.

### 4. Futures Must Be Unpin

Duroxide's `DurableFuture` MUST implement `Unpin`:

```rust
// Compile-time assertion in futures.rs
const fn assert_unpin<T: Unpin>() {}
const _: () = {
    assert_unpin::<DurableFuture>();
};
```

**Why?** The `poll` implementation uses `unsafe { self.get_unchecked_mut() }` to project through the `Pin`. This is only sound if the type is `Unpin` (meaning it's safe to move after pinning).

**What makes DurableFuture Unpin?** All its fields are `Unpin`:
- `String`: `Unpin`
- `Cell<Option<u64>>`: `Unpin`
- `RefCell<...>`: `Unpin`
- `OrchestrationContext` (contains `Arc<Mutex<...>>`): `Unpin`

### 5. Orchestration Functions Are NOT Long-Lived Futures

Unlike Tokio where a spawned task runs until completion, orchestration futures are:

1. Created fresh for each turn
2. Polled exactly once per turn
3. Dropped at the end of the turn

```rust
// Each turn creates a NEW future
let mut fut = Box::pin(orchestrator(ctx.clone()));
match poll_once(fut.as_mut()) {
    Poll::Ready(out) => { /* done */ }
    Poll::Pending => { /* turn ends, future is dropped */ }
}
// Next turn: create fresh future, it will replay from history
```

**Key insight**: The orchestration function is **deterministic**. Running it with the same history produces the same sequence of operations. This is why we can drop and recreate the future each turn.

### 6. You Can't Use Standard Async Primitives

| Standard Rust/Tokio | Duroxide Equivalent |
|---------------------|---------------------|
| `tokio::time::sleep()` | `ctx.schedule_timer().into_timer().await` |
| `tokio::select!` | `ctx.select2()` or `ctx.select()` |
| `futures::join!` | `ctx.join()` |
| `async { ... }` spawned task | Activity function |
| Channel-based communication | External events |

### 7. Local State IS Rebuilt Through Replay

Unlike what you might initially expect, local variables in orchestration code **do work correctly** because they are deterministically rebuilt through replay each turn:

```rust
// ✅ This works! Counter is rebuilt through replay each turn
let mut counter = 0;
for i in 0..3 {
    ctx.schedule_activity("Task", &i.to_string()).into_activity().await?;
    counter += 1;
}
// counter == 3 when orchestration completes
```

### Example: How Local State Gets Rebuilt

```rust
async fn counting_workflow(ctx: OrchestrationContext, _: String) -> Result<String, String> {
    let mut count = 0;  // Local variable
    
    for _ in 0..3 {
        ctx.schedule_activity("Increment", "").into_activity().await?;
        count += 1;
        println!("Count is now: {}", count);
    }
    
    Ok(format!("Final count: {}", count))
}
```

**Turn 1 (first iteration):**
```
Local state: count = 0
- schedule_activity → creates event_id: 2
- .await → Pending
Turn ends, future dropped
```

**Turn 2 (first activity completes):**
```
Local state: count = 0  ← Starts fresh
- Loop iteration 0 (replay):
  - schedule_activity → CLAIMS event_id: 2
  - .await → Ready (completion found)
  - count = 1, prints "Count is now: 1"
- Loop iteration 1:
  - schedule_activity → creates event_id: 4
  - .await → Pending
Turn ends
```

**Turn 3 (second activity completes):**
```
Local state: count = 0  ← Starts fresh again
- Loop iteration 0 (replay): count becomes 1, prints
- Loop iteration 1 (replay): count becomes 2, prints
- Loop iteration 2: schedules, pending...
```

**Turn 4 (all complete):**
```
Local state: count = 0  ← Starts fresh
- Loop iteration 0 (replay): count = 1, prints
- Loop iteration 1 (replay): count = 2, prints
- Loop iteration 2 (replay): count = 3, prints
- Returns "Final count: 3" ✓
```

**Key insight**: The `count` variable is correctly rebuilt each turn through deterministic replay. The final value is always correct. The code works exactly as you'd expect.

### What to Be Aware Of

The main thing to understand is that **side effects during replay happen multiple times**:

```rust
// The println! executes on EVERY turn during replay
println!("Count is now: {}", count);  // Printed 1+2+3 = 6 times total!
```

This is why:
- **Logging**: Use `ctx.trace()` for durable logging (recorded once in history)
- **External calls**: Always go through activities (not raw HTTP calls in orchestration)
- **Random/time**: Use `ctx.new_guid()` and `ctx.utc_now()` for deterministic values

```rust
// ❌ WRONG: side effects during replay
let id = Uuid::new_v4();  // Different value each turn!
let now = Utc::now();     // Different value each turn!

// ✅ CORRECT: use deterministic helpers
let id = ctx.new_guid().await;   // Same value on replay
let now = ctx.utc_now().await;   // Same value on replay
```

---

## Summary

| Concept | Description |
|---------|-------------|
| **DurableFuture** | Unified future type with common fields (`claimed_event_id`, `ctx`) and operation-specific `kind` |
| **Claim System** | Each future claims a scheduling event from history (or creates new) on first poll |
| **Global Order** | Scheduling events must be claimed in exact history order (nondeterminism detection) |
| **FIFO Completions** | Completion events must be consumed in history order (unless cancelled) |
| **Cancelled Source IDs** | Select2 losers are marked cancelled; their completions are skipped in FIFO checks |
| **Three-Phase Select** | Poll ALL children, find winner, mark losers as cancelled, return |
| **Fixed-Point Join** | Repeatedly poll until all children ready (FIFO may block some) |
| **Turn-Based** | Orchestration polled once per turn; future dropped after turn |
| **No Waker** | Uses noop_waker; progress driven by runtime, not wake notifications |
| **Unpin Required** | DurableFuture must be Unpin for safe Pin projection |

