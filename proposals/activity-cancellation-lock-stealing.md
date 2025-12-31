# Proposal: Activity Cancellation via Lock Stealing (Row Deletion)

## Status

Draft.

## Motivation

The current activity cancellation design infers cancellation from the parent orchestration's terminal status (`ExecutionState::{Terminal,Missing}` returned by `fetch_work_item` / `renew_work_item_lock`). This has shortcomings:

- **Cancellation scope is too narrow**: Activities only cancel when the orchestration becomes terminal/missing, not when cancellation should occur while the orchestration continues (e.g., `ctx.select()` timeouts, select losers).
- **Provider coupling**: Providers must look up orchestration execution status during activity fetch/renew.

This proposal replaces inferred cancellation with **lock stealing via row deletion**—the simplest possible approach.

## Goals

- **Remove `ExecutionState` completely** from the provider API.
- Make cancellation **explicit** by deleting the worker queue row for activities that should be cancelled.
- Ensure cancellation is **transactional** with orchestration turns (deletions happen inside `ack_orchestration_item`).
- Support the following cancellation triggers:
  - orchestration cancellation (`CancelInstance`)
  - orchestration failure
  - continue-as-new (old execution should cancel its outstanding activities)
  - activity loses a `ctx.select()` / `select2()` race (e.g., timeout wins)
- **Simplest possible implementation** - no new columns, no cancel flags, just delete the row.

## Non-goals

- Hard-kill of user code (we remain cooperative + best-effort abort after grace).
- Exactly-once cancellation delivery semantics.

## Breaking Change Notice

**This proposal introduces breaking changes to the Provider trait API.**

- `ExecutionState` enum will be removed entirely
- `fetch_work_item` return type changes from `(WorkItem, String, u32, ExecutionState)` to `(WorkItem, String, u32)`
- `renew_work_item_lock` return type changes from `ExecutionState` to `()`
- `ack_orchestration_item` gains a new `cancelled_activities` parameter

External provider implementations (e.g., `duroxide-pg`) must be updated simultaneously.

---

## High-level Design

### Core Idea: Cancellation = Delete the Worker Queue Row

When the orchestration runtime decides an activity should be cancelled:

1. The runtime includes the activity's identity in `cancelled_activities` passed to `ack_orchestration_item`
2. The provider **deletes** the worker queue row(s) matching those activities in the same transaction
3. The activity's lock is effectively "stolen" because the row no longer exists

### Worker Detects Cancellation via Missing Row

The worker's activity manager already renews locks periodically. When renewal fails:

- **Current behavior**: Returns `ProviderError::permanent` if row is missing/expired
- **New behavior**: Same - renewal fails, worker treats this as cancellation signal

The worker already handles renewal failure gracefully—it logs and breaks out of the renewal loop. We extend this to also signal the cancellation token.

### Completion Races with Deletion

**Critical invariant**: `ack_work_item` must atomically:
1. Delete the worker queue row (by lock_token)
2. **Fail if the row didn't exist** (was already deleted/cancelled)
3. Enqueue the completion to orchestrator queue (only if row existed)

If the row was already deleted (cancelled), the operation must fail atomically. The worker sees `ack_work_item` fail and logs a warning.

**⚠️ Current SQLite implementation does NOT do this correctly:**
```sql
BEGIN TRANSACTION
  DELETE FROM worker_queue WHERE lock_token = ?  -- Succeeds even if 0 rows affected!
  INSERT INTO orchestrator_queue ...              -- Still executes
COMMIT
```

A SQL `DELETE` that affects 0 rows is **not** an error—it silently succeeds. We must **check `rows_affected()`** and return an error if it's 0:

```rust
let result = sqlx::query("DELETE FROM worker_queue WHERE lock_token = ?")
    .bind(token)
    .execute(&mut *tx)
    .await?;

if result.rows_affected() == 0 {
    tx.rollback().await.ok();
    return Err(ProviderError::permanent(
        "ack_work_item",
        "Worker queue row not found - activity was cancelled or lock was stolen"
    ));
}
// Only now enqueue completion...
```

---

## API Changes

### Remove `ExecutionState`

```rust
// REMOVE this enum entirely
pub enum ExecutionState {
    Running,
    Terminal { status: String },
    Missing,
}
```

### Update `fetch_work_item` Return Type

