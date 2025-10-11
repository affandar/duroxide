# Provider Implementation Guide

**For:** LLMs and humans implementing new Duroxide providers  
**Reference:** See `src/providers/sqlite.rs` for complete working implementation

---

## Quick Start

To implement a Provider for Duroxide, you need to:

1. Implement the `Provider` trait from `duroxide::providers`
2. Store orchestration history (append-only event log)
3. Manage 3 work queues with peek-lock semantics
4. Ensure atomic commits for orchestration turns

**Key Principle:** Providers are storage abstractions. The runtime owns orchestration logic.

---

## Complete Implementation Template

```rust
use duroxide::providers::{Provider, WorkItem, OrchestrationItem, ExecutionMetadata};
use duroxide::Event;
use std::sync::Arc;

pub struct MyProvider {
    // Your storage client (e.g., connection pool)
    // Lock timeout (recommended: 30 seconds)
    // Any configuration
}

#[async_trait::async_trait]
impl Provider for MyProvider {
    // === REQUIRED: Core Orchestration Methods ===
    
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        // 1. Find next available message in orchestrator queue
        // 2. Lock ALL messages for that instance
        // 3. Load instance metadata (name, version, execution_id)
        // 4. Load history for current execution_id
        // 5. Return OrchestrationItem with unique lock_token
        
        todo!("See detailed docs below")
    }
    
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        timer_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), String> {
        // ALL operations must be atomic (single transaction):
        // 1. Append history_delta to event log
        // 2. Update execution status/output from metadata (DON'T inspect events!)
        // 3. Create next execution if metadata.create_next_execution=true
        // 4. Enqueue worker_items to worker queue
        // 5. Enqueue timer_items to timer queue
        // 6. Enqueue orchestrator_items to orchestrator queue
        // 7. Delete locked messages (release lock)
        
        todo!("See detailed docs below")
    }
    
    async fn abandon_orchestration_item(&self, lock_token: &str, delay_ms: Option<u64>) -> Result<(), String> {
        // Clear lock_token from messages
        // Optionally delay visibility for backoff
        
        todo!("See detailed docs below")
    }
    
    // === REQUIRED: History Access ===
    
    async fn read(&self, instance: &str) -> Vec<Event> {
        // Return events for LATEST execution, ordered by event_id
        // Return empty Vec if instance doesn't exist
        
        todo!()
    }
    
    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String> {
        // Append events to history for specified execution
        // DO NOT modify event_ids (runtime assigns these)
        // Reject or ignore duplicates (same event_id)
        
        todo!()
    }
    
    // === REQUIRED: Worker Queue ===
    
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        // Add item to worker queue
        // New items should have lock_token = NULL
        
        todo!()
    }
    
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        // Find next unlocked item
        // Lock it with unique token
        // Return item + token (item stays in queue)
        
        todo!()
    }
    
    async fn ack_worker(&self, token: &str) -> Result<(), String> {
        // Delete item from worker queue
        // Idempotent (Ok if already deleted)
        
        todo!()
    }
    
    // === REQUIRED: Orchestrator Queue ===
    
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
        // Extract instance from item (see WorkItem docs)
        // Set visible_at = now() + delay_ms.unwrap_or(0)
        // For StartOrchestration, create instance+execution rows
        
        todo!()
    }
    
    // === OPTIONAL: Multi-Execution Support ===
    
    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        // Return MAX(execution_id) for instance
        // Default implementation uses read() - override for performance
        
        let h = self.read(instance).await;
        if h.is_empty() { None } else { Some(1) }
    }
    
    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
        // Return events for specific execution_id (not just latest)
        // Used for debugging/testing only
        
        self.read(instance).await  // Default ignores execution_id
    }
    
    async fn create_new_execution(
        &self,
        instance: &str,
        orchestration: &str,
        version: &str,
        input: &str,
        parent_instance: Option<&str>,
        parent_id: Option<u64>,
    ) -> Result<u64, String> {
        // DEPRECATED: Handle in ack_orchestration_item via metadata instead
        todo!("Implement only if needed for backward compat")
    }
    
    // === OPTIONAL: Timer Queue (only if supports_delayed_visibility=true) ===
    
    fn supports_delayed_visibility(&self) -> bool {
        false  // Return true if you can delay message visibility
    }
    
    async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String> {
        // Only called if supports_delayed_visibility=true
        // Store timer with fire_at visibility delay
        
        Err("Not supported".to_string())
    }
    
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> {
        // Only called if supports_delayed_visibility=true
        // Return next timer where fire_at <= now()
        
        None
    }
    
    async fn ack_timer(&self, token: &str) -> Result<(), String> {
        // Only called if supports_delayed_visibility=true
        // Delete timer from queue
        
        Err("Not supported".to_string())
    }
    
    // === OPTIONAL: Management APIs ===
    
    async fn list_instances(&self) -> Vec<String> {
        Vec::new()  // Override if you want instance listing
    }
    
    async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let h = self.read(instance).await;
        if h.is_empty() { Vec::new() } else { vec![1] }
    }
}
```

