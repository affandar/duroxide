# Provider Migration Guide: Instance Creation Deferral

**Purpose:** Step-by-step instructions for updating an existing provider to defer instance creation from enqueue operations to runtime-driven creation via `ack_orchestration_item` metadata.

**Reference:** This guide documents the changes made to the SQLite provider. Use it as a template when updating other providers (PostgreSQL, MySQL, etc.).

---

## Overview of Changes

This migration implements two key changes:

1. **Schema Change**: `orchestration_version` column becomes nullable (TEXT instead of TEXT NOT NULL)
2. **Instance Creation Deferral**: Remove all instance creation from `enqueue_orchestrator_work_with_delay()` and move it to `ack_orchestration_item()` using `ExecutionMetadata`

---

## Step-by-Step Migration

### Step 1: Update Schema

**Change:** Make `orchestration_version` nullable in the `instances` table.

**SQLite:**
```sql
-- Old schema
CREATE TABLE instances (
    instance_id TEXT PRIMARY KEY,
    orchestration_name TEXT NOT NULL,
    orchestration_version TEXT NOT NULL,  -- ❌ Remove NOT NULL
    current_execution_id INTEGER NOT NULL DEFAULT 1,
    ...
);

-- New schema
CREATE TABLE instances (
    instance_id TEXT PRIMARY KEY,
    orchestration_name TEXT NOT NULL,
    orchestration_version TEXT,  -- ✅ Now nullable
    current_execution_id INTEGER NOT NULL DEFAULT 1,
    ...
);
```

**PostgreSQL:**
```sql
-- Migration SQL
ALTER TABLE instances ALTER COLUMN orchestration_version DROP NOT NULL;
```

**MySQL:**
```sql
-- Migration SQL
ALTER TABLE instances MODIFY orchestration_version TEXT NULL;
```

**Important:** For new providers, use the nullable schema from the start. For existing providers, create a migration script.

---

### Step 2: Remove Instance Creation from `enqueue_orchestrator_work_with_delay()`

**Find and remove:** All code that creates instances when enqueueing `StartOrchestration` work items.

**Before:**
```rust
async fn enqueue_orchestrator_work_with_delay(
    &self,
    item: WorkItem,
    delay_ms: Option<u64>,
) -> Result<(), String> {
    let instance = extract_instance(&item);
    
    // ❌ REMOVE THIS BLOCK
    if let WorkItem::StartOrchestration {
        orchestration,
        version,
        execution_id,
        ..
    } = &item
    {
        let version = version.as_deref().unwrap_or("1.0.0");
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO instances (instance_id, orchestration_name, orchestration_version)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(&instance)
        .bind(orchestration)
        .bind(version)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            INSERT OR IGNORE INTO executions (instance_id, execution_id)
            VALUES (?, ?)
            "#,
        )
        .bind(&instance)
        .bind(*execution_id as i64)
        .execute(&self.pool)
        .await?;
    }
    
    // Keep the rest of the function (enqueue logic)
    // ...
}
```

**After:**
```rust
async fn enqueue_orchestrator_work_with_delay(
    &self,
    item: WorkItem,
    delay_ms: Option<u64>,
) -> Result<(), String> {
    let instance = extract_instance(&item);
    
    // ✅ NO instance creation here - just enqueue the work item
    // Instance will be created by runtime via ack_orchestration_item metadata
    
    // Calculate visible_at and enqueue
    // ...
}
```

**Checklist:**
- [ ] Remove all `INSERT INTO instances` statements from `enqueue_orchestrator_work_with_delay()`
- [ ] Remove all `INSERT INTO executions` statements from `enqueue_orchestrator_work_with_delay()`
- [ ] Verify function only enqueues work items, doesn't create instances

---

### Step 3: Remove Instance Creation from `ack_orchestration_item()` Loop

**Find and remove:** All code that creates instances when enqueueing `StartOrchestration` work items in the `orchestrator_items` loop.

**Before:**
```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
) -> Result<(), String> {
    // ... lock validation, history append, etc ...
    
    // ❌ REMOVE THIS BLOCK
    for item in &orchestrator_items {
        if let WorkItem::StartOrchestration {
            instance,
            orchestration,
            version,
            execution_id,
            ..
        } = item
        {
            let version = version.as_deref().unwrap_or("1.0.0");
            sqlx::query(
                r#"
                INSERT OR IGNORE INTO instances (instance_id, orchestration_name, orchestration_version)
                VALUES (?, ?, ?)
                "#,
            )
            .bind(instance)
            .bind(orchestration)
            .bind(version)
            .execute(&mut *tx)
            .await?;
        }
    }
    
    // Keep the rest (enqueue orchestrator_items)
    // ...
}
```

**After:**
```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
) -> Result<(), String> {
    // ... lock validation, history append, etc ...
    
    // ✅ NO instance creation in loop - just enqueue work items
    // Sub-orchestrations will have their instances created when their first ack happens
    
    for item in &orchestrator_items {
        // Just enqueue - no instance creation
        // ...
    }
    
    // ...
}
```

