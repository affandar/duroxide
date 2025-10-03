# Remove CompletionMap - Simplify with Cursor

## What CompletionMap Currently Does

From analyzing the code:

1. **Batches incoming messages** (`OrchestratorMsg`) before orchestration turn
2. **Orders them by arrival time** (not by correlation ID)
3. **Enforces sequential consumption** via `is_next_ready()` and `get_ready_completion()`
4. **Detects duplicates** (same correlation_id arriving twice)
5. **Tracks ack tokens** for provider acknowledgment
6. **Handles external events** specially (maps name → correlation_id via history lookup)

**Files involved:**
- `src/runtime/completion_map.rs` - The map implementation (~280 lines)
- `src/runtime/completion_map_tests.rs` - Tests (~300 lines)
- `src/runtime/completion_aware_futures.rs` - Thread-local accessor (~260 lines)
- `src/runtime/orchestration_turn.rs` - Uses the map during turn execution
- `src/futures.rs` - Tries completion map first, falls back to history

**Total: ~840+ lines of code to remove!**

## Why We Don't Need It

### Core Insight: History IS the Ordered Queue

The CompletionMap exists to provide:
1. **Arrival ordering** - But we can add events to history in arrival order!
2. **Sequential consumption** - But a cursor over history does this!
3. **Duplicate detection** - But we can check history when converting messages!

**Simplified approach:**
```rust
// Instead of adding to CompletionMap, just add to history
fn prep_completions(&mut self, messages: Vec<OrchestratorMsg>) -> Vec<String> {
    let mut ack_tokens = Vec::new();
    
    for msg in messages {
        // Check for duplicates/filtering
        if self.should_skip_message(&msg) {
            ack_tokens.push(extract_ack_token(&msg));
            continue;
        }
        
        // Convert message to Event with event_id assigned
        let event = self.msg_to_event(msg);  // Assigns next event_id
        self.history_delta.push(event);
        ack_tokens.push(extract_ack_token(&msg));
    }
    
    ack_tokens
}
```

Then futures just use the cursor to consume sequentially!

## Replacement Design

### 1. CtxInner Gets a Cursor

```rust
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,
    next_event_id: u64,
    next_event_index: usize,             // ← SINGLE cursor for ALL events
    execution_id: u64,
    turn_index: u64,
    // No HashSets needed! Cursor position tracks what's processed
}
```

**Key simplifications**:
- All futures share the SAME cursor - ensures global sequential ordering
- No `claimed_scheduling_events` HashSet - cursor already moved past claimed events
- Simpler state, clearer semantics!

### 2. Messages Convert Directly to Events

In `OrchestrationTurn::prep_completions()`:

```rust
pub fn prep_completions(&mut self, messages: Vec<OrchestratorMsg>) -> Vec<String> {
    let mut ack_tokens = Vec::new();
    
    for msg in messages {
        // Filter out wrong execution, duplicates, etc.
        if !self.is_valid_completion(&msg) {
            ack_tokens.push(extract_ack_token(&msg));
            continue;
        }
        
        // Convert to Event and add to history_delta
        let event = match msg {
            OrchestratorMsg::ActivityCompleted { source_event_id, result, .. } => {
                Event::ActivityCompleted {
                    event_id: 0,  // Will be assigned
                    source_event_id,
                    result,
                }
            }
            OrchestratorMsg::ActivityFailed { source_event_id, error, .. } => {
                Event::ActivityFailed {
                    event_id: 0,
                    source_event_id,
                    error,
                }
            }
            OrchestratorMsg::TimerFired { source_event_id, fire_at_ms, .. } => {
                Event::TimerFired {
                    event_id: 0,
                    source_event_id,
                    fire_at_ms,
                }
            }
            OrchestratorMsg::ExternalByName { name, data, .. } => {
                Event::ExternalEvent {
                    event_id: 0,
                    name,
                    data,
                }
            }
            OrchestratorMsg::SubOrchCompleted { source_event_id, result, .. } => {
                Event::SubOrchestrationCompleted {
                    event_id: 0,
                    source_event_id,
                    result,
                }
            }
            OrchestratorMsg::SubOrchFailed { source_event_id, error, .. } => {
                Event::SubOrchestrationFailed {
                    event_id: 0,
                    source_event_id,
                    error,
                }
            }
            OrchestratorMsg::CancelRequested { reason, .. } => {
                Event::OrchestrationCancelRequested {
                    event_id: 0,
                    reason,
                }
            }
        };
        
        // Assign event_id and add to history
        let event_id = self.next_event_id();
        event.set_event_id(event_id);
        self.history_delta.push(event);
        
        ack_tokens.push(extract_ack_token(&msg));
    }
    
    ack_tokens
}
```

### 3. DurableFuture Uses Cursor for Completion

In `src/futures.rs`, the polling logic becomes:

