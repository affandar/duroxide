# Management API: Deletion and Pruning

> **Status**: Proposed  
> **Related**: `docs/management-api-improvements-proposal.md`

## 1. Summary

This proposal adds explicit deletion and retention capabilities to duroxide:

1. **Instance Deletion**: Remove terminated orchestrations and all associated data
2. **Bulk Instance Deletion**: Delete multiple instances by ID and/or time-based filter
3. **Execution Pruning**: Remove old executions from long-running `ContinueAsNew` chains
4. **Bulk Execution Pruning**: Prune executions across multiple instances matching a filter

## 2. Motivation

Currently, duroxide retains all history and execution data indefinitely. For long-running applications or high-volume systems, this leads to:

- **Unbounded Storage Growth**: "Eternal" orchestrations using `ContinueAsNew` accumulate history for every past execution.
- **Operational Clutter**: Terminated or failed test instances remain in the database.
- **Compliance/Privacy**: Inability to delete user-specific data (GDPR/CCPA "Right to be Forgotten").
- **No Retention Policy**: Operators cannot say "delete all completed workflows older than 30 days".

## 3. API Naming Philosophy

We use consistent terminology across the API:

| Term | Meaning | Scope |
|------|---------|-------|
| **`delete`** | Permanently remove an orchestration instance and ALL its data | Single instance |
| **`purge`** | Bulk delete multiple instances matching criteria | Multi-instance |
| **`prune`** | Remove old executions while keeping the instance alive | Execution-level |

**Rationale:**
- `delete` is the standard term for removing a single identified resource
- `purge` implies bulk/batch cleanup with filtering—common in retention systems
- `prune` implies trimming while preserving the living entity (the instance continues to exist)

---

## 4. Common Types

### 4.1 InstanceFilter

A reusable filter for selecting instances across management APIs.

```rust
/// Filter criteria for selecting orchestration instances.
/// 
/// Used by purge, bulk prune, and potentially future management APIs.
/// When multiple criteria are provided, they are ANDed together.
#[derive(Debug, Clone, Default)]
pub struct InstanceFilter {
    /// Specific instance IDs to select.
    /// When provided with other filters, acts as an allowlist that is
    /// further filtered by the other criteria.
    pub instance_ids: Option<Vec<String>>,
    
    /// Only select instances whose current execution completed before this time.
    /// Value is milliseconds since Unix epoch.
    pub completed_before: Option<u64>,
    
    /// Maximum number of instances to select.
    /// Use for batching large operations.
    /// Default: 1000
    pub limit: Option<u32>,
}
```

**Filter Semantics (AND):**
- If `instance_ids` AND `completed_before` are both set, only instances that are in the ID list AND completed before the cutoff are selected.
- `limit` is always applied last, after other filters.

**Examples:**
```rust
// Select specific instances
InstanceFilter {
    instance_ids: Some(vec!["order-1".into(), "order-2".into()]),
    ..Default::default()
}

// Select by time (retention policy)
InstanceFilter {
    completed_before: Some(five_days_ago_ms),
    limit: Some(500),
    ..Default::default()
}

// Select specific instances that are also old (intersection)
InstanceFilter {
    instance_ids: Some(vec!["order-1".into(), "order-2".into(), "order-3".into()]),
    completed_before: Some(five_days_ago_ms), // Only delete if also old
    ..Default::default()
}
```

### 4.2 PruneOptions

Options for pruning executions. When multiple criteria are provided, they are ANDed.

```rust
/// Options for pruning old executions.
/// 
/// When multiple criteria are provided, they are ANDed together.
/// The current (active) execution is NEVER pruned regardless of these options.
#[derive(Debug, Clone, Default)]
pub struct PruneOptions {
    /// Keep the last N executions (by execution_id).
    /// Executions outside the top N are eligible for deletion.
    pub keep_last: Option<u32>,
    
    /// Only delete executions completed before this time (milliseconds since epoch).
    pub completed_before: Option<u64>,
}
```