---

## Detailed Implementation Guide

### 1. fetch_orchestration_item() - The Orchestration Turn Fetcher

**Purpose:** Atomically fetch and lock work for one instance.

**Pseudo-code:**
```
BEGIN TRANSACTION

// Find next work (visibility + lock filtering)
row = SELECT id, instance_id FROM orchestrator_queue
      WHERE visible_at <= current_timestamp()
        AND (lock_token IS NULL OR locked_until <= current_timestamp())
      ORDER BY id ASC
      LIMIT 1

IF row IS NULL:
    ROLLBACK
    RETURN None

instance_id = row.instance_id

// Lock ALL messages for this instance
lock_token = generate_uuid()
locked_until = current_timestamp() + lock_timeout_ms

UPDATE orchestrator_queue
SET lock_token = lock_token, locked_until = locked_until
WHERE instance_id = instance_id
  AND (lock_token IS NULL OR locked_until <= current_timestamp())

// Fetch all locked messages
messages = SELECT work_item FROM orchestrator_queue
           WHERE lock_token = lock_token
           ORDER BY id ASC

messages = messages.map(|m| deserialize_workitem(m))

// Load instance metadata
metadata = SELECT orchestration_name, orchestration_version, current_execution_id
           FROM instances
           WHERE instance_id = instance_id

IF metadata IS NULL:
    // New instance - derive from first message or history
    IF messages.first() IS StartOrchestration:
        orchestration_name = messages.first().orchestration
        orchestration_version = messages.first().version.unwrap_or("1.0.0")
        current_execution_id = 1
    ELSE:
        // Shouldn't happen - check history as fallback
        RETURN None or build from history

orchestration_name = metadata.orchestration_name
orchestration_version = metadata.orchestration_version  
current_execution_id = metadata.current_execution_id

// Load history for current execution
history_rows = SELECT event_data FROM history
               WHERE instance_id = instance_id
                 AND execution_id = current_execution_id
               ORDER BY event_id ASC

history = history_rows.map(|row| deserialize_event(row.event_data))

COMMIT TRANSACTION

RETURN Some(OrchestrationItem {
    instance: instance_id,
    orchestration_name,
    execution_id: current_execution_id,
    version: orchestration_version,
    history,
    messages,
    lock_token,
})
```

**Edge Cases:**
- No work available → Return None (dispatcher will sleep)
- Lock contention → Transaction retry or return None
- Missing instance metadata → Derive from messages or history
- Empty history → Valid (new instance)

---

### 2. ack_orchestration_item() - The Atomic Commit

**Purpose:** Atomically commit all changes from an orchestration turn.

