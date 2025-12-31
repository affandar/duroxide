# Management Interface Extension Proposal

## P0: Deletion Support

The current management interface (`ProviderAdmin`) lacks the ability to delete instances and executions. This is critical for cleaning up test data and managing storage growth.

### Proposed API Changes

Add the following methods to the `ProviderAdmin` trait:

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    /// Delete an instance and all associated data (executions, history, queue items).
    async fn delete_instance(&self, instance: &str) -> Result<(), ProviderError>;

    /// Delete a specific execution and its history.
    /// Note: This does not affect queue items as they are ephemeral.
    async fn delete_execution(&self, instance: &str, execution_id: u64) -> Result<(), ProviderError>;
}
```

### SQLite Implementation Details

#### `delete_instance`
Must perform the following deletions atomically (transaction):
1. Delete from `instances`
2. Delete from `executions`
3. Delete from `history`
4. Delete from `orchestrator_queue` (where `instance_id` matches)
5. Delete from `instance_locks`
6. Delete from `worker_queue` (This is harder as `worker_queue` items store serialized `WorkItem`. We might need to scan or use a generated column if supported, or just accept that worker items might linger until processed/abandoned. However, `WorkItem::ActivityExecute` has `instance` field. We can parse it or rely on `fetch_work_item` filtering if we add a check there. But `fetch_work_item` already checks `ExecutionState`. If instance is missing, it returns `Missing` state, so the worker will drop it. So explicit deletion from `worker_queue` is optimization, not strict correctness requirement, but good to have if possible).

#### `delete_execution`
1. Delete from `executions`
2. Delete from `history`
3. Update `instances` if the deleted execution was the `current_execution_id`? (Maybe not allowed to delete current execution? Or if we do, what happens? Probably safest to allow deleting old executions).

## P1: Dashboard Optimization

Current `list_instances` returns `Vec<String>`. To build a dashboard, the frontend would need to call `get_instance_info` for each instance, resulting in N+1 queries.

### Proposed API Changes

Add methods for bulk retrieval of metadata:

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    /// List instances with comprehensive metadata, supporting pagination and filtering.
    async fn list_instances_detailed(
        &self, 
        limit: usize, 
        offset: usize,
        status_filter: Option<&str>
    ) -> Result<Vec<InstanceInfo>, ProviderError>;
    
    /// List all executions for an instance with metadata.
    async fn list_executions_detailed(&self, instance: &str) -> Result<Vec<ExecutionInfo>, ProviderError>;
}
```

### SQLite Implementation Details

#### `list_instances_detailed`
Single query joining `instances` and `executions` (for current execution status):

```sql
SELECT 
    i.instance_id, i.orchestration_name, i.orchestration_version, 
    i.current_execution_id, i.created_at, i.updated_at,
    e.status, e.output, e.completed_at
FROM instances i
LEFT JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
WHERE (? IS NULL OR e.status = ?)
ORDER BY i.created_at DESC
LIMIT ? OFFSET ?
```

#### `list_executions_detailed`
Single query on `executions` table, optionally joining `history` to count events (count might be expensive, so maybe optional or separately? `get_execution_info` does a subquery count. For list, maybe we omit event count or keep it if performant enough).

```sql
SELECT 
    e.execution_id, e.status, e.output, e.started_at, e.completed_at,
    (SELECT COUNT(*) FROM history h WHERE h.instance_id = e.instance_id AND h.execution_id = e.execution_id) as event_count
FROM executions e
WHERE e.instance_id = ?
ORDER BY e.execution_id DESC
```
