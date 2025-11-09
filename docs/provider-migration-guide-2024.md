# Provider Migration Guide: Recent Interface Changes

**Target Audience:** LLMs and developers implementing custom Duroxide providers

**Migration Date:** November 2024

## Overview

This guide describes recent changes to the Duroxide `Provider` trait interface and how to update custom provider implementations. The changes focus on:

1. **Error handling improvements** - Structured error types with retryability
2. **Method naming clarity** - More descriptive names for queue operations
3. **Trait reorganization** - Clearer separation between core runtime and administrative operations
4. **Instance lifecycle** - Deferred instance creation using runtime-provided metadata

---

## Breaking Changes Summary

### 1. Error Handling: Introduction of `ProviderError`

**What Changed:**
- All `Provider` methods now return `Result<T, ProviderError>` instead of `Result<T, String>`
- `ProviderError` classifies errors as retryable or non-retryable

**Migration Steps:**

```rust
// OLD: Return String errors
async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), String> {
    Err("database connection failed".to_string())
}

// NEW: Return ProviderError with retryability classification
async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> {
    // Transient errors (network, locks, etc.) should be retryable
    Err(ProviderError::retryable("enqueue_for_worker", "database connection failed"))
    
    // Permanent errors (invalid data, constraint violations) should NOT be retryable
    // Err(ProviderError::permanent("enqueue_for_worker", "duplicate key violation"))
}
```

**Classification Guidelines:**
- **Retryable**: Network failures, connection timeouts, deadlocks, transient storage issues
- **Non-retryable**: Invalid data, constraint violations, data corruption, schema mismatches

**Helper Method Pattern:**
```rust
impl MyProvider {
    fn db_error_to_provider(&self, operation: &str, err: DatabaseError) -> ProviderError {
        match err {
            DatabaseError::ConnectionFailed => 
                ProviderError::retryable(operation, &format!("connection failed: {}", err)),
            DatabaseError::ConstraintViolation => 
                ProviderError::permanent(operation, &format!("constraint violation: {}", err)),
            _ => ProviderError::retryable(operation, &format!("database error: {}", err)),
        }
    }
}
```

---

### 2. Method Renames: Queue Operations

**What Changed:**
Four core methods were renamed for clarity:

| Old Name | New Name | Purpose |
|----------|----------|---------|
| `enqueue_orchestrator_work` | `enqueue_for_orchestrator` | Enqueue work for orchestrator |
| `enqueue_worker_work` | `enqueue_for_worker` | Enqueue work for worker |
| `dequeue_worker_peek_lock` | `fetch_work_item` | Fetch and lock worker work item |
| `ack_worker` | `ack_work_item` | Acknowledge worker completion |

**Migration Steps:**

```rust
// OLD trait implementation
#[async_trait]
impl Provider for MyProvider {
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), ProviderError> { ... }
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), ProviderError> { ... }
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> { ... }
    async fn ack_worker(&self, token: &str, completion: WorkItem) -> Result<(), ProviderError> { ... }
}

// NEW trait implementation - just rename the methods
#[async_trait]
impl Provider for MyProvider {
    async fn enqueue_for_orchestrator(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), ProviderError> { ... }
    async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> { ... }
    async fn fetch_work_item(&self) -> Option<(WorkItem, String)> { ... }
    async fn ack_work_item(&self, token: &str, completion: WorkItem) -> Result<(), ProviderError> { ... }
}
```

**Action:** Perform a find-and-replace in your provider implementation.

---

### 3. Trait Reorganization: Provider + ProviderAdmin

**What Changed:**
- Management methods moved from `Provider` to new `ProviderAdmin` trait
- `ManagementCapability` trait renamed to `ProviderAdmin`
- Removed `list_instances()` and `list_executions()` from `Provider` trait

**Old Structure:**
```rust
trait Provider {
    // Core runtime methods
    async fn fetch_orchestration_item(...) -> ...;
    async fn ack_orchestration_item(...) -> ...;
    
    // Management methods (MOVED OUT)
    async fn list_instances(&self) -> Result<Vec<String>, ProviderError>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ProviderError>;
    
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability>;
}

trait ManagementCapability {
    // Additional management methods
}
```

