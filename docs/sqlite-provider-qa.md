# SQLite Provider - Design Questions & Answers

## Q1: On Fetch, why ON CONFLICT UPDATE and not abort/rollback?

**Current code:**
```sql
INSERT INTO instance_locks (instance_id, lock_token, locked_until, locked_at)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(instance_id) DO UPDATE
SET lock_token = ?2, locked_until = ?3, locked_at = ?4
WHERE locked_until <= ?4  -- Only update if expired
```

### Why ON CONFLICT UPDATE?

**Purpose**: Handle the case where an **expired lock** still exists in the table.

**Scenario without UPDATE**:
```
T1: Worker A acquires lock (locked_until = T1 + 30s)
T2: Worker A crashes (never acks or abandons)
T31: Lock expires (locked_until < now)
T32: Worker B tries to fetch
     INSERT INTO instance_locks (instance=foo, ...)
     → ERROR: UNIQUE constraint (instance_id already exists)
     → Would need explicit DELETE first
```

**With ON CONFLICT UPDATE WHERE**:
```
T32: Worker B tries to fetch
     INSERT ... ON CONFLICT DO UPDATE ... WHERE locked_until <= now
     → If lock expired: UPDATE succeeds, rows_affected = 1 ✅
     → If lock valid: WHERE clause false, rows_affected = 0 ❌
```

### Why not just abort/rollback?

**If we used `ON CONFLICT DO NOTHING` or `ABORT`:**
- Need separate DELETE to clean up expired locks
- Two queries instead of one
- Race condition: another worker could acquire between DELETE and INSERT

**Current approach**: Single atomic operation handles both fresh locks and expired lock takeover.

### Alternative (more explicit):

```sql
-- First try clean insert
INSERT INTO instance_locks VALUES (...)

-- If conflict, check if we can steal expired lock
ON CONFLICT(instance_id) DO UPDATE
SET lock_token = CASE 
    WHEN locked_until <= ?now THEN ?new_token
    ELSE lock_token  -- Don't change (will result in 0 rows_affected)
END
WHERE locked_until <= ?now
```

**Verdict**: Current approach is correct. ON CONFLICT UPDATE with WHERE clause is the right pattern for "acquire lock or steal if expired".

---

## Q2: Lock Token on Queue Messages - Confirmation

**Your understanding is CORRECT** ✅

### How it works:

**On Fetch:**
```sql
-- Mark messages we're fetching
UPDATE orchestrator_queue
SET lock_token = ?our_token
WHERE instance_id = ?instance AND visible_at <= ?now

-- Messages marked:
+-------------+-----------+---------+-------------+
| instance_id | work_item | visible | lock_token  |
+-------------+-----------+---------+-------------+
| foo         | Start     | T0      | token-A     | ← Marked
| foo         | Completed | T1      | token-A     | ← Marked
+-------------+-----------+---------+-------------+
```

**New message arrives AFTER fetch (while processing):**
```sql
INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
VALUES ('foo', 'ActivityCompleted(id=5)', T5)

-- New message NOT stamped:
+-------------+-----------+---------+-------------+
| instance_id | work_item | visible | lock_token  |
+-------------+-----------+---------+-------------+
| foo         | Start     | T0      | token-A     | ← Will be deleted
| foo         | Completed | T1      | token-A     | ← Will be deleted
| foo         | Completed | T5      | NULL        | ← Stays in queue
+-------------+-----------+---------+-------------+
```

**On Ack:**
```sql
DELETE FROM orchestrator_queue WHERE lock_token = ?our_token

-- Result:
+-------------+-----------+---------+-------------+
| instance_id | work_item | visible | lock_token  |
+-------------+-----------+---------+-------------+
| foo         | Completed | T5      | NULL        | ← Preserved for next turn
+-------------+-----------+---------+-------------+
```

**Next Fetch:**
- Instance lock is released
- New fetch sees the remaining message
- Processes it in the next turn

**This is the correct behavior!** Each turn processes only the messages available at fetch time. Late arrivals wait for the next turn.

---

## Q3: Why not update instance from metadata.status?

