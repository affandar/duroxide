# Event ID Implementation Details

## Critical: How DurableFuture Finds Its Event ID

### The Challenge

When `ctx.schedule_activity("Process", "data")` is called:
1. It returns a `DurableFuture` **immediately**
2. The `ActivityScheduled` event is NOT added to history yet (added when polled)
3. The `event_id` is NOT assigned yet (assigned when added to history)
4. But the future needs to know which completion to match against

### The Solution: Discovery During First Poll

The `DurableFuture` does NOT carry `event_id` directly. Instead:

```rust
pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
        claimed_event_id: Cell<Option<u64>>,  // ← Discovered lazily
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    // Similar for other kinds
}
```

### Polling Algorithm (Activity Example)

When `DurableFuture` is polled:

```rust
fn poll(&mut self) -> Poll<DurableOutput> {
    let mut inner = ctx.inner.lock().unwrap();
    
    // Step 1: Get or discover event_id from SCHEDULING events using CURSOR
    // Use a separate cursor for scheduling discovery (or reuse the same one)
    let event_id = if let Some(id) = self.claimed_event_id.get() {
        id  // Already claimed in previous poll
    } else {
        // Use cursor to find next unclaimed ActivityScheduled
        let mut found_event_id = None;
        
        for event in &inner.history {
            match event {
                Event::ActivityScheduled { event_id, name: n, input: inp, .. }
                    if !inner.claimed_scheduling_events.contains(event_id) => {
                    // Found an unclaimed scheduling event
                    // CORRUPTION CHECK: Verify it matches what we expected
                    if n != &self.name {
                        panic!(
                            "CORRUPTION/CODE CHANGE: Expected to schedule activity '{}' \
                             but found '{}' at event_id={}",
                            self.name, n, event_id
                        );
                    }
                    if inp != &self.input {
                        panic!(
                            "CORRUPTION/CODE CHANGE: Expected activity '{}' with input '{}' \
                             but found input '{}' at event_id={}",
                            self.name, self.input, inp, event_id
                        );
                    }
                    // Match! Claim this event_id
                    found_event_id = Some(*event_id);
                    break;
                }
                _ => continue,
            }
        }
        
        if let Some(id) = found_event_id {
            // Found existing event - claim it
            inner.claimed_scheduling_events.insert(id);
            self.claimed_event_id.set(Some(id));
            id
        } else {
            // No existing event - create new one (first execution, not replay)
            let new_id = inner.next_event_id;
            inner.next_event_id += 1;
            
            inner.history.push(Event::ActivityScheduled {
                event_id: new_id,
                name: self.name.clone(),
                input: self.input.clone(),
                execution_id: inner.execution_id,
            });
            
            inner.record_action(Action::CallActivity {
                scheduling_event_id: new_id,  // ← Pass event_id to dispatcher
                name: self.name.clone(),
                input: self.input.clone(),
            });
            
            inner.claimed_scheduling_events.insert(new_id);
            self.claimed_event_id.set(Some(new_id));
            self.scheduled.set(true);
            new_id
        }
    };
    
    // Step 2: Check for completion using STRICT SEQUENTIAL consumption with cursor
    // CRITICAL: We CANNOT search for completions! The next completion in history
    // MUST be for this future, otherwise it's non-deterministic.
    //
    // We use a simple cursor (next_completion_index) that points to the next
    // completion event to consume. This ensures FIFO ordering.
    
    // Check if next event at cursor is our completion (STRICT - no searching!)
    loop {
        if inner.next_completion_index >= inner.history.len() {
            return Poll::Pending;  // No more events
        }
        
        let event = &inner.history[inner.next_completion_index];
        
        match event {
            // Is this OUR completion?
            Event::ActivityCompleted { source_event_id, result, .. }
                if *source_event_id == event_id => {
                // YES! Validate it matches our scheduling event (corruption check)
                validate_activity_match(&inner.history, event_id, &name, &input);
                inner.next_completion_index += 1;
                return Poll::Ready(DurableOutput::Activity(Ok(result.clone())));
            }
            Event::ActivityFailed { source_event_id, error, .. }
                if *source_event_id == event_id => {
                // YES! Validate and consume
                validate_activity_match(&inner.history, event_id, &name, &input);
                inner.next_completion_index += 1;
                return Poll::Ready(DurableOutput::Activity(Err(error.clone())));
            }
            
            // Is this a completion for SOMEONE ELSE?
            Event::ActivityCompleted { source_event_id, .. }
            | Event::ActivityFailed { source_event_id, .. }
            | Event::TimerFired { source_event_id, .. }
            | Event::SubOrchestrationCompleted { source_event_id, .. }
            | Event::SubOrchestrationFailed { source_event_id, .. } => {
                // ❌ NON-DETERMINISTIC!
                // Next completion is for a different future (source_event_id != our event_id)
                // We CANNOT skip over it - that would break deterministic replay
                panic!(
                    "Non-deterministic execution detected: \
                     Activity future (event_id={}, name='{}', input='{}') \
                     expected its completion next, but found completion for event_id={}",
                    event_id, name, input, source_event_id
                );
            }
            
            // Special case: ExternalEvent (no source_event_id)
            Event::ExternalEvent { name: event_name, .. } => {
                // If we're waiting for this external event by name, consume it
                // Otherwise, it's non-deterministic (activity waiting but external event appears)
                panic!(
                    "Non-deterministic execution: \
                     Activity future expected completion but found ExternalEvent(name='{}')",
                    event_name
                );
            }
            
            // Not a completion - skip over scheduling/lifecycle events
            Event::OrchestrationStarted { .. }
            | Event::OrchestrationCompleted { .. }
            | Event::OrchestrationFailed { .. }
            | Event::ActivityScheduled { .. }
            | Event::TimerCreated { .. }
            | Event::ExternalSubscribed { .. }
            | Event::SubOrchestrationScheduled { .. }
            | Event::OrchestrationChained { .. }
            | Event::OrchestrationContinuedAsNew { .. }
            | Event::OrchestrationCancelRequested { .. } => {
                // ✓ Can skip over non-completion events
                inner.next_completion_index += 1;
                continue;  // Check next event
            }
        }
    }
}

// Corruption validation helper
fn validate_activity_match(history: &[Event], scheduling_event_id: u64, expected_name: &str, expected_input: &str) {
    // Find the ActivityScheduled event
    if let Some(sched) = history.iter().find(|e| match e {
        Event::ActivityScheduled { event_id, .. } if *event_id == scheduling_event_id => true,
        _ => false,
    }) {
        if let Event::ActivityScheduled { name, input, .. } = sched {
            assert_eq!(
                name, expected_name,
                "CORRUPTION: ActivityCompleted references scheduling event with different name"
            );
            assert_eq!(
                input, expected_input,
                "CORRUPTION: ActivityCompleted references scheduling event with different input"
            );
        }
    } else {
        panic!(
            "CORRUPTION: ActivityCompleted references non-existent scheduling event_id={}",
            scheduling_event_id
        );
    }

}
```