**Filter Semantics (AND):**
- If both `keep_last: Some(5)` and `completed_before: Some(ts)` are set, an execution is deleted only if it's outside the last 5 AND completed before the cutoff.
- The current execution (`current_execution_id`) is **NEVER** deleted regardless of options.
- An execution with `status = 'Running'` is **NEVER** pruned.

### 4.3 Result Types

```rust
/// Result of a single instance deletion.
#[derive(Debug, Clone, Default)]
pub struct DeleteResult {
    pub instance_deleted: bool,
    pub executions_deleted: u64,
    pub events_deleted: u64,
    pub queue_messages_deleted: u64,
}

/// Result of a bulk purge operation.
#[derive(Debug, Clone, Default)]
pub struct PurgeResult {
    pub instances_deleted: u64,
    pub executions_deleted: u64,
    pub events_deleted: u64,
    pub queue_messages_deleted: u64,
}

/// Result of an execution prune operation.
#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    pub instances_processed: u64,  // For bulk prune (1 for single instance prune)
    pub executions_deleted: u64,
    pub events_deleted: u64,
}
```

---

## 5. User-Facing API (Client)

These methods are exposed on the `Client` struct for end-user consumption.

### 5.1 Delete Single Instance

```rust
impl Client {
    /// Delete an orchestration instance and all its data.
    ///
    /// Removes the instance, all executions, all history events, and any
    /// pending queue messages for this instance.
    ///
    /// # Parameters
    /// * `instance_id` - The ID of the instance to delete
    /// * `force` - If true, delete even if instance has status='Running'
    ///
    /// # The `force` Parameter
    /// 
    /// When `force=false` (default):
    /// - Only deletes instances in terminal states (Completed, Failed)
    /// - Returns `Err(ClientError::InstanceStillRunning)` if status is Running
    ///
    /// When `force=true`:
    /// - Deletes the instance regardless of status
    /// - **Only affects database state** — does NOT kill in-flight tokio tasks
    /// - Use for instances that are "waiting" (between turns) but need removal
    /// - If an orchestration turn is actively executing, it will fail when
    ///   trying to persist state (instance gone)
    /// - If activities are running, they will detect deletion via lock renewal
    ///   failure and can terminate gracefully
    ///
    /// **Recommended pattern:** Use `cancel_instance()` first for graceful
    /// shutdown. Only use `force=true` for cleanup of instances stuck in
    /// Running state that won't respond to cancellation.
    ///
    /// # Returns
    /// * `Ok(DeleteResult)` - Details of what was deleted
    /// * `Err(ClientError::InstanceStillRunning)` - Instance is running and force=false
    /// * `Err(ClientError::InstanceNotFound)` - Instance doesn't exist
    ///
    /// # Example
    /// ```ignore
    /// // Delete a completed instance
    /// let result = client.delete_instance("order-123", false).await?;
    /// println!("Deleted {} events", result.events_deleted);
    ///
    /// // Graceful pattern: cancel first, then delete
    /// client.cancel_instance("workflow-456").await?;
    /// // Wait for cancellation to complete...
    /// client.delete_instance("workflow-456", false).await?;
    ///
    /// // Force delete an instance stuck in Running state
    /// // (e.g., waiting on event that will never arrive, cancel didn't help)
    /// client.delete_instance("stuck-workflow", true).await?;
    /// ```
    pub async fn delete_instance(
        &self,
        instance_id: &str,
        force: bool,
    ) -> Result<DeleteResult, ClientError>;
}
```

### 5.2 Purge Instances (Bulk Deletion)

```rust
impl Client {
    /// Purge multiple orchestration instances matching the filter criteria.
    ///
    /// This is the primary API for retention-based cleanup. Only instances
    /// in terminal states (Completed, Failed) are eligible for purging.
    /// Running instances are always skipped (not an error).
    ///
    /// # Filter Behavior
    /// 
    /// All filter criteria are ANDed together:
    /// - `instance_ids` + `completed_before`: Only purge IDs that are also old
    /// - `limit` is applied after other filters
    ///
    /// # Examples
    /// ```ignore
    /// // Purge specific instances
    /// let result = client.purge_instances(InstanceFilter {
    ///     instance_ids: Some(vec!["order-1".into(), "order-2".into()]),
    ///     ..Default::default()
    /// }).await?;
    ///
    /// // Purge by age (retention policy)
    /// let five_days_ago = now_ms - (5 * 24 * 60 * 60 * 1000);
    /// let result = client.purge_instances(InstanceFilter {
    ///     completed_before: Some(five_days_ago),
    ///     limit: Some(500),
    ///     ..Default::default()
    /// }).await?;
    ///
    /// // Purge specific instances only if they're old
    /// let result = client.purge_instances(InstanceFilter {
    ///     instance_ids: Some(vec!["order-1".into(), "order-2".into()]),
    ///     completed_before: Some(five_days_ago),
    ///     ..Default::default()
    /// }).await?;
    /// ```
    ///
    /// # Safety
    /// - Running instances are NEVER deleted (silently skipped)
    /// - Use `limit` to avoid long-running transactions
    pub async fn purge_instances(
        &self,
        filter: InstanceFilter,
    ) -> Result<PurgeResult, ClientError>;
}
```

### 5.3 Prune Executions (Single Instance)

```rust
impl Client {
    /// Prune old executions from a single long-running instance.
    ///
    /// Use this for `ContinueAsNew` workflows that accumulate many executions
    /// over time. The current (active) execution is never pruned.
    ///
    /// # Filter Behavior
    /// 
    /// When both options are set, they are ANDed:
    /// - `keep_last: 5` + `completed_before: ts` = delete executions outside 
    ///   the last 5 that are ALSO older than the cutoff
    ///
    /// # Examples
    /// ```ignore
    /// // Keep only the last 10 executions
    /// let result = client.prune_executions("eternal-workflow", PruneOptions {
    ///     keep_last: Some(10),
    ///     ..Default::default()
    /// }).await?;
    ///
    /// // Delete executions older than 30 days
    /// let thirty_days_ago = now_ms - (30 * 24 * 60 * 60 * 1000);
    /// let result = client.prune_executions("eternal-workflow", PruneOptions {
    ///     completed_before: Some(thirty_days_ago),
    ///     ..Default::default()
    /// }).await?;
    ///
    /// // Combined: delete old executions, but always keep at least 5
    /// let result = client.prune_executions("eternal-workflow", PruneOptions {
    ///     keep_last: Some(5),
    ///     completed_before: Some(thirty_days_ago),
    ///     ..Default::default()
    /// }).await?;
    /// ```
    pub async fn prune_executions(
        &self,
        instance_id: &str,
        options: PruneOptions,
    ) -> Result<PruneResult, ClientError>;
}
```

### 5.4 Bulk Prune Executions

```rust
impl Client {
    /// Prune old executions from multiple instances matching the filter.
    ///
    /// Applies the same prune options to all matching instances. Useful for
    /// system-wide retention policies on long-running workflows.
    ///
    /// # Examples
    /// ```ignore
    /// // Prune all terminal instances: keep last 10 executions each
    /// let result = client.prune_executions_bulk(
    ///     InstanceFilter {
    ///         completed_before: None,  // All terminal instances
    ///         limit: Some(100),
    ///         ..Default::default()
    ///     },
    ///     PruneOptions {
    ///         keep_last: Some(10),
    ///         ..Default::default()
    ///     },
    /// ).await?;
    ///
    /// // Prune specific instances: delete executions older than 30 days
    /// let result = client.prune_executions_bulk(
    ///     InstanceFilter {
    ///         instance_ids: Some(vec!["workflow-a".into(), "workflow-b".into()]),
    ///         ..Default::default()
    ///     },
    ///     PruneOptions {
    ///         completed_before: Some(thirty_days_ago),
    ///         ..Default::default()
    ///     },
    /// ).await?;
    /// println!("Pruned {} executions across {} instances", 
    ///     result.executions_deleted, result.instances_processed);
    /// ```
    ///
    /// # Notes
    /// - Only processes instances in terminal states (Completed, Failed)
    /// - Running instances are skipped
    /// - The current execution of each instance is never pruned
    /// - Active (Running) executions are never pruned
    pub async fn prune_executions_bulk(
        &self,
        filter: InstanceFilter,
        options: PruneOptions,
    ) -> Result<PruneResult, ClientError>;
}
```

---

## 6. Provider API (ProviderAdmin)

The `ProviderAdmin` trait is the internal interface that storage backends implement.
The `Client` methods delegate to these after validation.

### 6.1 Provider Trait Extensions

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    // ===== Deletion Operations =====

    /// Delete a single instance and all associated data.
    ///
    /// # Implementation Requirements
    /// - Delete from: history, executions, instances tables
    /// - Delete from: orchestrator_queue, worker_queue (by instance_id)
    /// - Delete from: instance_locks
    /// - Must be transactional (all-or-nothing)
    /// - Return error if status is 'Running' and force=false
    async fn delete_instance(
        &self,
        instance_id: &str,
        force: bool,
    ) -> Result<DeleteResult, ProviderError>;

    /// Purge multiple instances matching the filter.
    ///
    /// # Implementation Requirements
    /// - AND all filter criteria together
    /// - Skip any instance with status='Running' (don't error)
    /// - Should be efficient (batch SQL operations)
    /// - Apply limit after other filters
    async fn purge_instances(
        &self,
        filter: &InstanceFilter,
    ) -> Result<PurgeResult, ProviderError>;

    /// Prune old executions from a single instance.
    ///
    /// # Implementation Requirements  
    /// - NEVER delete the current_execution_id
    /// - NEVER delete executions with status='Running'
    /// - AND the options together (both must match for deletion)
    /// - When keep_last is set: executions outside top N are eligible
    /// - When completed_before is set: executions older than cutoff are eligible
    async fn prune_executions(
        &self,
        instance_id: &str,
        options: &PruneOptions,
    ) -> Result<PruneResult, ProviderError>;

    /// Prune old executions from multiple instances matching the filter.
    ///
    /// # Implementation Requirements
    /// - Select instances using InstanceFilter (AND semantics)
    /// - Apply PruneOptions to each selected instance (AND semantics)
    /// - Skip running instances
    /// - Never delete current_execution_id of any instance
    /// - Never delete executions with status='Running'
    async fn prune_executions_bulk(
        &self,
        filter: &InstanceFilter,
        options: &PruneOptions,
    ) -> Result<PruneResult, ProviderError>;
}
```

