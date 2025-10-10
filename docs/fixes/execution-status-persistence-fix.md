# Fix: Execution Status and Tracking Persistence

**Date:** 2025-10-10  
**Issues Fixed:**
1. `executions.status` column was never updated to reflect terminal orchestration states
2. `instances.current_execution_id` was never updated when ContinueAsNew created new executions

**Status:** ✅ Fixed and Tested

## Problems Found

### Issue 1: `executions.status` Never Updated

The SQLite provider's `executions` table has a `status` column that tracks orchestration execution state:
- ✅ **Written correctly**: Set to `'Running'` when executions are created
- ❌ **Never updated**: Did not change to `'Completed'`, `'Failed'`, or `'ContinuedAsNew'` when orchestrations finished
- ❌ **Never read**: No code queried this column (status derived from history events instead)

### Issue 2: `instances.current_execution_id` Never Updated

The `instances` table has a `current_execution_id` column to track the active execution:
- ✅ **Initialized correctly**: Set to `1` when instances are created
- ❌ **Never updated**: Did not increment when ContinueAsNew created execution 2, 3, etc.
- ✅ **Read by fetch**: Used by `fetch_orchestration_item()` to determine which execution to load

**Impact**: After ContinueAsNew, `fetch_orchestration_item()` would load the wrong (old) execution instead of the latest one.

Both were **data consistency issues**. Issue #1 was benign, but Issue #2 could cause subtle bugs in multi-execution scenarios.

## Root Cause

In `src/providers/sqlite.rs`, the `ack_orchestration_item()` method:
1. ✅ Correctly appended history events (including terminal events like `OrchestrationCompleted`)
2. ✅ Created executions with `status='Running'`
3. ❌ **Did not update** `executions.status` when terminal events were appended
4. ❌ **Did not update** `instances.current_execution_id` when ContinueAsNew created new executions

## Solutions

### Fix 1: Update `executions.status` on Terminal Events

Added logic after history appending (lines 734-802) to check for terminal events and update the `executions.status` column accordingly:

```rust:src/providers/sqlite.rs
// Update execution status based on terminal events
for event in &history_delta {
    match event {
        Event::OrchestrationCompleted { .. } => {
            sqlx::query(
                r#"
                UPDATE executions 
                SET status = 'Completed', completed_at = CURRENT_TIMESTAMP 
                WHERE instance_id = ? AND execution_id = ?
                "#
            )
            .bind(&instance_id)
            .bind(execution_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            break;
        }
        Event::OrchestrationFailed { .. } => {
            // Update to 'Failed'
            break;
        }
        Event::OrchestrationContinuedAsNew { .. } => {
            // Update to 'ContinuedAsNew'
            break;
        }
        _ => {}
    }
}
```

### Fix 2: Update `instances.current_execution_id` on ContinueAsNew

Added logic when creating new executions (lines 834-846) to update the current execution pointer:

```rust:src/providers/sqlite.rs
// Update instances.current_execution_id to point to the new execution
sqlx::query(
    r#"
    UPDATE instances 
    SET current_execution_id = ?
    WHERE instance_id = ?
    "#
)
.bind(next_exec_id)
.bind(&instance_id)
.execute(&mut *tx)
.await
.map_err(|e| e.to_string())?;
```

This ensures `fetch_orchestration_item()` loads the correct (latest) execution.

## Testing

### Tests Created

**File:** `tests/sqlite_provider_test.rs` (4 provider-level tests)

These tests use the **Provider API directly** to test SQLite-specific implementation details:

1. ✅ `test_execution_status_completed`
   - Uses `fetch_orchestration_item()` and `ack_orchestration_item()` directly
   - Verifies `executions.status = 'Completed'` when `OrchestrationCompleted` event is appended
   - Confirms `completed_at` timestamp is set
   - Queries database using `get_pool()` to verify state

2. ✅ `test_execution_status_failed`
   - Uses Provider API directly
   - Verifies `executions.status = 'Failed'` when `OrchestrationFailed` event is appended
   - Confirms `completed_at` timestamp is set

3. ✅ `test_execution_status_running`
   - Verifies status remains `'Running'` when only scheduling events are appended
   - Confirms `completed_at` remains NULL for running executions