**Current code in ack:**
```rust
// Update instance metadata if this is OrchestrationStarted
if let Some(crate::Event::OrchestrationStarted { name, version, .. }) = history_delta.first() {
    UPDATE instances 
    SET orchestration_name = ?, orchestration_version = ?
    WHERE instance_id = ?
}
```

**Your question**: Why not use `metadata.status` instead of inspecting events?

### Answer: Separation of Concerns

**Runtime Responsibility**:
- Compute semantic information
- Pass it as `ExecutionMetadata`
- Provider trusts metadata without inspecting events

**Current Issue**: We're violating this by inspecting `OrchestrationStarted` event!

### Better approach:

Add `orchestration_name` and `orchestration_version` to `ExecutionMetadata`:

```rust
pub struct ExecutionMetadata {
    pub status: Option<String>,
    pub output: Option<String>,
    pub orchestration_name: Option<String>,   // NEW
    pub orchestration_version: Option<String>, // NEW
}
```

Then in ack:
```rust
// Update instance from metadata (no event inspection)
if let (Some(name), Some(version)) = (&metadata.orchestration_name, &metadata.orchestration_version) {
    UPDATE instances SET orchestration_name = ?, orchestration_version = ?
}
```

**TODO**: Refactor to use metadata instead of event inspection.

---

## Q4: Instance Creation in Two Places

**You identified the duplication correctly!**

### Location 1: `ack_orchestration_item` (lines ~760-769)
```rust
// Create execution record if it doesn't exist (idempotent)
INSERT OR IGNORE INTO executions (instance_id, execution_id, status)
VALUES (?, ?, 'Running')
```

### Location 2: `enqueue_orchestrator_work` (lines ~876-898)
```rust
// Create instance row for StartOrchestration
if let WorkItem::StartOrchestration { orchestration, version, .. } = &item {
    INSERT OR IGNORE INTO instances (instance_id, orch_name, orch_version)
    VALUES (?, ?, ?)
    
    INSERT OR IGNORE INTO executions (instance_id, execution_id)
    VALUES (?, 1)
}
```

### Why two places?

**Location 1 (ack)**: Handles the NORMAL case
- Runtime processes StartOrchestration work item
- Adds OrchestrationStarted to history_delta
- Ack creates instance + execution rows

**Location 2 (enqueue)**: Handles EARLY ARRIVAL case
- Client calls `start_orchestration()` → enqueues StartOrchestration
- Before any worker fetches it, we need `instances` row to exist
- Why? So `list_instances()` shows it, management APIs work

**Also handles**: Sub-orchestration starts (parent enqueues StartOrchestration for child)

### Can we reconcile?

**Option A**: Remove from enqueue, only create in ack
- ❌ Breaks: `list_instances()` won't show instance until first fetch
- ❌ Breaks: Management APIs can't see pending instances

**Option B**: Keep both (current)
- ✅ Instance visible immediately after start
- ✅ INSERT OR IGNORE makes it idempotent
- ✅ No harm in double-creating (same data)

**Option C**: Only create in enqueue, remove from ack
- ❌ Breaks: ContinueAsNew creates new execution without StartOrchestration work item
- ❌ Breaks: Execution rows wouldn't be created

### Verdict: Keep both, they serve different purposes

**enqueue**: Early instance registration (for visibility)
**ack**: Execution creation (authoritative, handles CAN)

Both use `INSERT OR IGNORE` so they're idempotent and harmless.

---

## Q5: ack_worker - Critical Bug!

**Current code:**
```rust
async fn ack_worker(lock_token: &str) -> Result<()> {
    DELETE FROM worker_queue WHERE lock_token = lock_token
    Ok(())
}
```

**YOU'RE ABSOLUTELY RIGHT** - this is a critical bug!

### What should happen:

Worker dispatcher does:
```rust
match activity_handler.invoke(input).await {
    Ok(result) => {
        // Enqueue completion to orchestrator queue
        store.enqueue_orchestrator_work(
            WorkItem::ActivityCompleted { instance, execution_id, id, result },
            None
        ).await?;
        
        // Then ack worker item
        store.ack_worker(&token).await?;
    }
    Err(error) => {
        // Enqueue failure
        store.enqueue_orchestrator_work(
            WorkItem::ActivityFailed { instance, execution_id, id, error },
            None
        ).await?;
        
        store.ack_worker(&token).await?;
    }
}
```

