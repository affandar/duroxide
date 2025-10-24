# DTF Persistence and Failure Analysis

## Executive Summary

The DTF orchestration runtime has several critical failure points in its persistence layer that can lead to inconsistent state, lost work, or system corruption. This analysis identifies these weaknesses and proposes improvements.

## Architecture Overview

### Orchestration Turn Lifecycle
1. **Prep**: Load history, prepare completion map
2. **Execute**: Run orchestration logic deterministically  
3. **Persist**: Append history delta, dispatch actions
4. **Acknowledge**: Mark processed messages as complete

### Key Components
- **History Store**: Persistent event log (source of truth)
- **Work Queues**: Three queues (Orchestrator, Worker, Timer)
- **Dispatchers**: Background tasks processing work items
- **Completion Map**: In-memory structure for deterministic replay

## Critical Failure Points

### 1. History Append Failures (CRITICAL)

**Location**: `replay_engine.rs:...`
```rust
history_store
    .append(&self.instance, self.history_delta.clone())
    .await
    .map_err(|e| format!("failed to append history: {}", e))?;
```

**Impact**: 
- Orchestration execution fails
- Turn is aborted
- Messages remain unacknowledged
- Orchestration will retry with same messages

**Current Behavior**: ✅ SAFE - Failure propagates, messages not acked

### 2. Decision Apply Failures (SEVERE WEAKNESS)

**Location**: `dispatch.rs` - All dispatch functions use `let _ =` pattern
```rust
let _ = rt
    .history_store
    .enqueue_work(QueueKind::Worker, WorkItem::ActivityExecute { ... })
    .await;
```

**Impact**:
- History is already persisted
- Actions are silently dropped
- Orchestration believes activity was scheduled
- Activity never executes
- **ORCHESTRATION HANGS FOREVER**

**Current Behavior**: ❌ UNSAFE - Failures ignored, state inconsistent

### 3. Terminal Event Append Failures

**Location**: `execution.rs` - turn handling
```rust
// Append terminal event (OrchestrationCompleted/Failed/ContinuedAsNew)
history_store.append(&instance, vec![terminal_event]).await
```

**Impact**:
- Orchestration completes internally
- Terminal event not persisted
- System believes orchestration still running
- Parent not notified (for sub-orchestrations)
- Waiters timeout

**Current Behavior**: ⚠️ PARTIALLY SAFE - Error logged but state inconsistent

### 4. Parent Notification Failures

**Location**: `execution.rs:293-303`
```rust
let _ = self
    .history_store
    .enqueue_work(
        QueueKind::Orchestrator,
        WorkItem::SubOrchCompleted { ... }
    )
    .await;
```

**Impact**:
- Sub-orchestration completes
- Parent never receives completion
- Parent hangs waiting for sub-orchestration

**Current Behavior**: ❌ UNSAFE - Failures ignored

### 5. Message Acknowledgment Failures

**Location**: `replay_engine.rs` - completion acknowledgment path (now handled atomically by runtime)
```rust
for token in &self.ack_tokens {
    if let Err(e) = history_store.ack(QueueKind::Orchestrator, token).await {
        warn!(instance = %self.instance, token = %token, error = %e, 
              "failed to acknowledge message");
    }
}
```

**Impact**:
- Messages processed successfully
- Acks fail
- Messages redelivered
- Duplicate processing (mitigated by execution ID validation)

**Current Behavior**: ⚠️ PARTIALLY SAFE - Logged but continues

## Replay Mechanism Analysis

### Strengths
1. **Deterministic Replay**: History is source of truth
2. **Completion Map**: Ensures deterministic ordering
3. **Execution ID Validation**: Prevents stale completions
4. **Idempotent Operations**: Most operations can be safely retried

### Weaknesses
1. **No Transactional Guarantees**: History append and decision dispatch are separate
2. **Silent Failures**: Critical failures in dispatch are ignored
3. **No Rollback Mechanism**: Once history is appended, cannot undo
4. **Queue Reliability**: Assumes queue operations always succeed

## Proposed Improvements

### 1. Two-Phase Commit Pattern
```rust
// Phase 1: Prepare all operations
let prepared_actions = prepare_actions(&decisions)?;
let prepared_history = prepare_history(&delta)?;

// Phase 2: Commit atomically or rollback
match atomic_commit(prepared_history, prepared_actions).await {
    Ok(_) => acknowledge_messages().await,
    Err(e) => {
        rollback(prepared_history, prepared_actions).await;
        return Err(e);
    }
}
```

