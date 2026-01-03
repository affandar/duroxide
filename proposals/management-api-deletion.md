# Management API: Deletion and Pruning

> **Status**: Proposed
> **Related**: `docs/management-api-improvements-proposal.md`

## 1. Summary

This proposal adds explicit deletion capabilities to the `ManagementProvider` trait to allow:
1.  **Instance Deletion**: Removing terminated orchestrations and all their associated data (history, executions).
2.  **Execution Pruning**: Removing specific completed executions within a long-running orchestration (e.g., `ContinueAsNew` chains) to reclaim storage.

## 2. Motivation

Currently, duroxide retains all history and execution data indefinitely. For long-running applications or high-volume systems, this leads to:
- **Unbounded Storage Growth**: "Eternal" orchestrations that use `ContinueAsNew` accumulate history for every past execution.
- **Operational Clutter**: Terminated or failed test instances remain in the database, cluttering dashboards and queries.
- **Compliance/Privacy**: Inability to delete user-specific data when requested (GDPR/CCPA "Right to be Forgotten").

## 3. Proposed API

We will extend the `ManagementProvider` trait with the following methods.

### 3.1 Delete Instance

Deletes an entire orchestration instance and all its history.

```rust
#[async_trait::async_trait]
pub trait ManagementProvider: Send + Sync {
    // ... existing methods ...

    /// Delete an instance and all its associated data.
    ///
    /// # Parameters
    /// * `instance_id`: The ID of the instance to delete.
    ///
    /// # Returns
    /// * `Ok(true)` if the instance was found and deleted.
    /// * `Ok(false)` if the instance was not found.
    /// * `Err(ProviderError)` if the deletion failed or was rejected (e.g., instance is running).
    async fn delete_instance(&self, instance_id: &str) -> Result<bool, ProviderError>;
}
```

**Behavior:**
- **Target**: `instances` table row, all linked `executions` rows, all linked `history` rows.
- **Safety Check**: By default, this operation should fail if the instance is in a `Running` or `Created` state.
    - *Option*: We could add a `force: bool` parameter to override this safety check.

### 3.2 Delete Execution (Pruning)

Deletes a specific execution from an instance's history. This is primarily used for pruning old history in `ContinueAsNew` scenarios.

```rust
#[async_trait::async_trait]
pub trait ManagementProvider: Send + Sync {
    // ... existing methods ...

    /// Delete a specific execution and its history.
    ///
    /// # Parameters
    /// * `instance_id`: The ID of the instance.
    /// * `execution_id`: The ID of the execution to delete.
    ///
    /// # Returns
    /// * `Ok(true)` if the execution was found and deleted.
    /// * `Ok(false)` if the execution was not found.
    /// * `Err(ProviderError)` if the deletion failed or was rejected.
    async fn delete_execution(&self, instance_id: &str, execution_id: u64) -> Result<bool, ProviderError>;
}
```

**Behavior:**
- **Target**: Specific row in `executions` table and all matching rows in `history` table.
- **Safety Check**:
    - Cannot delete the `current_execution_id` of an active instance.
    - The execution must be in a terminal state (`Completed`, `Failed`, `Terminated`, `ContinuedAsNew`).

## 4. Detailed Design

### 4.1 Data Model Implications (SQL Provider)

Assuming a relational schema (like SQLite/Postgres):

**Tables:**
- `instances` (PK: `instance_id`)
- `executions` (PK: `instance_id`, `execution_id`)
- `history` (PK: `instance_id`, `execution_id`, `sequence_number`)

**Delete Instance Logic:**
```sql
BEGIN TRANSACTION;
-- Check status if safety is required
SELECT status FROM instances WHERE instance_id = ?;
-- If safe:
DELETE FROM history WHERE instance_id = ?;
DELETE FROM executions WHERE instance_id = ?;
DELETE FROM instances WHERE instance_id = ?;
COMMIT;
```

**Delete Execution Logic:**
```sql
BEGIN TRANSACTION;
-- Check if it's the current execution
SELECT current_execution_id FROM instances WHERE instance_id = ?;
-- If execution_id != current_execution_id:
DELETE FROM history WHERE instance_id = ? AND execution_id = ?;
DELETE FROM executions WHERE instance_id = ? AND execution_id = ?;
COMMIT;
```

### 4.2 API Refinement: Bulk Pruning

Deleting executions one by one might be inefficient for cleaning up thousands of loops. A bulk pruning API is recommended.

```rust
#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Keep the last N executions.
    pub keep_last: u32,
}

#[async_trait::async_trait]
pub trait ManagementProvider: Send + Sync {
    // ...
    
    /// Prune old executions for an instance.
    async fn prune_history(&self, instance_id: &str, options: PruneOptions) -> Result<u64, ProviderError>;
}
```

## 5. Edge Cases & Implications

### 5.1 Zombie Messages
Since the `orchestrator_queue` does not strictly enforce foreign key constraints in all providers, it is possible for external systems to send events (e.g., `ExternalRaised`) to an instance that has been deleted.
-   **Behavior**: The `fetch_orchestration_item` loop will pick up the message.
-   **Problem**: It will fail to load the instance metadata (since the instance is deleted).
-   **Resolution**: Providers must handle "Instance Not Found" during fetch by **dropping the message** (acking it without processing) and logging a warning. This prevents poison message loops.

### 5.2 Orphaned Activities
If an instance is deleted while activities are running on workers:
1.  The worker will eventually finish and call `ack_work_item`.
2.  The `ack_work_item` call will fail because the underlying `worker_queue` item was deleted by `delete_instance`.
3.  The worker should log this error and stop. The activity result is lost.
4.  **Note**: This is the expected behavior for a "hard delete".

### 5.3 Parent-Child Consistency
Deleting a child sub-orchestration will cause the parent orchestration to hang indefinitely (waiting for a completion event that will never come), unless the parent has a timeout configured.
-   **Recommendation**: Users should ensure parents have timeouts or manually terminate parents if they delete children.
-   **Future Work**: A "Cascading Delete" or "Notify Parent" option could be added, but is out of scope for this initial proposal.

### 5.4 Identity Reuse
Deleting an instance removes it from the `instances` table, allowing a new instance with the **same ID** to be created immediately.
-   This bypasses the usual "InstanceAlreadyExists" checks for completed instances.
-   This is a feature, not a bug (useful for testing/resetting), but operators should be aware of it.

## 6. Implementation Plan

1.  **Update `ManagementProvider` Trait**: Add `delete_instance` and `delete_execution` (or `prune_history`).
2.  **Update `SqliteProvider`**: Implement the new methods.
    - Ensure transactional integrity.
    - Implement safety checks (don't delete running instances without force).
    - **Crucial**: Ensure `fetch_orchestration_item` handles missing instances gracefully (drops messages).
3.  **Update Client**: Expose these methods via the `Client` struct.
4.  **CLI Support**: Add `duroxide-cli delete instance <id>` and `duroxide-cli prune <id>`.

## 7. Alternatives Considered

- **TTL (Time To Live)**: Automatically deleting old records.
    - *Pros*: Zero maintenance.
    - *Cons*: Less control. Hard to implement generically across providers without a background sweeper.
- **Soft Deletes**: Marking records as deleted.
    - *Pros*: Recoverable.
    - *Cons*: Doesn't solve storage growth. Queries need to filter out deleted items.

## 7. Recommendation

Implement explicit `delete_instance` and `delete_execution` first. This gives operators immediate control. Automatic pruning (TTL) can be built on top of these primitives later.
