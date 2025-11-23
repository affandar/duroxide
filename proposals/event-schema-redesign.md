# Event Schema Redesign: Common Fields + Kind Enum

## Problem with Current Schema

Every Event variant repeats the same fields:
```rust
Event::OrchestrationStarted { event_id: u64, name, version, ... }
Event::OrchestrationCompleted { event_id: u64, output }
Event::ActivityScheduled { event_id: u64, name, input, ... }
// event_id repeated 17 times!
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
    
    /// Instance this event belongs to
    /// Previously: Only in DB key, now self-contained in event
    pub instance_id: String,
    
    /// Execution this event belongs to
    /// Previously: Only in DB key, now self-contained in event
    pub execution_id: u64,
    
    /// Timestamp when event was created (milliseconds since Unix epoch)
    pub timestamp_ms: u64,
    
    /// Crate semver version that generated this event
    /// Format: "0.1.0", "0.2.0", etc.
    /// Enables compatibility detection across duroxide versions
    #[serde(default = "default_duroxide_version")]
    pub duroxide_version: String,
    
    // ===== Event-Specific Data =====
    
    /// Event type and associated data
    #[serde(flatten)]
    pub kind: EventKind,
}

fn default_duroxide_version() -> String { 
    env!("CARGO_PKG_VERSION").to_string()
}

/// Event-specific payloads
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
        // execution_id removed - now in Event.execution_id
    },
    
    #[serde(rename = "ActivityCompleted")]
    ActivityCompleted {
        source_event_id: u64,
        result: String,
    },
    
    #[serde(rename = "ActivityFailed")]
    ActivityFailed {
        source_event_id: u64,
        details: ErrorDetails,
    },
    
    #[serde(rename = "TimerCreated")]
    TimerCreated {
        fire_at_ms: u64,
        // execution_id removed - now in Event.execution_id
    },
    
    #[serde(rename = "TimerFired")]
    TimerFired {
        source_event_id: u64,
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
        // execution_id removed - now in Event.execution_id
    },
    
    #[serde(rename = "SubOrchestrationCompleted")]
    SubOrchestrationCompleted {
        source_event_id: u64,
        result: String,
    },
    
    #[serde(rename = "SubOrchestrationFailed")]
    SubOrchestrationFailed {
        source_event_id: u64,
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
        // execution_id removed - now in Event.execution_id
    },
}
```

### JSON Serialization Format

**New format:**
```json
{
  "event_id": 1,
  "timestamp_ms": 1700000000000,
  "schema_version": 1,
  "type": "OrchestrationStarted",
  "name": "ProcessOrder",
  "version": "1.0.0",
  "input": "{...}",
  "parent_instance": null,
  "parent_id": null
}
```

**Old format (still deserializes):**
```json
{
  "OrchestrationStarted": {
    "event_id": 1,
    "name": "ProcessOrder",
    "version": "1.0.0",
    "input": "{...}",
    "parent_instance": null,
    "parent_id": null
  }
}
```

### Backward Compatibility via Serde

```rust
// Custom deserializer to handle both formats
#[derive(Deserialize)]
#[serde(untagged)]
enum EventCompat {
    New(Event),      // New struct format
    Old(OldEvent),   // Old enum format
}

impl From<EventCompat> for Event {
    fn from(compat: EventCompat) -> Event {
        match compat {
            EventCompat::New(event) => event,
            EventCompat::Old(old_event) => {
                // Convert old enum to new struct+kind
                Event {
                    event_id: old_event.event_id(),
                    timestamp_ms: 0,  // Old events don't have timestamp
                    schema_version: 1,
                    kind: old_event.into_kind(),
                }
            }
        }
    }
}
```

## Benefits of This Design

### 1. Clean API
```rust
// Access common fields without matching
let timestamp = event.timestamp_ms;
let id = event.event_id;
let instance = &event.instance_id;
let exec_id = event.execution_id;

// Match only on event-specific data
match event.kind {
    EventKind::OrchestrationStarted { name, version, .. } => { /* ... */ },
    EventKind::ActivityCompleted { result, .. } => { /* ... */ },
}
```

**No More Duplication:**
- `event_id` was in all 17 variants → now once in Event struct
- `execution_id` was in 4 variants (ActivityScheduled, TimerCreated, SubOrchestrationScheduled, SystemCall) → now once in Event struct
- `instance_id` implicitly from DB context → now explicit in Event struct
- Future common fields added once, apply to all events automatically

**Storage Savings:**
- Removing `execution_id` from 4 EventKind variants saves ~8 bytes per event (offset by adding to Event struct once)
- Net effect: Slightly larger due to instance_id, but worth it for self-containment

### 2. Easy to Add Common Fields
```rust
// Future: Add trace_id to ALL events
pub struct Event {
    pub event_id: u64,
    pub timestamp_ms: u64,
    pub schema_version: u32,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,  // ← One line adds to ALL events!
    
    pub kind: EventKind,
}
```