**New Structure:**
```rust
trait Provider {
    // Core runtime methods ONLY
    async fn fetch_orchestration_item(...) -> ...;
    async fn ack_orchestration_item(...) -> ...;
    
    // Discovery method (return type changed)
    fn as_management_capability(&self) -> Option<&dyn ProviderAdmin>;
}

trait ProviderAdmin {
    // ALL management methods
    async fn list_instances(&self) -> Result<Vec<String>, ProviderError>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ProviderError>;
    async fn read_history_with_execution_id(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError>;
    async fn read_history(&self, instance: &str) -> Result<Vec<Event>, ProviderError>;
    async fn latest_execution_id(&self, instance: &str) -> Result<u64, ProviderError>;
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, ProviderError>;
    async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, ProviderError>;
    async fn get_system_metrics(&self) -> Result<SystemMetrics, ProviderError>;
    async fn get_queue_depths(&self) -> Result<QueueDepths, ProviderError>;
}
```

**Migration Steps:**

```rust
// Step 1: Remove list_instances() and list_executions() from Provider impl
#[async_trait]
impl Provider for MyProvider {
    // Remove these two methods:
    // async fn list_instances(&self) -> Result<Vec<String>, ProviderError> { ... }
    // async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ProviderError> { ... }
    
    // Update return type
    fn as_management_capability(&self) -> Option<&dyn ProviderAdmin> {
        Some(self as &dyn ProviderAdmin)  // Changed from ManagementCapability
    }
}

// Step 2: Rename trait and add moved methods
#[async_trait]
impl ProviderAdmin for MyProvider {  // Changed from ManagementCapability
    // Move list_instances() and list_executions() HERE
    async fn list_instances(&self) -> Result<Vec<String>, ProviderError> { ... }
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ProviderError> { ... }
    
    // Other ProviderAdmin methods...
}
```

---

### 4. Management Method Renames

**What Changed:**
- `read_execution` → `read_history_with_execution_id`
- Added new method: `read_history()` (convenience wrapper)

**Migration Steps:**

```rust
#[async_trait]
impl ProviderAdmin for MyProvider {
    // OLD: read_execution
    // async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> { ... }
    
    // NEW: read_history_with_execution_id (just rename it)
    async fn read_history_with_execution_id(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> {
        // Same implementation as before
    }
    
    // NEW: read_history (convenience wrapper - implement this)
    async fn read_history(&self, instance: &str) -> Result<Vec<Event>, ProviderError> {
        let execution_id = self.latest_execution_id(instance).await?;
        self.read_history_with_execution_id(instance, execution_id).await
    }
}
```

---

### 5. Removed: Provider::latest_execution_id()

**What Changed:**
- `latest_execution_id()` removed from `Provider` trait
- Kept only in `ProviderAdmin` trait for management operations

**Migration Steps:**

```rust
// OLD: latest_execution_id was in Provider trait
#[async_trait]
impl Provider for MyProvider {
    async fn latest_execution_id(&self, instance: &str) -> Result<u64, ProviderError> { ... }
}

// NEW: Move to ProviderAdmin trait ONLY
#[async_trait]
impl ProviderAdmin for MyProvider {
    async fn latest_execution_id(&self, instance: &str) -> Result<u64, ProviderError> {
        // Same implementation
    }
}
```

**Note:** The runtime no longer calls `latest_execution_id()` - it tracks execution IDs internally.

---

### 6. Signature Change: fetch_orchestration_item

**What Changed:**
- Return type changed from `Option<OrchestrationItem>` to `Result<Option<OrchestrationItem>, ProviderError>`
- Allows providers to signal errors (network failures, etc.) distinct from "no work available"

**Migration Steps:**

```rust
// OLD: Return Option (can't signal errors)
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    match self.db.fetch_work() {
        Ok(Some(item)) => Some(item),
        Ok(None) => None,
        Err(_) => None,  // Error gets hidden!
    }
}

// NEW: Return Result<Option> (can signal errors)
async fn fetch_orchestration_item(&self) -> Result<Option<OrchestrationItem>, ProviderError> {
    match self.db.fetch_work() {
        Ok(Some(item)) => Ok(Some(item)),
        Ok(None) => Ok(None),  // No work available - normal case
        Err(e) => Err(ProviderError::retryable("fetch_orchestration_item", &format!("db error: {}", e))),
    }
}
```

