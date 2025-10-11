# Provider Abstraction Analysis: Leaky Abstractions

**Date:** 2025-10-10  
**Status:** 🔴 Multiple leaky abstractions identified

## Executive Summary

The Provider trait and SQLite implementation contain **significant orchestration-specific logic** that violates the principle that "the provider shouldn't know anything about the semantics of an orchestration apart from the fact that some things need to be transactional."

**Current State:** Provider is tightly coupled to Duroxide orchestration semantics  
**Ideal State:** Provider should be a pure storage/queue abstraction

---

## Leaky Abstractions Identified

### 🔴 **CRITICAL: Provider Creates and Inspects Events**

**Location:** `src/providers/sqlite.rs:740-811`

```rust
// Update execution status based on terminal events
for event in &history_delta {
    match event {
        Event::OrchestrationCompleted { output, .. } => {
            // Provider understands what "completion" means
            // Provider extracts output from event
            sqlx::query("UPDATE executions SET status = 'Completed', output = ?...")
        }
        Event::OrchestrationFailed { error, .. } => {
            // Provider understands what "failure" means
            // Provider extracts error from event  
        }
        Event::OrchestrationContinuedAsNew { input, .. } => {
            // Provider understands ContinueAsNew semantics
            // Provider extracts next input
        }
    }
}
```

**Problem:**
- Provider pattern-matches on specific Event variants
- Provider interprets the semantic meaning of events (completion, failure, continuation)
- Provider extracts data from event payloads

**Why it's wrong:**
- Events should be opaque blobs to the provider
- The provider shouldn't need to know what "OrchestrationCompleted" means
- This couples the provider to Duroxide's specific event model

---

### 🔴 **CRITICAL: Provider Manages Multi-Execution Lifecycle**

**Location:** `src/providers/sqlite.rs:825-854`

```rust
let has_continue_as_new = orchestrator_items.iter()
    .any(|item| matches!(item, WorkItem::ContinueAsNew { .. }));

if has_continue_as_new {
    let next_exec_id = execution_id + 1;
    // Create next execution
    sqlx::query("INSERT INTO executions (instance_id, execution_id, status) VALUES (?, ?, 'Running')")
    // Update current_execution_id
    sqlx::query("UPDATE instances SET current_execution_id = ?")
}
```

**Problem:**
- Provider understands ContinueAsNew semantics
- Provider manages execution lifecycle
- Provider determines when to create new executions

**Why it's wrong:**
- The runtime should decide when/how to create executions
- Provider should just store data, not interpret orchestration control flow

---

### 🔴 **CRITICAL: Provider Creates Events**

**Location:** `src/providers/sqlite.rs:1325-1333`

```rust
async fn create_new_execution(...) -> Result<u64, String> {
    // Provider creates an OrchestrationStarted event!
    let start_event = Event::OrchestrationStarted {
        event_id: 1,
        name: orchestration.to_string(),
        version: version.to_string(),
        input: input.to_string(),
        parent_instance: parent_instance.map(|s| s.to_string()),
        parent_id,
    };
    
    self.append_history_in_tx(&mut tx, instance, new_exec_id, vec![start_event])
}
```

**Problem:**
- Provider constructs Event::OrchestrationStarted
- Provider knows the structure and meaning of orchestration start events
- This is application-level logic, not storage logic

**Why it's wrong:**
- Runtime should create all events
- Provider should only persist what the runtime gives it

---

### 🟡 **MEDIUM: Provider Inspects WorkItem Types**

**Location:** Multiple places (lines 25-34, 909-920, 826)

```rust
let instance = match &item {
    WorkItem::StartOrchestration { instance, .. } |
    WorkItem::ActivityCompleted { instance, .. } |
    WorkItem::ContinueAsNew { instance, .. } => instance,
    WorkItem::SubOrchCompleted { parent_instance, .. } => parent_instance,
    _ => ...
};
```

**Problem:**
- Provider pattern-matches on 11+ WorkItem variants
- Provider extracts different fields based on variant type
- Provider has special handling for StartOrchestration and ContinueAsNew