**Pseudo-code:**
```
BEGIN TRANSACTION

// Get instance_id from lock_token
instance_id = SELECT DISTINCT instance_id FROM orchestrator_queue
              WHERE lock_token = lock_token

IF instance_id IS NULL:
    ROLLBACK
    RETURN Err("Invalid lock token")

// Get current execution_id
execution_id = SELECT current_execution_id FROM instances
               WHERE instance_id = instance_id
               DEFAULT 1

// 1. Append history_delta (DO NOT inspect events!)
FOR event IN history_delta:
    // Validate event_id was set by runtime
    IF event.event_id() == 0:
        RETURN Err("event_id must be set by runtime")
    
    event_json = serialize(event)
    event_type = extract_discriminant_name(event)  // For indexing only
    
    INSERT INTO history (instance_id, execution_id, event_id, event_type, event_data, created_at)
    VALUES (instance_id, execution_id, event.event_id(), event_type, event_json, NOW())
    ON CONFLICT DO NOTHING  // Idempotent

// 2. Update execution metadata (NO event inspection!)
IF metadata.status IS NOT NULL:
    UPDATE executions
    SET status = metadata.status,
        output = metadata.output,
        completed_at = CURRENT_TIMESTAMP
    WHERE instance_id = instance_id
      AND execution_id = execution_id

// 3. Create next execution if requested
IF metadata.create_next_execution AND metadata.next_execution_id IS NOT NULL:
    next_id = metadata.next_execution_id
    
    INSERT INTO executions (instance_id, execution_id, status, started_at)
    VALUES (instance_id, next_id, 'Running', CURRENT_TIMESTAMP)
    ON CONFLICT DO NOTHING
    
    UPDATE instances
    SET current_execution_id = next_id
    WHERE instance_id = instance_id

// 4. Enqueue worker items
FOR item IN worker_items:
    work_json = serialize(item)
    INSERT INTO worker_queue (work_item, lock_token, locked_until, created_at)
    VALUES (work_json, NULL, NULL, NOW())

// 5. Enqueue timer items (with fire_at for delayed visibility)
FOR item IN timer_items:
    work_json = serialize(item)
    IF item IS TimerSchedule:
        fire_at = item.fire_at_ms
        INSERT INTO timer_queue (work_item, fire_at, lock_token, locked_until, created_at)
        VALUES (work_json, fire_at, NULL, NULL, NOW())

// 6. Enqueue orchestrator items
FOR item IN orchestrator_items:
    work_json = serialize(item)
    target_instance = extract_instance(item)  // See WorkItem docs
    visible_at = NOW()  // Immediate visibility
    
    // Special case: StartOrchestration needs instance creation
    IF item IS StartOrchestration:
        INSERT INTO instances (instance_id, orchestration_name, orchestration_version, current_execution_id)
        VALUES (target_instance, item.orchestration, item.version.unwrap_or("1.0.0"), 1)
        ON CONFLICT DO NOTHING
        
        INSERT INTO executions (instance_id, execution_id, status)
        VALUES (target_instance, 1, 'Running')
        ON CONFLICT DO NOTHING
    
    INSERT INTO orchestrator_queue (instance_id, work_item, visible_at, lock_token, locked_until, created_at)
    VALUES (target_instance, work_json, visible_at, NULL, NULL, NOW())

// 7. Release lock (delete acknowledged messages)
DELETE FROM orchestrator_queue WHERE lock_token = lock_token

COMMIT TRANSACTION
RETURN Ok(())
```

**Critical:**
- All 7 steps must be in ONE transaction
- If ANY step fails, ROLLBACK everything
- Never partially commit

---

### 3. abandon_orchestration_item() - Release Lock for Retry

**Pseudo-code:**
```
visible_at = IF delay_ms IS NOT NULL:
                 current_timestamp() + delay_ms
             ELSE:
                 current_timestamp()

UPDATE orchestrator_queue
SET lock_token = NULL,
    locked_until = NULL,
    visible_at = visible_at
WHERE lock_token = lock_token

// Idempotent - Ok if no rows updated
RETURN Ok(())
```