---

### 7. Instance Creation: Deferred to ack_orchestration_item

**What Changed:**
- Instance records are NO LONGER created by `enqueue_for_orchestrator` for `StartOrchestration` work items
- Instance creation happens in `ack_orchestration_item` using `ExecutionMetadata` provided by runtime
- Default version is now `NULL` instead of hardcoded "1.0.0"

**Old Pattern:**
```rust
async fn enqueue_for_orchestrator(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), ProviderError> {
    if let WorkItem::StartOrchestration { instance, name, version, input, parent_instance, parent_execution_id, .. } = &item {
        // OLD: Create instance record here
        self.create_instance(instance, name, version.as_deref().unwrap_or("1.0.0"), input).await?;
    }
    self.enqueue_work(item, delay_ms).await
}
```

**New Pattern:**
```rust
async fn enqueue_for_orchestrator(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), ProviderError> {
    // NEW: Don't create instance records here anymore
    // Just enqueue the work item
    self.enqueue_work(item, delay_ms).await
}

async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,  // Runtime provides this
) -> Result<(), ProviderError> {
    // NEW: Create/update instance record here using metadata
    let instance = self.get_instance_from_token(lock_token)?;
    
    self.transaction(|tx| {
        // Insert or update instance record
        tx.execute(
            "INSERT INTO instances (instance_id, orchestration_name, orchestration_version, current_execution_id, status, output, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(instance_id) DO UPDATE SET
                current_execution_id = excluded.current_execution_id,
                status = excluded.status,
                output = excluded.output,
                updated_at = excluded.updated_at",
            (
                instance,
                &metadata.orchestration_name,
                &metadata.orchestration_version,  // Can be NULL now
                execution_id,
                &metadata.status,
                &metadata.output,
                current_timestamp(),
            )
        )?;
        
        // Append history, enqueue work items, etc.
        // ... rest of ack implementation
        
        Ok(())
    }).await
}
```

**Schema Changes:**
```sql
-- OLD: orchestration_version was NOT NULL with default '1.0.0'
CREATE TABLE instances (
    orchestration_version TEXT NOT NULL DEFAULT '1.0.0',
    ...
);

-- NEW: orchestration_version is NULLABLE
CREATE TABLE instances (
    orchestration_version TEXT,  -- NULL allowed
    ...
);
```

---

### 8. Testing Methods in Provider Trait

**What Changed:**
- Testing methods (`read_with_execution`, `append_with_execution`) are now part of the main `Provider` trait
- These methods are marked as "not used by runtime hot path" in documentation

**Migration Steps:**

If you previously didn't implement these methods, you now need to:

```rust
#[async_trait]
impl Provider for MyProvider {
    // ... other Provider methods ...
    
    // Add these testing/debugging methods
    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> {
        // Return events for specific execution
        self.db.query(
            "SELECT event_data FROM history 
             WHERE instance_id = ? AND execution_id = ? 
             ORDER BY event_id",
            (instance, execution_id)
        ).await
    }
    
    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), ProviderError> {
        // Append events directly (for testing)
        for event in new_events {
            self.db.insert_event(instance, execution_id, &event).await?;
        }
        Ok(())
    }
}
```

**Note:** These methods are used by tests and validation suites, not by the main runtime.

---

## Migration Checklist

Use this checklist to ensure your provider is fully migrated:

### Error Handling
- [ ] All method return types changed from `Result<T, String>` to `Result<T, ProviderError>`
- [ ] Errors are classified as retryable or non-retryable appropriately
- [ ] Added helper method to convert database/storage errors to `ProviderError`

### Method Renames (Provider trait)
- [ ] `enqueue_orchestrator_work` → `enqueue_for_orchestrator`
- [ ] `enqueue_worker_work` → `enqueue_for_worker`
- [ ] `dequeue_worker_peek_lock` → `fetch_work_item`
- [ ] `ack_worker` → `ack_work_item`
- [ ] `fetch_orchestration_item` return type changed to `Result<Option<...>, ProviderError>`