### Why This Fixes The Bug

**Before (current broken code)**:
```
Turn 1: schedule_activity("Process", "data")
  → Allocates ID 1
  → Adds ActivityScheduled(id=1, name="Process", input="data")
  
Turn 2: schedule_activity("Process", "data")  
  → Finds ActivityScheduled with name="Process", input="data"
  → Adopts ID 1  ← BUG! Both futures have same ID
```

**After (new design)**:
```
Turn 1: schedule_activity("Process", "data")
  → Poll: No existing event found
  → Adds ActivityScheduled(event_id=5, name="Process", input="data")
  → Claims event_id=5
  → Future waits for completion with source_event_id=5
  
Turn 2: schedule_activity("Process", "data")
  → Poll: Finds ActivityScheduled(event_id=5)
  → But event_id=5 is already claimed!
  → Adds NEW ActivityScheduled(event_id=8, name="Process", input="data")
  → Claims event_id=8
  → Future waits for completion with source_event_id=8

Completion 1 arrives: ActivityCompleted(event_id=12, source_event_id=5)
  → Matches first activity ✓

Completion 2 arrives: ActivityCompleted(event_id=15, source_event_id=8)
  → Matches second activity ✓
```

Each scheduling event gets a **unique position in history** (event_id), even with identical name+input!

