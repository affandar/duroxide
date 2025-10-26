# Provider Correctness Test Plan

**Purpose:** Define comprehensive correctness tests for Duroxide providers operating at the provider interface layer (not SQLite-specific).

**Target:** Any provider implementing the `Provider` trait.

---

## Overview

This test plan validates provider correctness across six critical dimensions:

1. **Instance Locking** - Preventing concurrent processing of the same instance
2. **Atomicity** - Ensuring all-or-nothing transaction semantics
3. **Error Handling** - Proper handling of invalid inputs and corrupted state
4. **Lock Expiration** - Automatic redelivery after lock timeout
5. **Queue Semantics** - Correct peek-lock behavior for all queues
6. **Multi-Execution Support** - Proper isolation and lifecycle management

### Critical Race Condition Tests

Three critical tests address race conditions when completions arrive during locks:

- **Test 1.5: Message Arrival During Lock** - Completions arriving while an instance is locked are blocked from being fetched by other dispatchers until the lock is released
- **Test 1.6: Cross-Instance Lock Isolation** - Locks on one instance don't block processing of other instances
- **Test 1.7: Message Tagging During Lock** - Only messages present at fetch time are deleted on ack; new messages arriving during processing remain in queue

---

## Test Categories

### 1. Instance Locking Tests (7 tests)

#### Test 1.1: Exclusive Instance Lock Acquisition
**Goal:** Verify only one dispatcher can process an instance at a time.

**Steps:**
1. Create provider
2. Enqueue work for instance "A"
3. Fetch orchestration item (acquires lock)
4. Attempt to fetch again immediately → should return None
5. Verify the lock_token from first fetch is different from second attempt

**Expected:** Second fetch returns None (instance is locked)

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_exclusive_instance_lock() {
    let provider = create_provider().await;
    
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    let lock_token1 = item1.lock_token.clone();
    
    // Second fetch should fail (instance locked)
    assert!(provider.fetch_orchestration_item().await.is_none());
    
    // Wait for lock to expire
    tokio::time::sleep(LOCK_TIMEOUT + 100ms).await;
    
    // Now should be able to fetch again
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_ne!(item2.lock_token, lock_token1);
}
```

#### Test 1.2: Lock Token Uniqueness
**Goal:** Ensure each fetch generates a unique lock token.

**Steps:**
1. Create provider
2. Enqueue multiple work items
3. Fetch multiple orchestration items (different instances)
4. Verify all lock tokens are unique

**Expected:** All lock tokens are unique across instances

#### Test 1.3: Invalid Lock Token Rejection
**Goal:** Verify ack/abandon reject invalid lock tokens.

**Steps:**
1. Create provider
2. Enqueue and fetch an item
3. Try to ack with invalid token → should error
4. Try to abandon with invalid token → should error

**Expected:** Both operations return error

#### Test 1.4: Concurrent Fetch Attempts
**Goal:** Test provider under concurrent access from multiple dispatchers.

**Steps:**
1. Create provider
2. Enqueue work for 10 different instances
3. Spawn 10 concurrent tasks calling `fetch_orchestration_item()`
4. Collect results
5. Verify no instance is fetched twice
6. Verify exactly 10 different instances were fetched

**Expected:** Each instance fetched exactly once, no duplicates

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_concurrent_instance_fetching() {
    let provider = Arc::new(create_provider().await);
    
    // Seed 10 instances
    for i in 0..10 {
        provider.enqueue_orchestrator_work(start_item(&format!("inst-{}", i)), None).await.unwrap();
    }
    
    // Fetch concurrently
    let handles: Vec<_> = (0..10).map(|_| {
        let p = provider.clone();
        tokio::spawn(async move {
            p.fetch_orchestration_item().await
        })
    }).collect();
    
    let results: Vec<_> = futures::future::join_all(handles).await
        .into_iter()
        .collect::<Result<Vec<_>, _>>().unwrap();
    
    // Verify no duplicates
    let instances: HashSet<_> = results.iter()
        .filter_map(|r| r.as_ref())
        .map(|item| item.instance.clone())
        .collect();
    
    assert_eq!(instances.len(), 10);
}
```