```rust
// Before:
async fn fetch_work_item(...) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError>;

// After:
async fn fetch_work_item(...) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

The `ExecutionState` check on fetch is no longer needed—if the row exists, the activity should run.

### Update `renew_work_item_lock` Return Type

```rust
// Before:
async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) -> Result<ExecutionState, ProviderError>;

// After:
async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) -> Result<(), ProviderError>;
```

Returns `Ok(())` if lock was renewed. Returns `Err(ProviderError::permanent)` if:
- Row doesn't exist (cancelled/stolen)
- Lock token doesn't match (stolen by timeout)
- Lock already expired

The worker treats any error as "activity should be cancelled."

### Add Cancellation to `ack_orchestration_item`

```rust
/// Identity of an activity to cancel
#[derive(Debug, Clone)]
pub struct ActivityIdentity {
    pub instance: String,
    pub execution_id: u64,
    pub activity_id: u64,  // The scheduling event id
}

async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
    cancelled_activities: Vec<ActivityIdentity>,  // NEW
) -> Result<(), ProviderError>;
```

### Update `ack_work_item` Error Handling

`ack_work_item` must return a **non-retryable error** when the row doesn't exist (was cancelled/deleted or lock was stolen). See Provider Implementation section for details.

---

## Provider Implementation

### Schema Changes

**Required for efficiency.** We add denormalized columns to `worker_queue` to support efficient cancellation lookups without parsing JSON.

```sql
-- SQLite Migration
ALTER TABLE worker_queue ADD COLUMN instance_id TEXT;
ALTER TABLE worker_queue ADD COLUMN execution_id INTEGER;
ALTER TABLE worker_queue ADD COLUMN activity_id INTEGER;
CREATE INDEX idx_worker_queue_activity ON worker_queue(instance_id, execution_id, activity_id);
```

**📝 Implementation task:** Update `docs/provider-implementation-guide.md` to recommend this schema pattern for all providers.

### `ack_orchestration_item` Changes

Add deletion of cancelled activities within the transaction.

```sql
-- Inside ack_orchestration_item transaction, after other operations:

-- Delete cancelled activities from worker queue
-- Uses set-based deletion (or OR-chaining for SQLite) on indexed columns
DELETE FROM worker_queue 
WHERE (instance_id = ?1 AND execution_id = ?2 AND activity_id = ?3)
   OR (instance_id = ?4 AND execution_id = ?5 AND activity_id = ?6)
   ...
```

### `ack_work_item` Changes

Verify row was actually deleted:

```rust
async fn ack_work_item(&self, token: &str, completion: Option<WorkItem>) -> Result<(), ProviderError> {
    let mut tx = self.pool.begin().await?;

    // Delete worker item - MUST succeed if row exists
    let result = sqlx::query("DELETE FROM worker_queue WHERE lock_token = ?")
        .bind(token)
        .execute(&mut *tx)
        .await?;

    // NEW: Check if row existed
    if result.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ProviderError::permanent(
            "ack_work_item",
            "Worker queue row not found - activity was cancelled or lock was stolen"
        ));
    }

    // Rest of completion handling...
    if let Some(completion) = completion {
        // Enqueue to orchestrator queue
        ...
    }

    tx.commit().await?;
    Ok(())
}
```

### `renew_work_item_lock` Changes

Remove execution state lookup, just renew the lock:

```rust
async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) -> Result<(), ProviderError> {
    let now_ms = Self::now_millis();
    let locked_until = Self::timestamp_after(extend_for);

    let result = sqlx::query(
        r#"
        UPDATE worker_queue
        SET locked_until = ?1
        WHERE lock_token = ?2
          AND locked_until > ?3
        "#,
    )
    .bind(locked_until)
    .bind(token)
    .bind(now_ms)
    .execute(&self.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ProviderError::permanent(
            "renew_work_item_lock",
            "Lock token invalid, expired, row deleted, or already acked"
        ));
    }

    Ok(())
}
```

---

## Runtime Changes

### Worker Dispatcher: Activity Manager

Update to signal cancellation on ANY renewal failure:

```rust
loop {
    interval.tick().await;

    if shutdown.load(Ordering::Relaxed) {
        break;
    }

    match store.renew_work_item_lock(&token, lock_timeout).await {
        Ok(()) => {
            // Lock renewed successfully
            tracing::trace!(lock_token = %token, "Work item lock renewed");
        }
        Err(e) => {
            // Row missing, lock expired, or cancelled - signal cancellation
            tracing::info!(
                lock_token = %token,
                error = %e,
                "Lock renewal failed - signaling activity cancellation"
            );
            cancellation_token.cancel();
            break;
        }
    }
}
```

### Worker Dispatcher: Completion Handling

Update to handle ack failure gracefully:

```rust
match provider.ack_work_item(&token, Some(completion)).await {
    Ok(()) => {
        debug!("Activity completed and acknowledged");
    }
    Err(e) if !e.is_retryable() => {
        // Two possible causes:
        // 1. Row was deleted (cancelled) - orchestration decided result won't be observed
        // 2. Lock was stolen - another worker took over and may complete successfully
        // Either way, this worker should not retry - just move on
        warn!(
            lock_token = %token,
            error = %e,
            "Activity completion failed - row missing (cancelled) or lock stolen. Dropping completion."
        );
    }
    Err(e) => {
        // Retryable error - let normal retry logic handle it
        return Err(e);
    }
}
```

### Orchestration Dispatcher: Computing Cancelled Activities

The runtime computes which activities to cancel based on the turn outcome:

```rust
struct CancellationTriggers {
    instance: String,
    execution_id: u64,
    /// True when orchestration is winding down (cancelled, failed, continue-as-new)
    /// Causes ALL outstanding activities to be cancelled
    orchestration_terminal: bool,
    /// Activity IDs that lost a select/select2 race
    /// Only these specific activities are cancelled (orchestration continues)
    select_losers: Vec<u64>,
}

