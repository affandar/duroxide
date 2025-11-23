# SQLite Provider - Locking and Transaction Mechanics

## Overview

The SQLite provider implements a peek-lock queue pattern with instance-level locking to support concurrent orchestration processing. This document details the transaction boundaries, locking mechanics, and race prevention strategies.

## Core Concepts

### Instance-Level Locking

**Purpose**: Prevent concurrent processing of the same orchestration instance

**Mechanism**: `instance_locks` table tracks which instances are currently being processed

```sql
CREATE TABLE instance_locks (
    instance_id TEXT PRIMARY KEY,
    lock_token TEXT NOT NULL,
    locked_until INTEGER NOT NULL,  -- Milliseconds since epoch
    locked_at INTEGER NOT NULL
);
```

**Invariants**:
- Only ONE worker can hold a lock for an instance at a time
- Lock expires after timeout (e.g., 30 seconds) for crash recovery
- Instance with valid lock is invisible to other workers
- New messages for locked instances are queued but not fetchable until lock released

### Message-Level Markers

**Purpose**: Distinguish messages fetched in current batch from late arrivals

**Mechanism**: `lock_token` column in `orchestrator_queue`

**Why**: Messages can arrive while instance is being processed. We only delete messages that were part of the fetched batch, not new arrivals.

---

## Method Pseudocode

### Core Orchestration Queue Methods

#### `fetch_orchestration_item() -> Option<OrchestrationItem>`

**Purpose**: Atomically lock an available instance and return its messages + history

```
BEGIN TRANSACTION (deferred - default)
  now = current_timestamp_ms()
  
  // Find an unlocked instance with visible messages
  instance_id = SELECT q.instance_id 
                FROM orchestrator_queue q
                LEFT JOIN instance_locks il ON q.instance_id = il.instance_id
                WHERE q.visible_at <= now
                  AND (il.instance_id IS NULL OR il.locked_until <= now)
                ORDER BY q.id
                LIMIT 1
  
  IF no instance found:
    ROLLBACK
    RETURN None
  
  // Atomically acquire instance lock
  lock_token = generate_uuid()
  locked_until = now + lock_timeout (e.g., 30 seconds)
  
  rows_affected = INSERT INTO instance_locks 
                  (instance_id, lock_token, locked_until, locked_at)
                  VALUES (instance_id, lock_token, locked_until, now)
                  ON CONFLICT(instance_id) DO UPDATE
                  SET lock_token = lock_token, locked_until = locked_until, locked_at = now
                  WHERE locked_until <= now  // Only update if expired
  
  IF rows_affected == 0:
    // Another worker grabbed the lock between SELECT and INSERT
    ROLLBACK
    RETURN None
  
  // Mark messages with our lock_token for later deletion
  UPDATE orchestrator_queue
  SET lock_token = lock_token
  WHERE instance_id = instance_id AND visible_at <= now
  
  // Fetch marked messages
  messages = SELECT id, work_item 
             FROM orchestrator_queue
             WHERE lock_token = lock_token
             ORDER BY id
  
  IF messages is empty:
    DELETE FROM instance_locks WHERE instance_id = instance_id
    ROLLBACK
    RETURN None
  
  // Load instance metadata
  (orch_name, version, exec_id) = 
    SELECT orchestration_name, orchestration_version, current_execution_id
    FROM instances
    WHERE instance_id = instance_id
  
  IF no instance row:
    // Brand new instance - derive from messages
    (orch_name, version, exec_id) = extract_from_start_message(messages)
  
  // Load full history for current execution
  history = SELECT event_data 
            FROM history
            WHERE instance_id = instance_id AND execution_id = exec_id
            ORDER BY event_id
  
COMMIT  // Lock is now durable, messages marked, instance unavailable to others

RETURN OrchestrationItem {
  instance_id,
  orchestration_name,
  execution_id,
  version,
  history,  // Deserialized Vec<Event>
  messages, // Deserialized Vec<WorkItem>
  lock_token
}
```

