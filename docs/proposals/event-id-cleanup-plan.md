# Event ID Cleanup Plan

## Executive Summary

This plan addresses the inconsistent ID model in the Event enum by:
1. **Adding `event_id`** to all Event variants - a monotonic position in history
2. **Removing `id` from scheduling events** - `event_id` becomes THE id
3. **Adding `source_event_id`** to completion events - references the scheduling event's `event_id`
4. **Generating event_id in runtime** - assigned when events are added to history
5. **Using event_id as database key** - replacing `sequence_num` with `event_id`
6. **Updating dispatchers** - Workers/timers track and reference source event IDs
7. **Removing CompletionMap** - Replace with simple cursor over history (~840 lines deleted!)

**Breaking Change**: No backward compatibility. This requires database wipe/migration.

**Key Insights**: 
- Scheduling events don't need a separate correlation ID - `event_id` serves both purposes
- History IS the ordered queue - no need for separate CompletionMap
- **Single cursor** advances through ALL events sequentially (next_event_index)
- Cursor is shared across all futures - ensures global sequential ordering

**Bugs Fixed**: 
1. **Same name+input collision**: Two activities with same name+input can adopt the same correlation ID across turns
2. **Searching for completions**: Current code searches through history (`.iter().rev().find_map()`) instead of strict sequential consumption - allows non-deterministic execution patterns

The new design fixes both by using sequential `event_id` and strict cursor-based consumption.

## Problem Statement

Currently, the `Event` enum has an inconsistent ID model that creates confusion and limits functionality:

1. **Scheduling events** (e.g., `ActivityScheduled`, `TimerCreated`) have an `id` field that serves as a correlation ID for matching with completions
2. **Completion events** (e.g., `ActivityCompleted`, `TimerFired`) have an `id` field that **references** the scheduling event's ID
3. **Lifecycle events** (e.g., `OrchestrationStarted`, `OrchestrationCompleted`) have **no ID field** at all
4. There is **no monotonically increasing event ID** that represents the position of an event in history

### Critical Bug: Same Name+Input Collision

**Location**: `src/lib.rs` lines 729-743

The current ID adoption mechanism has a serious bug:

```rust
// Current broken code
let adopted_id = inner.history.iter().find_map(|e| match e {
    Event::ActivityScheduled { id, name: n, input: inp, .. }
        if n == &name && inp == &input && !inner.claimed_activity_ids.contains(id) 
        => Some(*id),
    _ => None,
})
```

**The Bug**: When scheduling two activities with the same name and input in different turns:
- Turn 1: `schedule_activity("Process", "data")` → allocates ID 1
- Turn 2: `schedule_activity("Process", "data")` → **adopts ID 1 from history**
- Result: Both futures have ID 1, causing completion collisions!

The `claimed_activity_ids` set only prevents this within a single turn, not across turns.

**Real-world impact**: Fan-out patterns that process identical items, retry logic that re-schedules failed activities, or any scenario with duplicate name+input pairs will experience non-deterministic behavior.

### Why This is Problematic

- **Semantic overloading**: The field name `id` means different things in different contexts (correlation vs reference)
- **No direct event addressing**: Cannot refer to "the Nth event in history" without counting through the list
- **Debugging challenges**: Difficult to trace specific events in logs and diagnostics
- **Future feature limitations**: Features like event replay from position, event-based debugging tools, or distributed tracing would benefit from unique event IDs
- **Inconsistency with storage**: The database already has a `sequence_num` column, but it's not part of the Event data model

## Proposed Solution

### 1. Add `event_id` to All Event Variants & Simplify IDs