```rust
fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<DurableOutput> {
    let this = unsafe { self.get_unchecked_mut() };
    let mut inner = ctx.inner.lock().unwrap();
    
    // Get or discover event_id from scheduling events (can search)
    let event_id = self.get_or_claim_event_id(&mut inner);
    
    // Check for completion using cursor (strict sequential, cannot search)
    while inner.next_completion_index < inner.history.len() {
        let event = &inner.history[inner.next_completion_index];
        
        match event {
            // Is this completion for us?
            Event::ActivityCompleted { source_event_id, result, .. }
                if *source_event_id == event_id => {
                inner.next_completion_index += 1;
                return Poll::Ready(DurableOutput::Activity(Ok(result.clone())));
            }
            Event::ActivityFailed { source_event_id, error, .. }
                if *source_event_id == event_id => {
                inner.next_completion_index += 1;
                return Poll::Ready(DurableOutput::Activity(Err(error.clone())));
            }
            
            // Is this a completion for someone else?
            Event::ActivityCompleted { .. }
            | Event::ActivityFailed { .. }
            | Event::TimerFired { .. }
            | Event::ExternalEvent { .. }
            | Event::SubOrchestrationCompleted { .. }
            | Event::SubOrchestrationFailed { .. } => {
                // Next completion is for different future - must wait!
                // Cannot skip over it - that would be non-deterministic
                return Poll::Pending;
            }
            
            // Not a completion - skip over scheduling/lifecycle events
            _ => {
                inner.next_completion_index += 1;
            }
        }
    }
    
    // No more events - still waiting
    Poll::Pending
}
```

### 4. What Happens to Each CompletionMap Feature?

| Feature | Current (CompletionMap) | New (Cursor) |
|---------|-------------------------|--------------|
| **Arrival ordering** | Tracked in `ordered` VecDeque | Events added to history in arrival order |
| **Sequential consumption** | `is_next_ready()` checks queue | Cursor walks history sequentially |
| **Duplicate detection** | Check `by_id` HashMap | Check history when converting message |
| **Ack tokens** | Returned from `add_completion()` | Collected in `prep_completions()` return value |
| **External event mapping** | `add_external_completion()` | Done when converting ExternalByName to ExternalEvent |
| **Cross-execution filtering** | Done before adding to map | Done before adding to history |

**Everything is simpler with history+cursor!**

## Files to Remove

1. ❌ `src/runtime/completion_map.rs` (~280 lines)
2. ❌ `src/runtime/completion_map_tests.rs` (~300 lines)
3. ❌ `src/runtime/completion_aware_futures.rs` (~260 lines)

**Total: ~840 lines removed!**

## Files to Modify

### `src/runtime/orchestration_turn.rs`
- Remove `completion_map` field
- Simplify `prep_completions()` to convert messages → events
- Remove `apply_completions_to_history()` (no longer needed)
- Remove `completion_map()`, `completion_map_mut()` getters
- Update `made_progress()` to just check `history_delta.is_empty()`
- Remove `has_unconsumed_completions()` - cursor handles this automatically

### `src/runtime/mod.rs`
- Remove CompletionMap imports
- Remove CompletionKind, CompletionValue exports

### `src/futures.rs`
- Remove `try_completion_map_poll()` method
- Simplify to use cursor-based polling only
- Remove thread-local accessor usage

### `src/lib.rs`
- Update `CtxInner` with `next_completion_index`
- Remove `claimed_activity_ids`, `claimed_timer_ids`, `claimed_external_ids`
- Add unified `claimed_scheduling_events: HashSet<u64>`

## Benefits of Removal

1. **Simpler mental model**: History is the single source of truth
2. **Less code**: ~840 lines removed
3. **No thread-local magic**: No more thread-local CompletionMapAccessor
4. **Clearer semantics**: Cursor explicitly shows sequential consumption
5. **Easier to debug**: Just look at history, no separate data structure
6. **Less state to manage**: No sync between CompletionMap and history

## Implementation Steps

1. Update Event model with `event_id` and `source_event_id`
2. Update `CtxInner` with cursor and unified claimed set
3. Update `OrchestrationTurn::prep_completions()` to convert messages → events
4. Update `DurableFuture::poll()` to use cursor
5. Remove completion_map.rs, completion_map_tests.rs, completion_aware_futures.rs
6. Update all imports and references
7. Update tests to work without CompletionMap

## Migration Strategy

Since this is a breaking change anyway (Event model changes), we can:
1. Implement event_id changes first
2. Implement cursor-based polling
3. Remove CompletionMap in same commit
4. Update all tests

**No need for incremental migration** - it's all one breaking change.

## Critical Implementation Detail: Cursor and Turn Lifecycle

### How Cursor Works Across Turn

**At turn start** (`OrchestrationTurn::new()`):
```rust
let ctx = OrchestrationContext::new(baseline_history, execution_id);
// Cursor starts at 0 (beginning of history)
```

**During `prep_completions()`**:
```rust
// Convert messages to completion Events
for msg in messages {
    let mut event = msg_to_event(msg);  // ActivityCompleted, TimerFired, etc.
    event.set_event_id(next_event_id++);
    
    // Add to context's history (NOT just history_delta)
    ctx.inner.lock().unwrap().history.push(event);
}
```

**During orchestration execution**:
```rust
// Futures poll and consume via cursor
// Cursor advances as completions are consumed
// When a future finds its completion at cursor position:
inner.next_completion_index += 1;  // Move cursor forward
```

**Key insight**: The cursor operates on the **live history vector** that grows as:
1. Scheduling events are added (when futures first poll)
2. Completion events are added (from incoming messages)

Both happen within the same turn, and cursor advances through both!

### Why This Replaces CompletionMap

**CompletionMap was trying to:**
- Batch messages before converting to events
- Track them separately from history
- Provide ordered access

**Cursor approach:**
- Convert messages to events immediately
- Add them to history in arrival order
- Cursor provides ordered access naturally

**Result**: History IS the queue. No separate data structure needed!

## Open Questions

1. ✅ **Decided**: Cursor resets at turn start (starts at 0)
2. ✅ **Decided**: Convert messages to events immediately in prep_completions()
3. Should we keep any completion-related tests?
   - **Answer**: Yes, convert key tests to cursor-based approach
   - Test: Sequential consumption works
   - Test: Cannot skip over completions  
   - Test: Duplicate detection still works