### 3. Forward Compatible
```rust
// Schema version allows handling multiple versions
match event.schema_version {
    1 => { /* current */ },
    2 => { /* future version with new fields */ },
    _ if event.schema_version > 2 => {
        // Forward compat: Accept unknown versions, ignore new fields
        tracing::warn!("Event schema version {} not recognized, treating as v2", event.schema_version);
    },
    _ => return Err("Unsupported schema version"),
}
```

### 4. Type Safety
```rust
impl Event {
    /// Get event timestamp (always available)
    pub fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms  // No matching needed!
    }
    
    /// Check if this is a terminal event
    pub fn is_terminal(&self) -> bool {
        matches!(self.kind, 
            EventKind::OrchestrationCompleted { .. } |
            EventKind::OrchestrationFailed { .. } |
            EventKind::OrchestrationContinuedAsNew { .. }
        )
    }
}
```

## Migration Impact Analysis

### Code Changes Required

**1. Event Creation Sites (~40 locations):**
```rust
// Old:
Event::OrchestrationStarted {
    event_id: 0,
    name: name.clone(),
    version: version.clone(),
    input,
    parent_instance: None,
    parent_id: None,
}

// New:
Event {
    event_id: 0,
    timestamp_ms: current_time_ms(),
    schema_version: 1,
    kind: EventKind::OrchestrationStarted {
        name: name.clone(),
        version: version.clone(),
        input,
        parent_instance: None,
        parent_id: None,
    },
}
```

**2. Event Matching Sites (~100+ locations):**
```rust
// Old:
match event {
    Event::OrchestrationStarted { event_id, name, .. } => { /* ... */ }
}

// New:
match &event.kind {
    EventKind::OrchestrationStarted { name, .. } => {
        let event_id = event.event_id;  // Access from outer struct
        /* ... */
    }
}
```

**3. Helper Methods:**
```rust
impl Event {
    pub fn event_id(&self) -> u64 {
        self.event_id  // Simple!
    }
    
    pub fn set_event_id(&mut self, id: u64) {
        self.event_id = id;  // Simple!
    }
    
    // No more 17-arm match statements!
}
```

### Database Changes

**None required!** JSON is stored as TEXT, serde handles it.

### Provider Changes

**Minimal:**
- Event serialization/deserialization automatic
- `event_type` extraction needs update (match on `event.kind`)
- Otherwise transparent

## Recommended Fields for Phase 1

```rust
pub struct Event {
    /// Sequential position in history
    pub event_id: u64,
    
    /// When event was created (milliseconds since Unix epoch)
    pub timestamp_ms: u64,
    
    /// Schema version (1 = current)
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    
    /// Optional: Instance this event belongs to (for multi-instance queries)
    /// Not strictly needed (already in DB key) but useful for event processing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    
    /// Optional: Execution this event belongs to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<u64>,
    
    /// Event-specific payload
    #[serde(flatten)]
    pub kind: EventKind,
}
```

## Summary: All Persisted Structs Requiring Versioning

Duroxide persists 3 types of data structures as JSON:

| Struct | Persisted In | Versioning Status | Recommendation |
|--------|-------------|-------------------|----------------|
| `Event` | `history.event_data` | ❌ None | ✅ Add struct with common fields + EventKind enum |
| `WorkItem` | `orchestrator_queue.work_item`, `worker_queue.work_item` | ❌ None | ✅ Add struct with common fields + WorkItemKind enum |
| `ErrorDetails` | Embedded in Event/WorkItem | ❌ None | ⚠️ Keep as enum (inherits version from parent) |

**Key Insight:** DB stores these as TEXT (JSON), so schema changes are backward compatible via serde defaults.

## Other Structs Requiring Versioning

### WorkItem (Persisted in Queue Tables)

**Current:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkItem {
    StartOrchestration { instance, orchestration, input, version, ... },
    ActivityExecute { instance, execution_id, id, name, input },
    ActivityCompleted { instance, execution_id, id, result },
    // ... 10 variants total
}
```

**Should become:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItem {
    /// Crate semver version that created this work item
    #[serde(default = "default_duroxide_version")]
    pub duroxide_version: String,
    
    /// When work item was enqueued
    #[serde(default)]
    pub enqueued_at_ms: u64,
    
    /// Work item type and payload
    #[serde(flatten)]
    pub kind: WorkItemKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum WorkItemKind {
    #[serde(rename = "StartOrchestration")]
    StartOrchestration {
        instance: String,
        orchestration: String,
        input: String,
        version: Option<String>,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
        execution_id: u64,
    },
    
    #[serde(rename = "ActivityExecute")]
    ActivityExecute {
        instance: String,
        execution_id: u64,
        id: u64,
        name: String,
        input: String,
    },
    
    // ... etc for all 10 work item types
}
```