**Problem**: Enqueue and ack are SEPARATE calls!
- If enqueue succeeds but ack fails → completion delivered, worker item redelivered → DUPLICATE
- If ack succeeds but enqueue fails → worker item deleted, completion lost → HANG

### Solution: Atomic combined ack

**New method**:
```rust
async fn ack_worker_with_completion(
    &self,
    worker_token: &str,
    completion: WorkItem,  // ActivityCompleted or ActivityFailed
) -> Result<(), String> {
    BEGIN TRANSACTION
        // Delete worker item
        DELETE FROM worker_queue WHERE lock_token = worker_token
        
        // Enqueue completion to orchestrator queue
        INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
        VALUES (extract_instance(completion), serialize(completion), now)
    COMMIT
    
    Ok(())
}
```

**TODO**: Implement this fix!

---

**Note:** Timer functionality is now handled directly via the orchestrator queue with delayed visibility (`TimerFired` work items). The separate timer queue has been removed.

---

## Q7: append() - Where is it used? Can we remove it?

**Checked codebase**: `Provider::append()` method does NOT exist!

**What exists**:
- `append_with_execution(instance, execution_id, events)` - defined in Provider trait
- `append_history_in_tx(&mut tx, instance, execution_id, events)` - internal SQLite helper

**Where `append_with_execution` is used**: 
- Possibly legacy/unused
- All history appending happens in `ack_orchestration_item`

**Grep results show**: No calls to `store.append()` or `provider.append()`

**Recommendation**: 
1. Check if `append_with_execution` is part of Provider trait API
2. If unused, mark as deprecated or remove
3. All history should flow through `ack_orchestration_item` for atomicity

---

## Summary of Issues to Fix

### 1. ack_worker atomicity ❌ CRITICAL
**Impact**: Duplicate processing or lost completions
**Fix**: Combine DELETE + INSERT in one transaction
**New method**: `ack_worker_with_completion(worker_token, completion_item)`

### 2. ack_timer atomicity ❌ CRITICAL
**Impact**: Same as ack_worker
**Fix**: Combine DELETE + INSERT
**New method**: `ack_timer_with_completion(timer_token, completion_item)`

### 3. Instance metadata from events instead of metadata ⚠️ MINOR
**Impact**: Provider inspects events (violates separation of concerns)
**Fix**: Add `orchestration_name/version` to ExecutionMetadata
**Benefit**: Cleaner abstraction

### 4. append() methods 🤔 CLEANUP
**Impact**: Possible dead code
**Fix**: Audit usage, remove if unused
**Benefit**: Simpler provider interface

### 5. Double instance creation ✅ OK
**Impact**: None (idempotent)
**Fix**: None needed
**Reason**: Different purposes (early registration vs execution creation)

---

## Verification of Current Behavior

### Lock Token Marking - VERIFIED ✅

I confirmed by code inspection:

**fetch_orchestration_item** (line ~594):
```rust
UPDATE orchestrator_queue
SET lock_token = ?1
WHERE instance_id = ?2 AND visible_at <= ?3
```

**ack_orchestration_item** (line ~750):
```rust
DELETE FROM orchestrator_queue WHERE lock_token = ?
```

✅ **Confirmed**: Only messages fetched in current batch are stamped and deleted. New arrivals preserved.

---

## Recommended Changes Priority

**High Priority (Correctness)**:
1. ✅ Fix ack_worker atomicity
2. ✅ Fix ack_timer atomicity

**Medium Priority (Cleanup)**:
3. ⚠️ Use metadata instead of event inspection
4. 🧹 Remove unused append methods

**Low Priority (Already Correct)**:
5. ✅ Double instance creation (keep as-is)
6. ✅ ON CONFLICT UPDATE (keep as-is)

Would you like me to implement the atomic ack fixes now?