**Why it's problematic:**
- Adding new WorkItem types requires provider changes
- Provider is coupled to orchestration work types

---

### 🟡 **MEDIUM: Provider Manages Instance Metadata**

**Location:** `src/providers/sqlite.rs:39-51, 709-718, 924-940`

```rust
// Provider creates instance rows
sqlx::query("INSERT OR IGNORE INTO instances (instance_id, orchestration_name, orchestration_version)")

// Provider updates instance metadata
sqlx::query("UPDATE instances SET orchestration_name = ?, orchestration_version = ?")
```

**Problem:**
- Provider maintains `instances` table with orchestration-specific metadata
- Provider knows about orchestration names and versions

**Why it's problematic:**
- Instance metadata is application-level, not storage-level
- Violates separation of concerns

---

### 🟡 **MEDIUM: Provider Determines "Latest" Execution**

**Location:** `src/providers/sqlite.rs:1063-1069` (`read()` method)

```rust
async fn read(&self, instance: &str) -> Vec<Event> {
    let execution_id: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(execution_id), 1) FROM executions WHERE instance_id = ?"
    )
    // ...returns latest execution's history
}
```

**Problem:**
- Provider decides which execution is "current" 
- Documentation says "returns the latest execution's history" - semantic knowledge

**Why it's problematic:**
- Runtime should decide which execution to load
- "Latest" is an orchestration concept, not a storage concept

---

### 🟡 **MEDIUM: OrchestrationItem Contains Semantic Fields**

**Location:** `src/providers/mod.rs:5-13`

```rust
pub struct OrchestrationItem {
    pub instance: String,
    pub orchestration_name: String,  // 🔴 Semantic
    pub execution_id: u64,           // 🔴 Semantic  
    pub version: String,             // 🔴 Semantic
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
}
```

**Problem:**
- OrchestrationItem exposes orchestration_name, execution_id, version
- These are application-level concepts

**Why it's problematic:**
- Runtime could derive these from history/messages
- Couples provider interface to orchestration concepts

---

### 🟢 **MINOR: Event Type String Mapping**

**Location:** `src/providers/sqlite.rs:462-480`

```rust
let event_type = match event {
    Event::OrchestrationStarted { .. } => "OrchestrationStarted",
    Event::OrchestrationCompleted { .. } => "OrchestrationCompleted",
    // ... 15+ event types
};
```

**Problem:**
- Provider needs to know all Event variants for storage

**Why it's minor:**
- This is just for indexing/debugging
- Doesn't affect runtime behavior
- Could be replaced with a single "Event" type column

---

## Impact Assessment

### What Works Despite The Leaks

✅ **Current functionality is correct** - All tests pass  
✅ **Transactional guarantees work** - ACID properties maintained  
✅ **Only one Provider implementation** - No actual abstraction burden yet

### What's Problematic

🔴 **Tight coupling** - Provider can't be reused for non-Duroxide workflows  
🔴 **Hard to extend** - Adding new event/work types requires provider changes  
🔴 **Violates SRP** - Provider has both storage AND orchestration logic  
🔴 **Testing complexity** - Provider tests need orchestration knowledge

---

## Recommended Refactoring

### Option 1: Minimal Fix - Move Logic to Runtime (Recommended)

**Goal:** Keep provider as-is, but move orchestration logic out

**Changes:**

1. **Move execution status updates to runtime**
   - Runtime inspects history_delta before calling `ack_orchestration_item`
   - Runtime builds "execution state" metadata
   - Provider gets simple key-value updates instead of event inspection

2. **Move execution creation to runtime**
   - Remove `create_new_execution` from Provider trait
   - Runtime handles ContinueAsNew by creating new execution records
   - Provider just stores what runtime provides

3. **Move OrchestrationStarted creation to runtime**
   - Already mostly done - runtime creates events
   - Remove create_new_execution that builds events

### Option 2: Clean Provider Interface (Ideal but Large Refactor)

**Goal:** Make Provider truly generic

