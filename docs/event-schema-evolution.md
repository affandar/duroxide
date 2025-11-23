# Event Schema Evolution and Forward Compatibility

## Current State Analysis

### Event Storage
- Events are JSON-serialized via `serde_json` and stored in `history.event_data` column (TEXT)
- Each event also has `event_type` column for querying without deserialization
- Events are immutable once written (append-only)

### Current Schema Issues

**1. Missing Timestamps**
- No `timestamp_ms` field on events
- Can't calculate accurate orchestration/activity durations
- Can't reconstruct timelines
- Current `duroxide_orchestration_duration_seconds` only measures final turn (incorrect!)

**2. Missing Metadata for Observability**
- No worker_id (which worker processed this?)
- No correlation metadata for tracing
- No retry count on ActivityScheduled events

**3. No Version Field**
- Events have no schema version indicator
- Can't detect old vs new format during deserialization
- Makes migration risky

## Proposed Schema Changes

### Phase 1: Add Timestamps and Schema Version (Required for Metrics)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "schema_version")]
pub enum Event {
    #[serde(rename = "1")]
    V1(EventV1),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventV1 {
    OrchestrationStarted {
        event_id: u64,
        timestamp_ms: u64,  // ← NEW: Milliseconds since epoch
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },
    OrchestrationCompleted { 
        event_id: u64,
        timestamp_ms: u64,  // ← NEW
        output: String 
    },
    OrchestrationFailed { 
        event_id: u64,
        timestamp_ms: u64,  // ← NEW
        details: ErrorDetails 
    },
    ActivityScheduled {
        event_id: u64,
        timestamp_ms: u64,  // ← NEW
        name: String,
        input: String,
        execution_id: u64,
    },
    ActivityCompleted {
        event_id: u64,
        timestamp_ms: u64,  // ← NEW
        source_event_id: u64,
        result: String,
    },
    // ... etc for all event types
}
```

**Migration Strategy:**
```rust
// Backward compatibility via serde
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum EventCompat {
    Versioned(Event),           // New format with schema_version
    Legacy(LegacyEvent),        // Old format without timestamps
}

impl EventCompat {
    fn into_event(self) -> Event {
        match self {
            EventCompat::Versioned(e) => e,
            EventCompat::Legacy(legacy) => {
                // Convert legacy to new format, synthesize timestamp
                legacy.into_v1_with_default_timestamp()
            }
        }
    }
}
```

### Phase 2: Add Optional Metadata (Future)

```rust
OrchestrationStarted {
    // ... existing fields ...
    timestamp_ms: u64,
    
    // Optional metadata (use Option for backward compat)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_id: Option<String>,  // Which worker processed
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,   // Distributed tracing correlation
}

ActivityScheduled {
    // ... existing fields ...
    timestamp_ms: u64,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_count: Option<u32>,   // Which retry attempt (for observability)
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<u64>,  // Activity timeout
}
```

## Forward Compatibility Strategy

### Option 1: Serde Default (Simple, Recommended)

Use `#[serde(default)]` for new fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    OrchestrationStarted {
        event_id: u64,
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
        
        // New field with default
        #[serde(default)]
        timestamp_ms: u64,  // Defaults to 0 if missing
    },
    // ...
}
```

**Pros:**
- Simple to implement
- JSON backward compatible (old events load with default values)
- No migration code needed

**Cons:**
- Old events have `timestamp_ms: 0` (inaccurate but non-breaking)
- No way to distinguish "old event" from "timestamp actually is 0"

### Option 2: Optional Fields (More Flexible)

```rust
OrchestrationStarted {
    event_id: u64,
    name: String,
    // ...
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp_ms: Option<u64>,  // None for old events, Some(ts) for new
}
```

**Pros:**
- Can distinguish old events (None) from new events (Some)
- Metrics can skip events without timestamps
- More explicit

**Cons:**
- Code must handle Option everywhere
- More complex

### Option 3: Schema Version Wrapper (Most Robust)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEvent {
    #[serde(default = "default_schema_version")]
    schema_version: u32,  // 1, 2, 3, ...
    
    #[serde(flatten)]
    event: Event,
}

fn default_schema_version() -> u32 { 1 }

// On read:
let persisted: PersistedEvent = serde_json::from_str(&event_data)?;
match persisted.schema_version {
    1 => { /* handle v1 */ },
    2 => { /* handle v2 with timestamps */ },
    _ => { /* error or forward compat */ },
}
```

**Pros:**
- Explicit versioning
- Can evolve schema safely
- Can have multiple versions coexist

**Cons:**
- More complex
- Requires version-specific handling code
- Larger serialized size

