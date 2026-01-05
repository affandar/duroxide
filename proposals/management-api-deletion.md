# Management API: Deletion and Pruning

> **Status**: Implemented
> **Related**: `docs/management-api-improvements-proposal.md`

## 1. Summary

This proposal adds explicit deletion and retention capabilities to duroxide:

1. **Instance Deletion**: Remove terminated orchestrations and all associated data (`delete_instance`)
2. **Bulk Instance Deletion**: Delete multiple instances by ID and/or time-based filter (`delete_instance_bulk`)
3. **Execution Pruning**: Remove old executions from long-running `ContinueAsNew` chains (`prune_executions`)
4. **Bulk Execution Pruning**: Prune executions across multiple instances matching a filter (`prune_executions_bulk`)

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
| **`delete_instance`** | Permanently remove an orchestration instance and ALL its data | Single instance |
| **`delete_instance_bulk`** | Bulk delete multiple instances matching criteria | Multi-instance |
| **`prune_executions`** | Remove old executions while keeping the instance alive | Single instance |
| **`prune_executions_bulk`** | Prune executions across multiple instances | Multi-instance |

**Rationale:**
- `delete_instance` is the standard term for removing a single identified resource
- `delete_instance_bulk` uses the same verb with `_bulk` suffix for batch operations (consistent pattern)
- `prune` implies trimming while preserving the living entity (the instance continues to exist)
- Both single and bulk deletion return the same `DeleteInstanceResult` type for API consistency

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
/// Result of instance deletion (single or bulk).
///
/// Used by both `delete_instance` and `delete_instance_bulk` for API consistency.
#[derive(Debug, Clone, Default)]
pub struct DeleteInstanceResult {
    /// Number of instances deleted (1 for single instance, N for bulk).
    pub instances_deleted: u64,
    /// Number of executions deleted.
    pub executions_deleted: u64,
    /// Number of history events deleted.
    pub events_deleted: u64,
    /// Number of queue messages deleted (orchestrator + worker + timer queues).
    pub queue_messages_deleted: u64,
}

/// Result of an execution prune operation.
#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    /// Number of instances processed (1 for single instance prune, N for bulk).
    pub instances_processed: u64,
    /// Number of executions deleted.
    pub executions_deleted: u64,
    /// Number of history events deleted.
    pub events_deleted: u64,
}
```

**Design Decision:** We use a single `DeleteInstanceResult` type for both single and bulk deletion operations. The `instances_deleted` field is `u64` rather than `bool` to support both cases uniformly. For single instance deletion, this will be `1` on success or `0` if the instance wasn't found.

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
    /// * `Ok(DeleteInstanceResult)` - Details of what was deleted (`instances_deleted` will be 1)
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
    ) -> Result<DeleteInstanceResult, ClientError>;
}
```

### 5.2 Bulk Delete Instances

```rust
impl Client {
    /// Delete multiple orchestration instances matching the filter criteria.
    ///
    /// This is the primary API for retention-based cleanup. Only instances
    /// in terminal states (Completed, Failed) are eligible for deletion.
    /// Running instances are always skipped (not an error).
    ///
    /// # Filter Behavior
    ///
    /// All filter criteria are ANDed together:
    /// - `instance_ids` + `completed_before`: Only delete IDs that are also old
    /// - `limit` is applied after other filters
    ///
    /// # Examples
    /// ```ignore
    /// // Delete specific instances
    /// let result = client.delete_instance_bulk(InstanceFilter {
    ///     instance_ids: Some(vec!["order-1".into(), "order-2".into()]),
    ///     ..Default::default()
    /// }).await?;
    ///
    /// // Delete by age (retention policy)
    /// let five_days_ago = now_ms - (5 * 24 * 60 * 60 * 1000);
    /// let result = client.delete_instance_bulk(InstanceFilter {
    ///     completed_before: Some(five_days_ago),
    ///     limit: Some(500),
    ///     ..Default::default()
    /// }).await?;
    ///
    /// // Delete specific instances only if they're old
    /// let result = client.delete_instance_bulk(InstanceFilter {
    ///     instance_ids: Some(vec!["order-1".into(), "order-2".into()]),
    ///     completed_before: Some(five_days_ago),
    ///     ..Default::default()
    /// }).await?;
    /// ```
    ///
    /// # Safety
    /// - Running instances are NEVER deleted (silently skipped)
    /// - Use `limit` to avoid long-running transactions
    pub async fn delete_instance_bulk(
        &self,
        filter: InstanceFilter,
    ) -> Result<DeleteInstanceResult, ClientError>;
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
    ) -> Result<DeleteInstanceResult, ProviderError>;

    /// Delete multiple instances matching the filter.
    ///
    /// # Implementation Requirements
    /// - AND all filter criteria together
    /// - Skip any instance with status='Running' (don't error)
    /// - Should be efficient (batch SQL operations)
    /// - Apply limit after other filters
    async fn delete_instance_bulk(
        &self,
        filter: InstanceFilter,
    ) -> Result<DeleteInstanceResult, ProviderError>;

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
| `delete_instance_bulk(filter)` | `delete_instance_bulk(filter)` |
| `prune_executions(id, options)` | `prune_executions(id, &options)` |
| `prune_executions_bulk(filter, options)` | `prune_executions_bulk(&filter, &options)` |