Add a monotonically increasing `event_id: u64` field to **every** variant. For scheduling events, `event_id` **replaces** the old `id` field entirely:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    // Lifecycle events - simple, just event_id
    OrchestrationStarted {
        event_id: u64,
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },
    OrchestrationCompleted {
        event_id: u64,
        output: String,
    },
    OrchestrationFailed {
        event_id: u64,
        error: String,
    },
    
    // Scheduling events - event_id is THE id (no separate correlation_id)
    ActivityScheduled {
        event_id: u64,        // ← THE id (replaces old 'id' field)
        name: String,
        input: String,
        execution_id: u64,
    },
    
    // Completion events - reference the scheduling event via source_event_id
    ActivityCompleted {
        event_id: u64,           // Position of THIS event in history
        source_event_id: u64,    // ← References ActivityScheduled.event_id
        result: String,
    },
    ActivityFailed {
        event_id: u64,
        source_event_id: u64,    // ← References ActivityScheduled.event_id
        error: String,
    },
    
    TimerCreated {
        event_id: u64,        // ← THE id (replaces old 'id' field)
        fire_at_ms: u64,
        execution_id: u64,
    },
    TimerFired {
        event_id: u64,
        source_event_id: u64,    // ← References TimerCreated.event_id
        fire_at_ms: u64,
    },
    
    ExternalSubscribed {
        event_id: u64,        // ← THE id
        name: String,
    },
    ExternalEvent {
        event_id: u64,
        name: String,            // ← Match by name (no source_event_id)
        data: String,
        // Note: External events are raised by external callers who don't know
        // internal event IDs. Matching is done by name within the execution.
    },
    
    SubOrchestrationScheduled {
        event_id: u64,        // ← THE id
        name: String,
        instance: String,
        input: String,
        execution_id: u64,
    },
    SubOrchestrationCompleted {
        event_id: u64,
        source_event_id: u64,    // ← References SubOrchestrationScheduled.event_id
        result: String,
    },
    SubOrchestrationFailed {
        event_id: u64,
        source_event_id: u64,    // ← References SubOrchestrationScheduled.event_id
        error: String,
    },
    
    OrchestrationChained {
        event_id: u64,        // ← THE id
        name: String,
        instance: String,
        input: String,
    },
    
    OrchestrationContinuedAsNew {
        event_id: u64,
        input: String,
    },
    
    OrchestrationCancelRequested {
        event_id: u64,
        reason: String,
    },
}
```

### 2. Remove Separate Correlation ID

**Scheduling events** no longer need a separate `id`/`correlation_id` field - `event_id` serves this purpose:
- `ActivityScheduled`: Uses `event_id` for correlation
- `TimerCreated`: Uses `event_id` for correlation  
- `ExternalSubscribed`: Uses `event_id` for correlation
- `SubOrchestrationScheduled`: Uses `event_id` for correlation
- `OrchestrationChained`: Uses `event_id` for correlation

**Completion events** gain `source_event_id` to reference their scheduling event:
- `ActivityCompleted.source_event_id` → `ActivityScheduled.event_id`
- `TimerFired.source_event_id` → `TimerCreated.event_id`
- `SubOrchestrationCompleted.source_event_id` → `SubOrchestrationScheduled.event_id`
- `SubOrchestrationFailed.source_event_id` → `SubOrchestrationScheduled.event_id`

**Exception - External Events**: `ExternalEvent` does NOT have `source_event_id`. External callers don't know internal event IDs. Matching is done by `name` field within the current execution.

### 3. Properties of `event_id`

- **Monotonically increasing**: Within an execution, event IDs are always increasing
- **Unique per execution**: Each event in an execution has a unique event_id
- **Starts at 1**: The first event (typically `OrchestrationStarted`) has `event_id = 1`
- **Sequential**: Event IDs are sequential (no gaps) within normal operation
- **Immutable**: Once assigned, an event's ID never changes

### 4. Properties of `source_event_id`

- **References scheduling event**: Points to the `event_id` of the scheduling event
- **Immutable**: Set once when completion event is created
- **Audit trail**: Creates clear lineage from scheduling → completion
- **Sequential consumption**: During replay, completions MUST be consumed in order
- **No searching**: Cannot skip over completions to find "yours" - next completion must match
- **Exception**: `ExternalEvent` does NOT have this field - matches by name instead

### 5. Critical: Single Cursor for ALL Events

**IMPORTANT**: The current code SEARCHES for completions (line 195 in `src/futures.rs`) - this is WRONG!

**NEW MODEL**: **One cursor** (`next_event_index`) advances sequentially through ALL events.

When a future polls:

```rust
fn poll(&mut self) -> Poll<DurableOutput> {
    let mut inner = ctx.inner.lock().unwrap();
    
    // Step 1: Claim scheduling event (if not already claimed)
    if self.claimed_event_id.is_none() {
        loop {
            if inner.next_event_index >= inner.history.len() {
                // Create new scheduling event (first execution)
                create_and_claim_new_scheduling_event();
                break;
            }
            
            let event = &inner.history[inner.next_event_index];
            
            // Is this an ActivityScheduled?
            if let Event::ActivityScheduled { event_id, name, input, .. } = event {
                // STRICT: Next ActivityScheduled MUST be ours!
                if name != &self.name || input != &self.input {
                    panic!(
                        "NON-DETERMINISTIC: Expected activity '{}' with input '{}', \
                         found '{}' with input '{}' at event_id={}",
                        self.name, self.input, name, input, event_id
                    );
                }
                
                // Valid! Claim and advance
                self.claimed_event_id.set(Some(*event_id));
                inner.next_event_index += 1;
                break;
            }
            
            // Not ActivityScheduled - skip it
            inner.next_event_index += 1;
        }
    }
    
    let our_event_id = self.claimed_event_id.get().unwrap();
    
    // Step 2: Look for completion (STRICT - same cursor continues)
    loop {
        if inner.next_event_index >= inner.history.len() {
            return Poll::Pending;
        }
        
        let event = &inner.history[inner.next_event_index];
        
        match event {
            // Our completion?
            Event::ActivityCompleted { source_event_id, result, .. } 
                if *source_event_id == our_event_id => {
                validate_completion_references_scheduling(history, our_event_id, &self.name);
                inner.next_event_index += 1;
                return Poll::Ready(DurableOutput::Activity(Ok(result.clone())));
            }
            
            Event::ActivityFailed { source_event_id, error, .. }
                if *source_event_id == our_event_id => {
                validate_completion_references_scheduling(history, our_event_id, &self.name);
                inner.next_event_index += 1;
                return Poll::Ready(DurableOutput::Activity(Err(error.clone())));
            }
            
            // ANY other completion event?
            Event::ActivityCompleted { .. } | Event::ActivityFailed { .. }
            | Event::TimerFired { .. } | Event::ExternalEvent { .. }
            | Event::SubOrchestrationCompleted { .. } | Event::SubOrchestrationFailed { .. } => {
                panic!(
                    "NON-DETERMINISTIC: Activity '{}' (event_id={}) expected its completion, \
                     but found different completion at cursor",
                    self.name, our_event_id
                );
            }
            
            // Non-completion event - skip it
            _ => inner.next_event_index += 1,
        }
    }
}
```

**Cursor is shared across ALL futures** - ensures global sequential ordering!

**Test Impact**: The test `correlation_out_of_order_completion` will FAIL - this is CORRECT!

### 6. Special Case: External Events

External events are unique because they're raised by external callers (users, APIs) who don't have visibility into internal event IDs:

- **Client raises event**: `client.raise_event("instance-1", "ApprovalEvent", "approved")`
- **Client knows**: instance ID, event name, data
- **Client does NOT know**: What `event_id` the `ExternalSubscribed` event has

Therefore:
- `ExternalEvent` only has: `event_id`, `name`, `data` (no `source_event_id`)
- Matching logic: Find `ExternalSubscribed` by `name` in current execution
- The orchestration engine correlates by name, not by event ID
- External events still follow sequential consumption rules (can't skip over them)

## Event ID Generation Strategy

### Where `event_id` Will Be Assigned

**Answer: In the runtime layer, when events are added to history (before persistence).**

#### Rationale

The runtime tracks history as it builds it up during orchestration execution. By assigning `event_id` when events are added to history:
- Event IDs are available to dispatchers (needed for referencing in completion events)
- IDs are known before persistence (can be used as unique keys in provider)
- Clean separation: runtime owns event semantics, provider owns storage
- No backward compatibility concerns (breaking change is acceptable)

#### Implementation Details

**Location 1: Runtime orchestration turn** - Assign event_id when adding events to history

In **`src/runtime/orchestration_turn.rs`** or the turn execution logic:

```rust
struct TurnExecutor {
    history: Vec<Event>,
    next_event_id: u64,
    // ... other fields
}