**Locking Invariants:**
- Instance lock prevents ANY other worker from fetching this instance
- New messages arriving for locked instance are queued but not fetchable
- Lock expires after timeout for crash recovery
- Only messages visible at fetch time are included (late arrivals wait for next turn)

---

#### `ack_orchestration_item(lock_token, exec_id, history_delta, worker_items, timer_items, orch_items, metadata) -> Result<()>`

**Purpose**: Commit turn results atomically and release instance lock

```
BEGIN TRANSACTION (deferred)
  now = current_timestamp_ms()
  
  // Lookup instance from lock token
  instance_id = SELECT instance_id 
                FROM instance_locks 
                WHERE lock_token = lock_token
  
  IF not found:
    ROLLBACK
    RETURN Err("Invalid lock token")
  
  // Validate lock hasn't expired
  lock_valid = SELECT COUNT(*) 
               FROM instance_locks
               WHERE instance_id = instance_id 
                 AND lock_token = lock_token
                 AND locked_until > now
  
  IF lock_valid == 0:
    // Lock expired during processing - abort
    ROLLBACK
    RETURN Err("Instance lock expired")
  
  // Delete messages we processed (marked with our lock_token)
  // New messages that arrived after fetch remain in queue
  DELETE FROM orchestrator_queue WHERE lock_token = lock_token
  
  // Update instance metadata if this is OrchestrationStarted
  IF first event in history_delta is OrchestrationStarted:
    UPDATE instances
    SET orchestration_name = name, orchestration_version = version
    WHERE instance_id = instance_id
  
  // Create execution row (idempotent)
  INSERT OR IGNORE INTO executions (instance_id, execution_id, status)
  VALUES (instance_id, exec_id, 'Running')
  
  // Update current_execution_id (for ContinueAsNew)
  UPDATE instances
  SET current_execution_id = MAX(current_execution_id, exec_id)
  WHERE instance_id = instance_id
  
  // Append history events
  FOR EACH event IN history_delta:
    INSERT INTO history (instance_id, execution_id, event_id, event_type, event_data)
    VALUES (instance_id, exec_id, event.event_id(), event.type(), serialize(event))
    // UNIQUE constraint on (instance_id, execution_id, event_id) prevents duplicates
  
  // Update execution status if terminal
  IF metadata.status IS NOT NULL:
    UPDATE executions
    SET status = metadata.status, output = metadata.output, completed_at = NOW
    WHERE instance_id = instance_id AND execution_id = exec_id
  
  // Enqueue worker items (activities to execute)
  FOR EACH item IN worker_items:
    INSERT INTO worker_queue (work_item) VALUES (serialize(item))
  
  // Enqueue orchestrator items (sub-orchestrations, timers, continuations, completions to parent)
  FOR EACH item IN orch_items:
    target_instance = extract_instance(item)
    
    // Create instance row for StartOrchestration
    IF item is StartOrchestration:
      INSERT OR IGNORE INTO instances (instance_id, orch_name, orch_version)
      VALUES (target_instance, name, version)
      
      INSERT OR IGNORE INTO executions (instance_id, execution_id)
      VALUES (target_instance, 1)
    
    INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
    VALUES (target_instance, serialize(item), now)
  
  // Release instance lock
  DELETE FROM instance_locks 
  WHERE instance_id = instance_id AND lock_token = lock_token

COMMIT  // All changes atomic: history + work items + lock release

RETURN Ok(())
```

**Critical Atomicity:**
- History append, work item enqueues, and lock release are ONE transaction
- If any part fails, entire transaction rolls back
- Lock validation ensures we don't commit stale work
- Partial ack is impossible
- Lock MUST be released or it blocks instance forever

---

#### `abandon_orchestration_item(lock_token, delay) -> Result<()>`

**Purpose**: Release instance lock without committing work (failure/timeout)

