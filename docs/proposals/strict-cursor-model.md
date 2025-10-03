# Strict Cursor Model for Deterministic Replay

## Critical Finding: Current Code Searches for Completions

**Location**: `src/futures.rs:195-200`

The current code **searches** through history to find completions:

```rust
// CURRENT (WRONG) - Searches through history
if let Some(outcome) = inner.history.iter().rev().find_map(|e| match e {
    Event::ActivityCompleted { id: cid, result } if cid == id => Some(Ok(result.clone())),
    _ => None,
}) {
    return Poll::Ready(DurableOutput::Activity(outcome));
}
```

This allows events to be "out of order" like in `tests/unit_tests.rs:correlation_out_of_order_completion`:
```
History:
1. ActivityScheduled(id=1)
2. TimerFired(id=42)          ← Unrelated completion in between!
3. ActivityCompleted(id=1)
```

## The Strict Assertion

**When a future polls looking for its completion, the NEXT event at the cursor position MUST be that completion.**

No searching. No skipping over other completions. Strict sequential consumption.

### What This Means

```rust
// NEW (CORRECT) - Cursor-based, no searching
fn poll(&mut self) -> Poll<DurableOutput> {
    // 1. Get our event_id (from scheduling event in history)
    let event_id = self.get_or_claim_event_id();
    
    // 2. Check ONLY the event at cursor position
    let cursor = ctx.next_completion_index;
    
    if cursor >= history.len() {
        return Poll::Pending;  // No more events
    }
    
    let next_event = &history[cursor];
    
    // 3. Is the NEXT event our completion?
    match next_event {
        Event::ActivityCompleted { source_event_id, result, .. }
            if *source_event_id == event_id => {
            // YES! This is our completion
            // Corruption check: validate name/input match
            validate_completion_matches_scheduling(event_id, &name, &input, history);
            ctx.next_completion_index += 1;
            return Poll::Ready(DurableOutput::Activity(Ok(result.clone())));
        }
        
        Event::ActivityFailed { source_event_id, error, .. }
            if *source_event_id == event_id => {
            // YES! This is our completion (failure)
            validate_completion_matches_scheduling(event_id, &name, &input, history);
            ctx.next_completion_index += 1;
            return Poll::Ready(DurableOutput::Activity(Err(error.clone())));
        }
        
        // Is it a completion for a DIFFERENT future?
        Event::ActivityCompleted { .. }
        | Event::ActivityFailed { .. }
        | Event::TimerFired { .. }
        | Event::SubOrchestrationCompleted { .. }
        | Event::SubOrchestrationFailed { .. } => {
            // ❌ Non-deterministic! Next completion is not for us
            panic!(
                "Non-deterministic execution: future waiting for event_id {} \
                 but next event is {:?}",
                event_id, next_event
            );
        }
        
        // Is it a non-completion event (scheduling, started, etc.)?
        _ => {
            // Skip over non-completion events
            ctx.next_completion_index += 1;
            // Continue polling (recursively or loop)
            return self.poll(...);
        }
    }
}
```

## Key Rules

1. **Can skip over non-completion events** (OrchestrationStarted, ActivityScheduled, TimerCreated, etc.)
2. **CANNOT skip over completion events** (ActivityCompleted, TimerFired, etc.)
3. **Next completion MUST be yours** - otherwise panic with non-deterministic error

## What Changes from Current Behavior

| Scenario | Current (Search) | New (Strict Cursor) |
|----------|------------------|---------------------|
| ActivityScheduled, TimerFired, ActivityCompleted | ✅ Works (searches past TimerFired) | ❌ ERROR: Cannot skip TimerFired completion |
| ActivityScheduled, ActivityCompleted | ✅ Works | ✅ Works (cursor skips scheduling) |
| Multiple scheduled, completions in same order | ✅ Works | ✅ Works (cursor consumes sequentially) |
| Multiple scheduled, completions in DIFFERENT order | ✅ Works (searches) | ❌ ERROR: Non-deterministic |

## Implications

**The test `correlation_out_of_order_completion` will FAIL with the new model!**

This is actually GOOD - it catches non-deterministic execution! Here's why:

```
History:
1. ActivityScheduled(id=1) for activity A
2. TimerFired(id=42) for timer T
3. ActivityCompleted(id=1) for activity A

Replay:
- Poll activity A future
- Cursor at position 1 (skipped past ActivityScheduled during discovery)
- Next event is TimerFired(id=42)
- ❌ ERROR: This is a completion, but not for us!
```

The question: **Why is TimerFired at position 2?**

Two possibilities:
1. The orchestration code scheduled activity A, then timer T, and A completed before T fired
   - If so, the orchestration must have been waiting on both simultaneously (select/join)
   - And during replay, BOTH futures would be polling
   - The cursor model would require timer T's future to consume TimerFired first

2. This history was manually constructed for the test and doesn't represent real execution
   - In real execution, events are added in a specific order based on turn execution
   - The test may be demonstrating a scenario that shouldn't actually happen

## The Real Execution Model

During actual orchestration execution, events are added in turns:

**Turn 1**: Orchestration polls
- Schedules activity A → ActivityScheduled added
- Schedules timer T → TimerCreated added
- Returns Pending
- History: [ActivityScheduled, TimerCreated]

**Turn 2**: Activity A completes
- Incoming message: ActivityCompleted(id=1)
- Added to history: ActivityCompleted
- Orchestration polls, consumes ActivityCompleted
- History: [ActivityScheduled, TimerCreated, ActivityCompleted]

**Turn 3**: Timer fires
- Incoming message: TimerFired(id=42)
- Added to history: TimerFired
- History: [ActivityScheduled, TimerCreated, ActivityCompleted, TimerFired]

**Cursor behavior**:
- When activity future A polls in Turn 2
- Cursor starts at 0
- Skip ActivityScheduled (not a completion)
- Skip TimerCreated (not a completion)  
- Find ActivityCompleted → consume it ✓

So we DO need to skip over non-completion events (scheduling events).

**But we CANNOT skip over other completions!**

## Refined Algorithm - Full Cursor Model

### Scheduling Discovery (Strict Cursor - Next MUST Be Ours)

```rust
// When claiming our scheduling event
fn claim_scheduling_event(&self, inner: &mut CtxInner) -> u64 {
    if let Some(id) = self.claimed_event_id.get() {
        return id;  // Already claimed
    }
    
    // Use shared cursor to find next ActivityScheduled
    loop {
        if inner.next_event_index >= inner.history.len() {
            // Reached end - create new scheduling event (first execution)
            let new_id = inner.next_event_id;
            inner.next_event_id += 1;
            inner.history.push(Event::ActivityScheduled {
                event_id: new_id,
                name: self.name.clone(),
                input: self.input.clone(),
                execution_id: inner.execution_id,
            });
            self.claimed_event_id.set(Some(new_id));
            return new_id;
        }
        
        let event = &inner.history[inner.next_event_index];
        
        // Is this an ActivityScheduled?
        if let Event::ActivityScheduled { event_id, name, input, .. } = event {
            // STRICT: Next ActivityScheduled MUST be ours!
            // No "if unclaimed" check - cursor already past claimed ones
            if name != &self.name {
                panic!(
                    "NON-DETERMINISTIC: Expected activity '{}', found '{}' at event_id={}",
                    self.name, name, event_id
                );
            }
            if input != &self.input {
                panic!(
                    "NON-DETERMINISTIC: Expected input '{}', found '{}' at event_id={}",
                    self.input, input, event_id
                );
            }
            
            // Valid! Claim and advance
            self.claimed_event_id.set(Some(*event_id));
            inner.next_event_index += 1;  // Cursor moves past it
            return *event_id;
        }
        
        // Not ActivityScheduled - skip it
        inner.next_event_index += 1;
    }
}
```

**No HashSet needed** - cursor position tracks what's been processed!

### Completion Consumption (Strict Cursor - No Skipping!)

```rust
// When polling for completion
loop {
    if cursor >= history.len() {
        return Poll::Pending;
    }
    
    let event = &history[cursor];
    
    // Check if this is our completion
    if is_completion_for_us(event, our_event_id) {
        // CORRUPTION CHECK: Validate scheduling event matches
        validate_scheduling_matches(history, our_event_id, &name, &input);
        ctx.cursor += 1;
        return Poll::Ready(extract_value(event));
    }
    
    // Check if this is ANY completion event
    if is_completion_event(event) {
        // It's a completion but NOT for us
        // ❌ NON-DETERMINISTIC! Cannot skip over it
        panic!("Non-deterministic: next completion is for event_id={:?}, not ours={}", 
               extract_source_event_id(event), our_event_id);
    }
    
    // It's a non-completion event (scheduling, lifecycle, etc.)
    // ✓ Can skip over these
    ctx.cursor += 1;
}
```

## Key Insight

**Scheduling events**: We iterate through history to find the right one, with corruption checks
**Completion events**: Strict cursor - next completion MUST be ours

Both use iteration/cursor, but with different strictness levels!

## Validation Check

Additionally, when we DO find our completion, validate it matches:

```rust
// After finding ActivityCompleted(source_event_id=X)
// Find the ActivityScheduled(event_id=X) and verify:
let scheduling_event = history.iter().find(|e| match e {
    Event::ActivityScheduled { event_id, .. } if *event_id == X => true,
    _ => false,
}).expect("scheduling event must exist");

if let Event::ActivityScheduled { name: sched_name, input: sched_input, .. } = scheduling_event {
    // Corruption check
    assert_eq!(sched_name, &self.name, "Corruption: activity name mismatch");
    assert_eq!(sched_input, &self.input, "Corruption: activity input mismatch");
}
```

This catches corruption where the history was tampered with or events got mixed up.

## Summary

**Your assertion is correct**:
- Skip over non-completion events (scheduling, lifecycle) ✓
- CANNOT skip over completion events ❌
- Next completion MUST be for the waiting future
- Add corruption validation (name/input match)

The current code's `.find_map()` search is too permissive and allows non-deterministic scenarios!