impl TurnExecutor {
    fn new(initial_history: Vec<Event>) -> Self {
        let next_event_id = initial_history.len() as u64 + 1;
        Self {
            history: initial_history,
            next_event_id,
            // ...
        }
    }
    
    fn add_event_to_history(&mut self, mut event: Event) {
        event.set_event_id(self.next_event_id);
        self.next_event_id += 1;
        self.history.push(event);
    }
}
```

**Location 2: DurableFuture discovers its event_id during polling**

**CRITICAL**: The `DurableFuture` does NOT carry `event_id` directly! It discovers/claims the event_id during first poll using **cursor with corruption checking**.

In **`src/futures.rs`**:

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

impl Future for DurableFuture {
    fn poll(&mut self) -> Poll<DurableOutput> {
        // Step 1: Claim our scheduling event (if not already claimed)
        let event_id = if let Some(id) = self.claimed_event_id.get() {
            id  // Already claimed
        } else {
            // Use cursor to find next ActivityScheduled
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
                    
                    inner.record_action(Action::CallActivity {
                        scheduling_event_id: new_id,
                        name: self.name.clone(),
                        input: self.input.clone(),
                    });
                    
                    self.claimed_event_id.set(Some(new_id));
                    break new_id;
                }
                
                let event = &inner.history[inner.next_event_index];
                
                // Is this an ActivityScheduled?
                if let Event::ActivityScheduled { event_id, name, input, .. } = event {
                    // Next ActivityScheduled MUST be ours (strict determinism)
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
                    
                    // Valid! Claim and advance cursor
                    self.claimed_event_id.set(Some(*event_id));
                    inner.next_event_index += 1;
                    break *event_id;
                }
                
                // Not an ActivityScheduled - skip it
                inner.next_event_index += 1;
            }
        };
        
        // Step 2: Check for completion using STRICT cursor
        // ... (see below)
    }
}
```