### 6.2 Client → Provider Mapping

| Client Method | Provider Method |
|---------------|-----------------|
| `delete_instance(id, force)` | `delete_instance(id, force)` |
| `purge_instances(filter)` | `purge_instances(&filter)` |
| `prune_executions(id, options)` | `prune_executions(id, &options)` |
| `prune_executions_bulk(filter, options)` | `prune_executions_bulk(&filter, &options)` |

---

## 7. Detailed Design

### 7.1 Schema Requirements

The following columns are required for time-based operations:

| Table | Column | Type | Purpose |
|-------|--------|------|---------|
| `executions` | `completed_at` | INTEGER (epoch ms) | Anchor for retention queries |
| `executions` | `status` | TEXT | Filter terminal vs running |
| `instances` | `current_execution_id` | INTEGER | Identify the active execution |

### 7.2 Timestamp Storage

All timestamps MUST be stored as **INTEGER milliseconds since Unix epoch** (Rust time via `SystemTime::now()`), NOT SQLite's `CURRENT_TIMESTAMP` (which produces TEXT).

**Rationale:**
- Consistent with existing `visible_at`, `locked_until` columns
- Enables simple numeric comparisons: `completed_at < ?`
- Avoids SQLite datetime parsing complexity
- Matches the `u64` epoch milliseconds used in the management API