4. ✅ `test_execution_status_continued_as_new`
   - **Most complex test**: Verifies multi-execution state tracking
   - Execution 1: `status = 'ContinuedAsNew'`, `completed_at` set
   - Execution 2: created with `status = 'Running'`
   - **Critical**: Verifies `instances.current_execution_id` is updated from 1 to 2
   - Completes execution 2 and verifies final state
   - Tests both fixes working together

### Test Results

**Provider-level tests (using Provider API directly):**
```
running 4 tests
test test_execution_status_completed ... ok
test test_execution_status_failed ... ok
test test_execution_status_running ... ok
test test_execution_status_continued_as_new ... ok

test result: ok. 4 passed; 0 failed
```

**High-level OrchestrationStatus tests:**
```
running 8 tests
test test_status_not_found ... ok
test test_status_running ... ok
test test_status_completed ... ok
test test_status_failed ... ok
test test_status_cancelled ... ok
test test_status_after_continue_as_new ... ok
test test_status_lifecycle_transitions ... ok
test test_status_independence ... ok

test result: ok. 8 passed; 0 failed
```

### Regression Testing

All existing tests pass:
- ✅ `sqlite_provider_test` (4 new + 3 existing = 7 tests)
- ✅ `orchestration_status_tests` (8 tests)
- ✅ `continue_as_new_tests` (6 tests) - **Critical validation**
- ✅ `versioning_tests` (17 tests)
- ✅ All other test suites (100% pass rate)

## Benefits

### From Fix 1 (`executions.status` update):
1. **Database consistency**: `executions` table accurately reflects orchestration state
2. **Debugging**: Direct SQL queries show correct execution status
3. **Future features**: Can query/filter by execution status efficiently
4. **Monitoring**: Can build dashboards directly from database state

### From Fix 2 (`instances.current_execution_id` update):
1. **Correctness**: `fetch_orchestration_item()` loads the correct execution after ContinueAsNew
2. **Multi-execution support**: Proper tracking of current execution across restarts
3. **Bug prevention**: Prevents loading stale execution data
4. **Critical for ContinueAsNew**: Ensures workflow can properly resume after process restarts

## Migration Notes

**No migration required** - existing rows with `status='Running'` will be corrected on next orchestration turn that produces a terminal event.

For historical accuracy, optionally run:
```sql
-- Fix completed orchestrations
UPDATE executions 
SET status = 'Completed' 
WHERE instance_id IN (
    SELECT DISTINCT instance_id 
    FROM history 
    WHERE event_type = 'OrchestrationCompleted'
);

-- Fix failed orchestrations  
UPDATE executions 
SET status = 'Failed' 
WHERE instance_id IN (
    SELECT DISTINCT instance_id 
    FROM history 
    WHERE event_type = 'OrchestrationFailed'
);

-- Fix continued orchestrations
UPDATE executions 
SET status = 'ContinuedAsNew' 
WHERE instance_id IN (
    SELECT DISTINCT instance_id 
    FROM history 
    WHERE event_type = 'OrchestrationContinuedAsNew'
);
```

## Impact

### Fix 1: `executions.status` update
- **Priority**: Low (data consistency improvement)
- **Breaking Changes**: None
- **Performance Impact**: Minimal (1 UPDATE query per terminal event)
- **Backward Compatibility**: 100% maintained

### Fix 2: `instances.current_execution_id` update
- **Priority**: Medium-High (correctness fix for ContinueAsNew)
- **Breaking Changes**: None
- **Performance Impact**: Minimal (1 UPDATE query per ContinueAsNew)
- **Backward Compatibility**: 100% maintained
- **Bug Severity**: Could cause incorrect execution loading after process restarts in ContinueAsNew workflows

## Summary

Two related issues in the SQLite provider were identified and fixed:
1. **Metadata consistency**: `executions.status` now properly reflects terminal states
2. **Execution tracking**: `instances.current_execution_id` now properly tracks latest execution

Both fixes are **low-risk, high-value improvements** that enhance database consistency and ensure correct behavior in multi-execution scenarios. All tests pass with 100% success rate.

