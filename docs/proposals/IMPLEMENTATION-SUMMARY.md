# Event ID Cleanup - Implementation Summary

## Final Design (After Full Code Review)

### Core Changes

1. **Add `event_id` to ALL events** - monotonic position in history (starts at 1)
2. **Remove `id` from scheduling events** - `event_id` is THE id
3. **Add `source_event_id` to completions** - references scheduling event (except ExternalEvent)
4. **Single cursor** (`next_event_index`) - advances through ALL events
5. **No HashSets** - cursor position tracks what's processed
6. **Remove CompletionMap** - ~840 lines deleted!

### Event Model

```rust
pub enum Event {
    // Lifecycle - just event_id
    OrchestrationStarted { event_id: u64, name: String, version: String, input: String, ... },
    OrchestrationCompleted { event_id: u64, output: String },
    OrchestrationFailed { event_id: u64, error: String },
    
    // Scheduling - event_id is THE id (no separate correlation_id)
    ActivityScheduled { event_id: u64, name: String, input: String, execution_id: u64 },
    TimerCreated { event_id: u64, fire_at_ms: u64, execution_id: u64 },
    ExternalSubscribed { event_id: u64, name: String },
    SubOrchestrationScheduled { event_id: u64, name: String, instance: String, input: String, execution_id: u64 },
    
    // Completion - event_id + source_event_id
    ActivityCompleted { event_id: u64, source_event_id: u64, result: String },
    ActivityFailed { event_id: u64, source_event_id: u64, error: String },
    TimerFired { event_id: u64, source_event_id: u64, fire_at_ms: u64 },
    SubOrchestrationCompleted { event_id: u64, source_event_id: u64, result: String },
    SubOrchestrationFailed { event_id: u64, source_event_id: u64, error: String },
    
    // Exception: External events (no source_event_id - external callers don't know it)
    ExternalEvent { event_id: u64, name: String, data: String },
    
    // ...
}
```

### CtxInner (Massively Simplified)

```rust
struct CtxInner {
    history: Vec<Event>,
    next_event_id: u64,        // For creating new events
    next_event_index: usize,   // Single cursor for ALL events
    actions: Vec<Action>,
    execution_id: u64,
    turn_index: u64,
    
    // REMOVED (no longer needed):
    // - next_correlation_id
    // - claimed_activity_ids, claimed_timer_ids, claimed_external_ids
    // - claimed_scheduling_events (cursor replaces this!)
}
```

### DurableFuture Kind

```rust
pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
        claimed_event_id: Cell<Option<u64>>,  // Discovered during first poll
        ctx: OrchestrationContext,
        // REMOVED: id (carried upfront), scheduled flag not needed
    },
    // Similar for Timer, External, SubOrch
}
```

### The Unified Cursor Algorithm

**When any future polls:**

```rust
fn poll(&mut self) -> Poll<DurableOutput> {
    let mut inner = ctx.inner.lock().unwrap();
    
    // PHASE 1: Claim our scheduling event (if not already claimed)
    if self.claimed_event_id.is_none() {
        loop {
            if cursor >= history.len() {
                // Create new event (first execution)
                break;
            }
            
            let event = &history[cursor];
            
            if let Event::ActivityScheduled { event_id, name, input, .. } = event {
                // STRICT: Next ActivityScheduled MUST be ours!
                assert_eq!(name, &self.name, "NON-DETERMINISTIC");
                assert_eq!(input, &self.input, "NON-DETERMINISTIC");
                
                self.claimed_event_id.set(Some(*event_id));
                cursor += 1;  // Advance past it
                break;
            }
            
            cursor += 1;  // Skip non-ActivityScheduled events
        }
    }
    
    let our_event_id = self.claimed_event_id.get().unwrap();
    
    // PHASE 2: Look for our completion (same cursor continues)
    loop {
        if cursor >= history.len() {
            return Poll::Pending;
        }
        
        let event = &history[cursor];
        
        match event {
            // Our completion?
            Event::ActivityCompleted { source_event_id, result, .. } 
                if *source_event_id == our_event_id => {
                validate_corruption(history, our_event_id, &self.name, &self.input);
                cursor += 1;
                return Poll::Ready(Ok(result.clone()));
            }
            
            Event::ActivityFailed { source_event_id, error, .. }
                if *source_event_id == our_event_id => {
                validate_corruption(history, our_event_id, &self.name, &self.input);
                cursor += 1;
                return Poll::Ready(Err(error.clone()));
            }
            
            // ANY other completion?
            Event::ActivityCompleted { .. } | Event::ActivityFailed { .. }
            | Event::TimerFired { .. } | Event::ExternalEvent { .. }
            | Event::SubOrchestrationCompleted { .. } | Event::SubOrchestrationFailed { .. } => {
                panic!("NON-DETERMINISTIC: Next completion not for us!");
            }
            
            // Non-completion - skip it
            _ => cursor += 1,
        }
    }
}
```

