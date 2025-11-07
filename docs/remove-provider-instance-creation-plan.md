# Plan: Remove Instance Creation from Provider Enqueue Operations

## Overview

This document outlines the plan to remove all orchestration instance creation from provider enqueue operations (`enqueue_orchestrator_work_with_delay` and the orchestrator_items loop in `ack_orchestration_item`). Instead, instance creation will be handled exclusively by the runtime through `ack_orchestration_item` using `ExecutionMetadata`.

## Problem Statement

Currently, the SQLite provider creates orchestration instances in two places:
1. **`enqueue_orchestrator_work_with_delay`** (line 55-88): Creates instance when enqueueing `StartOrchestration` work items (called by client)
2. **`ack_orchestration_item` loop** (line 853-882): Creates instance when enqueueing `StartOrchestration` work items (sub-orchestrations)

This creates several issues:
- **Redundant creation**: Instances are created twice (on enqueue and on ack)
- **Version defaulting**: Provider defaults to "1.0.0" instead of letting runtime resolve version
- **Race conditions**: Instance exists before runtime has resolved the correct version
- **Separation of concerns**: Provider makes orchestration-level decisions (version resolution)

## Desired State

- **Single source of truth**: Runtime resolves version from registry and provides it via `ExecutionMetadata`
- **No premature creation**: Instances are created only when runtime acknowledges the first turn
- **NULL version support**: Version starts as NULL, then gets set by runtime metadata
- **Clean separation**: Provider is pure storage, runtime owns orchestration semantics

## Current Flow

### Current Flow (Before Changes)

```
1. Client calls start_orchestration()
   └─> enqueue_orchestrator_work()
       └─> enqueue_orchestrator_work_with_delay()
           └─> Creates instance with version="1.0.0" (or provided version)
           └─> Enqueues StartOrchestration work item

2. Runtime fetches work item
   └─> fetch_orchestration_item()
       └─> Reads instance (already exists)
       └─> Extracts version from instance or work item

3. Runtime processes
   └─> Resolves version from registry
   └─> Creates OrchestrationStarted event with resolved version
   └─> Computes ExecutionMetadata with name+version

4. Runtime acks
   └─> ack_orchestration_item()
       └─> UPDATE instances SET version=? (assumes instance exists)
       └─> Loop enqueues orchestrator_items and creates instances for sub-orchestrations
```

### Desired Flow (After Changes)

```
1. Client calls start_orchestration()
   └─> enqueue_orchestrator_work()
       └─> enqueue_orchestrator_work_with_delay()
           └─> Enqueues StartOrchestration work item (NO instance creation)

2. Runtime fetches work item
   └─> fetch_orchestration_item()
       └─> Instance doesn't exist yet
       └─> Extracts name/version from work item (or history if exists)
       └─> Returns OrchestrationItem with extracted metadata

3. Runtime processes
   └─> Resolves version from registry
   └─> Creates OrchestrationStarted event with resolved version
   └─> Computes ExecutionMetadata with name+version

4. Runtime acks
   └─> ack_orchestration_item()
       └─> INSERT OR IGNORE instances (creates if doesn't exist, NULL version)
       └─> UPDATE instances SET name=?, version=? (sets resolved version)
       └─> Loop enqueues orchestrator_items (NO instance creation)
```

## Detailed Changes

### 1. Schema Changes

**File**: `src/providers/sqlite.rs` - `create_schema()`

**Change**: Make `orchestration_version` nullable

```rust
// Line 246: Change from
orchestration_version TEXT NOT NULL,
// To
orchestration_version TEXT,
```

**Rationale**: Version will be NULL initially, then set by runtime metadata.

---

### 2. Migration for Existing Databases

**Note**: No migration needed - we're only changing the initial schema. Existing databases will continue to work with the NOT NULL constraint until they're recreated.

---

### 3. Remove Instance Creation from `enqueue_orchestrator_work_with_delay`

**File**: `src/providers/sqlite.rs` - `enqueue_orchestrator_work_with_delay()`

**Lines**: 55-88

**Current Code**:
```rust
// Check if this is a StartOrchestration - if so, create instance
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
    .bind(instance)
    .bind(orchestration)
    .bind(version)
    .execute(&self.pool)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        r#"
        INSERT OR IGNORE INTO executions (instance_id, execution_id)
        VALUES (?, ?)
        "#,
    )
    .bind(instance)
    .bind(*execution_id as i64)
    .execute(&self.pool)
    .await
    .map_err(|e| e.to_string())?;
}
```

**New Code**:
```rust
// Remove entire block - just enqueue the work item
// Instance will be created by runtime via ack_orchestration_item metadata
```

**Rationale**: Let runtime handle instance creation through metadata.

---