### 6.3 Provider Simplification: Primitives vs Composites

To reduce the implementation burden on provider developers, we separate the `ProviderAdmin` trait into:

1. **Primitives** - Simple database operations providers MUST implement
2. **Composites** - Complex operations with DEFAULT implementations using primitives

This means provider developers only implement 3-4 simple methods, and get all cascade/tree logic for free.

#### Primitive Methods (Provider Implements)

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing read-only methods ...

    // ===== Primitive Hierarchy Operations =====

    /// List direct children of an instance.
    ///
    /// Returns instance IDs that have `parent_instance_id = instance_id`.
    /// Returns empty vec if instance has no children or doesn't exist.
    async fn list_children(&self, instance_id: &str) -> Result<Vec<String>, ProviderError>;

    /// Get the parent instance ID.
    ///
    /// Returns `Some(parent_id)` for sub-orchestrations, `None` for root orchestrations.
    /// Returns `Err` if instance doesn't exist.
    async fn get_parent_id(&self, instance_id: &str) -> Result<Option<String>, ProviderError>;

    /// Atomically delete a batch of instances.
    ///
    /// # Orphan Validation
    /// This method MUST validate that no orphans would be created:
    /// - If any instance in `ids` has children that are NOT in `ids`, return error
    /// - If any instance in `ids` has a parent that IS in `ids`, the parent must appear
    ///   AFTER the child (children deleted before parents)
    ///
    /// # Transaction Semantics
    /// All instances must be deleted atomically (all-or-nothing).
    ///
    /// # Status Check
    /// If `force=false`, all instances must be in terminal state.
    /// If `force=true`, delete regardless of status.
    async fn delete_instances_atomic(
        &self,
        ids: &[String],
        force: bool,
    ) -> Result<DeleteInstanceResult, ProviderError>;
}
```

#### Composite Methods (Default Implementations)

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... primitives above ...

    // ===== Composite Operations (default implementations) =====

    /// Get the full instance tree rooted at the given instance.
    ///
    /// Returns all instances in the tree: the root, all children, grandchildren, etc.
    /// Ordered for safe deletion: children before parents (depth-first, leaves first).
    ///
    /// Default implementation uses `list_children` recursively.
    async fn get_instance_tree(&self, instance_id: &str) -> Result<InstanceTree, ProviderError> {
        // Default implementation using list_children
        let mut tree = InstanceTree {
            root_id: instance_id.to_string(),
            all_ids: vec![],
        };

        // BFS to collect all descendants
        let mut to_process = vec![instance_id.to_string()];
        while let Some(parent_id) = to_process.pop() {
            tree.all_ids.push(parent_id.clone());
            let children = self.list_children(&parent_id).await?;
            to_process.extend(children);
        }

        // Reverse for depth-first deletion order (children before parents)
        tree.all_ids.reverse();
        Ok(tree)
    }

    /// Delete a single instance (and all descendants if root).
    ///
    /// Default implementation:
    /// 1. Check if instance has a parent (is sub-orchestration)
    /// 2. If sub-orchestration: return error (must delete root)
    /// 3. Get full tree via `get_instance_tree`
    /// 4. Call `delete_instances_atomic` with all IDs
    async fn delete_instance(
        &self,
        instance_id: &str,
        force: bool,
    ) -> Result<DeleteInstanceResult, ProviderError> {
        // Step 1: Check if this is a sub-orchestration
        let parent = self.get_parent_id(instance_id).await?;
        if parent.is_some() {
            return Err(ProviderError::permanent(
                "delete_instance",
                format!(
                    "Cannot delete sub-orchestration {} directly. Delete root instance instead.",
                    instance_id
                ),
            ));
        }

        // Step 2: Get full tree
        let tree = self.get_instance_tree(instance_id).await?;

        // Step 3: Atomic delete (tree.all_ids is already in deletion order)
        self.delete_instances_atomic(&tree.all_ids, force).await
    }

    /// Delete multiple instances matching filter.
    ///
    /// Default implementation iterates through matches and calls delete_instance.
    /// Provider can override for better performance (batch operations).
    async fn delete_instance_bulk(&self, filter: InstanceFilter) -> Result<DeleteInstanceResult, ProviderError>;
}
```

