# Remove Event Creation from Provider

## Problem

Providers currently create 2 types of events:

### 1. OrchestrationStarted (Line 1221, sqlite.rs)

**Location**: `create_new_execution` method

```rust
let start_event = Event::OrchestrationStarted {
    event_id: 0,
    name: orchestration.to_string(),
    version: version.to_string(),
    input: input.to_string(),
    parent_instance: parent_instance.map(|s| s.to_string()),
    parent_id,
};
self.append_history_in_tx(&mut tx, instance, next_exec_id as u64, vec![start_event])
```

**Called from**:
- Runtime `start_orchestration_with_parent` (runtime/mod.rs)
- Runtime `handle_continue_as_new_atomic` (runtime/mod.rs)

### 2. OrchestrationContinuedAsNew (Line 750, sqlite.rs)

**Location**: `ack_orchestration_item` method

```rust
vec![Event::OrchestrationContinuedAsNew { event_id: 0, input: can_input }]
```

**Called during**: Atomic ack when ContinueAsNew WorkItem is present

## Solution: Move Event Creation to Runtime

### Change 1: OrchestrationStarted

**Current Flow**:
1. Runtime calls `provider.create_new_execution(instance, orch, version, input, parent, parent_id)`
2. Provider creates OrchestrationStarted event
3. Provider writes to database

**New Flow**:
1. Runtime creates OrchestrationStarted event
2. Runtime calls `provider.create_new_execution_with_history(instance, exec_id, vec![start_event])`
3. Provider just writes the event

**Changes needed**:
- `create_new_execution` → Return just the execution_id, take events as parameter
- Runtime creates the OrchestrationStarted event before calling provider
- Already done in `runtime/mod.rs:488` and `runtime/mod.rs:555`!

### Change 2: OrchestrationContinuedAsNew

**Current Flow**:
1. Runtime produces TurnResult::ContinueAsNew
2. Runtime creates WorkItem::ContinueAsNew
3. Provider in `ack_orchestration_item` creates OrchestrationContinuedAsNew event
4. Provider appends it

**New Flow**:
1. Runtime produces TurnResult::ContinueAsNew
2. Runtime creates OrchestrationContinuedAsNew event
3. Runtime includes it in history_delta
4. Provider just persists the history_delta

**Changes needed**:
- Remove Event creation from provider `ack_orchestration_item` (line 734-753)
- Runtime should add OrchestrationContinuedAsNew to history_delta when TurnResult::ContinueAsNew is detected
- This should happen in `runtime/execution.rs` when handling TurnResult::ContinueAsNew

## Implementation Plan

### Step 1: Handle OrchestrationContinuedAsNew in Runtime

**File**: `src/runtime/execution.rs`

Current code at line 364:
```rust
TurnResult::ContinueAsNew { input, version } => {
    // For atomic execution, we don't add the ContinuedAsNew event here
    // It will be handled by the provider when transitioning executions

    // Enqueue continue as new work item
    orchestrator_items.push(WorkItem::ContinueAsNew {
        instance: instance.to_string(),
        orchestration: orchestration_name.to_string(),
        input: input.clone(),
        version: version.clone(),
    });

    Ok("continued as new".to_string())
}
```

**Change to**:
```rust
TurnResult::ContinueAsNew { input, version } => {
    // Add ContinuedAsNew terminal event to history
    let terminal_event = Event::OrchestrationContinuedAsNew {
        event_id: 0,  // Will be assigned by provider
        input: input.clone(),
    };
    history_delta.push(terminal_event);

    // Enqueue continue as new work item
    orchestrator_items.push(WorkItem::ContinueAsNew {
        instance: instance.to_string(),
        orchestration: orchestration_name.to_string(),
        input: input.clone(),
        version: version.clone(),
    });

    Ok("continued as new".to_string())
}
```

### Step 2: Remove Event Creation from Provider

**File**: `src/providers/sqlite.rs`

Remove lines 734-753:
```rust
// After appending history, check if we need to handle ContinueAsNew
let has_continue_as_new = orchestrator_items.iter().any(|item| matches!(item, WorkItem::ContinueAsNew { .. }));

if has_continue_as_new {
    // Handle ContinueAsNew transition
    // 1) Get the input from the ContinueAsNew work item
    let can_input = orchestrator_items.iter().find_map(|item| match item {
        WorkItem::ContinueAsNew { input, .. } => Some(input.clone()),
        _ => None,
    }).unwrap_or_default();
    
    // 2) Append ContinuedAsNew to current execution
    self.append_history_in_tx(
        &mut tx,
        &instance_id,
        execution_id as u64,
        vec![Event::OrchestrationContinuedAsNew { event_id: 0, input: can_input }],
    )
    .await
    .map_err(|e| format!("Failed to append CAN event: {}", e))?;
    
    // 3) Create next execution
    ...
}
```

**Replace with**:
```rust
// After appending history, check if we need to handle ContinueAsNew
let has_continue_as_new = orchestrator_items.iter().any(|item| matches!(item, WorkItem::ContinueAsNew { .. }));

if has_continue_as_new {
    // OrchestrationContinuedAsNew event should already be in history_delta from runtime
    // Just create the next execution
    
    let can_item = orchestrator_items.iter().find_map(|item| match item {
        WorkItem::ContinueAsNew { orchestration, input, version, .. } => {
            Some((orchestration.clone(), input.clone(), version.clone()))
        }
        _ => None,
    }).expect("ContinueAsNew work item must be present");
    
    // Create next execution
    ...
}
```

### Step 3: Refactor create_new_execution

**Current signature**:
```rust
async fn create_new_execution(
    &self,
    instance: &str,
    orchestration: &str,
    version: &str,
    input: &str,
    parent_instance: Option<&str>,
    parent_id: Option<u64>,
) -> Result<u64, String>
```

Creates OrchestrationStarted internally.

**New signature** (if we want to be pure):
```rust
async fn create_new_execution(
    &self,
    instance: &str,
    initial_events: Vec<Event>,  // Runtime provides the OrchestrationStarted
) -> Result<u64, String>
```

**However**, this is tricky because:
- We need orchestration name/version for the instances table
- OrchestrationStarted contains this info
- We'd need to extract it from the event

**Compromise**: Keep `create_new_execution` as is for now since it's called from runtime code that already creates the event. The provider just writes what it's given.

## Summary

**Provider Event Creation**:
1. ✅ OrchestrationStarted - Actually already created by runtime! Provider just needs refactoring.
2. ❌ OrchestrationContinuedAsNew - Currently created by provider, needs to move to runtime

**Action Items**:
1. Add OrchestrationContinuedAsNew to history_delta in execution.rs
2. Remove OrchestrationContinuedAsNew creation from sqlite.rs ack method
3. (Optional) Refactor create_new_execution to be clearer about event ownership

## Benefits

- Clear separation: Runtime creates events, Provider stores them
- Easier to reason about event sourcing
- Provider becomes pure persistence layer
- Event_id assignment can happen in runtime before persistence

## Estimated Work

- 30 minutes to 1 hour
- Low risk (ContinueAsNew is well-tested)
