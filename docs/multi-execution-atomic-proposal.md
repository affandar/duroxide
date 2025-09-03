# Multi-Execution Support for Atomic Provider API

## Problem Statement

The current atomic provider API doesn't properly support multi-execution scenarios where `ContinueAsNew` creates new executions. This is causing the `continue_as_new_multiexec_fs` test to fail.

### Current Issues

1. **No execution ID tracking** - The `OrchestrationItem` doesn't include execution ID
2. **No new execution creation** - When processing `ContinueAsNew`, we don't create a new execution
3. **History mixing** - Events from different executions are being mixed together
4. **Lock token ambiguity** - Lock tokens don't distinguish between executions

## Proposed Changes

### 1. Update `OrchestrationItem` Structure

```rust
pub struct OrchestrationItem {
    pub instance: String,
    pub orchestration_name: String,
    pub execution_id: u64,      // Already exists
    pub version: String,
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
    pub is_new_execution: bool,  // NEW: Indicates if this is a fresh execution
}
```

### 2. Modify `fetch_orchestration_item` Behavior

When fetching an orchestration item:
- If a `ContinueAsNew` work item is present, create a new execution
- Set `is_new_execution = true` for new executions
- Return empty history for new executions (except the initial `OrchestrationStarted`)
- Include the correct execution ID

### 3. Update `ack_orchestration_item` for Multi-Execution

```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    timer_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
) -> Result<(), String>
```

When acking:
1. If `history_delta` contains `OrchestrationContinuedAsNew`:
   - Append it to the current execution's history
   - Create a new execution if the orchestrator_items contains a `ContinueAsNew` work item
2. Use execution-specific append methods (`append_with_execution`)

### 4. Provider Implementation Changes

#### FileSystem Provider
```rust
// In fetch_orchestration_item
if let WorkItem::ContinueAsNew { instance, orchestration, input, version } = &messages[0] {
    // Create new execution
    let new_exec_id = self.create_new_execution(
        &instance,
        &orchestration,
        &version.as_deref().unwrap_or("1.0.0"),
        &input,
        None, // No parent for ContinueAsNew
        None,
    ).await?;
    
    // Return fresh history for new execution
    return Some(OrchestrationItem {
        instance: instance.clone(),
        orchestration_name: orchestration.clone(),
        execution_id: new_exec_id,
        version: version.as_deref().unwrap_or("1.0.0").to_string(),
        history: vec![Event::OrchestrationStarted {
            name: orchestration.clone(),
            version: version.as_deref().unwrap_or("1.0.0").to_string(),
            input: input.clone(),
            parent_instance: None,
            parent_id: None,
        }],
        messages: vec![], // No completion messages for new execution
        lock_token: format!("{}-{}-{}", instance, new_exec_id, timestamp),
        is_new_execution: true,
    });
}
```

#### In-Memory Provider
Similar changes but using in-memory data structures to track executions.

### 5. Runtime Changes

#### In `handle_continue_as_new_atomic`
```rust
async fn handle_continue_as_new_atomic(
    self: &Arc<Self>,
    instance: &str,
    orchestration: &str,
    input: &str,
    version: Option<&str>,
    completion_messages: Vec<OrchestratorMsg>,
    existing_history: &[Event],
    _lock_token: &str,
) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>) {
    let mut history_delta = Vec::new();
    
    // Don't add OrchestrationStarted here - it will be added when the 
    // ContinueAsNew work item is processed in the next fetch
    
    // Just run the execution with empty history (new execution)
    let (exec_history_delta, exec_worker_items, exec_timer_items, exec_orchestrator_items, _result) = 
        self.clone()
            .run_single_execution_atomic(instance, orchestration, vec![], completion_messages)
            .await;
    
    // The execution should return ContinueAsNew turn result
    // which will add OrchestrationContinuedAsNew to history_delta
    history_delta.extend(exec_history_delta);
    
    (history_delta, exec_worker_items, exec_timer_items, exec_orchestrator_items)
}
```

### 6. Lock Token Format

Update lock tokens to include execution ID:
```
Format: {instance}-{execution_id}-{timestamp}
Example: "inst-can-1-2-1234567890"
```

This ensures locks are specific to an execution, not just an instance.

## Implementation Steps

1. **Update data structures** - Add `is_new_execution` to `OrchestrationItem`
2. **Modify providers** - Update `fetch_orchestration_item` to handle `ContinueAsNew` 
3. **Fix runtime** - Update `handle_continue_as_new_atomic` to work with new execution model
4. **Update tests** - Ensure multi-execution tests pass

## Benefits

1. **Proper isolation** - Each execution has its own history
2. **Correct semantics** - `ContinueAsNew` creates a true new execution
3. **Better debugging** - Clear separation between executions
4. **Consistency** - Aligns with existing multi-execution support in non-atomic API

## Testing

The `continue_as_new_multiexec_fs` test should pass with these changes, verifying:
- Each execution has its own history file
- Events are properly isolated between executions
- Execution IDs increment correctly
- Lock tokens are execution-specific