**Key**: We iterate to find scheduling events (with corruption checks), but use strict cursor for completions.

**Location 3: Completion events reference source** - When creating completion events, reference the scheduling event

In **`src/futures.rs`** when materializing completions:

```rust
// When creating ActivityCompleted event
Event::ActivityCompleted {
    event_id: 0,  // Will be assigned when added to history
    source_event_id: scheduling_event_id,  // ← Reference to ActivityScheduled
    result: result.clone(),
}

// Exception: ExternalEvent - match by name only
Event::ExternalEvent {
    event_id: 0,  // Will be assigned when added to history
    name: event_name,
    data: event_data,
    // No source_event_id - external caller doesn't know it
}
```

**Location 4: Provider uses event_id as key** - Update schema to use event_id

In **`migrations/20240101000000_initial_schema.sql`**:

```sql
CREATE TABLE IF NOT EXISTS history (
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,  -- ← Renamed from sequence_num
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (instance_id, execution_id, event_id)  -- ← event_id is the key
);
```

## Implementation Plan

### Phase 1: Data Model Changes

**File**: `src/lib.rs`

1. Add `event_id: u64` field to every Event variant
2. Remove `id` field from scheduling events - `event_id` replaces it
3. Add `source_event_id: u64` field to all completion events (except ExternalEvent)

4. Update `CtxInner` with **single unified cursor** (no HashSets needed!):
```rust
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,
    next_event_id: u64,                          // ← NEW: for creating new events
    next_event_index: usize,                     // ← NEW: SINGLE cursor for ALL events
    execution_id: u64,
    turn_index: u64,
    logging_enabled_this_poll: bool,
    log_buffer: Vec<(LogLevel, String)>,
    // Remove: next_correlation_id
    // Remove: claimed_activity_ids, claimed_timer_ids, claimed_external_ids
    // Remove: claimed_scheduling_events (cursor position tracks what's processed!)
}

impl CtxInner {
    fn new(history: Vec<Event>, execution_id: u64) -> Self {
        let next_event_id = history.last()
            .map(|e| e.event_id() + 1)
            .unwrap_or(1);
        
        Self {
            history,
            next_event_id,
            next_event_index: 0,  // Single cursor starts at beginning
            // No claimed sets needed!
            // ...
        }
    }
}
```

5. Add helper methods to `Event` enum:
```rust
impl Event {
    /// Get the event_id (position in history)
    pub fn event_id(&self) -> u64 {
        match self {
            Event::OrchestrationStarted { event_id, .. } => *event_id,
            Event::OrchestrationCompleted { event_id, .. } => *event_id,
            Event::ActivityScheduled { event_id, .. } => *event_id,
            Event::ActivityCompleted { event_id, .. } => *event_id,
            // ... all variants
        }
    }
    
    /// Set the event_id (used by runtime when adding to history)
    pub(crate) fn set_event_id(&mut self, id: u64) {
        match self {
            Event::OrchestrationStarted { event_id, .. } => *event_id = id,
            Event::OrchestrationCompleted { event_id, .. } => *event_id = id,
            Event::ActivityScheduled { event_id, .. } => *event_id = id,
            Event::ActivityCompleted { event_id, .. } => *event_id = id,
            // ... all variants
        }
    }
    
    /// Get the source_event_id if this is a completion event
    /// Note: ExternalEvent does not have source_event_id (matched by name)
    pub fn source_event_id(&self) -> Option<u64> {
        match self {
            Event::ActivityCompleted { source_event_id, .. } => Some(*source_event_id),
            Event::ActivityFailed { source_event_id, .. } => Some(*source_event_id),
            Event::TimerFired { source_event_id, .. } => Some(*source_event_id),
            Event::SubOrchestrationCompleted { source_event_id, .. } => Some(*source_event_id),
            Event::SubOrchestrationFailed { source_event_id, .. } => Some(*source_event_id),
            _ => None,  // ExternalEvent intentionally excluded
        }
    }
}
```