**Implementation:** Use `Self::now_millis()` helper (already exists in SqliteProvider).

### 7.3 SQL Implementation Sketches

**Delete Single Instance:**
```sql
BEGIN TRANSACTION;

-- Safety check (unless force=true)
SELECT e.status FROM instances i
JOIN executions e ON i.instance_id = e.instance_id 
    AND i.current_execution_id = e.execution_id
WHERE i.instance_id = ?;
-- If status = 'Running' AND NOT force: ROLLBACK and return error

-- Delete all related data
DELETE FROM history WHERE instance_id = ?;
DELETE FROM executions WHERE instance_id = ?;
DELETE FROM orchestrator_queue WHERE instance_id = ?;
DELETE FROM worker_queue WHERE instance_id = ?;
DELETE FROM instance_locks WHERE instance_id = ?;
DELETE FROM instances WHERE instance_id = ?;

COMMIT;
```

**Purge Instances (with ANDed filters):**
```sql
BEGIN TRANSACTION;

-- Build WHERE clause dynamically based on filter
-- Base: only terminal instances
CREATE TEMP TABLE purge_candidates AS
SELECT i.instance_id
FROM instances i
JOIN executions e ON i.instance_id = e.instance_id 
    AND i.current_execution_id = e.execution_id
WHERE e.status IN ('Completed', 'Failed')
  -- AND instance_id IN (...) if instance_ids provided
  -- AND e.completed_at < ? if completed_before provided
LIMIT ?;

-- Delete all related data for candidates
DELETE FROM history WHERE instance_id IN (SELECT instance_id FROM purge_candidates);
DELETE FROM executions WHERE instance_id IN (SELECT instance_id FROM purge_candidates);
DELETE FROM orchestrator_queue WHERE instance_id IN (SELECT instance_id FROM purge_candidates);
DELETE FROM worker_queue WHERE instance_id IN (SELECT instance_id FROM purge_candidates);
DELETE FROM instance_locks WHERE instance_id IN (SELECT instance_id FROM purge_candidates);
DELETE FROM instances WHERE instance_id IN (SELECT instance_id FROM purge_candidates);

DROP TABLE purge_candidates;
COMMIT;
```