#### Test 1.5: Message Arrival During Lock (Critical)
**Goal:** Verify completions arriving during a lock cannot be fetched by other dispatchers.

**Steps:**
1. Create provider
2. Enqueue and fetch instance "A" (acquires lock)
3. While instance "A" is locked, enqueue completions for "A"
4. Spawn dispatcher task attempting to fetch "A"
5. Verify fetch returns None (instance still locked)
6. Wait for lock expiration
7. Fetch again → should now see the completions that arrived during lock

**Expected:** Instance-level lock prevents fetching completions that arrive mid-processing

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_completions_arriving_during_lock_blocked() {
    let provider = Arc::new(create_provider().await);
    
    // Step 1: Enqueue initial work
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    
    // Step 2: Fetch and acquire lock
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item1.instance, "instance-A");
    let lock_token = item1.lock_token.clone();
    
    // Step 3: While locked, completions arrive
    for i in 1..=3 {
        provider.enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: i,
                result: format!("result-{}", i),
            },
            None,
        ).await.unwrap();
    }
    
    // Step 4: Another dispatcher tries to fetch "instance-A"
    let item2 = provider.fetch_orchestration_item().await;
    assert!(item2.is_none(), "Instance still locked, no fetch possible");
    
    // Step 5: Wait for lock expiration
    tokio::time::sleep(LOCK_TIMEOUT + 100ms).await;
    
    // Step 6: Now completions should be fetchable
    let item3 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item3.instance, "instance-A");
    assert_eq!(item3.messages.len(), 3, "All 3 completions should be fetched");
    
    // Verify they're the completions that arrived during lock
    for (i, msg) in item3.messages.iter().enumerate() {
        assert!(matches!(msg, WorkItem::ActivityCompleted { id, .. } if *id == (i+1) as u64));
    }
}
```

#### Test 1.6: Cross-Instance Lock Isolation
**Goal:** Verify locks on one instance don't block other instances.

**Steps:**
1. Create provider
2. Enqueue work for instance "A" and "B"
3. Fetch instance "A" (acquires lock for A)
4. Verify can still fetch instance "B" (different instance, not blocked)
5. Enqueue completion for "B" while "A" is locked
6. Fetch "B" → should succeed (B is not locked)

**Expected:** Locks are per-instance, not global

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_cross_instance_lock_isolation() {
    let provider = Arc::new(create_provider().await);
    
    // Enqueue work for two different instances
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    provider.enqueue_orchestrator_work(start_item("instance-B"), None).await.unwrap();
    
    // Lock instance A
    let item_a = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item_a.instance, "instance-A");
    
    // Should still be able to fetch instance B (different instance, not blocked)
    let item_b = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item_b.instance, "instance-B");
    
    // Ack B to release its lock, then enqueue another completion for B
    provider.ack_orchestration_item(
        &item_b.lock_token,
        1,
        vec![],
        vec![],
        vec![],
        vec![],
        ExecutionMetadata::default(),
    ).await.unwrap();
    
    // While A is still locked, completion arrives for B
    provider.enqueue_orchestrator_work(
        WorkItem::ActivityCompleted {
            instance: "instance-B".to_string(),
            execution_id: 1,
            id: 1,
            result: "done".to_string(),
        },
        None,
    ).await.unwrap();
    
    // Should be able to fetch B again (B is not locked)
    let item_b2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item_b2.instance, "instance-B");
    
    // Key assertion: instance-level locks don't block other instances
    assert_ne!(item_a.instance, item_b.instance);
}
```

#### Test 1.7: Completing Messages During Lock (Message Tagging)
**Goal:** Verify only messages present at fetch time are deleted on ack.