6. Update `DurableFuture` Kind to discover event_id lazily:
```rust
pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
        claimed_event_id: Cell<Option<u64>>,  // ← NEW: discovered during first poll
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    // Similar for Timer, External, SubOrch
}
```

7. Simplify `schedule_activity()` and related methods:
```rust
pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
    // No ID allocation here! Just create the future with name/input
    // Event_id is discovered during first poll
    DurableFuture(Kind::Activity {
        name: name.into(),
        input: input.into(),
        claimed_event_id: Cell::new(None),  // Will be discovered
        ctx: self.clone(),
    })
}
```

8. Update all code that currently uses the `id` field:
   - Remove ID adoption logic from `schedule_activity()`, `schedule_timer()`, etc.
   - `DurableFuture::poll()` - Implement cursor-based claiming/consumption
   - Remove `find_history_index()` and `synth_output_from_history()` - cursor replaces these
   - Remove `claimed_ids_snapshot()` - no longer needed

### Phase 2: Remove CompletionMap, Use Cursor

**Files to DELETE**:
- ❌ `src/runtime/completion_map.rs` (~280 lines)
- ❌ `src/runtime/completion_map_tests.rs` (~300 lines)  
- ❌ `src/runtime/completion_aware_futures.rs` (~260 lines)

**File**: `src/runtime/orchestration_turn.rs`

1. **Remove CompletionMap field**:
```rust
pub struct OrchestrationTurn {
    instance: String,
    turn_index: u64,
    execution_id: u64,
    // REMOVE: completion_map: CompletionMap,
    history_delta: Vec<Event>,
    pending_actions: Vec<crate::Action>,
    baseline_history: Vec<Event>,
    next_event_id: u64,              // ← NEW
}
```

2. **Simplify prep_completions() - convert messages to events**:
```rust
pub fn prep_completions(&mut self, messages: Vec<OrchestratorMsg>) -> Vec<String> {
    let mut ack_tokens = Vec::new();
    
    for msg in messages {
        // Check filtering (execution_id, duplicates, trace activities)
        if !self.should_process_message(&msg) {
            ack_tokens.push(extract_ack_token(&msg));
            continue;
        }
        
        // Convert message to Event
        let event = self.msg_to_event(msg);
        
        // Assign event_id and add to history_delta
        event.set_event_id(self.next_event_id);
        self.next_event_id += 1;
        self.history_delta.push(event);
        
        ack_tokens.push(extract_ack_token(&msg));
    }
    
    ack_tokens
}

fn msg_to_event(&self, msg: OrchestratorMsg) -> Event {
    match msg {
        OrchestratorMsg::ActivityCompleted { source_event_id, result, .. } => {
            Event::ActivityCompleted {
                event_id: 0,  // Will be set by caller
                source_event_id,
                result,
            }
        }
        // ... other message types
    }
}
```

3. **Remove methods**:
   - `apply_completions_to_history()` - No longer needed
   - `completion_map()`, `completion_map_mut()` - Removed
   - Update `made_progress()` to just check `history_delta.is_empty()`

**File**: `src/providers/mod.rs` - Update WorkItem to carry source_event_id

```rust
pub enum WorkItem {
    // ... existing variants ...
    
    ActivityExecute {
        instance: String,
        execution_id: u64,
        scheduling_event_id: u64,  // ← Changed: event_id of ActivityScheduled (was 'id')
        name: String,
        input: String,
    },
    ActivityCompleted {
        instance: String,
        execution_id: u64,
        source_event_id: u64,      // ← Changed: references ActivityScheduled.event_id (was 'id')
        result: String,
    },
    ActivityFailed {
        instance: String,
        execution_id: u64,
        source_event_id: u64,      // ← Changed: references ActivityScheduled.event_id (was 'id')
        error: String,
    },
    
    TimerSchedule {
        instance: String,
        execution_id: u64,
        scheduling_event_id: u64,  // ← Changed: event_id of TimerCreated (was 'id')
        fire_at_ms: u64,
    },
    TimerFired {
        instance: String,
        execution_id: u64,
        source_event_id: u64,      // ← Changed: references TimerCreated.event_id (was 'id')
        fire_at_ms: u64,
    },
    
    SubOrchCompleted {
        parent_instance: String,
        parent_execution_id: u64,
        source_event_id: u64,      // ← Changed: references SubOrchestrationScheduled.event_id (was 'parent_id')
        result: String,
    },
    SubOrchFailed {
        parent_instance: String,
        parent_execution_id: u64,
        source_event_id: u64,      // ← Changed: references SubOrchestrationScheduled.event_id (was 'parent_id')
        error: String,
    },
}
```