```
// Get instance before removing lock
instance_id = SELECT instance_id 
              FROM instance_locks 
              WHERE lock_token = lock_token

IF not found:
  RETURN Err("Invalid lock token")

// Remove lock immediately (not transactional - simple DELETE with auto-commit)
DELETE FROM instance_locks WHERE lock_token = lock_token

// Optionally delay messages for backoff (delay is Duration)
IF delay is Some:
  visible_at = now + delay.as_millis()
  UPDATE orchestrator_queue
  SET visible_at = now + delay_ms
  WHERE instance_id = instance_id AND visible_at <= now

RETURN Ok(())
```

**Note**: Not wrapped in explicit transaction - each statement auto-commits. Lock removal is immediate, instance becomes available to other workers.

---

#### `enqueue_orchestrator_work(item, delay_ms) -> Result<()>`

**Purpose**: Add message to orchestrator queue with optional visibility delay

```
BEGIN TRANSACTION
  instance = extract_instance_from_item(item)
  
  // Create instance row if this is StartOrchestration
  IF item is StartOrchestration:
    INSERT OR IGNORE INTO instances 
    (instance_id, orchestration_name, orchestration_version)
    VALUES (instance, orch_name, version)
    
    INSERT OR IGNORE INTO executions (instance_id, execution_id)
    VALUES (instance, 1)
  
  // Insert message with visibility
  visible_at = IF delay THEN (now + delay.as_millis()) ELSE now
  
  INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
  VALUES (instance, serialize(item), visible_at)

COMMIT

RETURN Ok(())
```

**Locking behavior:**
- Does NOT check if instance is locked
- Messages for locked instances are queued but won't be fetched until lock released
- Multiple completions can queue up while instance is processing
- Messages become fetchable only when: `visible_at <= now AND instance not locked`

---

### Worker Queue Methods

#### `dequeue_worker_peek_lock() -> Option<(WorkItem, lock_token)>`

**Purpose**: Peek-lock one activity from worker queue

```
BEGIN TRANSACTION
  now = current_timestamp_ms()
  
  // Find first unlocked worker item
  row = SELECT id, work_item
        FROM worker_queue
        WHERE (lock_token IS NULL OR locked_until <= now)
        ORDER BY id
        LIMIT 1
  
  IF not found:
    ROLLBACK
    RETURN None
  
  // Acquire message-level lock
  lock_token = generate_uuid()
  locked_until = now + lock_timeout
  
  UPDATE worker_queue
  SET lock_token = lock_token, locked_until = locked_until
  WHERE id = row.id
  
  work_item = deserialize(row.work_item)

COMMIT

RETURN Some((work_item, lock_token))
```

**Note**: Message-level locking (not instance-level) - workers can execute different activities for same instance concurrently. Activities are independent work units.

---

#### `ack_worker(lock_token) -> Result<()>`

**Purpose**: Remove completed activity from queue

```
// Simple delete (auto-commit)
DELETE FROM worker_queue WHERE lock_token = lock_token

RETURN Ok(())
```

---

**Note:** Timer functionality is now handled directly via the orchestrator queue with delayed visibility. The `timer_queue` table and related methods (`dequeue_timer_peek_lock`, `ack_timer`) have been removed. `TimerFired` work items are enqueued to the orchestrator queue with `visible_at = fire_at_ms`.

---

### History Management Methods

#### `read(instance) -> Vec<Event>`

**Purpose**: Read full history for an instance's current execution

```
BEGIN TRANSACTION (read-only)
  // Get current execution_id
  exec_id = SELECT current_execution_id 
            FROM instances 
            WHERE instance_id = instance
  
  IF no instance:
    ROLLBACK
    RETURN empty vec
  
  // Load history for that execution
  rows = SELECT event_data
         FROM history
         WHERE instance_id = instance AND execution_id = exec_id
         ORDER BY event_id
  
  history = rows.map(|r| deserialize(r.event_data))

COMMIT (or ROLLBACK - read-only)

RETURN history
```

---

#### `append(instance, execution_id, events) -> Result<()>`

**Purpose**: Append events to history (standalone, not part of ack transaction)

