# External Event Queue Semantics

> **Status: UNIMPLEMENTED**
>
> This is a design document for proposed changes. The implementation has not been started.
>
> **Future Consideration:** Before implementing this, we may want to consider building a more general pub/sub mechanism that could support:
> - Topic-based subscriptions
> - Pattern matching on event names
> - Fan-out to multiple subscribers
> - Event filtering and transformation
> - Backpressure and flow control
>
> The current external event design is point-to-point (one producer, one consumer per event name). A pub/sub model would be more flexible for complex event-driven architectures.

---

## Overview

External events allow orchestrations to receive signals from outside systems via `client.raise_event()` and `ctx.schedule_wait()`. This document describes the queue semantics, limits, and behavior across execution boundaries.

## Current Behavior (Problem)

The current implementation has a critical limitation:

```rust
// In replay_engine.rs materialize_events_from_completion_batch
WorkItem::ExternalRaised { name, data, .. } => {
    let subscribed = self.baseline_history.iter().any(
        |e| matches!(&e.kind, EventKind::ExternalSubscribed { name: hist_name } if hist_name == &name),
    );
    if subscribed {
        Some(Event::new(...))
    } else {
        warn!("dropping ExternalByName with no matching subscription");
        None  // EVENT DROPPED!
    }
}
```

**Problems:**
1. Events raised BEFORE `schedule_wait()` is called are dropped
2. Events raised in execution N are invisible to execution N+1 (after `continue_as_new`)
3. No queue semantics - events must arrive AFTER subscription

## New Behavior (Queue Semantics)

### Always Persist External Events

External events are always persisted to history when raised, regardless of whether a subscription exists:

```rust
WorkItem::ExternalRaised { name, data, .. } => {
    // Always persist (queue semantics) - subject to limits
    Some(Event::new(&self.instance, self.execution_id, None, EventKind::ExternalEvent { name, data }))
}
```

This enables:
- Raise-before-wait: Events can be raised before `schedule_wait()` is called
- The `schedule_wait()` future already searches history by name

### Carry Forward on Continue-As-New

When an orchestration calls `continue_as_new`, unconsumed external events are carried forward to the new execution:

1. Scan current execution's history for `ExternalEvent` events
2. Find events with no matching `ExternalSubscribed` (by name)
3. Append these events to the new execution's history (after `OrchestrationStarted`)

An `ExternalEvent` is "unconsumed" if no `ExternalSubscribed` with the same name exists in the history.

## Limits (DoS Protection)

To prevent abuse, two limits are enforced:

### Limit 1: Max Unconsumed Events Per Instance

**Constant:** `MAX_UNCONSUMED_EXTERNAL_EVENTS_PER_INSTANCE = 100`

When processing a new `ExternalRaised`:
- Count unconsumed external events in current history
- If count >= 100, drop the new event with warning

```rust
if unconsumed_count >= MAX_UNCONSUMED_EXTERNAL_EVENTS_PER_INSTANCE {
    warn!(
        instance = %self.instance,
        event_name = %name,
        unconsumed_count = unconsumed_count,
        limit = MAX_UNCONSUMED_EXTERNAL_EVENTS_PER_INSTANCE,
        "Dropping external event - unconsumed event limit reached"
    );
    None
}
```

### Limit 2: Max Carry-Forward Executions

**Constant:** `MAX_EXTERNAL_EVENT_CARRY_FORWARD_EXECUTIONS = 5`