**Steps:**
1. Create provider
2. Enqueue messages M1, M2 for instance "A"
3. Fetch "A" → receives M1, M2 (with lock_token "abc")
4. While processing, M3 arrives for "A" (no lock_token)
5. Ack with lock_token "abc"
6. Verify M1, M2 deleted, M3 still in queue
7. Fetch again → should receive M3

**Expected:** Only messages marked with lock_token are deleted

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_message_tagging_during_lock() {
    let provider = Arc::new(create_provider().await);
    
    // Enqueue initial messages
    provider.enqueue_orchestrator_work(
        WorkItem::ActivityCompleted {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 1,
            result: "msg1".to_string(),
        },
        None,
    ).await.unwrap();
    
    provider.enqueue_orchestrator_work(
        WorkItem::ActivityCompleted {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 2,
            result: "msg2".to_string(),
        },
        None,
    ).await.unwrap();
    
    // Fetch (marks messages with lock_token)
    let item = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item.instance, "instance-A");
    assert_eq!(item.messages.len(), 2);
    let lock_token = item.lock_token.clone();
    
    // While locked, new message arrives
    provider.enqueue_orchestrator_work(
        WorkItem::ActivityCompleted {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 3,
            result: "msg3".to_string(),
        },
        None,
    ).await.unwrap();
    
    // Ack (deletes only messages with lock_token)
    provider.ack_orchestration_item(
        &lock_token,
        1,
        vec![],
        vec![],
        vec![],
        vec![],
        ExecutionMetadata::default(),
    ).await.unwrap();
    
    // Fetch again - should get msg3
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
    assert_eq!(item2.messages.len(), 1);
    assert!(matches!(&item2.messages[0], WorkItem::ActivityCompleted { id: 3, .. }));
}
```

---

### 2. Atomicity Tests

#### Test 2.1: All-or-Nothing Ack
**Goal:** Verify `ack_orchestration_item` commits everything atomically.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Prepare ack with history, worker items, timer items, orchestrator items
4. Intentionally corrupt the ack (e.g., duplicate event_id, invalid data)
5. Attempt ack → should fail
6. Verify NO changes were persisted (history, queues remain unchanged)

**Expected:** Transaction rolled back, no partial state

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_atomicity_failure_rollback() {
    let provider = create_provider().await;
    
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();
    
    // Verify initial state
    let initial_history = provider.read(&item.instance).await;
    let initial_depth = provider.get_queue_depths().await.unwrap();
    
    // Try to ack with invalid data that should fail
    let result = provider.ack_orchestration_item(
        &lock_token,
        1,
        vec![/* corrupt event */],
        vec![],
        vec![],
        vec![],
        ExecutionMetadata::default(),
    ).await;
    
    assert!(result.is_err());
    
    // Verify state unchanged
    let after_history = provider.read(&item.instance).await;
    let after_depth = provider.get_queue_depths().await.unwrap();
    
    assert_eq!(initial_history.len(), after_history.len());
    assert_eq!(initial_depth.orchestrator_queue, after_depth.orchestrator_queue);
}
```

#### Test 2.2: Multi-Operation Atomic Ack
**Goal:** Verify complex ack with multiple outputs succeeds atomically.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Ack with:
   - 5 history events
   - 3 worker items
   - 2 timer items
   - 2 orchestrator items
4. Verify ALL operations succeeded together

**Expected:** All 12 operations committed atomically

#### Test 2.3: Lock Released Only on Successful Ack
**Goal:** Ensure lock is only released after successful commit.

**Steps:**
1. Create provider
2. Fetch orchestration item (acquires lock)
3. Attempt ack with invalid data (should fail)
4. Verify lock is still held
5. Wait for lock expiration
6. Verify instance becomes available again

**Expected:** Lock remains until successful ack or expiration

#### Test 2.4: Concurrent Ack Prevention
**Goal:** Ensure two acks with same lock token don't both succeed.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Spawn two concurrent tasks trying to ack with same lock token
4. Only one should succeed
5. Other should fail with "lock expired" or similar

**Expected:** Exactly one ack succeeds