**Prune Executions (ANDed options):**
```sql
BEGIN TRANSACTION;

-- Get current execution (never delete)
SELECT current_execution_id FROM instances WHERE instance_id = ?;

-- Find executions to prune
-- Must satisfy ALL provided conditions
-- Never prune Running executions or current_execution_id
CREATE TEMP TABLE prune_candidates AS
SELECT execution_id FROM executions
WHERE instance_id = ?
  AND execution_id != ?  -- current_execution_id
  AND status != 'Running'  -- never prune active executions
  -- AND execution_id NOT IN (top N by execution_id) if keep_last provided
  -- AND completed_at < ? if completed_before provided
;

DELETE FROM history 
WHERE instance_id = ? 
  AND execution_id IN (SELECT execution_id FROM prune_candidates);

DELETE FROM executions 
WHERE instance_id = ? 
  AND execution_id IN (SELECT execution_id FROM prune_candidates);

DROP TABLE prune_candidates;
COMMIT;
```

---

## 8. Edge Cases & Implications

### 8.1 Zombie Messages

When an instance is deleted, queued messages (e.g., `ExternalRaised`) may still exist.

**Problem:** `fetch_orchestration_item` picks up a message for a deleted instance.

**Resolution:** The orchestration dispatcher already handles missing instances gracefully. When loading instance metadata fails with "not found", the message should be acked without processing and a warning logged. This prevents poison message loops.