### 4. Remove Instance Creation from `ack_orchestration_item` Loop

**File**: `src/providers/sqlite.rs` - `ack_orchestration_item()` (orchestrator_items loop)

**Lines**: 853-882

**Current Code**:
```rust
// Check if this is a StartOrchestration - if so, create instance
if let WorkItem::StartOrchestration {
    orchestration, version, ..
} = &item
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
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        r#"
        INSERT OR IGNORE INTO executions (instance_id, execution_id)
        VALUES (?, 1)
        "#,
    )
    .bind(instance)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
}
```

**New Code**:
```rust
// Remove entire block - just enqueue the work item
// Instance will be created by runtime when that work item is processed
```

**Rationale**: Sub-orchestrations should follow the same pattern - instance created on their first ack.

---

### 5. Update `ack_orchestration_item` to Create Instance from Metadata

**File**: `src/providers/sqlite.rs` - `ack_orchestration_item()`

**Lines**: 739-754

**Current Code**:
```rust
// Update instance metadata from runtime-provided metadata (no event inspection)
if let (Some(name), Some(version)) = (&metadata.orchestration_name, &metadata.orchestration_version) {
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
```

**New Code**:
```rust
// Create or update instance metadata from runtime-provided metadata
if let (Some(name), Some(version)) = (&metadata.orchestration_name, &metadata.orchestration_version) {
    // First, ensure instance exists (INSERT OR IGNORE if new)
    sqlx::query(
        r#"
        INSERT OR IGNORE INTO instances 
        (instance_id, orchestration_name, orchestration_version, current_execution_id)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(&instance_id)
    .bind(name)
    .bind(version.as_deref()) // NULL if version is None (shouldn't happen, but safe)
    .bind(execution_id as i64)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Then update with resolved version (will update if instance exists, no-op if just created)
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
```

**Rationale**: 
- Creates instance if it doesn't exist (first ack)
- Updates instance with resolved version from runtime
- Atomic within the same transaction

---

### 6. Handle NULL Version in `fetch_orchestration_item`

**File**: `src/providers/sqlite.rs` - `fetch_orchestration_item()`

**Lines**: 637-638, 672

**Current Code**:
```rust
let name: String = info.try_get("orchestration_name").ok()?;
let version: String = info.try_get("orchestration_version").ok()?;
// ...
} else if let Some(WorkItem::StartOrchestration {
    orchestration, version, ..
})
| Some(WorkItem::ContinueAsNew {
    orchestration, version, ..
}) = work_items.first()
{
    // Brand new instance - use work item
    (
        orchestration.clone(),
        version.clone().unwrap_or_else(|| "1.0.0".to_string()),
        1u64,
        Vec::new(),
    )
}
```

**New Code**:
```rust
let name: String = info.try_get("orchestration_name").ok()?;
let version: Option<String> = info.try_get("orchestration_version").ok();
// Extract from history if NULL
let version = version.or_else(|| {
    hist.iter().find_map(|e| {
        if let crate::Event::OrchestrationStarted { version, .. } = e {
            Some(version.clone())
        } else {
            None
        }
    })
}).unwrap_or_else(|| "unknown".to_string()); // Fallback only if no history
// ...
} else if let Some(WorkItem::StartOrchestration {
    orchestration, version, ..
})
| Some(WorkItem::ContinueAsNew {
    orchestration, version, ..
}) = work_items.first()
{
    // Brand new instance - use work item (version may be None, that's OK)
    (
        orchestration.clone(),
        version.clone().unwrap_or_else(|| "unknown".to_string()), // Runtime will resolve
        1u64,
        Vec::new(),
    )
}
```

**Rationale**: 
- Handle NULL version from database
- Extract from history if available
- Use "unknown" as fallback (runtime will resolve and update)

---

### 7. Update `get_instance_info` to Handle NULL Version

**File**: `src/providers/sqlite.rs` - `get_instance_info()`

**Lines**: 1344

**Current Code**:
```rust
let orchestration_version: String = row.try_get("orchestration_version").map_err(|e| e.to_string())?;
```

**New Code**:
```rust
let orchestration_version: Option<String> = row.try_get("orchestration_version").ok();
let orchestration_version = orchestration_version.unwrap_or_else(|| {
    // Could fetch from history, but for management API, "unknown" is acceptable
    "unknown".to_string()
});
```

**Rationale**: Management API should handle NULL gracefully. Could fetch from history, but "unknown" is acceptable for display.

---

### 8. Update Documentation Comments

**File**: `src/providers/mod.rs`

**Lines**: 172, 424

**Current**:
```rust
/// - `version`: None means use provider default (e.g., "1.0.0")
```

**New**:
```rust
/// - `version`: None means runtime will resolve from registry
```