#### New Type: InstanceTree

```rust
/// Represents an instance and all its descendants.
///
/// Used for inspecting hierarchies before deletion, or for understanding
/// sub-orchestration relationships.
#[derive(Debug, Clone)]
pub struct InstanceTree {
    /// The root instance ID.
    pub root_id: String,

    /// All instance IDs in the tree (including root).
    /// Ordered for safe deletion: children before parents.
    pub all_ids: Vec<String>,
}

impl InstanceTree {
    /// Returns true if this tree contains only the root (no children/descendants).
    pub fn is_root_only(&self) -> bool {
        self.all_ids.len() == 1
    }

    /// Returns the number of instances in the tree.
    pub fn size(&self) -> usize {
        self.all_ids.len()
    }
}
```

#### Provider Implementation Burden

| Before | After |
|--------|-------|
| Implement `delete_instance` with full cascade logic | Implement `list_children` (1 query) |
| Implement `purge_instances` with tree traversal | Implement `get_parent_id` (1 query) |
| Handle orphan validation | Implement `delete_instances_atomic` (batch delete) |
| ~200 lines per provider | ~50 lines per provider |

#### Example SQLite Primitive Implementation

```rust
impl ProviderAdmin for SqliteProvider {
    async fn list_children(&self, instance_id: &str) -> Result<Vec<String>, ProviderError> {
        let rows = sqlx::query("SELECT instance_id FROM instances WHERE parent_instance_id = ?")
            .bind(instance_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("instance_id")).collect())
    }

    async fn get_parent_id(&self, instance_id: &str) -> Result<Option<String>, ProviderError> {
        let row = sqlx::query("SELECT parent_instance_id FROM instances WHERE instance_id = ?")
            .bind(instance_id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(r.get("parent_instance_id")),
            None => Err(ProviderError::permanent("get_parent_id", "Instance not found")),
        }
    }

    async fn delete_instances_atomic(
        &self,
        ids: &[String],
        force: bool,
    ) -> Result<DeleteResult, ProviderError> {
        let mut tx = self.pool.begin().await?;

        // Status check (if not force)
        if !force {
            // Check all instances are terminal
            // ...
        }

        // Delete all instances
        let mut result = DeleteResult::default();
        for id in ids {
            // Delete from all tables
            // Aggregate counts into result
        }

        tx.commit().await?;
        Ok(result)
    }

    // delete_instance: uses default implementation!
    // get_instance_tree: uses default implementation!
}
```

#### Public API: get_instance_tree

The `get_instance_tree` method is also exposed via the `Client` API for users who want to inspect the hierarchy before deletion:

```rust
impl Client {
    /// Get the full instance tree rooted at the given instance.
    ///
    /// Useful for inspecting hierarchy before deletion, or for
    /// understanding sub-orchestration relationships.
    ///
    /// # Returns
    /// * `Ok(InstanceTree)` - The tree with all descendant IDs
    /// * `Err(ClientError::InstanceNotFound)` - Instance doesn't exist
    ///
    /// # Example
    /// ```ignore
    /// let tree = client.get_instance_tree("order-123").await?;
    /// println!("Will delete {} instances", tree.size());
    /// for id in &tree.all_ids {
    ///     println!("  - {}", id);
    /// }
    /// client.delete_instance("order-123", false).await?;
    /// ```
    pub async fn get_instance_tree(&self, instance_id: &str) -> Result<InstanceTree, ClientError>;
}
```

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

**Rule:** Sub-orchestrations cannot be deleted directly. Only root orchestrations can be deleted, and deletion cascades to all descendants.

**Problem:** If a child sub-orchestration is deleted while the parent is still running:
- Parent hangs indefinitely waiting for `SubOrchCompleted` event that never arrives
- No timeout → permanent stuck state
- Even if parent is completed, allowing direct child deletion creates inconsistent state

**Resolution: Cascading Delete from Root Only**

1. **Block direct sub-orchestration deletion:** Any attempt to delete an instance that has a parent returns `Err(ClientError::CannotDeleteSubOrchestration)`. This applies to both `force=true` and `force=false`.

2. **Cascade from root:** When deleting a root orchestration:
   - Recursively discover all descendant sub-orchestrations
   - Delete all descendants first (depth-first, children before parents)
   - Then delete the root instance
   - All deletions happen in a single transaction

3. **Force applies to entire tree:** When `force=true` is used on a root:
   - The force flag applies to the root AND all descendants
   - If any instance in the tree is running, all are force-deleted together

**Implementation Requirements:**
- Add `parent_instance_id` column to `instances` table (nullable, NULL for root orchestrations)
- Populate `parent_instance_id` when sub-orchestration is created
- On delete: query for parent, reject if parent exists
- On delete of root: recursively find and delete all children

**Schema Addition:**
```sql
ALTER TABLE instances ADD COLUMN parent_instance_id TEXT REFERENCES instances(instance_id);
CREATE INDEX idx_instances_parent ON instances(parent_instance_id);
```

**Cascade Delete Algorithm:**
```
delete_instance(instance_id, force):
    1. Check if instance has a parent_instance_id
       - If yes: return Err(CannotDeleteSubOrchestration)
    2. Collect all descendants (recursive CTE or iterative query)
    3. Check status of root (and all descendants if force=false)
       - If any is Running and force=false: return Err(InstanceStillRunning)
    4. In single transaction:
       - Delete all descendants (ordered by depth, deepest first)
       - Delete root instance
    5. Return aggregated DeleteResult
```

### 8.5 Critical: Instance Lock Deletion Prevents Zombie Recreation

**This is a critical implementation requirement for all providers.**

When force-deleting an instance, the `instance_locks` table entry MUST be deleted. This prevents a race condition where an in-flight orchestration turn could recreate a deleted instance.

**The Race (if locks are NOT deleted):**
```
T0: Dispatcher fetches item, acquires lock in instance_locks
T1: Orchestration turn executes in memory...
T2: Force delete runs but does NOT delete from instance_locks
    - Deletes from: instances, executions, history, queues
T3: Turn completes, calls ack_orchestration_item()
T4: Lock lookup SUCCEEDS (lock still exists!)
T5: INSERT OR IGNORE INTO instances → RECREATES deleted instance!
T6: History written to "zombie" instance

❌ Result: Instance exists after being "deleted"
```

**The Fix (locks ARE deleted):**
```
T0: Dispatcher fetches item, acquires lock in instance_locks
T1: Orchestration turn executes in memory...
T2: Force delete runs:
    - DELETE FROM instance_locks WHERE instance_id = ?  ← Critical!
    - Deletes from: instances, executions, history, queues
T3: Turn completes, calls ack_orchestration_item()
T4: Lock lookup FAILS → "Invalid lock token" error
T5: Turn result discarded, no recreation