### 8.2 Force Delete: What It Does (and Doesn't Do)

`force=true` deletes **database state only**. It does NOT:
- Kill in-flight tokio tasks executing orchestration turns
- Abort activity code mid-execution
- Provide any runtime signal to running code

**What happens with `force=true` in various scenarios:**

| Scenario | What Happens | Result |
|----------|--------------|--------|
| Instance waiting (between turns) | DB records deleted, pending queue messages removed | Clean deletion |
| Orchestration turn actively executing | Turn completes, tries to persist → fails (no instance) | Turn result lost, error logged |
| Activity executing (worker has lock) | Worker's next lock renewal fails → detects "cancellation" | Activity can terminate gracefully |
| Activity completes during delete | `ack_work_item` fails (no queue entry) | Activity result lost, error logged |
| Timer pending in queue | Queue message deleted | Timer never fires |
| Waiting on external event | Instance deleted | Future events have no target |

### 8.3 Orphaned Activities

If an instance is deleted while activities are running:

1. Worker's periodic lock renewal fails (queue entry gone)
2. Worker detects this as cancellation signal via `ctx.is_cancelled()`
3. Well-behaved activities check cancellation and terminate gracefully
4. If activity ignores cancellation and completes, `ack_work_item` fails
5. Activity result is lost, worker logs error

**Recommended pattern for graceful cleanup:**
```rust
// 1. Request cancellation
client.cancel_instance("my-workflow").await?;

// 2. Wait for completion (with timeout)
match tokio::time::timeout(
    Duration::from_secs(30),
    client.wait_for_completion("my-workflow")
).await {
    Ok(_) => {
        // 3a. Gracefully completed, safe to delete
        client.delete_instance("my-workflow", false).await?;
    }
    Err(_) => {
        // 3b. Timeout - force delete as last resort
        client.delete_instance("my-workflow", true).await?;
    }
}
```

### 8.4 Parent-Child Consistency (Sub-Orchestrations)

**Rule:** A sub-orchestration can only be deleted if its entire parent chain has completed.

**Problem:** If a child sub-orchestration is deleted while the parent is still running:
- Parent hangs indefinitely waiting for `SubOrchCompleted` event that never arrives
- No timeout → permanent stuck state

**Resolution Options:**
1. **Validation (Recommended):** Before deleting an instance, check if it has a parent that is still running. If so, reject the deletion with an error.
2. **Force flag:** Allow deletion with `force=true`, but document the risk.
3. **Future work:** Cascading delete or parent notification (out of scope).

**Implementation:** Query for parent relationship in `OrchestrationStarted` event or add a `parent_instance_id` column to the schema.

### 8.5 Identity Reuse

After deletion, a new instance with the same ID can be created immediately. This is intentional (useful for testing/resetting).

### 8.6 Active Execution Protection

The following are **NEVER** deleted/pruned:
- The `current_execution_id` of any instance
- Any execution with `status = 'Running'`

This ensures that:
- In-progress orchestrations are not corrupted
- `ContinueAsNew` chains don't lose their active head

---

## 9. Implementation Plan

### Phase 1: Schema Fix
1. **Fix `completed_at` storage** — Use Rust epoch milliseconds instead of `CURRENT_TIMESTAMP`
2. Add migration if needed for existing data

### Phase 2: Core Types & Provider
1. Add `InstanceFilter`, `PruneOptions` types to `src/providers/management.rs`
2. Add `DeleteResult`, `PurgeResult`, `PruneResult` types
3. Extend `ProviderAdmin` trait with new methods
4. Implement in `SqliteProvider`
5. Add provider validation tests

### Phase 3: Client Integration  
1. Expose methods on `Client` struct
2. Add error types (`ClientError::InstanceStillRunning`, `ClientError::ParentStillRunning`, etc.)
3. Add integration tests

---

## 10. Test Plan

### 10.1 Provider Validation Tests (`src/provider_validations.rs`)

