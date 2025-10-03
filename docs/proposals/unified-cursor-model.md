# Unified Single Cursor Model

## The Simplification

**Instead of**: Separate logic for scheduling discovery vs completion consumption

**Use**: **ONE cursor** that advances sequentially through ALL events

## How It Works

### Single Shared Cursor

```rust
struct CtxInner {
    history: Vec<Event>,
    next_event_index: usize,          // ← Single cursor for ALL events
    next_event_id: u64,               // For creating new events
    claimed_scheduling_events: HashSet<u64>,
    // ...
}
```

The cursor advances forward-only through history, processing ALL events sequentially.

### Unified Polling Algorithm

```rust
impl Future for DurableFuture {
    fn poll(&mut self) -> Poll<DurableOutput> {
        let mut inner = ctx.inner.lock().unwrap();
        
        // Step 1: Claim our scheduling event_id (if not already claimed)
        if self.claimed_event_id.is_none() {
            // Walk cursor forward until we find our scheduling event
            loop {
                if inner.next_event_index >= inner.history.len() {
                    // Reached end without finding scheduling event
                    // This is first execution - create new scheduling event
                    let new_id = inner.next_event_id;
                    inner.next_event_id += 1;
                    
                    inner.history.push(Event::ActivityScheduled {
                        event_id: new_id,
                        name: self.name.clone(),
                        input: self.input.clone(),
                        execution_id: inner.execution_id,
                    });
                    
                    inner.record_action(Action::CallActivity {
                        scheduling_event_id: new_id,
                        name: self.name.clone(),
                        input: self.input.clone(),
                    });
                    
                    self.claimed_event_id.set(Some(new_id));
                    self.scheduled.set(true);
                    // Note: cursor doesn't advance - new event is at end
                    break;
                }
                
                let event = &inner.history[inner.next_event_index];
                
                match event {
                    Event::ActivityScheduled { event_id, name: n, input: inp, .. }
                        if !inner.claimed_scheduling_events.contains(event_id) => {
                        // Found unclaimed scheduling event
                        // CORRUPTION CHECK: Validate match
                        if n != &self.name {
                            panic!("CORRUPTION: Expected activity '{}', found '{}'", self.name, n);
                        }
                        if inp != &self.input {
                            panic!("CORRUPTION: Expected input '{}', found '{}'", self.input, inp);
                        }
                        
                        // Valid! Claim it
                        inner.claimed_scheduling_events.insert(*event_id);
                        self.claimed_event_id.set(Some(*event_id));
                        inner.next_event_index += 1;  // ← Advance cursor past it
                        break;
                    }
                    
                    // Any other event (already claimed scheduling, completions, lifecycle)
                    _ => {
                        inner.next_event_index += 1;  // Skip and continue
                    }
                }
            }
        }
        
        let our_event_id = self.claimed_event_id.get().unwrap();
        
        // Step 2: Look for our completion (STRICT - no skipping completions!)
        loop {
            if inner.next_event_index >= inner.history.len() {
                return Poll::Pending;  // No more events
            }
            
            let event = &inner.history[inner.next_event_index];
            
            match event {
                // Is this OUR completion?
                Event::ActivityCompleted { source_event_id, result, .. }
                    if *source_event_id == our_event_id => {
                    // YES! Consume it
                    validate_completion_matches_scheduling(inner.history, our_event_id, &self.name, &self.input);
                    inner.next_event_index += 1;  // ← Advance cursor
                    return Poll::Ready(DurableOutput::Activity(Ok(result.clone())));
                }
                
                Event::ActivityFailed { source_event_id, error, .. }
                    if *source_event_id == our_event_id => {
                    validate_completion_matches_scheduling(inner.history, our_event_id, &self.name, &self.input);
                    inner.next_event_index += 1;
                    return Poll::Ready(DurableOutput::Activity(Err(error.clone())));
                }
                
                // Is this a completion for SOMEONE ELSE?
                Event::ActivityCompleted { source_event_id, .. }
                | Event::ActivityFailed { source_event_id, .. }
                | Event::TimerFired { source_event_id, .. }
                | Event::SubOrchestrationCompleted { source_event_id, .. }
                | Event::SubOrchestrationFailed { source_event_id, .. } => {
                    // ❌ NON-DETERMINISTIC!
                    panic!(
                        "Non-deterministic: Activity '{}' expected its completion next, \
                         but found completion for event_id={}",
                        self.name, source_event_id
                    );
                }
                
                Event::ExternalEvent { name, .. } => {
                    // Activity waiting but ExternalEvent appeared
                    panic!("Non-deterministic: Activity expected completion but found ExternalEvent('{}')", name);
                }
                
                // Any other event (scheduling, lifecycle) - skip over it
                _ => {
                    inner.next_event_index += 1;
                }
            }
        }
    }
}
```

## Benefits of Single Cursor

1. **Simpler**: One cursor instead of separate logic
2. **Clearer**: Cursor position = "how far we've processed through history"
3. **More strict**: Forces sequential processing of ALL events
4. **Shared state**: All futures share the same cursor, ensuring global ordering
5. **Natural flow**: Cursor just keeps moving forward

## Cursor Lifecycle

**Turn starts**:
- `next_event_index = 0` (reset to beginning)

**During turn**:
- Future A polls → cursor advances to find/claim its scheduling event → cursor at N
- Future B polls → cursor continues from N to find its scheduling event → cursor at M
- Future A polls again → cursor continues from M, skips non-completions, finds its completion → cursor at K
- Future B polls again → cursor continues from K...

**Cursor always moves forward, never backward!**

## Example Trace

```
History:
  0. OrchestrationStarted
  1. ActivityScheduled(event_id=1, name="A", input="x")
  2. ActivityScheduled(event_id=2, name="B", input="y")
  3. TimerCreated(event_id=3, delay=100)
  4. ActivityCompleted(event_id=4, source_event_id=1)  // A completes
  5. TimerFired(event_id=5, source_event_id=3)         // Timer fires
  6. ActivityCompleted(event_id=6, source_event_id=2)  // B completes

Execution trace:
  cursor=0: Start
  
  Poll future_A:
    cursor=0: OrchestrationStarted → skip, cursor=1
    cursor=1: ActivityScheduled(1, "A", "x") → MUST be ours! Validate and claim. cursor=2
    [Now looking for completion]
    cursor=2: ActivityScheduled(2, "B", "y") → skip (not a completion), cursor=3
    cursor=3: TimerCreated(3) → skip, cursor=4
    cursor=4: ActivityCompleted(source=1) → YES! This is ours! cursor=5
    → Returns Ready("a_result")
  
  Poll future_B:
    cursor=5: TimerFired(source=3) → This is a completion but NOT for us!
    ❌ PANIC: Non-deterministic! Activity B expected its completion but timer fired next.

This catches the bug! Activity B can't complete before the timer if the timer fired first in history.
```

## Updated CtxInner

```rust
struct CtxInner {
    history: Vec<Event>,
    next_event_index: usize,              // ← Single unified cursor
    next_event_id: u64,
    claimed_scheduling_events: HashSet<u64>,
    execution_id: u64,
    // ...
}
```

Much simpler! Should I update all three plan documents to use this unified single cursor approach?