## Implementation Changes Needed

### 1. Update CtxInner

```rust
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,
    next_event_id: u64,                                    // ← NEW
    next_correlation_id: u64,                              // REMOVE (not needed)
    execution_id: u64,
    turn_index: u64,
    claimed_scheduling_events: HashSet<u64>,               // ← NEW: claimed event_ids
    // Remove: claimed_activity_ids, claimed_timer_ids, etc.
}
```

### 2. Initialize next_event_id from History

```rust
impl CtxInner {
    fn new(history: Vec<Event>, execution_id: u64) -> Self {
        let next_event_id = history.last()
            .map(|e| e.event_id() + 1)
            .unwrap_or(1);
        
        Self {
            history,
            actions: Vec::new(),
            next_event_id,
            execution_id,
            turn_index: 0,
            claimed_scheduling_events: Default::default(),
            // ...
        }
    }
}
```

### 3. Update DurableFuture Kind

```rust
pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
        claimed_event_id: Cell<Option<u64>>,  // ← NEW
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    Timer {
        delay_ms: u64,
        claimed_event_id: Cell<Option<u64>>,  // ← NEW
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    External {
        name: String,
        claimed_event_id: Cell<Option<u64>>,  // ← NEW
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    SubOrch {
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
        claimed_event_id: Cell<Option<u64>>,  // ← NEW
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
}
```

### 4. Update Action to Carry event_id

```rust
pub enum Action {
    CallActivity {
        scheduling_event_id: u64,  // ← NEW: event_id of ActivityScheduled
        name: String,
        input: String,
    },
    CreateTimer {
        scheduling_event_id: u64,  // ← NEW: event_id of TimerCreated
        delay_ms: u64,
    },
    WaitExternal {
        scheduling_event_id: u64,  // ← NEW: event_id of ExternalSubscribed
        name: String,
    },
    StartSubOrchestration {
        scheduling_event_id: u64,  // ← NEW: event_id of SubOrchestrationScheduled
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
    },
    // StartOrchestrationDetached: no change needed (fire-and-forget)
    // ContinueAsNew: no change needed
}
```

### 5. Update Dispatchers

Dispatchers receive the `scheduling_event_id` from the Action and pass it to WorkItems:

```rust
// In src/runtime/dispatch.rs
pub async fn dispatch_call_activity(
    rt: &Arc<Runtime>,
    instance: &str,
    history: &[Event],
    scheduling_event_id: u64,  // ← From Action
    name: String,
    input: String,
) {
    // Check if already completed by source_event_id
    let already_done = history.iter().any(|e| match e {
        Event::ActivityCompleted { source_event_id, .. } 
            if *source_event_id == scheduling_event_id => true,
        Event::ActivityFailed { source_event_id, .. } 
            if *source_event_id == scheduling_event_id => true,
        _ => false,
    });
    
    if already_done {
        return;
    }
    
    rt.history_store.enqueue_worker_work(WorkItem::ActivityExecute {
        instance: instance.to_string(),
        execution_id,
        scheduling_event_id,  // ← Pass to worker
        name,
        input,
    }).await;
}
```

## Replay Determinism Guarantee

The new design guarantees replay determinism through **strict sequential completion consumption**:

### Unified Cursor Strategy with Corruption Checking

**Both scheduling discovery and completion consumption use cursor-based traversal**, but with different rules:

**Single Cursor for Everything**:
- ONE cursor (`next_event_index`) advances through ALL events
- Shared across all futures - global sequential ordering
- When looking for `ActivityScheduled`: next one at cursor MUST be ours (validate name/input)
- When looking for completion: next completion MUST be ours (cannot skip)
- Can skip over: non-scheduling events (when claiming) and non-completion events (when waiting)
- **Cannot skip**: ActivityScheduled when claiming, or ANY completion when waiting