## Key Rules

### What You CAN Skip Over
- ✓ OrchestrationStarted, OrchestrationCompleted, OrchestrationFailed
- ✓ ActivityScheduled, TimerCreated, ExternalSubscribed (scheduling events)
- ✓ OrchestrationChained, OrchestrationContinuedAsNew, OrchestrationCancelRequested

### What You CANNOT Skip Over
- ❌ ActivityCompleted, ActivityFailed
- ❌ TimerFired
- ❌ ExternalEvent
- ❌ SubOrchestrationCompleted, SubOrchestrationFailed

**Any completion event that's not yours = non-deterministic error!**

## Bugs Fixed

### Bug #1: Same Name+Input Collision
**Before**: Two `schedule_activity("Process", "data")` calls could adopt same ID
**After**: Each gets unique `event_id` based on position in history

### Bug #2: Searching for Completions  
**Before**: Code searches through history with `.iter().rev().find_map()`
**After**: Strict cursor - next completion MUST be ours or panic

## Code Simplifications

| Removed | Reason |
|---------|--------|
| `next_correlation_id` | event_id serves this purpose |
| `claimed_activity_ids` | Cursor tracks processed events |
| `claimed_timer_ids` | Cursor tracks processed events |
| `claimed_external_ids` | Cursor tracks processed events |
| `claimed_scheduling_events` | Cursor tracks processed events |
| `CompletionMap` (~280 lines) | History IS the queue |
| `completion_map_tests.rs` (~300 lines) | No longer needed |
| `completion_aware_futures.rs` (~260 lines) | No longer needed |
| `find_history_index()` | Cursor replaces search |
| `synth_output_from_history()` | Cursor replaces search |
| **Total: ~850+ lines removed!** | |

## Files to Modify

### Core changes:
- `src/lib.rs` - Event enum, CtxInner, schedule methods, DurableFuture
- `src/futures.rs` - Unified cursor polling (remove searching)
- `src/runtime/orchestration_turn.rs` - Remove CompletionMap, convert messages to events
- `src/providers/mod.rs` - WorkItem updates (add source_event_id)
- `src/providers/sqlite.rs` - Schema change (sequence_num → event_id)
- `src/runtime/dispatch.rs` - Pass scheduling_event_id
- `src/runtime/router.rs` - OrchestratorMsg updates
- `src/runtime/execution.rs` - Update event creation
- `src/runtime/mod.rs` - Remove CompletionMap imports

### Files to DELETE:
- `src/runtime/completion_map.rs`
- `src/runtime/completion_map_tests.rs`
- `src/runtime/completion_aware_futures.rs`

### Tests to update:
- ALL test files (~30 files) - update Event construction
- Remove/update `correlation_out_of_order_completion` test
- Update tests that check CompletionMap

### Documentation:
- `docs/architecture.md`
- `docs/dtf-runtime-design.md`
- All examples in `examples/`

## Timeline

~10 days of focused work

## Breaking Changes

- Event serialization format changes (database wipe required)
- Event struct fields renamed/added
- WorkItem struct fields renamed
- OrchestratorMsg struct fields renamed
- Test `correlation_out_of_order_completion` removed (demonstrates non-determinism)

## Implementation Order

1. Update Event enum (add event_id, source_event_id, remove id)
2. Update CtxInner (add cursor, remove HashSets)
3. Update DurableFuture polling (unified cursor algorithm)
4. Remove CompletionMap files
5. Update WorkItem, OrchestratorMsg, Actions
6. Update database schema
7. Update all tests
8. Update documentation

## Validation

After implementation, verify:
- ✓ Two activities with same name+input get different event_ids
- ✓ Cannot skip over completions (panic with clear message)
- ✓ Corruption checks catch name/input mismatches
- ✓ History cursor advances correctly
- ✓ All tests pass (except correlation_out_of_order_completion)

