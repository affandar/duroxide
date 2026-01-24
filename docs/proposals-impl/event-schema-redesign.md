# Event Schema Redesign: Common Fields + Kind Enum [IMPLEMENTED]

**Status:** Implemented

## Problem with Current Schema

Every Event variant repeats the same fields:
```rust
Event::OrchestrationStarted { event_id: u64, name, version, ... }
Event::OrchestrationCompleted { event_id: u64, output }
Event::ActivityScheduled { event_id: u64, name, input, execution_id, ... }
Event::ActivityCompleted { event_id: u64, source_event_id: u64, result }
// event_id repeated 17 times!
// source_event_id repeated in 5 completion variants!
// execution_id repeated in 4 scheduling variants!
```

This makes adding common fields (like timestamps) tedious and error-prone.

## Proposed Design

### Structure

```rust
/// Unified event with common metadata and type-specific payload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    // ===== Common Fields (Present on EVERY event) =====
    
    /// Sequential position in history (monotonically increasing per execution)
    pub event_id: u64,
    
    /// For completion events: references the scheduling event this completes
    /// None for lifecycle events (OrchestrationStarted, etc.) and scheduling events
    /// Some(id) for completion events (ActivityCompleted, TimerFired, etc.)
    pub source_event_id: Option<u64>,
    
    /// Instance this event belongs to
    /// Previously: Only in DB key, now self-contained in event
    pub instance_id: String,
    
    /// Execution this event belongs to
    /// Previously: Only in DB key (and in 4 variants), now self-contained in event
    pub execution_id: u64,
    
    /// Timestamp when event was created (milliseconds since Unix epoch)
    pub timestamp_ms: u64,
    
    /// Crate semver version that generated this event
    /// Format: "0.1.0", "0.2.0", etc.
    /// Enables compatibility detection across duroxide versions
    pub duroxide_version: String,
    
    // ===== Event-Specific Data =====
    
    /// Event type and associated data
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Event-specific payloads
/// 
/// Note: Common fields have been extracted to Event struct:
/// - event_id: moved to Event.event_id
/// - source_event_id: moved to Event.source_event_id (Option<u64>)
/// - execution_id: moved to Event.execution_id (was in 4 variants)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]  // Discriminator field in JSON
pub enum EventKind {
    #[serde(rename = "OrchestrationStarted")]
    OrchestrationStarted {
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },
    
    #[serde(rename = "OrchestrationCompleted")]
    OrchestrationCompleted {
        output: String,
    },
    
    #[serde(rename = "OrchestrationFailed")]
    OrchestrationFailed {
        details: ErrorDetails,
    },
    
    #[serde(rename = "ActivityScheduled")]
    ActivityScheduled {
        name: String,
        input: String,
        // execution_id REMOVED - now in Event.execution_id
    },
    
    #[serde(rename = "ActivityCompleted")]
    ActivityCompleted {
        // source_event_id REMOVED - now in Event.source_event_id
        result: String,
    },
    
    #[serde(rename = "ActivityFailed")]
    ActivityFailed {
        // source_event_id REMOVED - now in Event.source_event_id
        details: ErrorDetails,
    },
    
    #[serde(rename = "TimerCreated")]
    TimerCreated {
        fire_at_ms: u64,
        // execution_id REMOVED - now in Event.execution_id
    },
    
    #[serde(rename = "TimerFired")]
    TimerFired {
        // source_event_id REMOVED - now in Event.source_event_id
        fire_at_ms: u64,
    },
    
    #[serde(rename = "ExternalSubscribed")]
    ExternalSubscribed {
        name: String,
    },
    
    #[serde(rename = "ExternalEvent")]
    ExternalEvent {
        name: String,
        data: String,
    },
    
    #[serde(rename = "OrchestrationChained")]
    OrchestrationChained {
        name: String,
        instance: String,
        input: String,
    },
    
    #[serde(rename = "SubOrchestrationScheduled")]
    SubOrchestrationScheduled {
        name: String,
        instance: String,
        input: String,
        // execution_id REMOVED - now in Event.execution_id
    },
    
    #[serde(rename = "SubOrchestrationCompleted")]
    SubOrchestrationCompleted {
        // source_event_id REMOVED - now in Event.source_event_id
        result: String,
    },
    
    #[serde(rename = "SubOrchestrationFailed")]
    SubOrchestrationFailed {
        // source_event_id REMOVED - now in Event.source_event_id
        details: ErrorDetails,
    },
    
    #[serde(rename = "OrchestrationContinuedAsNew")]
    OrchestrationContinuedAsNew {
        input: String,
    },
    
    #[serde(rename = "OrchestrationCancelRequested")]
    OrchestrationCancelRequested {
        reason: String,
    },
    
    #[serde(rename = "SystemCall")]
    SystemCall {
        op: String,
        value: String,
        // execution_id REMOVED - now in Event.execution_id
    },
}
```

