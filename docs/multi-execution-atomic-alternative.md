# Alternative: Simplified Multi-Execution Support

## Simpler Approach

Instead of full multi-execution support in the atomic API, we could take a simpler approach that treats `ContinueAsNew` specially.

### Key Insight

`ContinueAsNew` is essentially:
1. Terminate current execution with `OrchestrationContinuedAsNew` event
2. Enqueue a new `StartOrchestration` work item (internally)

### Proposed Changes

#### 1. In `run_single_execution_atomic`

When `TurnResult::ContinueAsNew` is returned:
```rust
TurnResult::ContinueAsNew { input, version } => {
    // Add continue as new event to current execution
    let continued_event = Event::OrchestrationContinuedAsNew {
        input: input.clone(),
    };
    history_delta.push(continued_event);
    
    // Instead of enqueueing ContinueAsNew, enqueue StartOrchestration
    orchestrator_items.push(WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: orchestration_name.to_string(),
        input: input.clone(),
        version: version.clone(),
        parent_instance: None,  // No parent for ContinueAsNew
        parent_id: None,
    });
    
    Ok("continued as new".to_string())
}
```

#### 2. In `process_orchestration_item`

Remove special handling for `WorkItem::ContinueAsNew` since it won't be used anymore.

#### 3. In `ack_orchestration_item` (fs provider)

Check if we're creating a new execution:
```rust
// Check if we need to create a new execution
let needs_new_execution = orchestrator_items.iter().any(|item| 
    matches!(item, WorkItem::StartOrchestration { instance: i, .. } if i == instance)
);

if needs_new_execution {
    // Append the ContinuedAsNew event to close current execution
    if !history_delta.is_empty() {
        self.append_with_execution(instance, current_exec_id, history_delta).await?;
    }
    
    // Create new execution for the StartOrchestration
    if let Some(WorkItem::StartOrchestration { orchestration, input, version, .. }) = 
        orchestrator_items.iter().find(|item| 
            matches!(item, WorkItem::StartOrchestration { instance: i, .. } if i == instance)
        ) 
    {
        self.create_new_execution(
            instance,
            orchestration,
            version.as_deref().unwrap_or("1.0.0"),
            input,
            None,
            None,
        ).await?;
    }
    
    // Don't enqueue the StartOrchestration to self - it's already processed
    orchestrator_items.retain(|item| 
        !matches!(item, WorkItem::StartOrchestration { instance: i, .. } if i == instance)
    );
}
```

### Benefits

1. **Minimal changes** - Works within existing atomic API structure
2. **No new fields** - `OrchestrationItem` stays the same
3. **Reuses existing logic** - `StartOrchestration` handling already exists
4. **Clear semantics** - `ContinueAsNew` = terminate + start new

### Drawbacks

1. **Special casing** - Need to detect and handle self-targeted `StartOrchestration`
2. **Provider complexity** - Each provider needs this special logic
3. **Not fully atomic** - New execution creation happens outside the main atomic operation

## Recommendation

Start with this simpler approach to unblock the tests, then implement the full multi-execution support later if needed.