---

### 3. Error Handling Tests

#### Test 3.1: Invalid Lock Token on Ack
**Goal:** Provider should reject invalid lock tokens.

**Steps:**
1. Create provider
2. Attempt to ack with non-existent lock token
3. Verify error returned

**Expected:** Error with message indicating invalid token

#### Test 3.2: Duplicate Event ID Handling
**Goal:** Provider should detect and handle duplicate event_ids.

**Steps:**
1. Create provider with existing history (event_id 1, 2, 3)
2. Attempt to append event with event_id=2 (duplicate)
3. Verify provider either:
   - Rejects with error (preferred), OR
   - Ignores idempotently (INSERT OR IGNORE pattern)

**Expected:** Duplicate rejected or ignored, never overwrites

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_duplicate_event_id_rejection() {
    let provider = create_provider().await;
    
    // Append initial event
    provider.append_with_execution("instance", 1, vec![Event::OrchestrationStarted {
        event_id: 1,
        name: "Test".to_string(),
        version: "1.0.0".to_string(),
        input: "{}".to_string(),
        parent_instance: None,
        parent_id: None,
    }]).await.unwrap();
    
    // Try to append duplicate event_id
    let result = provider.append_with_execution("instance", 1, vec![Event::ActivityScheduled {
        event_id: 1, // DUPLICATE!
        name: "Activity".to_string(),
        input: "{}".to_string(),
        execution_id: 1,
    }]).await;
    
    // Should either error or idempotently ignore
    if result.is_ok() {
        // Verify idempotent: history unchanged
        let history = provider.read("instance").await;
        assert_eq!(history.len(), 1);
        assert!(matches!(history[0], Event::OrchestrationStarted { .. }));
    } else {
        // Verify error message mentions duplicate
        assert!(result.unwrap_err().contains("duplicate") || 
                result.unwrap_err().contains("event_id"));
    }
}
```

#### Test 3.3: Missing Instance Metadata
**Goal:** Provider should handle missing instance gracefully.

**Steps:**
1. Create provider
2. Attempt to read history for non-existent instance
3. Verify returns empty Vec (not error)

**Expected:** Empty Vec returned

#### Test 3.4: Corrupted Serialization Data
**Goal:** Provider should handle corrupted JSON in queue/work items.

**Steps:**
1. Manually insert corrupted data into provider's storage
2. Attempt to fetch orchestration item
3. Verify provider either:
   - Returns None (can't deserialize), OR
   - Handles gracefully and skips corrupted item

**Expected:** No panic, graceful degradation

#### Test 3.5: Lock Expiration During Ack
**Goal:** Provider should detect and reject expired locks.

**Steps:**
1. Create provider with short lock timeout
2. Fetch orchestration item
3. Wait for lock to expire
4. Attempt to ack
5. Verify error indicating lock expired

**Expected:** Ack fails with "lock expired" error

---

### 4. Lock Expiration and Redelivery Tests

#### Test 4.1: Basic Lock Expiration
**Goal:** Verify messages become available after lock timeout.

**Steps:**
1. Create provider with lock timeout = 2s
2. Enqueue work item
3. Fetch (acquires lock)
4. Don't ack, wait 2.1s
5. Verify item can be fetched again

**Expected:** Redelivered after timeout

#### Test 4.2: Lock Refresh Prevention
**Goal:** Verify locks cannot be refreshed/extended after initial acquisition.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Attempt to "refresh" lock (is this supported?)
4. Verify behavior matches specification

**Expected:** Behavior matches Provider trait spec

#### Test 4.3: Expired Lock Token Reuse
**Goal:** Ensure expired lock tokens cannot be reused.

**Steps:**
1. Create provider
2. Fetch and record lock token
3. Wait for expiration
4. Try to ack with expired token
5. Verify error

**Expected:** Ack fails with "lock expired"

#### Test 4.4: Abandon Releases Lock Immediately
**Goal:** Verify abandon makes items available immediately.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Abandon with no delay
4. Immediately fetch again
5. Verify same item fetched

**Expected:** Available immediately after abandon

---

### 5. Queue Semantics Tests

#### Test 5.1: Worker Queue FIFO Ordering
**Goal:** Verify worker items dequeued in order (if supported).

**Steps:**
1. Create provider
2. Enqueue 5 worker items
3. Dequeue all 5
4. Verify order matches enqueue order

**Expected:** FIFO ordering maintained

**Note:** FIFO not strictly required by trait, but should be documented

#### Test 5.2: Worker Peek-Lock Semantics
**Goal:** Verify dequeue doesn't remove item until ack.

**Steps:**
1. Create provider
2. Enqueue worker item
3. Dequeue (gets item + token)
4. Attempt second dequeue → should return None
5. Ack with token
6. Verify queue is now empty

**Expected:** Item stays in queue until acked

#### Test 5.3: Worker Ack Atomicity
**Goal:** Verify ack_worker atomically removes item and enqueues completion.

**Steps:**
1. Create provider
2. Enqueue worker item
3. Dequeue and get token
4. Ack with completion
5. Verify:
   - Worker queue is empty
   - Orchestrator queue has completion item

**Expected:** Both operations succeed atomically

#### Test 5.4: Timer Queue Visibility Filtering
**Goal:** Verify timers only dequeued when fire_at <= now.

**Steps:**
1. Create provider supporting delayed visibility
2. Enqueue timer with fire_at = now + 1 hour
3. Dequeue immediately → should return None
4. Manually advance provider's clock (if possible)
5. Dequeue → should return timer

**Expected:** Timer only visible when due

#### Test 5.5: Timer Ack Atomicity
**Goal:** Verify ack_timer atomically removes timer and enqueues TimerFired.

**Steps:**
1. Create provider
2. Enqueue timer item
3. Dequeue timer
4. Ack with TimerFired
5. Verify:
   - Timer queue empty
   - Orchestrator queue has TimerFired

**Expected:** Both operations atomic

#### Test 5.6: Lost Lock Token Handling
**Goal:** Verify locked items eventually become available if token lost.

**Steps:**
1. Create provider
2. Enqueue worker item
3. Dequeue (gets token)
4. Lose token (simulate crash)
5. Wait for lock expiration
6. Dequeue again → should succeed

**Expected:** Item redelivered after timeout

---

### 6. Multi-Execution Support Tests

#### Test 6.1: Execution Isolation
**Goal:** Verify each execution has separate history.

**Steps:**
1. Create provider
2. Create execution 1 with 3 events
3. Create execution 2 with 2 events
4. Read execution 1 → should return 3 events
5. Read execution 2 → should return 2 events
6. Read latest (default) → should return execution 2's events

**Expected:** Complete isolation between executions

#### Test 6.2: Latest Execution Detection
**Goal:** Verify read() returns latest execution's history.

**Steps:**
1. Create provider
2. Create execution 1 with event "A"
3. Create execution 2 with event "B"
4. Call read() → should return execution 2 (latest)
5. Call read_with_execution(instance, 1) → should return execution 1

**Expected:** read() returns latest, read_with_execution returns specific

#### Test 6.3: Execution ID Sequencing
**Goal:** Verify execution IDs are sequential and unique.

**Steps:**
1. Create provider
2. Create 5 executions sequentially
3. List executions → should return [1, 2, 3, 4, 5]
4. Latest execution ID → should be 5

**Expected:** Sequential, unique IDs

#### Test 6.4: Execution Status Persistence
**Goal:** Verify status and output persisted correctly.

**Steps:**
1. Create provider
2. Ack with metadata.status = "Completed"
3. Ack with metadata.output = "result"
4. Read execution info
5. Verify status = "Completed", output = "result"

**Expected:** Metadata persisted correctly

#### Test 6.5: Current Execution ID Update
**Goal:** Verify current_execution_id updated correctly.

**Steps:**
1. Create provider
2. Ack execution 1
3. Verify current_execution_id = 1
4. Ack execution 3 (skip 2)
5. Verify current_execution_id = MAX(1, 3) = 3

**Expected:** MAX logic applied correctly

---

### 7. Concurrency and Thread-Safety Tests

#### Test 7.1: Concurrent Enqueue Operations
**Goal:** Verify provider handles concurrent enqueues safely.

**Steps:**
1. Create provider
2. Spawn 100 concurrent tasks, each enqueuing different items
3. Verify all enqueues succeed
4. Count total items → should be 100

**Expected:** All operations succeed, no data corruption

#### Test 7.2: Concurrent Fetch and Ack
**Goal:** Test high contention scenario.

**Steps:**
1. Create provider
2. Enqueue work for 10 instances
3. Spawn 10 concurrent fetch tasks
4. Spawn 10 concurrent ack tasks (for fetched items)
5. Verify no errors, no duplicates

**Expected:** All operations succeed without corruption

#### Test 7.3: Race Condition: Fetch During Ack
**Goal:** Test edge case where fetch occurs during ack.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Spawn task to ack
4. Simultaneously spawn task to fetch again
5. Verify appropriate behavior (either succeeds or fails cleanly)

**Expected:** No panic, no data corruption

---

### 8. Provider Contract Tests

#### Test 8.1: Runtime ID Assignment Enforcement
**Goal:** Verify provider does NOT generate execution_id or event_id.

**Steps:**
1. Create provider
2. Call ack_orchestration_item with event.event_id() = 0
3. Verify provider rejects (must be set by runtime)

**Expected:** Provider validates runtime-set IDs

**Implementation Pattern:**
```rust
#[tokio::test]
async fn test_provider_does_not_generate_ids() {
    let provider = create_provider().await;
    
    let item = provider.fetch_orchestration_item().await.unwrap();
    
    // Try to ack with unset event_id (should be rejected)
    let mut bad_event = Event::OrchestrationStarted {
        event_id: 0, // INVALID - not set by runtime
        name: "Test".to_string(),
        version: "1.0.0".to_string(),
        input: "{}".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    let result = provider.ack_orchestration_item(
        &item.lock_token,
        1,
        vec![bad_event],
        vec![],
        vec![],
        vec![],
        ExecutionMetadata::default(),
    ).await;
    
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("event_id"));
}
```

#### Test 8.2: No Event Content Inspection
**Goal:** Verify provider uses ExecutionMetadata, not event contents.

**Steps:**
1. Create provider
2. Ack with OrchestrationCompleted event but metadata.status = None
3. Read execution → should have status from metadata, not from event
4. Verify provider did NOT inspect event to derive status

**Expected:** Provider trusts metadata only

#### Test 8.3: History Append-Only Enforcing
**Goal:** Verify provider never modifies existing history.

**Steps:**
1. Create provider
2. Append event with event_id = 1
3. Try to append same event_id again → should reject or ignore
4. Verify original event unchanged

**Expected:** History remains immutable

---

### 9. Edge Case Tests

#### Test 9.1: Empty History Delta
**Goal:** Verify ack with no history delta succeeds.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Ack with empty history_delta
4. Verify succeeds

**Expected:** Valid operation

#### Test 9.2: Large Batch Operations
**Goal:** Test provider with large batches.

**Steps:**
1. Create provider
2. Ack with 1000 events in history_delta
3. Verify all persisted
4. Read back → should return all 1000

**Expected:** Handles large batches correctly

#### Test 9.3: Visibility Delay Edge Cases
**Goal:** Test delayed visibility with edge case timings.

**Steps:**
1. Create provider
2. Enqueue with delay = 0 → should be immediately visible
3. Enqueue with delay = 1ns → should be visible after tiny delay
4. Enqueue with very large delay → should not be visible

**Expected:** Delay semantics respected

#### Test 9.4: Concurrent Abandon and Ack
**Goal:** Test race between abandon and ack.

**Steps:**
1. Create provider
2. Fetch orchestration item
3. Spawn task to abandon
4. Spawn task to ack
5. Verify exactly one succeeds

**Expected:** No double-processing

---

### 10. Integration Tests

#### Test 10.1: Full Workflow Simulation
**Goal:** Test complete workflow end-to-end.

**Steps:**
1. Create provider
2. Enqueue StartOrchestration
3. Fetch and ack (creates OrchestrationStarted)
4. Enqueue ActivityCompleted
5. Fetch and ack (schedules 2 activities)
6. Dequeue and ack workers (executes activities)
7. Fetch orchestration completions
8. Verify final state correct

**Expected:** Complete workflow succeeds

#### Test 10.2: Provider State Consistency
**Goal:** Verify all provider state consistent after operations.

**Steps:**
1. Run extensive test suite
2. After each operation, verify:
   - Queue depths consistent
   - Lock tokens valid
   - History ordered correctly
   - No orphaned records

**Expected:** State always consistent

---

## Test Implementation Guidelines

### Generic Provider Factory

Create a trait-based factory to enable testing any provider:

```rust
pub trait ProviderFactory {
    async fn create_provider() -> Box<dyn Provider>;
    fn supports_delayed_visibility() -> bool;
    fn supports_lock_expiration() -> bool;
}