When carrying forward events on `continue_as_new`:
- Calculate event age: `new_execution_id - event.execution_id`
- If age > 5, drop the event (don't carry forward)

```rust
let age = new_execution_id.saturating_sub(event.execution_id);
if age > MAX_EXTERNAL_EVENT_CARRY_FORWARD_EXECUTIONS {
    warn!(
        event_name = %name,
        event_execution_id = event.execution_id,
        new_execution_id = new_execution_id,
        "Dropping external event - exceeded carry-forward limit"
    );
    // Don't carry forward
}
```

**Example:**
- Event raised in execution 1
- Carried to executions 2, 3, 4, 5, 6 ✅
- NOT carried to execution 7 ❌ (age = 6 > 5)

## Counting Unconsumed Events

An external event is "unconsumed" if there is no `ExternalSubscribed` event with the same name in the history.

```rust
fn count_unconsumed_external_events(history: &[Event]) -> usize {
    let subscribed_names: HashSet<String> = history.iter()
        .filter_map(|e| match &e.kind {
            EventKind::ExternalSubscribed { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    
    history.iter()
        .filter(|e| match &e.kind {
            EventKind::ExternalEvent { name, .. } => !subscribed_names.contains(name),
            _ => false,
        })
        .count()
}
```

**Note:** This is a simplification. One subscription "consumes" all events with that name. If you need 1:1 matching (one subscription per event), the counting logic would need to track quantities.

## Implementation Locations

| Change | File | Location |
|--------|------|----------|
| Always persist ExternalEvent | `src/runtime/replay_engine.rs` | `materialize_events_from_completion_batch` |
| Count unconsumed events | `src/runtime/replay_engine.rs` | New helper function |
| Enforce per-instance limit | `src/runtime/replay_engine.rs` | In ExternalRaised handling |
| Collect events for carry-forward | `src/runtime/dispatchers/orchestration.rs` | New helper function |
| Carry forward on CAN | `src/runtime/dispatchers/orchestration.rs` | After `OrchestrationStarted` append |
| Enforce carry-forward limit | `src/runtime/dispatchers/orchestration.rs` | In collect function |

## Constants

```rust
// src/runtime/mod.rs or src/lib.rs

/// Maximum number of unconsumed external events allowed per instance.
/// New events are dropped when this limit is reached.
pub const MAX_UNCONSUMED_EXTERNAL_EVENTS_PER_INSTANCE: usize = 100;

/// Maximum number of executions an external event can be carried forward.
/// Events older than this are dropped on continue-as-new.
pub const MAX_EXTERNAL_EVENT_CARRY_FORWARD_EXECUTIONS: u64 = 5;
```

---

# Test Plan

Tests go in `tests/events_tests.rs`.

## Test Categories

### 1. Basic Queue Semantics

#### Test 1.1: Raise Before Wait (Same Execution)
```
Scenario: Event raised before schedule_wait is called
Given: Orchestration not yet waiting
When: client.raise_event("approval", "yes")
And: Orchestration calls schedule_wait("approval")
Then: schedule_wait returns "yes" immediately
```

#### Test 1.2: Raise After Wait (Same Execution)
```
Scenario: Event raised after schedule_wait is called (existing behavior)
Given: Orchestration waiting on schedule_wait("approval")
When: client.raise_event("approval", "yes")
Then: schedule_wait returns "yes"
```

#### Test 1.3: Multiple Events Same Name
```
Scenario: Multiple events raised with same name
Given: Orchestration not started
When: client.raise_event("data", "first")
And: client.raise_event("data", "second")
And: Orchestration calls schedule_wait("data")
Then: First call returns "first"
And: Second schedule_wait("data") would return "second"
```

#### Test 1.4: Events With Different Names
```
Scenario: Events with different names don't interfere
Given: client.raise_event("event_a", "data_a")
And: client.raise_event("event_b", "data_b")
When: Orchestration calls schedule_wait("event_b")
Then: Returns "data_b" (not "data_a")
```

### 2. Continue-As-New Carry Forward

#### Test 2.1: Basic Carry Forward
```
Scenario: Unconsumed event carried to next execution
Given: Execution 1 running
And: client.raise_event("signal", "payload")
And: No schedule_wait called for "signal"
When: Orchestration calls continue_as_new
And: Execution 2 starts
And: Execution 2 calls schedule_wait("signal")
Then: Returns "payload" (carried from execution 1)
```

#### Test 2.2: Consumed Event Not Carried Forward
```
Scenario: Consumed events are not carried forward
Given: Execution 1 calls schedule_wait("signal")
And: client.raise_event("signal", "data")
And: schedule_wait returns "data"
When: Orchestration calls continue_as_new
And: Execution 2 calls schedule_wait("signal")
Then: Execution 2 waits (no event to consume)
```

#### Test 2.3: Multiple Executions Carry Forward
```
Scenario: Event carried through multiple executions
Given: Execution 1: raise_event("signal", "data"), no wait, continue_as_new
And: Execution 2: no wait, continue_as_new
And: Execution 3: no wait, continue_as_new
When: Execution 4 calls schedule_wait("signal")
Then: Returns "data" (carried through 3 executions)
```

#### Test 2.4: Carry Forward Limit Reached
```
Scenario: Event not carried after 5 executions
Given: Execution 1: raise_event("signal", "data")
And: Executions 2-6: continue_as_new without consuming
When: Execution 7 calls schedule_wait("signal")
Then: Waits forever (event was dropped at execution 7, age=6 > 5)
```

#### Test 2.5: Carry Forward Limit Boundary
```
Scenario: Event carried exactly 5 times
Given: Execution 1: raise_event("signal", "data")
And: Executions 2-5: continue_as_new without consuming
When: Execution 6 calls schedule_wait("signal")
Then: Returns "data" (age=5, within limit)
```

### 3. Per-Instance Limit

#### Test 3.1: Under Limit
```
Scenario: Events accepted when under limit
Given: Empty orchestration instance
When: Raise 50 events with different names
Then: All 50 events are persisted to history
```

#### Test 3.2: At Limit - New Event Dropped
```
Scenario: New events dropped when at limit
Given: 100 unconsumed external events in history
When: client.raise_event("new_event", "data")
Then: Event is dropped (not in history)
And: Warning logged: "unconsumed event limit reached"
```

#### Test 3.3: Consumed Events Free Up Space
```
Scenario: Consuming events frees up capacity
Given: 100 unconsumed external events
When: Orchestration calls schedule_wait for 10 of them
Then: 10 events become "consumed"
And: Can now raise 10 new events
```

#### Test 3.4: Mixed Consumed and Unconsumed
```
Scenario: Only unconsumed events count toward limit
Given: 50 unconsumed + 50 consumed events = 100 total
When: client.raise_event("new", "data")
Then: Event is persisted (only 50 unconsumed, under 100 limit)
```

### 4. Edge Cases

#### Test 4.1: Raise Event to Non-Existent Instance
```
Scenario: Raising event to instance that doesn't exist yet
Given: Instance "order-123" does not exist
When: client.raise_event("order-123", "signal", "data")
And: client.start_orchestration("order-123", "ProcessOrder", "{}")
And: Orchestration calls schedule_wait("signal")
Then: Returns "data"
```

#### Test 4.2: Raise Event to Completed Instance
```
Scenario: Event raised after orchestration completed
Given: Orchestration "order-123" completed successfully
When: client.raise_event("order-123", "late_signal", "data")
Then: Event is persisted but never consumed
And: No error (graceful handling)
```

#### Test 4.3: Same Name Event Across Executions
```
Scenario: Same name used in different executions independently
Given: Execution 1: schedule_wait("signal") consumes event A
And: continue_as_new
And: Execution 2: raise_event("signal", "new_data")
When: Execution 2 calls schedule_wait("signal")
Then: Returns "new_data" (new event, not carried forward)
```

#### Test 4.4: Empty Event Data
```
Scenario: Event with empty data string
Given: client.raise_event("signal", "")
When: schedule_wait("signal")
Then: Returns "" (empty string is valid)
```

#### Test 4.5: Large Event Data
```
Scenario: Event with large payload
Given: client.raise_event("signal", large_json_payload_100kb)
When: schedule_wait("signal")
Then: Returns the full payload correctly
```

### 5. Concurrent Scenarios

#### Test 5.1: Multiple Runtimes Raising Events
```
Scenario: Events from multiple sources
Given: Runtime A and Runtime B both have clients
When: A.raise_event("inst", "sig", "from_a")
And: B.raise_event("inst", "sig", "from_b")
And: Orchestration calls schedule_wait("sig") twice
Then: Both events received (order may vary)
```

#### Test 5.2: Race Between Raise and Wait
```
Scenario: Concurrent raise and wait
Given: Orchestration blocked on schedule_wait("signal")
When: Concurrently raise_event("signal", "data")
Then: schedule_wait unblocks and returns "data"
```

### 6. History Verification

#### Test 6.1: ExternalEvent in History
```
Scenario: Verify event appears in history
Given: client.raise_event("signal", "data")
When: Read history via admin API
Then: History contains ExternalEvent { name: "signal", data: "data" }
And: Event has correct execution_id and timestamp
```

#### Test 6.2: Carried Event Has Original Execution ID
```
Scenario: Carried events retain original execution_id
Given: Execution 1: raise_event creates event with execution_id=1
And: continue_as_new to execution 2
When: Read execution 2's history
Then: Carried ExternalEvent has execution_id=1 (original)
```

## Test File Structure

```rust
// tests/events_tests.rs

mod common;

use duroxide::{Event, EventKind, ...};

// ============================================
// 1. Basic Queue Semantics
// ============================================

#[tokio::test]
async fn test_raise_before_wait_same_execution() { ... }

#[tokio::test]
async fn test_raise_after_wait_same_execution() { ... }

#[tokio::test]
async fn test_multiple_events_same_name() { ... }

#[tokio::test]
async fn test_events_different_names() { ... }

// ============================================
// 2. Continue-As-New Carry Forward
// ============================================

#[tokio::test]
async fn test_basic_carry_forward() { ... }

#[tokio::test]
async fn test_consumed_event_not_carried() { ... }

#[tokio::test]
async fn test_carry_forward_multiple_executions() { ... }

#[tokio::test]
async fn test_carry_forward_limit_exceeded() { ... }

#[tokio::test]
async fn test_carry_forward_limit_boundary() { ... }

// ============================================
// 3. Per-Instance Limit
// ============================================

#[tokio::test]
async fn test_events_under_limit() { ... }

#[tokio::test]
async fn test_events_at_limit_dropped() { ... }

#[tokio::test]
async fn test_consumed_events_free_capacity() { ... }

#[tokio::test]
async fn test_mixed_consumed_unconsumed() { ... }

// ============================================
// 4. Edge Cases
// ============================================

#[tokio::test]
async fn test_raise_to_nonexistent_instance() { ... }

#[tokio::test]
async fn test_raise_to_completed_instance() { ... }

#[tokio::test]
async fn test_same_name_across_executions() { ... }

#[tokio::test]
async fn test_empty_event_data() { ... }

#[tokio::test]
async fn test_large_event_data() { ... }

// ============================================
// 5. Concurrent Scenarios
// ============================================

#[tokio::test]
async fn test_multiple_runtimes_raising() { ... }

#[tokio::test]
async fn test_race_between_raise_and_wait() { ... }

// ============================================
// 6. History Verification
// ============================================

#[tokio::test]
async fn test_external_event_in_history() { ... }

#[tokio::test]
async fn test_carried_event_retains_execution_id() { ... }
```

## Implementation Order

1. Add constants for limits
2. Implement `count_unconsumed_external_events` helper
3. Modify `materialize_events_from_completion_batch` to always persist + enforce limit
4. Implement `collect_unconsumed_external_events_for_carry_forward` helper
5. Modify CAN handling to carry forward events
6. Write tests in order listed above
7. Verify existing tests still pass