```
BEGIN TRANSACTION
  FOR EACH event IN events:
    INSERT INTO history (instance_id, execution_id, event_id, event_type, event_data)
    VALUES (instance, execution_id, event.id(), event.type(), serialize(event))
    // UNIQUE constraint prevents duplicate event_ids

COMMIT

RETURN Ok(())
```

**Note**: Rarely used - most history appends happen in `ack_orchestration_item`

---

### Management/Introspection Methods

#### `list_instances() -> Vec<InstanceInfo>`

**Purpose**: List all orchestration instances

```
BEGIN TRANSACTION (read-only)
  rows = SELECT i.instance_id, i.orchestration_name, i.orchestration_version,
                i.current_execution_id, e.status, e.output
         FROM instances i
         LEFT JOIN executions e ON i.instance_id = e.instance_id 
                                AND i.current_execution_id = e.execution_id
         ORDER BY i.created_at DESC

COMMIT

RETURN rows.map(|r| InstanceInfo { ... })
```

---

#### `read_execution_history(instance, execution_id) -> Vec<Event>`

**Purpose**: Read history for a specific execution (not necessarily current)

```
BEGIN TRANSACTION (read-only)
  rows = SELECT event_data
         FROM history
         WHERE instance_id = instance AND execution_id = execution_id
         ORDER BY event_id

COMMIT

RETURN rows.map(|r| deserialize(r.event_data))
```

---

#### `latest_execution_id(instance) -> Option<u64>`

**Purpose**: Get current execution ID for an instance

```
BEGIN TRANSACTION (read-only)
  exec_id = SELECT current_execution_id 
            FROM instances 
            WHERE instance_id = instance

COMMIT

RETURN exec_id
```

---

## Race Prevention Scenarios

### Scenario 1: Concurrent Fetch of Same Instance

**What happens when two workers try to fetch the same instance?**

```
Time  Worker A                              Worker B
----  ------------------------------------  ------------------------------------
T1    BEGIN TRANSACTION
T2    SELECT instance (finds "foo")         BEGIN TRANSACTION
T3                                          SELECT instance (finds "foo")
T4    INSERT INTO instance_locks            
      (instance=foo, token=A)
      → rows_affected = 1 ✅
T5                                          INSERT INTO instance_locks
                                            (instance=foo, token=B)
                                            ON CONFLICT DO UPDATE WHERE ...
                                            → rows_affected = 0 ❌ (lock still valid)
T6    COMMIT (lock acquired)                
T7                                          ROLLBACK (failed to acquire lock)
T8                                          RETURN None
T9    ... process instance foo ...
T10   ack() → validates lock → releases
```

**Key Protection**: `ON CONFLICT ... WHERE locked_until <= now` only updates if lock expired.
If lock is still valid, UPDATE affects 0 rows, worker detects failure and aborts.

---

### Scenario 2: New Messages Arrive During Lock

**What happens when completions arrive while instance is being processed?**

```
State of orchestrator_queue:

T1: Worker A fetches instance "foo"
    +-------------+-----------+---------+---------------+
    | instance_id | work_item | visible | lock_token    |
    +-------------+-----------+---------+---------------+
    | foo         | Start     | T0      | token-A       | ← Marked by fetch
    +-------------+-----------+---------+---------------+

T2: ActivityCompleted arrives for "foo" (while A is processing)
    +-------------+-----------+---------+---------------+
    | instance_id | work_item | visible | lock_token    |
    +-------------+-----------+---------+---------------+
    | foo         | Start     | T0      | token-A       | ← Will be deleted on ack
    | foo         | Completed | T2      | NULL          | ← NOT marked, queued
    +-------------+-----------+---------+---------------+

    State of instance_locks:
    +-------------+------------+--------------+
    | instance_id | lock_token | locked_until |
    +-------------+------------+--------------+
    | foo         | token-A    | T1 + 30s     |
    +-------------+------------+--------------+

T3: Worker B tries to fetch
    SELECT q.instance_id
    FROM orchestrator_queue q
    LEFT JOIN instance_locks il ON q.instance_id = il.instance_id
    WHERE q.visible_at <= now
      AND (il.instance_id IS NULL OR il.locked_until <= now)
    
    → Filters OUT "foo" because:
      - il.instance_id IS NOT NULL (lock exists)
      - il.locked_until > now (lock still valid)
    
    → Worker B gets None or different instance

T4: Worker A completes processing and acks
    DELETE FROM orchestrator_queue WHERE lock_token = token-A  // Removes Start only
    DELETE FROM instance_locks WHERE instance_id = foo         // Releases lock
    
    State after:
    +-------------+-----------+---------+---------------+
    | instance_id | work_item | visible | lock_token    |
    +-------------+-----------+---------+---------------+
    | foo         | Completed | T2      | NULL          | ← Now available
    +-------------+-----------+---------+---------------+
    
    instance_locks: (empty for "foo")

T5: Worker B fetches again
    → Gets instance "foo" with ActivityCompleted message
    → Processes next turn
```