### Trait Reorganization
- [ ] Removed `list_instances()` from `Provider` impl
- [ ] Removed `list_executions()` from `Provider` impl
- [ ] Removed `latest_execution_id()` from `Provider` impl
- [ ] Changed `as_management_capability()` return type to `Option<&dyn ProviderAdmin>`
- [ ] Renamed `impl ManagementCapability` to `impl ProviderAdmin`
- [ ] Moved management methods to `ProviderAdmin` impl

### Method Renames (ProviderAdmin trait)
- [ ] `read_execution` → `read_history_with_execution_id`
- [ ] Added `read_history()` convenience method
- [ ] Added `latest_execution_id()` to `ProviderAdmin` impl

### Instance Lifecycle
- [ ] Removed instance creation from `enqueue_for_orchestrator`
- [ ] Updated `ack_orchestration_item` to create/update instance using `ExecutionMetadata`
- [ ] Changed `orchestration_version` column to allow NULL
- [ ] Updated instance upsert logic to use `metadata.orchestration_version`

### Testing Methods
- [ ] Implemented `read_with_execution()` in `Provider` trait
- [ ] Implemented `append_with_execution()` in `Provider` trait

### Validation
- [ ] Code compiles without errors
- [ ] All provider validation tests pass
- [ ] Tested with multi-execution scenarios (ContinueAsNew)
- [ ] Tested version resolution (NULL versions handled correctly)

---

## Example: Complete Migration Diff

Here's a simplified example showing before/after for a provider:

```rust
// ===== BEFORE =====

use duroxide::providers::{Provider, ManagementCapability, WorkItem, ExecutionMetadata};

#[async_trait]
impl Provider for MyProvider {
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
        if let WorkItem::StartOrchestration { instance, name, version, .. } = &item {
            self.create_instance(instance, name, version.as_deref().unwrap_or("1.0.0")).await?;
        }
        self.enqueue(item, delay_ms).await
    }
    
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        self.enqueue_worker(item).await.map_err(|e| e.to_string())
    }
    
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        self.fetch_worker_work().await.ok().flatten()
    }
    
    async fn ack_worker(&self, token: &str, completion: WorkItem) -> Result<(), String> {
        self.ack(token, completion).await.map_err(|e| e.to_string())
    }
    
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        self.fetch_orch().await.ok().flatten()
    }
    
    async fn latest_execution_id(&self, instance: &str) -> Result<u64, String> {
        self.db.get_latest_execution(instance).await.map_err(|e| e.to_string())
    }
    
    async fn list_instances(&self) -> Result<Vec<String>, String> {
        self.db.list_all_instances().await.map_err(|e| e.to_string())
    }
    
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String> {
        self.db.list_execs(instance).await.map_err(|e| e.to_string())
    }
    
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        Some(self as &dyn ManagementCapability)
    }
}

#[async_trait]
impl ManagementCapability for MyProvider {
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String> {
        self.db.read(instance, execution_id).await.map_err(|e| e.to_string())
    }
}

// ===== AFTER =====

use duroxide::providers::{Provider, ProviderAdmin, ProviderError, WorkItem, ExecutionMetadata};

#[async_trait]
impl Provider for MyProvider {
    // Renamed, don't create instance anymore
    async fn enqueue_for_orchestrator(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), ProviderError> {
        self.enqueue(item, delay_ms).await
            .map_err(|e| self.db_error(e))
    }
    
    // Renamed, return ProviderError
    async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> {
        self.enqueue_worker(item).await
            .map_err(|e| self.db_error(e))
    }
    
    // Renamed
    async fn fetch_work_item(&self) -> Option<(WorkItem, String)> {
        self.fetch_worker_work().await.ok().flatten()
    }
    
    // Renamed, return ProviderError
    async fn ack_work_item(&self, token: &str, completion: WorkItem) -> Result<(), ProviderError> {
        self.ack(token, completion).await
            .map_err(|e| self.db_error(e))
    }
    
    // Return type changed to Result<Option<...>, ProviderError>
    async fn fetch_orchestration_item(&self) -> Result<Option<OrchestrationItem>, ProviderError> {
        self.fetch_orch().await
            .map_err(|e| self.db_error(e))
    }
    
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,  // Use this!
    ) -> Result<(), ProviderError> {
        let instance = self.get_instance_from_token(lock_token)?;
        
        // Create/update instance using metadata
        self.upsert_instance(
            instance,
            &metadata.orchestration_name,
            metadata.orchestration_version.as_deref(),  // Can be None now
            execution_id,
            &metadata.status,
            metadata.output.as_deref(),
        ).await?;
        
        // ... rest of ack logic
        Ok(())
    }
    
    // Removed latest_execution_id from Provider
    // Removed list_instances from Provider
    // Removed list_executions from Provider
    
    // Add testing methods
    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> {
        self.db.read_execution_events(instance, execution_id).await
            .map_err(|e| self.db_error(e))
    }
    
    async fn append_with_execution(&self, instance: &str, execution_id: u64, new_events: Vec<Event>) -> Result<(), ProviderError> {
        self.db.append_events(instance, execution_id, new_events).await
            .map_err(|e| self.db_error(e))
    }
    
    // Return type changed
    fn as_management_capability(&self) -> Option<&dyn ProviderAdmin> {
        Some(self as &dyn ProviderAdmin)
    }
}

// Trait renamed
#[async_trait]
impl ProviderAdmin for MyProvider {
    // Moved from Provider
    async fn list_instances(&self) -> Result<Vec<String>, ProviderError> {
        self.db.list_all_instances().await
            .map_err(|e| self.db_error(e))
    }
    
    // Moved from Provider
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ProviderError> {
        self.db.list_execs(instance).await
            .map_err(|e| self.db_error(e))
    }
    
    // Moved from Provider
    async fn latest_execution_id(&self, instance: &str) -> Result<u64, ProviderError> {
        self.db.get_latest_execution(instance).await
            .map_err(|e| self.db_error(e))
    }
    
    // Renamed from read_execution
    async fn read_history_with_execution_id(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> {
        self.db.read(instance, execution_id).await
            .map_err(|e| self.db_error(e))
    }
    
    // New convenience method
    async fn read_history(&self, instance: &str) -> Result<Vec<Event>, ProviderError> {
        let execution_id = self.latest_execution_id(instance).await?;
        self.read_history_with_execution_id(instance, execution_id).await
    }
    
    // Other ProviderAdmin methods...
}

impl MyProvider {
    fn db_error(&self, err: DbError) -> ProviderError {
        match err {
            DbError::ConnectionFailed => ProviderError::retryable("db_operation", "connection failed"),
            DbError::Constraint => ProviderError::permanent("db_operation", "constraint violation"),
            _ => ProviderError::retryable("db_operation", &format!("{}", err)),
        }
    }
}
```