---

### 4. read() - Load Latest Execution History

**Pseudo-code:**
```
// Get latest execution ID
execution_id = SELECT COALESCE(MAX(execution_id), 1)
               FROM executions
               WHERE instance_id = instance

// Load events for that execution
rows = SELECT event_data FROM history
       WHERE instance_id = instance
         AND execution_id = execution_id
       ORDER BY event_id ASC

events = rows.map(|row| deserialize_event(row.event_data))
         .collect()

RETURN events  // Empty Vec if instance doesn't exist
```

---

### 5. Worker Queue Operations

**enqueue_worker_work():**
```
work_json = serialize(item)
INSERT INTO worker_queue (work_item, lock_token, locked_until)
VALUES (work_json, NULL, NULL)
```

**dequeue_worker_peek_lock():**
```
BEGIN TRANSACTION

row = SELECT id, work_item FROM worker_queue
      WHERE lock_token IS NULL OR locked_until <= current_timestamp()
      ORDER BY id ASC
      LIMIT 1

IF row IS NULL:
    ROLLBACK
    RETURN None

lock_token = generate_uuid()
locked_until = current_timestamp() + lock_timeout_ms

UPDATE worker_queue
SET lock_token = lock_token, locked_until = locked_until
WHERE id = row.id

COMMIT
item = deserialize_workitem(row.work_item)
RETURN Some((item, lock_token))
```

**ack_worker():**
```
DELETE FROM worker_queue WHERE lock_token = lock_token
RETURN Ok(())  // Idempotent
```

---

## Schema Recommendations

### Minimum Required Tables

1. **history** - Append-only event log
   - PRIMARY KEY: (instance_id, execution_id, event_id)
   - Columns: event_data (JSON), event_type (for indexing), created_at

2. **orchestrator_queue** - Orchestration work items
   - PRIMARY KEY: id (auto-increment)
   - Columns: instance_id, work_item (JSON), visible_at, lock_token, locked_until
   - INDEX: (visible_at, lock_token) for fetch performance

3. **worker_queue** - Activity execution requests
   - PRIMARY KEY: id (auto-increment)
   - Columns: work_item (JSON), lock_token, locked_until

4. **instances** - Instance metadata
   - PRIMARY KEY: instance_id
   - Columns: orchestration_name, orchestration_version, current_execution_id

5. **executions** - Execution tracking
   - PRIMARY KEY: (instance_id, execution_id)
   - Columns: status, output, started_at, completed_at

### Optional Tables

6. **timer_queue** - Timer schedules (only if supports_delayed_visibility=true)
   - PRIMARY KEY: id (auto-increment)
   - Columns: work_item (JSON), fire_at, lock_token, locked_until
   - INDEX: (fire_at, lock_token)

---

## Common Pitfalls

### ❌ DON'T: Inspect Event Contents

```rust
// WRONG - Provider inspecting events
for event in &history_delta {
    match event {
        Event::OrchestrationCompleted { output, .. } => {
            // Provider understanding orchestration semantics
            self.update_status("Completed", output)?;
        }
    }
}
```

```rust
// CORRECT - Use metadata from runtime
if let Some(status) = &metadata.status {
    self.update_status(status, &metadata.output)?;
}
```

### ❌ DON'T: Modify Event IDs

```rust
// WRONG - Renumbering events
let mut next_id = self.get_next_event_id()?;
for mut event in history_delta {
    event.set_event_id(next_id);  // DON'T DO THIS!
    next_id += 1;
}
```

```rust
// CORRECT - Store as-is
for event in &history_delta {
    assert!(event.event_id() > 0, "Runtime must set event_ids");
    self.insert_event(event)?;  // Use runtime-assigned IDs
}
```

### ❌ DON'T: Break Atomicity

```rust
// WRONG - Non-atomic
self.append_history(history_delta).await?;  // Committed!
self.enqueue_workers(worker_items).await?;  // If this fails, history already saved!
self.release_lock(lock_token).await?;
```