// Implement for each provider
pub struct SqliteProviderFactory;

impl ProviderFactory for SqliteProviderFactory {
    async fn create_provider() -> Box<dyn Provider> {
        Box::new(SqliteProvider::new_in_memory().await.unwrap())
    }
    
    fn supports_delayed_visibility() -> bool { true }
    fn supports_lock_expiration() -> bool { true }
}
```

### Test Organization

Organize tests by category:
```
tests/
  provider_correctness/
    mod.rs                    # Main test module
    instance_locking.rs       # Category 1 tests
    atomicity.rs              # Category 2 tests
    error_handling.rs         # Category 3 tests
    lock_expiration.rs        # Category 4 tests
    queue_semantics.rs       # Category 5 tests
    multi_execution.rs        # Category 6 tests
    concurrency.rs            # Category 7 tests
    provider_contract.rs      # Category 8 tests
    edge_cases.rs             # Category 9 tests
    integration.rs            # Category 10 tests
```

### Test Helper Functions

Create reusable helpers:
```rust
mod helpers {
    use duroxide::providers::{Provider, WorkItem};
    
    pub async fn create_start_item(instance: &str) -> WorkItem {
        WorkItem::StartOrchestration {
            instance: instance.to_string(),
            orchestration: "TestOrch".to_string(),
            input: "{}".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
            execution_id: 1,
        }
    }
    