These tests validate the `ProviderAdmin` trait implementation.

#### Delete Instance Tests
| Test | Description |
|------|-------------|
| `test_delete_completed_instance` | Delete a completed instance, verify all tables cleaned |
| `test_delete_failed_instance` | Delete a failed instance |
| `test_delete_running_instance_rejected` | Attempt to delete running instance without force, expect error |
| `test_delete_running_instance_force` | Delete running instance with force=true |
| `test_delete_nonexistent_instance` | Delete non-existent instance returns `instance_deleted: false` |
| `test_delete_instance_cleans_queues` | Verify orchestrator_queue and worker_queue entries are deleted |
| `test_delete_instance_cleans_locks` | Verify instance_locks are deleted |

#### Purge Instance Tests
| Test | Description |
|------|-------------|
| `test_purge_by_instance_ids` | Purge specific instances by ID |
| `test_purge_by_completed_before` | Purge instances completed before cutoff |
| `test_purge_with_and_filter` | Purge with both instance_ids AND completed_before (intersection) |
| `test_purge_skips_running_instances` | Running instances in filter are silently skipped |
| `test_purge_respects_limit` | Only deletes up to limit count |
| `test_purge_empty_filter` | Empty filter purges all terminal instances (up to limit) |

#### Prune Execution Tests
| Test | Description |
|------|-------------|
| `test_prune_keep_last` | Keep last N executions, delete older ones |
| `test_prune_completed_before` | Delete executions completed before cutoff |
| `test_prune_and_filter` | Both keep_last AND completed_before (intersection) |
| `test_prune_never_deletes_current_execution` | Current execution is never pruned |
| `test_prune_never_deletes_running_execution` | Running executions are never pruned |
| `test_prune_empty_options` | Empty options = no deletions |
| `test_prune_nonexistent_instance` | Prune non-existent instance returns 0 |

#### Bulk Prune Tests
| Test | Description |
|------|-------------|
| `test_prune_executions_bulk_by_ids` | Bulk prune specific instances |
| `test_prune_executions_bulk_by_time` | Bulk prune instances completed before cutoff |
| `test_prune_executions_bulk_skips_running` | Running instances are skipped |
| `test_prune_executions_bulk_respects_limit` | Processes up to limit instances |

### 10.2 Force Delete Behavior Tests

These tests verify correct behavior when force-deleting instances with in-flight work.

#### Activity Interaction Tests
| Test | Description |
|------|-------------|
| `test_force_delete_activity_lock_renewal_fails` | Force delete instance while activity running; verify worker's next lock renewal fails |
| `test_force_delete_activity_detects_cancellation` | After lock renewal fails, verify `ctx.is_cancelled()` returns true |
| `test_force_delete_activity_ack_fails_gracefully` | Activity completes after delete; verify ack fails with appropriate error (not panic) |
| `test_force_delete_clears_worker_queue` | Verify all worker_queue entries for instance are deleted |
| `test_force_delete_clears_orchestrator_queue` | Verify all orchestrator_queue entries for instance are deleted |

#### Orchestration Turn Interaction Tests
| Test | Description |
|------|-------------|
| `test_force_delete_during_orchestration_turn` | Delete while turn is executing; turn completion fails to persist |
| `test_force_delete_between_turns` | Delete while instance is waiting (idle); clean deletion |
| `test_force_delete_pending_timer` | Delete instance with pending timer message; timer never fires |
| `test_force_delete_pending_external_event` | Delete instance waiting on event; event delivery fails gracefully |

#### Concurrent Operation Tests
| Test | Description |
|------|-------------|
| `test_concurrent_force_delete_and_activity_ack` | Race between delete and activity ack; one succeeds, other fails gracefully |
| `test_concurrent_force_delete_and_turn_completion` | Race between delete and turn persist; one succeeds, other fails gracefully |
| `test_concurrent_force_delete_and_lock_renewal` | Race between delete and lock renewal; renewal fails after delete |