**Key insight**: 
- No "searching" or "finding unclaimed"
- Next event of the type we're looking for MUST match what we expect
- Cursor position = what's been processed (no HashSet needed!)

### Why This Works

1. **Event IDs are sequential**: Position in history determines event_id
2. **Replay sees same history**: Same events in same order → same event_ids
3. **Scheduling can be discovered**: Orchestration code deterministically schedules operations
4. **Completions must be sequential**: Next completion must match next future waiting
5. **Cursor enforces order**: `next_completion_index` ensures FIFO consumption of completions

### Example: Sequential Consumption

```rust
History:
  1. OrchestrationStarted
  2. ActivityScheduled(event_id=2, name="A", input="x")    // First schedule
  3. ActivityScheduled(event_id=3, name="A", input="x")    // Second schedule (identical!)
  4. ActivityCompleted(event_id=4, source_event_id=2)      // Completes first
  5. ActivityCompleted(event_id=5, source_event_id=3)      // Completes second

Replay execution:
  Turn 1:
    - f1 = schedule_activity("A", "x")
    - f2 = schedule_activity("A", "x")
    - Poll f1: Finds ActivityScheduled(event_id=2), claims it
    - Poll f2: Finds ActivityScheduled(event_id=3), claims it (event_id=2 already claimed!)
    - Poll f1 again: Checks next completion → ActivityCompleted(source_event_id=2) ✓ Match!
    - Poll f2 again: Checks next completion → ActivityCompleted(source_event_id=3) ✓ Match!

Non-deterministic case (error):
  History:
    ...
    4. ActivityCompleted(event_id=4, source_event_id=3)   // Second completed first!
    5. ActivityCompleted(event_id=5, source_event_id=2)
    
  Replay:
    - Poll f1 (waiting for source_event_id=2)
    - Next completion is source_event_id=3
    - ❌ ERROR: Non-deterministic execution! Next completion doesn't match waiting future.
```

Even with identical operations:
- `schedule_activity("A", "x")` at position 2 → event_id=2
- `schedule_activity("A", "x")` at position 3 → event_id=3
- Completions reference by source_event_id (2 vs 3), consumed sequentially
- Order of completion determines which future completes first

## Key Insights

### 1. Event IDs Represent Position, Not Identity

**Event IDs represent WHEN an operation was scheduled (position in history), not WHAT was scheduled (name+input).**

This is fundamentally different from correlation IDs, which tried to represent "what" and led to collisions with duplicate operations.

### 2. Two-Phase Matching: Search vs Sequential

**Scheduling events**: CAN search/discover in history
- Deterministic based on orchestration code path
- Matching by (name, input) with claimed set prevents reuse

**Completion events**: MUST consume sequentially
- The next completion in history MUST be for the next waiting future
- Cannot skip over completions - that would be non-deterministic
- CompletionMap enforces strict FIFO ordering

### 3. Simple Cursor Enforces Sequential Consumption

Instead of a complex `CompletionMap`, we just need a **cursor** over history:

```rust
struct CtxInner {
    history: Vec<Event>,               // Combined: baseline + delta during turn
    next_event_index: usize,           // ← Single cursor for ALL events
    next_event_id: u64,                // For assigning IDs to new events
    execution_id: u64,
    // No claimed_scheduling_events HashSet needed!
    // ...
}
```

When a future polls for completion:
1. Check `history[next_completion_index]`
2. If it's a completion for this future → consume it, increment cursor
3. If it's a completion for different future → return Pending (can't skip!)
4. If it's not a completion event → increment cursor, continue

**Critical**: The cursor walks the **combined history** (baseline + newly added events from this turn):
- `prep_completions()` converts incoming messages to Events and adds them to history
- Futures poll and consume these newly-added completion events via cursor
- Cursor advances as futures consume, enabling sequential execution

This is simpler and more explicit than CompletionMap!

**Note**: The existing `CompletionMap` in `src/runtime/completion_map.rs` is useful for the runtime's orchestrator worker pattern (batching incoming messages), but it's not a core requirement for deterministic replay. A cursor is sufficient.