    pub async fn wait_for_lock_expiration<P: Provider>(provider: &P) {
        // Provider-specific implementation
        tokio::time::sleep(Duration::from_secs(31)).await;
    }
}
```

---

## Success Criteria

A provider implementation is considered correct if it passes:

- ✅ All tests in categories 1-6 (critical correctness)
- ✅ 80%+ of tests in categories 7-10 (concurrency and edge cases)
- ✅ No test failures under concurrent execution (RUST_TEST_THREADS=10)
- ✅ Performance benchmarks within 2x of SQLite provider

---

## Running the Tests

```bash
# Run all provider correctness tests
cargo test --test provider_correctness

# Run specific category
cargo test --test provider_correctness -- instance_locking

# Run with high concurrency
RUST_TEST_THREADS=10 cargo test --test provider_correctness

# Run for specific provider (when multiple providers exist)
DUROXIDE_PROVIDER=sqlite cargo test --test provider_correctness
```

---

## Notes

- **Provider-specific tests:** Some tests may need to be skipped for providers that don't support certain features (e.g., delayed visibility)
- **Timing-sensitive tests:** Use provider-specific timing mechanisms where possible
- **Test isolation:** Each test should be independent and cleanup after itself
- **Debugging:** Providers should implement `debug_dump()` method for troubleshooting