**Why:** 
- WorkItems persisted in queues, read back by potentially different runtime version
- `enqueued_at_ms` enables queue age metrics (how long items wait)
- Version tracking enables compatibility checks
- Same pattern as Event for consistency

**Benefits:**
- Can track queue wait times: `current_time - work_item.enqueued_at_ms`
- Can detect old work items in queue during upgrade
- Self-contained (no hidden dependencies on DB context)

### ErrorDetails (Embedded in Events/WorkItems)

**Current:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorDetails {
    Infrastructure { operation, message, retryable },
    Configuration { kind, resource, message },
    Application { kind, message, retryable },
}
```

**Recommendation:** Keep as-is (enum), but consider:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorDetails {
    // Add version if error handling changes significantly
    #[serde(rename = "infra_v1")]
    Infrastructure { operation, message, retryable },
    // ...
}
```

**Reason:** ErrorDetails is embedded in Event/WorkItem (gets their version), not standalone.

## Database Implications

### With instance_id and execution_id in Event

**Before (DB is smart):**
```sql
SELECT event_data FROM history 
WHERE instance_id = ? AND execution_id = ?
-- DB enforces these are correct via PRIMARY KEY
```

**After (DB is dumb):**
```sql
SELECT event_data FROM history
WHERE instance_id = ? AND execution_id = ?
-- DB still filters, but events are self-contained
```

**Benefits:**
- Events can be moved between databases without losing context
- Events can be exported/imported independently
- Query results are self-documenting
- Easier to build event streaming/audit log systems

**Trade-offs:**
- Slight storage overhead (~30 bytes per event)
- Must ensure runtime sets these fields correctly
- Denormalization (instance_id repeated for each event)

**Recommendation:** Worth it for self-contained events.

## Migration Strategy

### Phase 1: Add Common Fields with Defaults

```rust
pub struct Event {
    pub event_id: u64,
    
    #[serde(default)]  // Empty string for old events
    pub instance_id: String,
    
    #[serde(default = "default_execution_id")]  // 1 for old events
    pub execution_id: u64,
    
    #[serde(default)]  // 0 for old events
    pub timestamp_ms: u64,
    
    #[serde(default = "default_duroxide_version")]  // "0.0.0" for old events
    pub duroxide_version: String,
    
    #[serde(flatten)]
    pub kind: EventKind,
}

fn default_execution_id() -> u64 { 1 }
fn default_duroxide_version() -> String { "0.0.0".to_string() }
```

**Backward Compat:**
- Old events deserialize with default values
- Runtime can detect old events: `if event.duroxide_version == "0.0.0" { /* old */ }`
- Metrics skip old events: `if event.timestamp_ms > 0 { /* use timestamp */ }`

### Phase 2: Runtime Changes

**Update ALL Event creation sites:**
```rust
// Helper method
fn create_event(instance: &str, execution_id: u64, kind: EventKind) -> Event {
    Event {
        event_id: 0,  // Will be set by history manager
        instance_id: instance.to_string(),
        execution_id,
        timestamp_ms: current_time_ms(),
        duroxide_version: env!("CARGO_PKG_VERSION").to_string(),
        kind,
    }
}

// Usage:
history.push(create_event(
    instance,
    execution_id,
    EventKind::OrchestrationStarted {
        name: name.clone(),
        version,
        input,
        parent_instance: None,
        parent_id: None,
    }
));
```

### Phase 3: Update Duration Calculations

```rust
// In orchestration dispatcher:
let start_event = history.iter()
    .find(|e| matches!(e.kind, EventKind::OrchestrationStarted { .. }))
    .unwrap();

let end_event = history.last().unwrap();

let duration_seconds = if start_event.timestamp_ms > 0 && end_event.timestamp_ms > 0 {
    // Accurate end-to-end duration
    (end_event.timestamp_ms - start_event.timestamp_ms) as f64 / 1000.0
} else {
    // Fallback for old events without timestamps
    start_time.elapsed().as_secs_f64()
};
```

## Next Steps

1. **Implement Event struct redesign** with common fields
2. **Implement WorkItem struct redesign** (same pattern)
3. **Add helper functions** for event creation with auto-timestamp
4. **Update all event creation sites** (~40 locations)
5. **Update all event matching sites** (~140+ locations)
6. **Update duration calculations** to use timestamps
7. **Test backward compatibility** with old JSON
8. **Update metrics specification** to note timestamp-based duration

## Risk Assessment

**Low Risk:**
- Serde handles schema evolution gracefully
- No database schema changes
- Gradual migration (old events work, new events better)

**Medium Risk:**
- Large refactor touching many files
- Must update all pattern matches
- Need comprehensive testing

**High Value:**
- Fixes critical metrics bug (duration calculation)
- Enables accurate observability
- Future-proof schema
- Cleaner codebase

**Recommendation:** This is the right architectural fix. Worth the refactoring effort.