**Note**: The old `id` field in WorkItems is replaced with either `scheduling_event_id` (for execute/schedule items) or `source_event_id` (for completion items).

**Exception - ExternalRaised WorkItem**: Does not include event IDs since external callers don't know them:

```rust
ExternalRaised {
    instance: String,
    name: String,      // ← Match by name only
    data: String,
    // No event IDs - external callers don't have this information
}
```

**File**: `src/runtime/dispatch.rs`

Update dispatchers to use `event_id` directly from the Action:
```rust
pub async fn dispatch_call_activity(
    rt: &Arc<Runtime>,
    instance: &str,
    history: &[Event],
    scheduling_event_id: u64,  // ← Changed: now passed directly from Action
    name: String,
    input: String,
) {
    // Check if already completed
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
    
    // Pass scheduling_event_id to the worker
    rt.history_store.enqueue_worker_work(WorkItem::ActivityExecute {
        instance: instance.to_string(),
        execution_id,
        scheduling_event_id,  // ← Pass through
        name,
        input,
    }).await;
}
```

**Note**: Actions will need to carry the `event_id` of the scheduling event they reference.

### Phase 3: Database Schema Updates

**Breaking change - no backward compatibility needed.**

**File**: `migrations/20240101000000_initial_schema.sql`

Update the history table schema:

```sql
-- Drop old table (breaking change is acceptable)
DROP TABLE IF EXISTS history;

-- Recreate with event_id as the key
CREATE TABLE IF NOT EXISTS history (
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,       -- ← Renamed from sequence_num
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL,        -- JSON with event_id, correlation_id, etc.
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (instance_id, execution_id, event_id)
);

-- Index for common queries
CREATE INDEX IF NOT EXISTS idx_history_instance ON history(instance_id, execution_id);
```

**File**: `src/providers/sqlite.rs`

Update all SQL queries to use `event_id` instead of `sequence_num`:

```rust
async fn append_history_in_tx(
    &self,
    tx: &mut Transaction<'_, Sqlite>,
    instance: &str,
    execution_id: u64,
    events: Vec<Event>,  // Already have event_id set by runtime
) -> Result<(), sqlx::Error> {
    // No need to compute next sequence - events already have event_id
    for event in events {
        let event_type = match event { /* ... */ };
        let event_data = serde_json::to_string(&event).unwrap();
        let event_id = event.event_id();  // ← Use the event's own ID
        
        sqlx::query(
            r#"
            INSERT INTO history (instance_id, execution_id, event_id, event_type, event_data)
            VALUES (?, ?, ?, ?, ?)
            "#
        )
        .bind(instance)
        .bind(execution_id as i64)
        .bind(event_id as i64)  // ← Direct from event
        .bind(event_type)
        .bind(event_data)
        .execute(&mut **tx)
        .await?;
    }
    
    Ok(())
}

async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
    let rows = sqlx::query(
        r#"
        SELECT event_data 
        FROM history 
        WHERE instance_id = ? AND execution_id = ?
        ORDER BY event_id  -- ← Changed from sequence_num
        "#
    )
    .bind(instance)
    .bind(execution_id as i64)
    .fetch_all(&self.pool)
    .await
    .unwrap_or_default();
    
    rows.into_iter()
        .filter_map(|row| {
            row.try_get::<String, _>("event_data")
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .collect()
}
```

### Phase 4: Code Updates Throughout Codebase

Search for all usages of event fields and update:

1. **Pattern matching on events** - `id` field is removed from scheduling events:
   ```rust
   // Before
   Event::ActivityScheduled { id, name, input, execution_id }
   
   // After - event_id replaces id
   Event::ActivityScheduled { event_id, name, input, execution_id }
   ```