**Key Protection**: Instance-level lock in `instance_locks` table controls fetch eligibility.
Messages for locked instances accumulate but are invisible to other workers.

---

### Scenario 3: Timeout Validation on Ack

**What happens when a worker takes too long to process?**

```
Time  Worker A                              Worker B
----  ------------------------------------  ------------------------------------
T1    fetch() → lock acquired
      locked_until = T1 + 30s
T2    ... processing (slow database, etc) ...
...   
T31   ... still processing ...              
T32   try to ack()                          fetch() finds instance unlocked
                                            (locked_until = T1+30s < T32)
T33   SELECT COUNT(*) WHERE                 INSERT into instance_locks
      instance_id=foo AND                   (instance=foo, token=B)
      lock_token=A AND                      → acquires NEW lock ✅
      locked_until > T32
      → returns 0 ❌ (expired)
T34   ROLLBACK                              COMMIT
      RETURN Err("lock expired")            RETURN Some(OrchestrationItem)
T35                                         ... processes instance ...
      // Worker A's work is discarded
```

**Key Protection**: Lock validation in ack prevents stale commits. If lock expired, another worker may have already acquired it.

---

### Scenario 4: Multiple Completions Flood In

**What happens when many completions arrive rapidly?**

```
T1: Worker A fetches "foo" with [StartOrchestration]
    Locked: foo (locked_until = T1 + 30s)

T2-T10: 10 ActivityCompleted messages enqueued for "foo"
    orchestrator_queue:
    +-------------+----------------+---------+-------------+
    | instance_id | work_item      | visible | lock_token  |
    +-------------+----------------+---------+-------------+
    | foo         | Start          | T0      | token-A     | ← Being processed
    | foo         | Completed(id=1)| T2      | NULL        | ← Queued
    | foo         | Completed(id=2)| T3      | NULL        | ← Queued
    | ...         | ...            | ...     | NULL        | ← Queued
    +-------------+----------------+---------+-------------+

T11: Worker B tries to fetch
    LEFT JOIN filters out "foo" (locked)
    → Returns None or different instance

T15: Worker A acks
    DELETE WHERE lock_token = token-A  // Removes only Start message
    DELETE FROM instance_locks WHERE instance_id = foo
    
    orchestrator_queue after:
    +-------------+----------------+---------+-------------+
    | instance_id | work_item      | visible | lock_token  |
    +-------------+----------------+---------+-------------+
    | foo         | Completed(id=1)| T2      | NULL        | ← Available
    | foo         | Completed(id=2)| T3      | NULL        |
    | ...         | ...            | ...     | NULL        |
    +-------------+----------------+---------+-------------+

T16: Worker B fetches
    → Gets "foo" with ALL 10 completion messages
    → Processes them in one turn (fan-in)
```

**Key Behavior**: Messages accumulate while locked, all delivered in next fetch after lock release.

---

## Transaction Isolation

### SQLite Isolation Level

**Default**: SERIALIZABLE (strongest isolation)
- Read locks acquired on first SELECT
- Write locks acquired on first INSERT/UPDATE/DELETE
- All transactions see consistent snapshot
- No dirty reads, no non-repeatable reads, no phantom reads