✅ Result: Instance stays deleted
```

**Provider Validation Test:** `test_force_delete_prevents_ack_recreation` verifies this behavior:
1. Create and start an orchestration
2. Fetch orchestration item (acquires lock)
3. Force delete the instance
4. Attempt to ack the item
5. Assert: ack returns error (not success)
6. Assert: instance does NOT exist in database

### 8.6 Identity Reuse

After deletion, a new instance with the same ID can be created immediately. This is intentional (useful for testing/resetting).

### 8.7 Active Execution Protection

The following are **NEVER** deleted/pruned:
- The `current_execution_id` of any instance
- Any execution with `status = 'Running'`

This ensures that:
- In-progress orchestrations are not corrupted
- `ContinueAsNew` chains don't lose their active head

---

## 9. Implementation Plan

### Phase 1: Schema Updates
1. **Fix `completed_at` storage** — Use Rust epoch milliseconds instead of `CURRENT_TIMESTAMP`
2. **Add `parent_instance_id` column** — Track parent-child relationships for cascading delete
3. Add migration if needed for existing data
4. **Populate `parent_instance_id`** — Update sub-orchestration creation to set parent reference

### Phase 2: Core Types & Provider
1. Add `InstanceFilter`, `PruneOptions` types to `src/providers/management.rs`
2. Add `DeleteResult`, `PurgeResult`, `PruneResult` types
3. Extend `ProviderAdmin` trait with new methods
4. Implement in `SqliteProvider`
5. Add provider validation tests

### Phase 3: Client Integration
1. Expose methods on `Client` struct
2. Add error types:
   - `ClientError::InstanceStillRunning` — Instance is running and force=false
   - `ClientError::CannotDeleteSubOrchestration` — Cannot delete sub-orchestration directly; delete root instead
   - `ClientError::InstanceNotFound` — Instance doesn't exist
3. Add integration tests

---

## 10. Test Plan

### 10.1 Provider Validation Tests (`src/provider_validations.rs`)

These tests validate the `ProviderAdmin` trait implementation.

#### Delete Instance Tests
| Test | Description |
|------|-------------|
| `test_delete_terminal_instances` | Delete completed, failed, and cancelled instances, verify all tables cleaned |
| `test_delete_running_rejected_force_succeeds` | Attempt to delete running instance without force (expect error), then with force (succeeds) |
| `test_delete_nonexistent_instance` | Delete non-existent instance returns `instances_deleted: 0` |
| `test_delete_cleans_queues_and_locks` | Verify orchestrator_queue, worker_queue, and instance_locks entries are deleted |
| `test_force_delete_prevents_ack_recreation` | **CRITICAL**: Fetch orchestration item (acquires lock), force delete instance, then try to ack - ack must fail, instance must NOT be recreated |
| `test_cascade_delete_hierarchy` | Delete root with children, verify all descendants deleted |

#### Bulk Delete Instance Tests
| Test | Description |
|------|-------------|
| `test_delete_instance_bulk_filter_combinations` | Delete by instance_ids, non-existent IDs, and empty filter |
| `test_delete_instance_bulk_safety_and_limits` | Skips running instances, respects limit parameter |
| `test_delete_instance_bulk_completed_before_filter` | Delete instances completed before/after cutoff |
| `test_delete_instance_bulk_cascades_to_children` | Bulk delete cascades to sub-orchestrations |

#### Prune Execution Tests
| Test | Description |
|------|-------------|
| `test_prune_options_combinations` | Keep last N, completed_before, and combined filters |
| `test_prune_safety` | Current execution and running executions are never pruned |

#### Bulk Prune Tests
| Test | Description |
|------|-------------|
| `test_prune_bulk` | Bulk prune with instance filter and prune options |

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
| `test_force_delete_prevents_activity_completion_delivery` | **CRITICAL**: Fetch activity (acquires lock), force delete instance, ack activity - verify ack fails and no ActivityCompleted message is enqueued to orchestrator_queue |

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

### 10.3 Parent-Child Hierarchy Tests (Cascading Delete)

| Test | Description |
|------|-------------|
| `test_cascade_delete_hierarchy` | Sub-orchestrations cannot be deleted directly; cascade delete from root |
| `test_delete_get_instance_tree` | Get full tree with multiple levels of children |
| `test_delete_get_parent_id` | Get parent for sub-orchestrations, None for roots |
| `test_list_children` | List direct children of an instance |
| `test_delete_instances_atomic` | Atomic batch delete with orphan validation |
| `test_delete_instances_atomic_force` | Atomic batch delete with force flag |
| `test_delete_instances_atomic_orphan_detection` | Reject deletion if it would create orphans |

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
| `test_delete_sub_orchestration_always_rejected` | Direct deletion of sub-orchestration blocked regardless of force flag |
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

| Operation | User API | Provider API | Scope | Return Type |
|-----------|----------|--------------|-------|-------------|
| Delete one instance | `client.delete_instance(id, force)` | `delete_instance(id, force)` | Single instance | `DeleteInstanceResult` |
| Bulk delete instances | `client.delete_instance_bulk(filter)` | `delete_instance_bulk(filter)` | Multi-instance | `DeleteInstanceResult` |
| Prune one instance | `client.prune_executions(id, options)` | `prune_executions(id, &options)` | Single instance | `PruneResult` |
| Bulk prune executions | `client.prune_executions_bulk(filter, options)` | `prune_executions_bulk(&filter, &options)` | Multi-instance | `PruneResult` |

**Key Design Decisions:**
- `InstanceFilter` is reusable across management APIs
- All filter criteria use AND semantics
- Running instances are always protected (skipped, not errored)
- Current execution is always protected during pruning
- Active (Running) executions are never pruned
- Sub-orchestrations can only be deleted via their root (cascade delete)
- Timestamps stored as Rust epoch milliseconds (not SQLite CURRENT_TIMESTAMP)
- Unified `DeleteInstanceResult` type for both single and bulk deletion (uses `instances_deleted: u64` instead of bool)