### Fields Moved to Common Event Struct

| Field | Previously In | Now In | Notes |
|-------|--------------|--------|-------|
| `event_id` | All 17 variants | `Event.event_id` | Always present |
| `source_event_id` | 5 completion variants | `Event.source_event_id` | `Option<u64>` - None for non-completions |
| `execution_id` | 4 scheduling variants | `Event.execution_id` | Always present (denormalized from DB key) |
| `instance_id` | DB key only | `Event.instance_id` | NEW - denormalized from DB key |
| `timestamp_ms` | Nowhere | `Event.timestamp_ms` | NEW - enables duration metrics |
| `duroxide_version` | Nowhere | `Event.duroxide_version` | NEW - enables compatibility detection |

### source_event_id Semantics

Events with `source_event_id = Some(id)` (completion events):
- `ActivityCompleted` - references `ActivityScheduled`
- `ActivityFailed` - references `ActivityScheduled`
- `TimerFired` - references `TimerCreated`
- `SubOrchestrationCompleted` - references `SubOrchestrationScheduled`
- `SubOrchestrationFailed` - references `SubOrchestrationScheduled`

Events with `source_event_id = None`:
- All lifecycle events (OrchestrationStarted, OrchestrationCompleted, etc.)
- All scheduling events (ActivityScheduled, TimerCreated, etc.)
- External events (ExternalSubscribed, ExternalEvent)
- System calls (SystemCall)

### JSON Serialization Format

**New format (ActivityCompleted example):**
```json
{
  "event_id": 2,
  "source_event_id": 1,
  "instance_id": "order-123",
  "execution_id": 1,
  "timestamp_ms": 1700000000000,
  "duroxide_version": "0.1.0",
  "type": "ActivityCompleted",
  "result": "\"success\""
}
```

**New format (OrchestrationStarted example - no source_event_id):**
```json
{
  "event_id": 1,
  "source_event_id": null,
  "instance_id": "order-123",
  "execution_id": 1,
  "timestamp_ms": 1700000000000,
  "duroxide_version": "0.1.0",
  "type": "OrchestrationStarted",
  "name": "ProcessOrder",
  "version": "1.0.0",
  "input": "{...}",
  "parent_instance": null,
  "parent_id": null
}
```

**Old format (NO BACKWARD COMPATIBILITY - breaking change):**
```json
{
  "ActivityCompleted": {
    "event_id": 2,
    "source_event_id": 1,
    "result": "\"success\""
  }
}
```

## Benefits of This Design

### 1. Clean API
```rust
// Access common fields without matching
let id = event.event_id;
let source = event.source_event_id;  // Option<u64>
let instance = &event.instance_id;
let exec_id = event.execution_id;
let timestamp = event.timestamp_ms;

// Match only on event-specific data
match &event.kind {
    EventKind::OrchestrationStarted { name, version, .. } => { /* ... */ },
    EventKind::ActivityCompleted { result } => { 
        // source_event_id available via event.source_event_id
        /* ... */ 
    },
}
```