```rust
// CORRECT - Single transaction
let tx = self.begin_transaction().await?;
self.append_history_in_tx(&tx, history_delta).await?;
self.enqueue_workers_in_tx(&tx, worker_items).await?;
self.release_lock_in_tx(&tx, lock_token).await?;
tx.commit().await?;  // All or nothing
```

---

## Testing Your Provider

Use `tests/sqlite_provider_test.rs` as a template. Key tests:

1. **Basic workflow** (test_sqlite_provider_basic)
   - Enqueue StartOrchestration
   - Fetch and verify OrchestrationItem
   - Ack with history
   - Verify history saved

2. **Atomicity** (test_sqlite_provider_transactional)
   - Ack with multiple operations
   - Verify all succeeded or all failed

3. **Lock expiration** (test_lock_expiration)
   - Fetch but don't ack
   - Wait for lock timeout
   - Verify message redelivered

4. **Execution status persistence** (test_execution_status_*)
   - Verify metadata properly stored
   - Test Completed, Failed, ContinuedAsNew states

5. **Multi-execution** (test_execution_status_continued_as_new)
   - ContinueAsNew creates execution 2
   - Verify current_execution_id updated
   - Complete execution 2, verify status

---

## Performance Considerations

### Indexes (Critical for Performance)

```sql
-- Orchestrator queue (hot path)
CREATE INDEX idx_orch_visible ON orchestrator_queue(visible_at, lock_token);
CREATE INDEX idx_orch_instance ON orchestrator_queue(instance_id);

-- Worker queue
CREATE INDEX idx_worker_lock ON worker_queue(lock_token);

-- Timer queue (if supported)
CREATE INDEX idx_timer_fire ON timer_queue(fire_at, lock_token);

-- History (for read operations)
CREATE INDEX idx_history_lookup ON history(instance_id, execution_id, event_id);
```

### Connection Pooling

- Use connection pools for concurrent dispatcher access
- Recommended pool size: 5-10 connections
- SQLite example: `SqlitePoolOptions::new().max_connections(10)`

### Lock Timeout

- Recommended: 30 seconds
- Too short: False retries under load
- Too long: Slow recovery from crashes

---

## Validation Checklist

Before considering your provider production-ready:

- [ ] All 12 provider tests from sqlite_provider_test.rs pass
- [ ] fetch_orchestration_item returns None when queue empty
- [ ] ack_orchestration_item is fully atomic (single transaction)
- [ ] Lock expiration works (messages redelivered after timeout)
- [ ] Multi-execution support (ContinueAsNew creates execution 2, 3, ...)
- [ ] Execution metadata stored correctly (status, output)
- [ ] History ordering preserved (events returned in event_id order)
- [ ] Concurrent access safe (run with RUST_TEST_THREADS=10)
- [ ] No event content inspection (use ExecutionMetadata only)
- [ ] Worker queue FIFO behavior
- [ ] No duplicate event IDs (PRIMARY KEY enforced)

---

## Example Providers to Implement

### Easy
- **In-Memory Provider**: Use HashMap + RwLock
- **File-based Provider**: JSON files + file locks

### Medium
- **PostgreSQL Provider**: Similar to SQLite, better concurrency
- **MySQL Provider**: Similar to PostgreSQL

### Advanced
- **Redis Provider**: Lua scripts for atomicity, sorted sets for history
- **DynamoDB Provider**: Conditional writes, GSI for queues
- **Cosmos DB Provider**: Change feed for queues, partition by instance_id

---

## Getting Help

- **Reference Implementation**: `src/providers/sqlite.rs`
- **Interface Definition**: `src/providers/mod.rs` (this file, with full docs)
- **Tests**: `tests/sqlite_provider_test.rs`
- **Schema**: `migrations/20240101000000_initial_schema.sql`

---

**With these annotations, an LLM can implement a fully functional Provider!** 🎉