---

## Testing Your Migration

After migrating, run these tests to verify correctness:

1. **Provider Validation Suite:**
```bash
cargo test provider_validation
```

2. **Multi-Execution Tests:**
```bash
cargo test continue_as_new
```

3. **Error Handling Tests:**
```bash
cargo test provider_error
```

4. **Management API Tests:**
```bash
cargo test management_interface
```

---

## Common Pitfalls

### 1. Forgetting to Remove Instance Creation
**Symptom:** Duplicate instance creation, runtime panics on version mismatch

**Fix:** Remove all instance creation logic from `enqueue_for_orchestrator`. Let `ack_orchestration_item` handle it.

### 2. Not Classifying Errors Correctly
**Symptom:** Runtime retries on permanent errors, or gives up on transient errors

**Fix:** Review error classification. Network/connection errors should be retryable. Constraint violations should be permanent.

### 3. Not Updating as_management_capability Return Type
**Symptom:** Type mismatch errors at compile time

**Fix:** Change return type from `Option<&dyn ManagementCapability>` to `Option<&dyn ProviderAdmin>`

### 4. Keeping Methods in Wrong Trait
**Symptom:** Compile errors about missing trait implementations

**Fix:** Double-check which methods belong in `Provider` vs `ProviderAdmin` using the checklist above.

---

## Support

If you encounter issues during migration:

1. Check the SQLite provider implementation in `src/providers/sqlite.rs` as a reference
2. Run provider validation tests to catch issues early
3. Consult the main provider implementation guide in `docs/provider-implementation-guide.md`

---

**Last Updated:** November 2024
**Duroxide Version:** 0.1.x