### Option 4: Compatibility Layer (Hybrid)

```rust
// Internal Event representation (with all new fields)
pub enum Event { /* ... with timestamps */ }

// Serialization wrapper
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum EventSerde {
    V1(EventV1),  // New format
    V0(EventV0),  // Old format (auto-detected)
}

impl From<EventSerde> for Event {
    fn from(serde: EventSerde) -> Event {
        match serde {
            EventSerde::V1(v1) => v1.into(),
            EventSerde::V0(v0) => v0.migrate_to_v1(),
        }
    }
}
```

## Recommended Approach (Updated)

**See `proposals/event-schema-redesign.md` for the full architectural redesign.**

**Summary:** Restructure Event from flat enum to struct + EventKind enum with common fields pulled out.

**For Backward Compatibility: Use Serde Default**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    OrchestrationStarted {
        event_id: u64,
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
        
        #[serde(default)]
        timestamp_ms: u64,  // 0 for old events
    },
    // ... all events get timestamp_ms with #[serde(default)]
}

// Helper to check if event has valid timestamp
impl Event {
    pub fn has_timestamp(&self) -> bool {
        self.timestamp_ms() > 0
    }
    
    pub fn timestamp_ms(&self) -> u64 {
        match self {
            Event::OrchestrationStarted { timestamp_ms, .. } => *timestamp_ms,
            // ... all events
        }
    }
}

// In metrics calculation:
if start_event.has_timestamp() && end_event.has_timestamp() {
    let duration_ms = end_event.timestamp_ms() - start_event.timestamp_ms();
    // Use accurate duration
} else {
    // Fall back to turn duration for old events
}
```

**Benefits:**
- ✅ Backward compatible (old events load with timestamp_ms: 0)
- ✅ New events have accurate timestamps
- ✅ Gradual migration (old events age out naturally)
- ✅ Simple implementation
- ✅ Metrics work for new events, degraded for old events

## Other Fields to Add

### High Priority (Add with Timestamps)

1. **All Events:**
   - `timestamp_ms: u64` - When event was created

2. **Scheduling Events (ActivityScheduled, SubOrchestrationScheduled, TimerCreated):**
   - `scheduled_by_worker_id: String` - Which worker created this (for debugging)

3. **Completion Events (ActivityCompleted, ActivityFailed):**
   - `completed_by_worker_id: String` - Which worker executed this

### Medium Priority (Future Enhancement)

4. **ActivityScheduled:**
   - `timeout_ms: Option<u64>` - Activity timeout configuration
   - `retry_policy: Option<RetryPolicy>` - Retry configuration

5. **OrchestrationStarted:**
   - `correlation_id: Option<String>` - Distributed tracing
   - `tags: Option<HashMap<String, String>>` - User metadata

### Low Priority (Nice to Have)

6. **All Events:**
   - `runtime_version: String` - Which duroxide version created this

## Migration Plan

### Step 1: Add `timestamp_ms` with `#[serde(default)]`
- Add to all Event variants
- Default to 0 for old events
- Runtime stamps timestamp when creating events
- Metrics use timestamp if > 0, else fall back

### Step 2: Database Migration (Optional)
- No SQL schema changes needed (JSON is in TEXT column)
- Old events remain unchanged
- New events have timestamps
- Consider adding `has_timestamp` column for efficient querying (optional optimization)

### Step 3: Provider Updates
- Minimal changes - just pass through new fields
- Serialization/deserialization automatic via serde

### Step 4: Deprecation Timeline
- After 6 months, assume all events have timestamps
- Remove fallback code for `timestamp_ms: 0`
- Or: Add migration tool to backfill timestamps from `created_at` column

## Testing Strategy

1. **Backward Compat Test:**
   - Deserialize old JSON without timestamp
   - Verify timestamp_ms defaults to 0
   - Verify event otherwise works

2. **Forward Compat Test:**
   - Serialize new event with timestamp
   - Deserialize in "old" code path
   - Verify extra field is ignored gracefully

3. **Duration Calculation Test:**
   - Create events with timestamps
   - Calculate durations
   - Verify accuracy

## Risk Assessment

**Low Risk:**
- Serde handles missing fields gracefully with `#[serde(default)]`
- JSON format is flexible
- No SQL schema changes needed
- Old events continue to work

**Medium Risk:**
- Need to update ALL Event creation sites to stamp timestamp
- Need to audit that timestamp is set before persistence
- Metrics have degraded accuracy for old events (acceptable)

**Recommendation:** Proceed with Option 1 (Serde Default) for timestamps as first step.