**New Interface:**
```rust
trait Provider {
    // Pure append-only log
    async fn append_events(instance: &str, partition_key: u64, events: Vec<Event>);
    async fn read_events(instance: &str, partition_key: u64) -> Vec<Event>;
    
    // Generic queue with peek-lock
    async fn enqueue(queue: &str, item: Vec<u8>, visible_after_ms: Option<u64>);
    async fn dequeue(queue: &str) -> Option<(Vec<u8>, LockToken)>;
    async fn ack(token: LockToken);
    
    // Atomic batch
    async fn atomic_batch(ops: Vec<Operation>) -> Result<(), Error>;
}
```

**Benefits:**
- Provider knows nothing about orchestrations
- Can store any event-sourced system
- Clean separation of concerns

**Drawbacks:**
- Large refactor
- Might lose some SQLite-specific optimizations

---

## Option 3: Hybrid - Provider with Minimal Semantic Hooks

**Goal:** Allow provider optimizations while limiting semantic knowledge

**Changes:**

1. **Add generic metadata table**
   ```sql
   CREATE TABLE metadata (
       instance_id TEXT,
       partition_id INTEGER,  -- execution_id
       key TEXT,
       value TEXT,
       PRIMARY KEY (instance_id, partition_id, key)
   );
   ```

2. **Runtime provides metadata updates**
   ```rust
   // Runtime code:
   let metadata = vec![
       ("status", "Completed"),
       ("output", output_string),
       ("completed_at", timestamp),
   ];
   
   provider.ack_orchestration_item(
       token,
       history_delta,
       worker_items,
       timer_items,
       orchestrator_items,
       metadata  // NEW: generic key-value updates
   );
   ```

3. **Provider blindly stores metadata**
   - No inspection of events
   - No understanding of what "status" or "output" mean
   - Just key-value storage

---

## Concrete Action Items (Priority Order)

### Phase 1: Quick Wins (Low Risk)

1. ✅ **Remove `create_new_execution` from Provider trait**
   - Move logic to runtime
   - Provider only needs `append_events` for any execution

2. ✅ **Move status/output extraction to runtime**
   - Runtime inspects history_delta
   - Runtime calls: `update_execution_metadata(instance, exec_id, metadata)`
   - Provider stores metadata without interpretation

3. ✅ **Remove orchestration_name/version from OrchestrationItem**
   - Runtime can derive from history if needed
   - Reduces provider interface coupling

### Phase 2: Interface Cleanup (Medium Risk)

4. ⚠️ **Simplify WorkItem**
   - Use tagged union or simple struct
   - Reduce variant count
   - Make instance extraction generic

5. ⚠️ **Make read() generic**
   - `read(instance, partition_id)` instead of "latest execution" logic
   - Runtime decides which partition to read

### Phase 3: Complete Abstraction (High Risk, High Reward)

6. 🔄 **Full provider redesign** (Option 2 above)
   - Pure event log + queue interface
   - No orchestration knowledge
   - Reusable for any event-sourced system

---

## Immediate Recommendation

**Start with Phase 1, item #2**: Move the status/output update logic out of the provider.

This is the biggest leak (provider inspecting event contents) and can be fixed with minimal risk:

1. Runtime inspects `history_delta` BEFORE calling `ack_orchestration_item`
2. Runtime determines: `execution_status`, `output_value`, `should_create_next_execution`
3. Runtime passes simple metadata to provider
4. Provider blindly stores metadata

This gives us 80% of the benefit with 20% of the risk.

---

## Files Requiring Changes (Phase 1, Item #2)

1. `src/providers/mod.rs` - Add metadata parameter to `ack_orchestration_item`
2. `src/providers/sqlite.rs` - Replace event inspection with metadata storage
3. `src/runtime/execution.rs` - Inspect history_delta and build metadata
4. Tests - Update to pass metadata

**Estimated effort:** 2-3 hours  
**Risk:** Low (existing tests will catch regressions)  
**Benefit:** Cleaner abstraction, easier to add new providers

---

## Conclusion

The current Provider implementation works correctly but violates separation of concerns by embedding orchestration semantics. The most egregious leak is **event content inspection to update execution status** (lines 740-811 in sqlite.rs).

Recommended fix: Extract this logic into the runtime where it belongs, and have the provider accept pre-computed metadata instead of interpreting events.