### Our Transaction Pattern

**Short-lived transactions**:
- Lock → Read → Commit (fetch)
- Validate → Write → Release → Commit (ack)
- Typical duration: <10ms
- No long-held locks

**Why this works**:
- Minimizes contention window
- Write locks acquired immediately on UPDATE/INSERT
- Commits happen quickly
- Lock timeouts handle crashes/hangs

---

## Concurrency Model

### Multi-Worker Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Runtime                              │
│                                                         │
│  Worker 1    Worker 2    Worker 3                      │
│     │            │            │                         │
│     ↓            ↓            ↓                         │
│  fetch()      fetch()      fetch()                     │
│     │            │            │                         │
│     └────────────┴────────────┘                         │
│                  │                                       │
│                  ↓                                       │
│        ┌─────────────────────┐                         │
│        │  instance_locks     │                         │
│        │  +-----------+---+  │                         │
│        │  | inst-A    | A |  │  ← Only Worker 1 gets A│
│        │  | inst-B    | B |  │  ← Only Worker 2 gets B│
│        │  | inst-C    | C |  │  ← Only Worker 3 gets C│
│        │  +-----------+---+  │                         │
│        └─────────────────────┘                         │
└─────────────────────────────────────────────────────────┘
```

**Key**: Each instance can only be processed by one worker at a time. Workers process different instances in parallel.

---

## Failure Modes

### Lock Leak Prevention

**Scenario**: Worker crashes after fetch, before ack/abandon

**Protection**: Lock timeout
- Lock has `locked_until` timestamp
- After timeout, lock is considered expired
- Next fetch can acquire the instance (ON CONFLICT WHERE locked_until <= now)
- Crashed worker's messages are reprocessed

**Detection**: Periodic cleanup (optional)
```sql
DELETE FROM instance_locks WHERE locked_until <= current_timestamp_ms()
```

### Duplicate Event Prevention

**Scenario**: Two workers try to append same event_id

**Protection**: UNIQUE constraint
```sql
PRIMARY KEY (instance_id, execution_id, event_id)
```

**Handling**: Instance lock should prevent this, but if it happens:
- Second INSERT fails with UNIQUE constraint error
- Transaction rolls back
- Worker retries (lock expired scenario)

### Deadlock Prevention

**SQLite Locking Order**:
1. Acquire instance lock (instance_locks table)
2. Read/write to other tables
3. Release instance lock

**No deadlocks because**:
- Instance lock is acquired FIRST
- No circular dependencies
- Short-lived transactions
- Single database (no distributed locks)

---

## Performance Characteristics

### Lock Contention

**Low contention** (instances >> workers):
- Each worker gets unique instance
- No lock acquisition failures
- Maximum parallelism

**High contention** (workers >> instances):
- Multiple workers compete for same instances
- Lock acquisition returns 0 rows_affected
- Workers retry fetch (get None temporarily)
- Still correct, but some wasted cycles

### Query Performance

**fetch_orchestration_item**:
- LEFT JOIN on `instance_locks` (small table)
- Index on `orchestrator_queue(visible_at, lock_token)`
- Index on `orchestrator_queue(instance_id)`
- Typical: <5ms

**ack_orchestration_item**:
- Multiple INSERTs (history events, work items)
- Single UPDATE (execution status)
- DELETE (queue messages, instance lock)
- Typical: <10ms for normal turn

### Scalability

**Horizontal scaling**: Not supported (single SQLite file)
**Vertical scaling**: Limited by SQLite write concurrency
**Sweet spot**: 3-5 orchestration workers
**Beyond that**: Consider Postgres/Redis provider with `SELECT FOR UPDATE SKIP LOCKED`

---

## Implementation File

**Source**: `src/providers/sqlite.rs`
**Schema**: `migrations/20240101000000_initial_schema.sql`
**Tests**: `tests/provider_atomic_tests.rs`, `tests/sqlite_tests.rs`

