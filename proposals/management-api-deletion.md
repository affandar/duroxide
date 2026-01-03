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
/// The current execution is ALWAYS preserved regardless of these options.
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
- The current execution (`current_execution_id`) is never deleted regardless of options.

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
    pub instances_skipped: u64,  // Running instances that were skipped
    pub executions_deleted: u64,
    pub events_deleted: u64,
    pub queue_messages_deleted: u64,
}

/// Result of an execution prune operation.
#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    pub instances_processed: u64,  // For bulk prune
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
    /// * `force` - If true, delete even if instance is still running
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
    /// // Force delete a stuck instance
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
    /// let result = client.bulk_prune_executions(
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
    /// let result = client.bulk_prune_executions(
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
    /// - Only processes instances in terminal states (Completed, Failed, ContinuedAsNew)
    /// - Running instances are skipped
    /// - The current execution of each instance is never pruned
    pub async fn bulk_prune_executions(
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
    /// - Skip any instance with status='Running' (don't error, count in skipped)
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
    async fn bulk_prune_executions(
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
| `bulk_prune_executions(filter, options)` | `bulk_prune_executions(&filter, &options)` |

---

## 7. Detailed Design

### 7.1 Schema Requirements

The following columns are required for time-based operations:

| Table | Column | Purpose |
|-------|--------|---------|
| `executions` | `completed_at` | Anchor for retention queries |
| `executions` | `status` | Filter terminal vs running |
| `instances` | `current_execution_id` | Identify the active execution |

**SQLite Implementation Note:** `completed_at` is set via `CURRENT_TIMESTAMP` when status is updated. For time comparisons, providers should convert to epoch milliseconds consistently.

### 7.2 SQL Implementation Sketches

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
CREATE TEMP TABLE prune_candidates AS
SELECT execution_id FROM executions
WHERE instance_id = ?
  AND execution_id != ?  -- current_execution_id
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

**Bulk Prune Executions:**
```sql
-- For each instance matching InstanceFilter:
--   Apply prune_executions logic with PruneOptions
-- Can be done in a single transaction with careful SQL,
-- or iterate over matching instances
```

---

## 8. Edge Cases & Implications

### 8.1 Zombie Messages

When an instance is deleted, queued messages (e.g., `ExternalRaised`) may still exist.

**Problem:** `fetch_orchestration_item` picks up a message for a deleted instance.

**Resolution:** The orchestration dispatcher already handles missing instances gracefully. When loading instance metadata fails with "not found", the message should be acked without processing and a warning logged. This prevents poison message loops.

### 8.2 Orphaned Activities

If an instance is deleted while activities are running:

1. Worker finishes and calls `ack_work_item`
2. The call fails (worker_queue row was deleted)
3. Worker logs error and stops—activity result is lost

This is expected "hard delete" behavior. Users wanting graceful cleanup should cancel the instance first and wait for activities to complete.

### 8.3 Parent-Child Consistency

Deleting a child sub-orchestration causes the parent to hang indefinitely (waiting for a completion event that never arrives).

**Recommendation:** 
- Parents should have timeouts configured
- Future: "Cascading Delete" option (out of scope)

### 8.4 Identity Reuse

After deletion, a new instance with the same ID can be created immediately. This is intentional (useful for testing/resetting).

### 8.5 Timestamp Storage Consistency

For time-based filtering to work correctly, `executions.completed_at` must be stored consistently:
- SQLite: Use `CURRENT_TIMESTAMP` (stored as TEXT) and compare with `datetime()` functions
- Future providers: Store as INTEGER (epoch milliseconds) for simpler comparisons

---

## 9. Implementation Plan

### Phase 1: Core Types & Provider
1. Add `InstanceFilter`, `PruneOptions` types to `src/providers/management.rs`
2. Add `DeleteResult`, `PurgeResult`, `PruneResult` types
3. Extend `ProviderAdmin` trait with new methods
4. Implement in `SqliteProvider`
5. Add validation tests

### Phase 2: Client Integration  
1. Expose methods on `Client` struct
2. Add error types (`ClientError::InstanceStillRunning`, etc.)
3. Add integration tests

### Phase 3: CLI Support
1. `duroxide delete <instance_id> [--force]`
2. `duroxide purge [--ids id1,id2] [--completed-before <timestamp>] [--limit N]`
3. `duroxide prune <instance_id> [--keep-last N] [--completed-before <timestamp>]`
4. `duroxide bulk-prune [--ids id1,id2] [--completed-before <ts>] --keep-last N`

---

## 10. Alternatives Considered

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

## 11. Summary

| Operation | User API | Provider API | Scope |
|-----------|----------|--------------|-------|
| Delete one instance | `client.delete_instance(id, force)` | `delete_instance(id, force)` | Single instance |
| Bulk delete instances | `client.purge_instances(filter)` | `purge_instances(&filter)` | Multi-instance |
| Prune one instance | `client.prune_executions(id, options)` | `prune_executions(id, &options)` | Single instance |
| Bulk prune executions | `client.bulk_prune_executions(filter, options)` | `bulk_prune_executions(&filter, &options)` | Multi-instance |

**Key Design Decisions:**
- `InstanceFilter` is reusable across management APIs
- All filter criteria use AND semantics
- Running instances are always protected (skipped, not errored)
- Current execution is always protected during pruning