### 2. Explicit Failure Handling in Dispatch
```rust
// Instead of: let _ = enqueue_work(...)
// Use:
if let Err(e) = rt.history_store.enqueue_work(...).await {
    // Append compensation event to history
    let compensation = Event::ActionDispatchFailed { 
        action_type: "activity",
        id,
        error: e.to_string()
    };
    history_store.append(&instance, vec![compensation]).await?;
    return Err(format!("Failed to dispatch activity: {}", e));
}
```

### 3. Saga Pattern for Complex Operations
```rust
struct OrchestrationSaga {
    steps: Vec<SagaStep>,
    compensations: Vec<CompensationAction>,
}

impl OrchestrationSaga {
    async fn execute(&mut self) -> Result<(), SagaError> {
        for (i, step) in self.steps.iter().enumerate() {
            if let Err(e) = step.execute().await {
                // Run compensations in reverse order
                for comp in self.compensations[0..i].iter().rev() {
                    comp.compensate().await;
                }
                return Err(e);
            }
        }
        Ok(())
    }
}
```

### 4. Introduce Write-Ahead Log (WAL)
```rust
// Before any operations
let wal_entry = WalEntry {
    instance,
    planned_history: delta.clone(),
    planned_actions: decisions.clone(),
};
wal.write(wal_entry).await?;

// Execute operations
execute_turn().await?;

// Mark WAL entry as committed
wal.commit(wal_entry.id).await?;
```

### 5. Health Checks and Recovery
```rust
// Periodic health check task
async fn health_check_orchestrations() {
    for instance in active_instances {
        let last_activity = get_last_activity_time(&instance);
        if last_activity > STUCK_THRESHOLD {
            // Check for missing dispatches
            let pending_actions = find_undispatched_actions(&instance);
            if !pending_actions.is_empty() {
                // Re-dispatch missing actions
                redispatch_actions(pending_actions).await;
            }
        }
    }
}
```

### 6. Transactional Queue Operations
```rust
// Batch all queue operations
let queue_batch = QueueBatch::new();
queue_batch.add(QueueKind::Worker, work_item1);
queue_batch.add(QueueKind::Timer, work_item2);

// Commit all or none
match history_store.enqueue_batch(queue_batch).await {
    Ok(_) => Ok(()),
    Err(e) => {
        // Append failure event to history
        append_failure_event(&instance, e).await?;
        Err(e)
    }
}
```

## Immediate Action Items

### High Priority (Fix Now)
1. **Fix dispatch failures**: Replace all `let _ =` with proper error handling
2. **Add retry logic**: Implement exponential backoff for queue operations
3. **Add monitoring**: Log and metric all failure points

### Medium Priority (Next Sprint)
1. **Implement WAL**: Add write-ahead logging for crash recovery
2. **Add health checks**: Detect and recover stuck orchestrations
3. **Improve error messages**: Provide actionable error context

### Low Priority (Future)
1. **Two-phase commit**: Implement full transactional semantics
2. **Saga pattern**: For complex multi-step operations
3. **Queue transactions**: Batch queue operations atomically

## Testing Recommendations

### Chaos Testing
```rust
#[test]
async fn test_history_append_failure_recovery() {
    // Inject failure after history append
    let store = FailingHistoryStore::fail_after_append();
    // Verify orchestration recovers correctly
}

#[test]
async fn test_dispatch_failure_detection() {
    // Inject queue failure
    let store = FailingHistoryStore::fail_queue_operations();
    // Verify failure is detected and handled
}
```

### Property-Based Testing
```rust
proptest! {
    #[test]
    fn orchestration_never_loses_work(
        failures in failure_injection_strategy()
    ) {
        // Run orchestration with injected failures
        // Verify all scheduled work eventually completes
    }
}
```

## Conclusion

The DTF orchestration runtime has a solid foundation with deterministic replay and execution ID validation. However, critical weaknesses exist in the persistence layer, particularly around silent failures in action dispatch. These issues can cause orchestrations to hang indefinitely.

The proposed improvements focus on:
1. **Explicit failure handling** - Never ignore errors
2. **Transactional semantics** - All or nothing operations
3. **Observability** - Monitor and alert on failures
4. **Recovery mechanisms** - Detect and fix stuck orchestrations

Implementing these changes will significantly improve the reliability and debuggability of the system.