### 2. No More Duplication
- `event_id` was in all 17 variants → now once in Event struct
- `source_event_id` was in 5 completion variants → now once in Event struct (as Option)
- `execution_id` was in 4 variants → now once in Event struct
- `instance_id` implicitly from DB context → now explicit in Event struct
- Future common fields added once, apply to all events automatically

### 3. Easy to Add Common Fields
```rust
// Future: Add trace_id to ALL events
pub struct Event {
    pub event_id: u64,
    pub source_event_id: Option<u64>,
    // ...existing fields...
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,  // ← One line adds to ALL events!
    
    pub kind: EventKind,
}
```

### 4. Self-Contained Events
Events now contain all context needed to understand them:
- Can be exported/imported independently
- Can be streamed to external systems
- Query results are self-documenting
- No hidden dependency on DB key context

## Migration Impact Analysis

### Code Changes Required

**1. Event Creation Sites (~40 locations):**
```rust
// Old:
Event::ActivityCompleted {
    event_id: 0,
    source_event_id: scheduling_id,
    result: output,
}

// New:
Event {
    event_id: 0,
    source_event_id: Some(scheduling_id),
    instance_id: instance.clone(),
    execution_id,
    timestamp_ms: current_time_ms(),
    duroxide_version: env!("CARGO_PKG_VERSION").to_string(),
    kind: EventKind::ActivityCompleted {
        result: output,
    },
}
```

**2. Event Matching Sites (~100+ locations):**
```rust
// Old:
match event {
    Event::ActivityCompleted { event_id, source_event_id, result } => { /* ... */ }
}

// New:
match &event.kind {
    EventKind::ActivityCompleted { result } => {
        let event_id = event.event_id;
        let source_event_id = event.source_event_id;  // Option<u64>
        /* ... */
    }
}
```

**3. source_event_id Access:**
```rust
// Old:
impl Event {
    pub fn source_event_id(&self) -> Option<u64> {
        match self {
            Event::ActivityCompleted { source_event_id, .. } => Some(*source_event_id),
            Event::ActivityFailed { source_event_id, .. } => Some(*source_event_id),
            // ... 5 arms total
            _ => None,
        }
    }
}

// New:
// Just access event.source_event_id directly!
let source = event.source_event_id;  // Option<u64>
```

**4. Helper Methods Simplified:**
```rust
impl Event {
    // These become trivial
    pub fn event_id(&self) -> u64 { self.event_id }
    pub fn source_event_id(&self) -> Option<u64> { self.source_event_id }
    pub fn set_event_id(&mut self, id: u64) { self.event_id = id; }
    
    // No more 17-arm match statements!
}
```

### Database Changes

**None required!** 

The `history` table stores events as JSON in the `event_data` TEXT column:
```sql
CREATE TABLE IF NOT EXISTS history (
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL,  -- JSON serialized Event
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (instance_id, execution_id, event_id)
);
```

The new Event struct serializes to JSON automatically via serde. No schema migration needed.

Note: `instance_id` and `execution_id` are now **denormalized** - they exist both as DB columns (for indexing/querying) and inside the JSON (for self-containment). The provider continues to use the parameters passed to it for the column values.

### Provider Changes (SQLite)

**1. `event_type` extraction in `append_history_in_tx()`:**
```rust
// Old:
let event_type = match event {
    Event::OrchestrationStarted { .. } => "OrchestrationStarted",
    Event::ActivityCompleted { .. } => "ActivityCompleted",
    // ... 17 arms
};

// New:
let event_type = match &event.kind {
    EventKind::OrchestrationStarted { .. } => "OrchestrationStarted",
    EventKind::ActivityCompleted { .. } => "ActivityCompleted",
    // ... 17 arms
};
```