**Checklist:**
- [ ] Remove all `INSERT INTO instances` statements from the `orchestrator_items` loop
- [ ] Verify loop only enqueues work items, doesn't create instances
- [ ] Check for similar patterns in `ContinueAsNew` handling

---

### Step 4: Add Instance Creation to `ack_orchestration_item()` Using Metadata

**Add:** Instance creation logic using `ExecutionMetadata` provided by the runtime.

**Location:** In `ack_orchestration_item()`, after lock validation, before history append.

**Pattern:**
```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
) -> Result<(), String> {
    // Step 1: Lock validation
    // ...
    
    // Step 2: Extract instance_id from lock
    // ...
    
    // Step 3: ✅ ADD INSTANCE CREATION HERE
    // Create or update instance metadata from runtime-provided metadata
    // Runtime resolves version from registry and provides it via metadata
    if let (Some(name), Some(version)) = (&metadata.orchestration_name, &metadata.orchestration_version) {
        // Step 3a: Ensure instance exists (creates if new, ignores if exists)
        // Use INSERT OR IGNORE to handle race conditions
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO instances 
            (instance_id, orchestration_name, orchestration_version, current_execution_id)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&instance_id)
        .bind(name)
        .bind(version.as_str())
        .bind(execution_id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        // Step 3b: Update instance with resolved version
        // This ensures version is always set correctly, even if instance was created in step 3a
        sqlx::query(
            r#"
            UPDATE instances 
            SET orchestration_name = ?, orchestration_version = ?
            WHERE instance_id = ?
            "#,
        )
        .bind(name)
        .bind(version)
        .bind(&instance_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    // If metadata is missing, skip instance creation
    // Instance will be created on next ack when metadata is available
    
    // Step 4: Continue with rest of ack logic (history append, etc.)
    // ...
}
```

**Checklist:**
- [ ] Add instance creation logic using `metadata.orchestration_name` and `metadata.orchestration_version`
- [ ] Use `INSERT OR IGNORE` pattern (or equivalent for your database) to handle race conditions
- [ ] Follow with `UPDATE` to ensure version is always set correctly
- [ ] Only create instance if both `orchestration_name` and `orchestration_version` are present in metadata

---

### Step 5: Update `fetch_orchestration_item()` to Handle NULL Versions

**Change:** Read `orchestration_version` as nullable and handle NULL values.

**Before:**
```rust
let version: String = info.try_get("orchestration_version").ok()?;
```

**After:**
```rust
let version: Option<String> = info.try_get("orchestration_version").ok();

// If version is NULL in database, use "unknown"
// If instance exists but version is NULL, there shouldn't be an OrchestrationStarted event
// in history (version should have been set when instance was created via metadata)
let version = version.unwrap_or_else(|| {
    debug_assert!(
        !hist.iter().any(|e| matches!(e, crate::Event::OrchestrationStarted { .. })),
        "Instance exists with NULL version but history contains OrchestrationStarted event"
    );
    "unknown".to_string()
});
```

**Important:** Don't try to extract version from history. If version is NULL, use "unknown". The debug assertion helps catch data inconsistencies during development.

**Checklist:**
- [ ] Change `orchestration_version` read to `Option<String>`
- [ ] Use `unwrap_or_else(|| "unknown".to_string())` to handle NULL
- [ ] Add debug assertion to catch inconsistencies (optional but recommended)

---

### Step 6: Update `fetch_orchestration_item()` Fallback Logic

**Change:** Update fallback logic to search ALL work items for `StartOrchestration` or `ContinueAsNew`, not just the first one.

**Before:**
```rust
} else if let Some(WorkItem::StartOrchestration {
    orchestration, version, ..
})
| Some(WorkItem::ContinueAsNew {
    orchestration, version, ..
}) = work_items.first()
{
    // Brand new instance - use work item
    let version = version.clone().unwrap_or_else(|| "1.0.0".to_string());
    // ...
}
```

**After:**
```rust
} else if let Some(start_item) = work_items.iter().find(|item| {
    matches!(item, WorkItem::StartOrchestration { .. } | WorkItem::ContinueAsNew { .. })
}) {
    // Brand new instance - find StartOrchestration or ContinueAsNew in work items
    // Version may be None - runtime will resolve and set via metadata
    let (orchestration, version) = match start_item {
        WorkItem::StartOrchestration { orchestration, version, .. }
        | WorkItem::ContinueAsNew { orchestration, version, .. } => {
            (orchestration.clone(), version.clone())
        }
        _ => unreachable!(),
    };
    let version = version.unwrap_or_else(|| "unknown".to_string());
    // ...
}
```

**Why:** External events might arrive before `StartOrchestration`, so we need to search all work items, not just check the first one.

**Checklist:**
- [ ] Change from `work_items.first()` to `work_items.iter().find(...)`
- [ ] Search for `StartOrchestration` OR `ContinueAsNew` in all work items
- [ ] Use `"unknown"` instead of `"1.0.0"` as fallback version