fn compute_cancelled_activities(
    history: &[Event],
    history_delta: &[Event],
    triggers: &CancellationTriggers,
) -> Vec<ActivityIdentity> {
    let mut cancelled = Vec::new();

    // Find all scheduled activities
    let scheduled: HashSet<u64> = history.iter()
        .chain(history_delta.iter())
        .filter_map(|e| match e.kind() {
            EventKind::ActivityScheduled { .. } => Some(e.event_id()),
            _ => None,
        })
        .collect();

    // Find all completed activities
    let completed: HashSet<u64> = history.iter()
        .chain(history_delta.iter())
        .filter_map(|e| match e.kind() {
            EventKind::ActivityCompleted { source_event_id, .. } |
            EventKind::ActivityFailed { source_event_id, .. } => Some(*source_event_id),
            _ => None,
        })
        .collect();

    // Outstanding = scheduled - completed
    let outstanding: HashSet<u64> = scheduled.difference(&completed).copied().collect();

    if triggers.orchestration_terminal {
        // Orchestration winding down: cancel ALL outstanding activities
        for activity_id in &outstanding {
            cancelled.push(ActivityIdentity {
                instance: triggers.instance.clone(),
                execution_id: triggers.execution_id,
                activity_id: *activity_id,
            });
        }
    } else {
        // Normal turn: only cancel select losers
        for loser_id in &triggers.select_losers {
            if outstanding.contains(loser_id) {
                cancelled.push(ActivityIdentity {
                    instance: triggers.instance.clone(),
                    execution_id: triggers.execution_id,
                    activity_id: *loser_id,
                });
            }
        }
    }

    cancelled
}
```

### When to Request Cancellation

The runtime requests activity cancellation (by deleting rows via `ack_orchestration_item`) in two distinct scenarios:

**A. Orchestration winding down (cancel ALL outstanding activities):**

These triggers cause the orchestration to stop processing, so all outstanding activities should be cancelled:

1. **Orchestration cancellation requested** (`WorkItem::CancelInstance` → `OrchestrationCancelRequested`)
2. **Orchestration failure** (turn results in `OrchestrationFailed`)
3. **ContinueAsNew** (current execution ends, new execution starts fresh)

**B. Select race losers (cancel SPECIFIC activities):**

During normal orchestration turns, only the losing futures from `ctx.select()` / `ctx.select2()` are cancelled:

4. **`ctx.select()` / `select2()` loser**:
   - If an activity future loses a select race (e.g., timer wins), cancel that specific activity
   - The orchestration continues running with the winner's result
   - This covers `schedule_activity_with_retry` per-attempt timeout (implemented via `select2(activity, timer)`)

---

## Edge Cases and Semantics

### If cancellation happens before activity starts (still in queue, unlocked)

- Row is deleted by `ack_orchestration_item`
- Worker never sees the activity
- Clean cancellation, no work done

### If cancellation happens while activity is running (locked)

- Row is deleted by `ack_orchestration_item`
- Worker's next renewal attempt fails (row gone)
- Activity manager signals cancellation token
- Worker waits grace period, aborts if needed
- Worker tries to ack → fails (row gone) → logs warning, continues

### If activity completes between cancellation decision and row deletion

Race condition timeline:
1. Orchestration turn decides to cancel activity (e.g., timer won select2)
2. Activity finishes executing
3. Worker calls `ack_work_item(completion)`
4. `ack_orchestration_item` runs with cancellation
5. Both try to delete the same row

**Resolution**: Whoever commits first wins.

- If worker acks first: Row deleted, completion enqueued. Orchestration's delete affects 0 rows (idempotent). Orchestration will receive completion and handle it normally (the turn may have already committed a timer-won result, so the completion will be filtered by `is_completion_already_in_history` or similar).

- If orchestration deletes first: Row deleted. Worker's ack fails (row gone). Worker logs warning. Completion is lost, but that's correct—the orchestration already decided not to observe it.

### Orphan activities (instance deleted via management API)

If an instance is deleted without processing `CancelInstance`:
- Worker fetches activity, executes it
- Worker completes, enqueues completion
- Completion sits in orchestrator queue (no instance to process it)

This is acceptable—management deletion is an admin operation. Best practice: enqueue `CancelInstance` before deletion.

### Multiple workers racing for the same activity

If lock expires and another worker fetches:
- Original worker's renewal fails → cancels activity
- New worker executes activity
- Only one completion reaches orchestrator queue
- Replay engine handles duplicates

---

## Migration Plan

### SQLite Provider

1. **Schema Migration**:
   - Add `instance_id`, `execution_id`, `activity_id` columns to `worker_queue`
   - Add index `idx_worker_queue_activity`

2. Update `enqueue_for_worker` / activity enqueue in `ack_orchestration_item`:
   - Populate `instance_id`, `execution_id`, `activity_id` from `WorkItem::ActivityExecute`

3. Update `ack_orchestration_item`:
   - Accept `cancelled_activities: Vec<ActivityIdentity>`
   - Delete matching rows using the new indexed columns

4. Update `renew_work_item_lock`:
   - Remove `ExecutionState` lookup and return
   - Return `Ok(())` on success, `Err` on failure

5. Update `fetch_work_item`:
   - Remove `ExecutionState` from return value
   - Remove execution state lookup logic

6. Update `ack_work_item`:
   - Check `rows_affected()` and return error if row doesn't exist

### Postgres Provider (`duroxide-pg`)

Similar changes to stored procedures. Ensure denormalized activity identity columns are added for efficient mass cancellation.

---

## Testing

### Provider Validation Tests

**Tests to REMOVE** (these validate `ExecutionState` which is being removed):

From `src/provider_validation/cancellation.rs`:
- `test_fetch_returns_running_state_for_active_orchestration`
- `test_fetch_returns_terminal_state_when_orchestration_completed`
- `test_fetch_returns_terminal_state_when_orchestration_failed`
- `test_fetch_returns_terminal_state_when_orchestration_continued_as_new`
- `test_fetch_returns_missing_state_when_instance_deleted`
- `test_renew_returns_running_when_orchestration_active`
- `test_renew_returns_terminal_when_orchestration_completed`
- `test_renew_returns_missing_when_instance_deleted`

Keep:
- `test_ack_work_item_none_deletes_without_enqueue` (still valid behavior)

**New tests to ADD:**

1. **`test_ack_orchestration_item_deletes_cancelled_activities`**
   - Enqueue multiple activities via `ack_orchestration_item`
   - Call `ack_orchestration_item` again with `cancelled_activities` containing some of them
   - Verify cancelled activities are deleted from worker queue
   - Verify non-cancelled activities still exist

2. **`test_cancellation_of_locked_activity_causes_renewal_failure`**
   - Enqueue activity, fetch and lock it
   - Call `ack_orchestration_item` with that activity in `cancelled_activities`
   - Verify `renew_work_item_lock` returns error (row gone)

3. **`test_cancellation_of_unlocked_activity_removes_from_queue`**
   - Enqueue activity (don't fetch)
   - Call `ack_orchestration_item` with cancellation
   - Verify `fetch_work_item` doesn't return that activity

4. **`test_ack_work_item_fails_on_cancelled_row`**
   - Enqueue activity, fetch and lock it
   - Cancel the activity via `ack_orchestration_item`
   - Call `ack_work_item` with completion
   - Verify returns non-retryable error

5. **`test_ack_work_item_succeeds_if_row_exists`**
   - Normal flow: enqueue, fetch, complete, ack
   - Verify success (regression test)

6. **`test_mass_cancellation_deletes_all_activities`**
   - Enqueue 50 activities for the same instance/execution
   - Call `ack_orchestration_item` with all 50 in `cancelled_activities`
   - Verify all 50 are deleted
   - Verify `fetch_work_item` returns `None` (queue empty)

### Runtime Integration Tests

1. **Select timeout cancels losing activity**
   - `select2(activity, timer)`, timer wins
   - Verify activity is cancelled (handler sees cancellation token)
   - Verify orchestration completes with timer result (not blocked waiting for activity)

2. **`schedule_activity_with_retry` timeout cancels timed-out attempt**
   - Call `schedule_activity_with_retry` with short per-attempt timeout
   - Activity handler blocks longer than timeout
   - Verify timeout fires and activity handler sees cancellation token
   - Verify orchestration receives timeout error (or retries, depending on policy)

3. **CancelInstance cancels outstanding activities**
   - Schedule long activity, cancel instance
   - Verify activity cancelled

4. **Orchestration failure cancels activities**
   - Schedule activity, fail orchestration
   - Verify activity cancelled

5. **ContinueAsNew cancels previous execution activities**
   - Schedule activity, continue-as-new
   - Verify activity cancelled

6. **Mass cancellation on orchestration cancel**
   - Schedule 50+ activities (fan-out pattern)
   - Cancel instance before activities complete
   - Verify ALL activities are cancelled in a single `ack_orchestration_item` call
   - Verify workers detect cancellation via renewal failure
   - Verify no completions are enqueued to orchestrator queue after cancellation

## Documentation Updates

**📝 Implementation task:** Update the following guides to reflect these changes:

1. **`docs/provider-implementation-guide.md`**:
   - Update `fetch_work_item` signature (remove `ExecutionState`).
   - Update `renew_work_item_lock` signature (remove `ExecutionState`).
   - Update `ack_orchestration_item` signature (add `cancelled_activities`).
   - Add recommendation for denormalized columns on `worker_queue`.
   - Explain the "lock stealing" cancellation mechanism.

2. **`docs/provider-testing-guide.md`**:
   - Remove references to `ExecutionState` tests.
   - Add the new cancellation test cases (mass cancellation, renewal failure, etc.).

---

## Comparison with Cancel Flag Approach

| Aspect | Cancel Flag | Lock Stealing (Row Deletion) |
|--------|-------------|------------------------------|
| New columns | `cancel_requested`, `cancel_reason` | `instance_id`, `execution_id`, `activity_id` (denormalized) |
| Detection mechanism | Worker polls flag via renew | Renew fails (row gone) |
| Unfetched activities | Set flag, worker skips on fetch | Delete row, worker never sees it |
| Complexity | Moderate (new state machine) | Simple (just delete) |
| Audit trail | Cancel reason preserved in queue | Inferred from history (ActivityScheduled with no completion) |
| Implementation effort | More code paths | Fewer code paths |

**Lock stealing is simpler** because:
- No new "cancelled" state to manage
- No flag polling logic
- Worker already handles renewal failure
- Leverages existing transactional delete semantics

---

## Summary

This proposal simplifies activity cancellation by using **row deletion as the cancellation signal**:

1. Runtime decides to cancel → includes activity in `cancelled_activities`
2. Provider deletes row in `ack_orchestration_item` transaction
3. Worker's renewal fails → triggers cancellation token
4. Worker's ack fails → logs warning, moves on

Cancellation happens in two scenarios:
- **Orchestration winding down** (cancelled/failed/continue-as-new): cancel ALL outstanding activities
- **Select race**: cancel only the losing activity (orchestration continues)

No new columns required for SQLite (uses JSON extraction), no new state machine, minimal code changes.