**2. `event.event_id()` → direct field access:**
```rust
// Old:
if event.event_id() == 0 { ... }
let event_id = event.event_id() as i64;

// New:
if event.event_id == 0 { ... }
let event_id = event.event_id as i64;
```

**3. Pattern matching on `Event::OrchestrationStarted`:**
```rust
// Old:
if let crate::Event::OrchestrationStarted { name, version, .. } = e {
    Some((name.clone(), version.clone()))
}

// New:
if let crate::EventKind::OrchestrationStarted { name, version, .. } = &e.kind {
    Some((name.clone(), version.clone()))
}
```

**4. No changes needed for:**
- SQL schema (JSON is flexible)
- `instance_id`/`execution_id` handling (provider uses parameters, not event fields)
- Queue operations (WorkItem unchanged in this phase)

### Provider Changes Summary

| Change | Location | Impact |
|--------|----------|--------|
| `event_type` extraction | `append_history_in_tx()` | Match on `event.kind` instead of `event` |
| `event.event_id()` calls | 2 locations | Direct field access `event.event_id` |
| Pattern matching | `fetch_orchestration_item()` | Match on `event.kind` |
| SQL schema | None | JSON handles new format |
| Tests | Multiple | Update event construction |

## Runtime Changes

The runtime must populate all common fields when creating events:

```rust
/// Helper to create events with all common fields populated
fn create_event(
    instance_id: &str,
    execution_id: u64,
    source_event_id: Option<u64>,
    kind: EventKind,
) -> Event {
    Event {
        event_id: 0,  // Will be set by history manager
        source_event_id,
        instance_id: instance_id.to_string(),
        execution_id,
        timestamp_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        duroxide_version: env!("CARGO_PKG_VERSION").to_string(),
        kind,
    }
}

// Usage for scheduling event (no source):
let event = create_event(
    instance,
    execution_id,
    None,  // No source for scheduling events
    EventKind::ActivityScheduled {
        name: activity_name.clone(),
        input: input.clone(),
    },
);

// Usage for completion event (has source):
let event = create_event(
    instance,
    execution_id,
    Some(scheduling_event_id),  // References the ActivityScheduled
    EventKind::ActivityCompleted {
        result: output.clone(),
    },
);
```

## Files to Update

| File | Estimated Changes |
|------|-------------------|
| `src/lib.rs` | Event struct + EventKind enum definition, remove helper methods |
| `src/futures.rs` | ~25 pattern matches on source_event_id |
| `src/runtime/replay_engine.rs` | ~15 pattern matches |
| `src/runtime/execution.rs` | ~3 pattern matches |
| `src/runtime/dispatchers/orchestration.rs` | Event creation sites |
| `src/runtime/dispatchers/worker.rs` | Event creation sites |
| `src/providers/sqlite.rs` | event_type extraction, pattern matches |
| `tests/*.rs` | Event construction in tests |

## Backward Compatibility

**None.** This is a breaking change to the event JSON format.

Existing histories will NOT deserialize with the new format. This is acceptable because:
1. Duroxide is pre-1.0 and not yet released publicly
2. Test databases can be recreated
3. Clean break is simpler than compatibility shims

## Next Steps

1. **Update Event struct** in `src/lib.rs` with new design
2. **Update all event creation sites** (~40 locations)
3. **Update all event matching sites** (~100+ locations)
4. **Update provider** event_type extraction
5. **Update tests** with new event construction
6. **Run full test suite** to verify

## Risk Assessment

**Low Risk:**
- No database schema changes
- Provider changes are minimal
- Compiler will catch all pattern match updates

**Medium Risk:**
- Large refactor touching many files
- Must update all pattern matches (compiler helps)

**High Value:**
- Fixes critical metrics bug (duration calculation)
- Enables accurate observability
- Future-proof schema
- Cleaner codebase
- Self-contained events

**Recommendation:** This is the right architectural fix. Worth the refactoring effort.