**File**: `src/providers/mod.rs` - Schema comment

**Line**: 424

**Current**:
```rust
///     orchestration_version TEXT NOT NULL,
```

**New**:
```rust
///     orchestration_version TEXT,  -- NULLable, set by runtime via metadata
```

---

## ContinueAsNew Handling

ContinueAsNew work items are for **existing instances**, so no instance creation is needed:

- ContinueAsNew is enqueued for an instance that already exists
- Runtime processes ContinueAsNew and creates a new execution
- Runtime creates `OrchestrationStarted` event for the new execution
- Runtime acks with metadata containing name+version
- Provider updates instance metadata (already exists, just updates)

**No changes needed** for ContinueAsNew - it already works correctly.

---

## Testing Strategy

### Unit Tests

1. **Test instance creation on first ack**
   - Enqueue StartOrchestration (no instance created)
   - Fetch work item via `fetch_orchestration_item` (instance doesn't exist, extracts from work item)
   - Ack with metadata (instance created with resolved version)

2. **Test NULL version handling**
   - Create instance with NULL version
   - Fetch work item via `fetch_orchestration_item` (extracts version from history)
   - Verify version is set after ack

3. **Test sub-orchestration flow**
   - Parent orchestration enqueues StartOrchestration for child
   - Child instance not created on enqueue
   - Child instance created on first ack

4. **Test ContinueAsNew**
   - Existing instance continues as new
   - Instance already exists, just updates metadata

### Integration Tests

1. **End-to-end flow**
   - Client starts orchestration
   - Verify instance created only after first ack
   - Verify version resolved from registry

2. **Migration test**
   - Create database with old schema
   - Run migration
   - Verify instances table has nullable version
   - Verify existing instances still work

### Edge Cases

1. **Instance doesn't exist on ack**
   - Should create instance from metadata

2. **Metadata missing name/version**
   - Should not create/update instance
   - Should still process other ack operations

3. **Multiple acks before instance creation**
   - Should be impossible (first ack creates instance)
   - But handle gracefully if it happens

---

## Migration Plan

### Phase 1: Schema Changes
1. Update `create_schema()` to use nullable version column
2. Test schema creation on new databases
3. Verify nullable column works

### Phase 2: Code Changes
1. Remove instance creation from `enqueue_orchestrator_work_with_delay`
2. Remove instance creation from `ack_orchestration_item` loop
3. Update `ack_orchestration_item` to create from metadata
4. Update `fetch_orchestration_item` to handle NULL version
5. Update `get_instance_info` to handle NULL version

### Phase 3: Testing
1. Run existing test suite
2. Add new tests for NULL version handling
3. Test on fresh databases (new schema)

### Phase 4: Documentation
1. Update code comments
2. Update provider implementation guide if needed

---

## Rollback Plan

If issues arise:

1. **Code rollback**: Revert to creating instances on enqueue
2. **Schema rollback**: Change schema back to NOT NULL (only affects new databases)
3. **Note**: Existing databases with NOT NULL constraint will continue to work

---

## Benefits

1. **Single source of truth**: Runtime resolves version, provider stores it
2. **No race conditions**: Instance created atomically with first history append
3. **Cleaner separation**: Provider is pure storage, runtime owns semantics
4. **NULL version support**: Version starts NULL, set by runtime
5. **Consistent behavior**: All instance creation follows same pattern

---

## Risks and Mitigations

### Risk 1: Instance doesn't exist when expected
**Mitigation**: `fetch_orchestration_item` already handles missing instances by extracting from work items

### Risk 2: NULL version causes issues
**Mitigation**: Runtime always provides version in metadata for new instances

### Risk 3: Existing databases have NOT NULL constraint
**Mitigation**: Existing databases will continue to work. Only new databases will have nullable version. This is acceptable as existing instances already have versions set.

### Risk 4: Performance impact
**Mitigation**: INSERT OR IGNORE + UPDATE is efficient, no additional queries

---

## Success Criteria

- [ ] No instance creation in `enqueue_orchestrator_work_with_delay`
- [ ] No instance creation in `ack_orchestration_item` loop
- [ ] Instance created in `ack_orchestration_item` from metadata
- [ ] NULL version supported throughout
- [ ] All tests pass
- [ ] Schema creation works correctly for new databases
- [ ] Documentation updated

---

## Timeline Estimate

- **Schema changes**: 30 minutes
- **Code changes**: 2-3 hours
- **Testing**: 2-3 hours
- **Documentation**: 1 hour
- **Total**: ~5-7 hours

---

## Related Documents

- `docs/provider-implementation-guide.md` - Provider responsibilities
- `docs/architecture.md` - Overall architecture
- `src/providers/mod.rs` - Provider trait definitions