#### Purge Behavior Tests
| Test | Description |
|------|-------------|
| `test_purge_skips_running_instances` | Bulk purge with Running instances in filter; they are silently skipped |
| `test_purge_only_deletes_terminal` | Purge only affects Completed/Failed instances |

### 10.3 Parent-Child Hierarchy Tests

| Test | Description |
|------|-------------|
| `test_delete_child_with_running_parent_rejected` | Cannot delete sub-orchestration if parent is running |
| `test_delete_child_with_completed_parent_allowed` | Can delete sub-orchestration if parent completed |
| `test_delete_parent_before_child` | Delete parent first, then child (should work) |
| `test_force_delete_child_with_running_parent` | Force delete bypasses parent check |

### 10.4 Time-Based Retention Tests

| Test | Description |
|------|-------------|
| `test_purge_5_day_retention` | Purge instances completed more than 5 days ago |
| `test_prune_30_day_execution_retention` | Prune executions older than 30 days |
| `test_timestamp_comparison_accuracy` | Verify millisecond precision in time comparisons |
| `test_completed_at_stored_as_integer` | Verify completed_at is stored as epoch ms, not TEXT |

### 10.5 Edge Case Tests

| Test | Description |
|------|-------------|
| `test_zombie_orchestrator_message_after_delete` | Delete instance, then dispatcher picks up orphaned message; verify graceful ack without processing |
| `test_zombie_worker_message_after_delete` | Delete instance, then worker picks up orphaned activity; verify graceful handling |
| `test_identity_reuse_after_delete` | Delete instance, immediately create new instance with same ID; verify clean slate |
| `test_prune_continue_as_new_chain` | Prune old executions from long ContinueAsNew chain |
| `test_delete_instance_multiple_pending_activities` | Delete instance with 3+ pending activities; all queue entries cleaned |
| `test_force_delete_sub_orchestration_orphans_parent` | Force delete child while parent waiting; parent hangs (documents expected behavior) |
| `test_external_event_arrives_after_delete` | Send event to deleted instance; verify graceful "not found" handling |

---

## 11. Alternatives Considered

### Automatic TTL
Automatically deleting old records via background sweeper.
- *Pros*: Zero maintenance after configuration
- *Cons*: Complex to implement generically; less control; harder to debug

**Decision:** Provide explicit APIs first. Automatic TTL can be built on top of `purge_instances` later (e.g., a scheduled task calling the API).

### Soft Deletes
Marking records as deleted rather than removing them.
- *Pros*: Recoverable; audit trail
- *Cons*: Doesn't solve storage growth; all queries need to filter deleted items

**Decision:** Hard deletes only. Users needing audit trails should export data before deletion.

### OR vs AND Filter Semantics
Using OR between filter criteria (match any).
- *Cons*: Less intuitive; harder to express "only delete old items from this list"

**Decision:** AND semantics. Users wanting OR can make multiple calls.

---

## 12. Summary

| Operation | User API | Provider API | Scope |
|-----------|----------|--------------|-------|
| Delete one instance | `client.delete_instance(id, force)` | `delete_instance(id, force)` | Single instance |
| Bulk delete instances | `client.purge_instances(filter)` | `purge_instances(&filter)` | Multi-instance |
| Prune one instance | `client.prune_executions(id, options)` | `prune_executions(id, &options)` | Single instance |
| Bulk prune executions | `client.prune_executions_bulk(filter, options)` | `prune_executions_bulk(&filter, &options)` | Multi-instance |

**Key Design Decisions:**
- `InstanceFilter` is reusable across management APIs
- All filter criteria use AND semantics
- Running instances are always protected (skipped, not errored)
- Current execution is always protected during pruning
- Active (Running) executions are never pruned
- Sub-orchestrations can only be deleted if parent chain is completed
- Timestamps stored as Rust epoch milliseconds (not SQLite CURRENT_TIMESTAMP)