---

### Step 7: Update `get_instance_info()` to Handle NULL Versions

**Change:** Read `orchestration_version` as nullable and handle NULL values.

**Before:**
```rust
let orchestration_version: String = row.try_get("orchestration_version").ok()?;
```

**After:**
```rust
let orchestration_version: Option<String> = row.try_get("orchestration_version").ok();
let orchestration_version = orchestration_version.unwrap_or_else(|| {
    // Could fetch from history, but for management API, "unknown" is acceptable
    "unknown".to_string()
});
```

**Checklist:**
- [ ] Change `orchestration_version` read to `Option<String>`
- [ ] Use `unwrap_or_else(|| "unknown".to_string())` to handle NULL

---

### Step 8: Update Tests

**Update:** All tests that call `ack_orchestration_item()` to provide proper `ExecutionMetadata`.

**Before:**
```rust
store.ack_orchestration_item(
    &lock_token,
    execution_id,
    vec![],
    vec![],
    vec![],
    ExecutionMetadata::default(),  // ❌ Missing orchestration_name and version
)
.await?;
```

**After:**
```rust
store.ack_orchestration_item(
    &lock_token,
    execution_id,
    vec![Event::OrchestrationStarted {
        event_id: duroxide::INITIAL_EVENT_ID,
        name: "TestOrch".to_string(),
        version: "1.0.0".to_string(),
        input: "".to_string(),
        parent_instance: None,
        parent_id: None,
    }],
    vec![],
    vec![],
    ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        orchestration_version: Some("1.0.0".to_string()),
        ..Default::default()
    },
)
.await?;
```

**Checklist:**
- [ ] Find all `ack_orchestration_item()` calls in tests
- [ ] Update to provide `orchestration_name` and `orchestration_version` in `ExecutionMetadata`
- [ ] Ensure tests create instances via metadata, not on enqueue

---

## Verification Checklist

After completing the migration, verify:

- [ ] Schema: `orchestration_version` is nullable (TEXT, not TEXT NOT NULL)
- [ ] `enqueue_orchestrator_work_with_delay()`: No instance creation
- [ ] `ack_orchestration_item()` loop: No instance creation for sub-orchestrations
- [ ] `ack_orchestration_item()`: Instance creation using metadata (INSERT OR IGNORE + UPDATE pattern)
- [ ] `fetch_orchestration_item()`: Handles NULL versions, uses "unknown" fallback
- [ ] `fetch_orchestration_item()`: Searches all work items for StartOrchestration/ContinueAsNew
- [ ] `get_instance_info()`: Handles NULL versions, uses "unknown" fallback
- [ ] All tests pass with proper `ExecutionMetadata`
- [ ] Provider validation tests pass (especially instance creation tests)

---

## Testing

Run the provider validation tests to ensure everything works:

```bash
cargo test --features provider-test --test <your_provider>_provider_validations
```

**Key tests to verify:**
- `test_instance_creation_via_metadata` - Instances created via ack metadata
- `test_no_instance_creation_on_enqueue` - No instance created on enqueue
- `test_null_version_handling` - NULL version handled correctly
- `test_sub_orchestration_instance_creation` - Sub-orchestrations follow same pattern

---

## Common Issues

### Issue: Tests fail with "instance not found"

**Cause:** Tests are calling `ack_orchestration_item()` without providing `ExecutionMetadata` with `orchestration_name` and `orchestration_version`.

**Fix:** Update tests to provide proper metadata:
```rust
ExecutionMetadata {
    orchestration_name: Some("TestOrch".to_string()),
    orchestration_version: Some("1.0.0".to_string()),
    ..Default::default()
}
```

### Issue: `fetch_orchestration_item()` returns None when StartOrchestration exists

**Cause:** Only checking first work item, but external events might arrive first.

**Fix:** Search all work items:
```rust
work_items.iter().find(|item| {
    matches!(item, WorkItem::StartOrchestration { .. } | WorkItem::ContinueAsNew { .. })
})
```

### Issue: Version is NULL but instance exists

**Cause:** Instance was created before metadata was available (edge case).

**Fix:** Use "unknown" as fallback, don't try to extract from history. Add debug assertion to catch inconsistencies.

---

## Summary

This migration:
1. ✅ Makes `orchestration_version` nullable in schema
2. ✅ Removes instance creation from `enqueue_orchestrator_work_with_delay()`
3. ✅ Removes instance creation from `ack_orchestration_item()` loop
4. ✅ Adds instance creation to `ack_orchestration_item()` using metadata
5. ✅ Updates `fetch_orchestration_item()` to handle NULL versions
6. ✅ Updates `get_instance_info()` to handle NULL versions
7. ✅ Updates tests to provide proper metadata

**Result:** Instance creation is now runtime-driven via `ack_orchestration_item` metadata, providing better separation of concerns and consistent version resolution.