2. **Event construction for completions** - Add `source_event_id`:
   ```rust
   // Before
   Event::ActivityCompleted { id, result }
   
   // After
   Event::ActivityCompleted { 
       event_id: 0,            // Will be assigned when added to history
       source_event_id,        // References the scheduling event
       result 
   }
   ```

3. **Finding completions** - Match by `source_event_id` (or name for external events):
   ```rust
   // Before - matching by correlation id
   history.iter().find(|e| match e {
       Event::ActivityCompleted { id, .. } if *id == correlation_id => true,
       _ => false,
   })
   
   // After - match by source_event_id
   history.iter().find(|e| match e {
       Event::ActivityCompleted { source_event_id, .. } 
           if *source_event_id == scheduling_event.event_id() => true,
       _ => false,
   })
   
   // Exception: External events match by name
   history.iter().find(|e| match e {
       Event::ExternalEvent { name, .. } if name == event_name => true,
       _ => false,
   })
   ```

4. **Tests**: 
   - Update all test code to include `event_id: 0` in event construction
   - Or use `..` to ignore it in pattern matching
   - Tests that serialize/deserialize events will need event_id

5. **Documentation**: Update all docs that show Event examples

### Phase 5: Worker/Timer Dispatcher Updates

**File**: Worker dispatcher (activity execution)
- When an activity completes, create completion WorkItem with `source_event_id`
- Worker receives `WorkItem::ActivityExecute` with `scheduling_event_id`
- Worker sends back `WorkItem::ActivityCompleted` with `source_event_id = scheduling_event_id`

```rust
// In worker/activity dispatcher
async fn execute_activity(item: WorkItem) {
    match item {
        WorkItem::ActivityExecute { 
            instance, 
            execution_id, 
            scheduling_event_id,  // ← Receive the ActivityScheduled.event_id
            name, 
            input 
        } => {
            let result = run_activity(&name, &input).await;
            
            let completion = WorkItem::ActivityCompleted {
                instance,
                execution_id,
                source_event_id: scheduling_event_id,  // ← Pass it through
                result,
            };
            
            provider.enqueue_orchestrator_work(completion).await;
        }
    }
}
```

**File**: Timer dispatcher
- Similar pattern: `TimerSchedule` includes `scheduling_event_id`
- `TimerFired` WorkItem includes `source_event_id`

**File**: `src/runtime/router.rs` - Update OrchestratorMsg
- Add `source_event_id` to completion messages:

```rust
pub enum OrchestratorMsg {
    ActivityCompleted {
        instance: String,
        execution_id: u64,
        source_event_id: u64,  // ← Changed from 'id' - references scheduling event
        result: String,
        ack_token: Option<String>,
    },
    ActivityFailed {
        instance: String,
        execution_id: u64,
        source_event_id: u64,  // ← Changed from 'id'
        error: String,
        ack_token: Option<String>,
    },
    TimerFired {
        instance: String,
        execution_id: u64,
        source_event_id: u64,  // ← Changed from 'id'
        fire_at_ms: u64,
        ack_token: Option<String>,
    },
    // ... similar for other completion variants
}
```

### Phase 6: Validation and Testing

1. **Unit tests**: Update all existing tests to use the new structure
2. **Integration tests**: Ensure replay works correctly with new IDs
3. **Migration tests**: Test backward compatibility with old data
4. **Property tests**: Verify that `event_id` is always sequential and unique

## Affected Files

### Core files requiring changes:
- `src/lib.rs` - Event enum definition and all matching logic
- `src/providers/sqlite.rs` - Event storage and retrieval
- `src/providers/mod.rs` - WorkItem definitions if needed
- `src/runtime/dispatch.rs` - Action to Event materialization
- `src/runtime/orchestration_turn.rs` - Turn execution
- `src/runtime/execution.rs` - Orchestration lifecycle
- `src/futures.rs` - DurableFuture correlation logic

### Test files:
- All test files in `tests/` directory (30+ files)

### Documentation:
- `docs/architecture.md`
- `docs/dtf-runtime-design.md`
- `README.md`
- All examples in `examples/`

## Benefits After Implementation

1. **Clear semantics**: `event_id` is THE id - single source of truth for event identity
2. **Simpler model**: No confusion between correlation_id and event_id - they're the same thing
3. **Better debugging**: Can refer to specific events by their unique ID in logs
4. **Simplified queries**: Can fetch "events after event_id N" easily
5. **Audit trail**: `source_event_id` creates explicit linkage from scheduling → completion
6. **Future features**: Enables event replay from position, distributed tracing, event-based snapshots
7. **Storage alignment**: Event data model now matches the database schema
8. **Consistency**: All events have an ID, no special cases
9. **Less redundancy**: Removed duplicate ID fields that meant the same thing
10. **Massive code reduction**: ~840 lines of CompletionMap code removed
11. **No thread-local magic**: Removed thread-local accessor complexity
12. **History is truth**: Single source of truth - no sync between map and history
13. **No HashSets needed**: Cursor position tracks processed events - simpler state
14. **Bugs fixed**: Both same name+input collision AND searching for completions
15. **Stricter determinism**: Next event of expected type MUST match (catches more bugs)

## Breaking Changes

This is a **breaking change** that affects:
1. **Serialized data**: Existing events in databases need migration
2. **Public API**: Event construction and pattern matching code
3. **Tests**: All tests that construct or match on events
4. **External consumers**: Any code that serializes/deserializes Events

### Versioning Recommendation

- Bump to next **minor version** (e.g., 0.2.0 → 0.3.0) if pre-1.0
- Bump to next **major version** (e.g., 1.x.y → 2.0.0) if post-1.0

## Timeline Estimate

- **Phase 1** (Data model + helpers + CtxInner cursor): 1.5 days
- **Phase 2** (Remove CompletionMap, simplify prep_completions): 1.5 days
- **Phase 3** (Database schema + provider updates): 1 day
- **Phase 4** (Code updates throughout + cursor-based polling): 3 days
- **Phase 5** (Worker/Timer dispatchers + WorkItem updates): 1 day
- **Phase 6** (Testing + validation): 2 days

**Total**: ~10 days of focused work

**Bonus**: Removing ~840 lines of CompletionMap code makes the codebase simpler!

## Open Questions

1. ✅ **Decided**: Add helper methods `event.event_id()`, `event.set_event_id()`, `event.source_event_id()`
2. ✅ **Decided**: Generate event_id in runtime, not provider
3. ✅ **Decided**: No backward compatibility needed (breaking change is OK)
4. ✅ **Decided**: Completion events get `source_event_id` field
5. ✅ **Decided**: Remove correlation_id entirely - event_id serves both purposes
6. ✅ **Decided**: External events are special - no `source_event_id`, match by name only
7. ✅ **Decided**: Single cursor for ALL events - no separate logic
8. ✅ **Decided**: No HashSets for claimed events - cursor position is sufficient  
9. ✅ **Decided**: Next scheduling event MUST be ours (strict validation, no searching)
10. ✅ **Decided**: Next completion MUST be ours (panic if not - no skipping!)
11. ✅ **Decided**: Add corruption checks when claiming and consuming
12. ✅ **Decided**: Actions carry `scheduling_event_id` - needed for dispatchers
13. Test `correlation_out_of_order_completion` must be removed - non-deterministic pattern
14. Do we want to expose `event_id` in public Client API for queries?
15. For `OrchestrationStarted.parent_id`, rename to `parent_event_id`?

## References

- Database schema: `migrations/20240101000000_initial_schema.sql` (line 24-33)
- Event enum: `src/lib.rs` (line 238-306)
- **Current bug**: ID adoption logic in `src/lib.rs` (line 729-743)
- SQLite storage: `src/providers/sqlite.rs` (line 416-476)
- Event matching: `src/lib.rs` (line 1064-1102)
- DurableFuture polling: `src/futures.rs` (line 174-326)
- Completion map (deterministic ordering): `src/runtime/completion_map.rs`
- Orchestration turn execution: `src/runtime/orchestration_turn.rs`

## Related Documents

1. **`unified-cursor-model.md`** - **READ THIS FIRST** - Single cursor design:
   - ONE cursor for ALL events (next_event_index)
   - Cursor shared across all futures
   - Advances forward-only through history
   - Example execution trace showing cursor movement

2. **`strict-cursor-model.md`** - Critical findings:
   - Current code SEARCHES for completions (wrong!)
   - Strict rules: cannot skip completions
   - Test `correlation_out_of_order_completion` will fail (correct!)

3. **`event-id-implementation-details.md`** - Deep dive:
   - Lazy event_id discovery
   - Corruption validation checks
   - Why this fixes the collision bug

4. **`remove-completion-map-plan.md`** - CompletionMap removal:
   - Files to delete (~840 lines!)
   - Message to Event conversion

