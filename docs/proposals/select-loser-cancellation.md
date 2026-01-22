# Unobserved Future Cancellation Implementation Plan

**Status:** Draft  
**Created:** 2025-01-22  
**Related:** [replay-simplification-PROGRESS.md](../replay-simplification-PROGRESS.md)

## Problem Statement

When a `ScheduledFuture` is dropped without completing, the underlying scheduled work should be cancelled. This applies to three scenarios:

### Scenario 1: Select Losers

```rust
// activity_b scheduled but loses the race - should be cancelled
match ctx.select2(timer, activity_b).await {
    Either2::First(()) => { /* timer won, activity_b dropped */ }
    Either2::Second(result) => { /* activity won */ }
}
```

### Scenario 2: Non-Awaited Futures (Never Polled)

```rust
async fn orchestration(ctx: OrchestrationContext) -> Result<String, String> {
    let should_do_task = check_condition();
    
    if should_do_task {
        let task_future = ctx.schedule_activity("Task", input);
        // Oops! Forgot to await, future goes out of scope here
    }
    
    // Orchestration continues - but Task activity was created and dropped
    let result = ctx.schedule_activity("OtherWork", data).await?;
    Ok(result)
}
```

Or with explicit nested scope:

```rust
async fn orchestration(ctx: OrchestrationContext) -> Result<String, String> {
    {
        let _unused = ctx.schedule_activity("Task", input);  // Created but never awaited
        // _unused dropped here when scope ends
    }
    
    // Orchestration continues running...
    ctx.schedule_timer(Duration::from_secs(10)).await;
    Ok("done".into())
}
```

### Scenario 3: Abandoned Futures (Partially Polled)

```rust
async fn orchestration(ctx: OrchestrationContext) -> Result<String, String> {
    let activity = ctx.schedule_activity("LongTask", input);
    
    // Race activity against a timer using ctx.select2
    match ctx.select2(ctx.schedule_timer(Duration::from_secs(5)), activity).await {
        Either2::First(()) => {
            // Timer won - activity future is dropped here by select2
            // This is the "select loser" case
        }
        Either2::Second(result) => {
            return result;
        }
    }
    
    // Or: conditional early return that abandons a future
    let fut = ctx.schedule_activity("AnotherTask", data);
    
    if should_skip() {
        // fut is dropped here without completing - should be cancelled
        return Ok("skipped".into());
    }
    
    fut.await
}
```

### Current Issues

1. **Issue 1 (Design Required):** Unobserved future cancellation infrastructure exists but is never populated:
   - `ReplayEngine::cancelled_activity_ids: Vec<u64>` exists but is always empty
   - `execution.rs` iterates this vec but finds nothing
   - The `schedule_*` methods return opaque `impl Future` - we can't detect when they're dropped

2. **Issue 2 (Documentation):** The cancellation model isn't clearly documented.

## Current Behavior

Activities are only cancelled when the orchestration reaches a **terminal state** (Completed, Failed, ContinuedAsNew). This means:

- **Select losers:** Continue running until orchestration completes
- **Non-awaited futures:** The action may never be emitted (if never polled), but if polled once, runs until terminal state
- **Abandoned futures:** Continue running until orchestration completes

This is wasteful - a long-running activity that was abandoned will continue consuming resources.

## Proposed Solution: ScheduledFuture Wrapper Type

### Design Overview

Wrap all `schedule_*` return types in a `ScheduledFuture<T>` that:
1. Carries a **token** (known at creation) before `schedule_id` is assigned
2. Implements `Drop` to mark the token as cancelled
3. The replay engine handles cancelled tokens at bind time

### Key Insight: Token-Based Pre-Binding Cancellation

The challenge is that `schedule_id` isn't known until the future is first polled (when the action is emitted). But we need to cancel futures that are dropped before ever being polled.

**Solution:** Use a token assigned at creation time. The replay engine maps tokens to schedule_ids when binding occurs. If a token is marked cancelled before binding, the replay engine skips emitting the action entirely.

### Type Design

```rust
/// Discriminator for different schedule types
#[derive(Debug, Clone)]
pub enum ScheduleKind {
    Activity { name: String },
    Timer,
    ExternalWait { event_name: String },
    SubOrchestration { child_instance_id: String },
}

/// Wrapper for all scheduled futures with cancellation support
pub struct ScheduledFuture<T> {
    /// Token assigned at creation (before schedule_id is known)
    token: u64,
    /// What kind of schedule this represents
    kind: ScheduleKind,
    /// Reference to context for cancellation registration
    ctx: OrchestrationContext,
    /// The underlying future
    inner: Pin<Box<dyn Future<Output = T> + Send>>,
}

impl<T> Future for ScheduledFuture<T> {
    type Output = T;
    
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        self.inner.as_mut().poll(cx)
    }
}

impl<T> Drop for ScheduledFuture<T> {
    fn drop(&mut self) {
        // Only cancel if future wasn't completed
        if !self.completed {
            self.ctx.mark_token_cancelled(self.token, &self.kind);
        }
    }
}
```

### Replay Engine Changes

```rust
struct ReplayEngine {
    // Existing fields...
    
    /// Tokens that have been cancelled (before or after binding)
    cancelled_tokens: HashSet<u64>,
    
    /// Map from token to schedule_id (populated at bind time)
    token_to_schedule_id: HashMap<u64, u64>,
}

impl ReplayEngine {
    /// Called by ScheduledFuture::drop()
    pub fn mark_token_cancelled(&mut self, token: u64, kind: &ScheduleKind) {
        self.cancelled_tokens.insert(token);
        
        // If already bound to a schedule_id, add to cancelled list
        if let Some(schedule_id) = self.token_to_schedule_id.get(&token) {
            match kind {
                ScheduleKind::Activity { .. } => {
                    self.cancelled_activity_ids.push(*schedule_id);
                }
                ScheduleKind::SubOrchestration { child_instance_id } => {
                    self.cancelled_sub_orchestrations.push(child_instance_id.clone());
                }
                // Timers and external waits don't need explicit cancellation
                _ => {}
            }
        }
    }
    
    /// Called when binding a token to a schedule_id
    fn bind_token(&mut self, token: u64, schedule_id: u64) {
        self.token_to_schedule_id.insert(token, schedule_id);
        
        // Check if token was cancelled before binding
        if self.cancelled_tokens.contains(&token) {
            // Handle pre-binding cancellation
        }
    }
}
```

### Cancellation Consumption Flow

The `cancelled_activity_ids` populated by `ScheduledFuture::Drop` flows through the system to trigger **lock stealing** at the provider level:

```
┌──────────────────────────────────────────────────────────────────────────┐
│ ScheduledFuture::drop()                                                  │
│   Calls ctx.mark_token_cancelled(token, kind)                            │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ ReplayEngine                                                             │
│   cancelled_activity_ids: Vec<u64>   ← Populated with schedule_ids       │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │ turn.cancelled_activity_ids()
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ execution.rs (run_single_execution_atomic)                               │
│   Converts schedule_ids → ScheduledActivityIdentifier {                  │
│       instance, execution_id, activity_id                                │
│   }                                                                      │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │ returned as part of tuple
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ orchestration.rs (OrchestrationDispatcher)                               │
│   Passes cancelled_activities to ack_orchestration_item()                │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │ via Provider trait
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Provider::ack_orchestration_item() (e.g., sqlite.rs)                     │
│                                                                          │
│   DELETE FROM worker_queue                                               │
│   WHERE (instance_id, execution_id, activity_id) IN (VALUES ...)         │
│                                                                          │
│   This is "lock stealing" - removes activities from queue                │
└──────────────────────────────────────────────────────────────────────────┘
```

**Lock stealing effect:**
1. **Activity not yet started:** Worker won't pick it up (row deleted)
2. **Activity already running:** Worker's next lock renewal fails (row gone), triggering cooperative cancellation via `ActivityContext::is_cancelled()`

### Sub-Orchestration Cancellation Flow

Sub-orchestrations use a different cancellation mechanism than activities. Instead of lock stealing, they receive a `CancelInstance` work item:

```
┌──────────────────────────────────────────────────────────────────────────┐
│ ScheduledFuture::drop() for SubOrchestration                             │
│   Calls ctx.mark_token_cancelled(token, ScheduleKind::SubOrchestration)  │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ ReplayEngine                                                             │
│   cancelled_sub_orchestrations: Vec<String>  ← child instance IDs        │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │ turn.cancelled_sub_orchestrations()
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ execution.rs (run_single_execution_atomic)                               │
│   For each cancelled child, creates:                                     │
│   WorkItem::CancelInstance { instance: child_id, reason: "parent drop" } │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │ added to orchestrator_items
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Provider::ack_orchestration_item()                                       │
│   Enqueues CancelInstance to orchestrator_queue for child instance       │
└────────────────────────┬─────────────────────────────────────────────────┘
                         │ child picks up work item
                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Child Orchestration (OrchestrationDispatcher)                            │
│   Processes CancelInstance → TurnResult::Cancelled                       │
│   Sets status = "Cancelled", propagates to its children recursively      │
└──────────────────────────────────────────────────────────────────────────┘
```

**Key differences from activity cancellation:**

| Aspect | Activity | Sub-Orchestration |
|--------|----------|-------------------|
| Mechanism | Lock stealing (DELETE from worker_queue) | CancelInstance work item |
| Propagation | Single level | Recursive (children of children) |
| In-flight handling | Lock renewal fails | TurnResult::Cancelled on next turn |
| State tracking | `cancelled_activity_ids: Vec<u64>` | `cancelled_sub_orchestrations: Vec<String>` |

**Replay Engine additions for sub-orchestration:**

```rust
struct ReplayEngine {
    // Existing...
    cancelled_activity_ids: Vec<u64>,
    
    // New for sub-orchestrations
    cancelled_sub_orchestrations: Vec<String>,  // child instance IDs
}

impl ReplayEngine {
    pub fn mark_token_cancelled(&mut self, token: u64, kind: &ScheduleKind) {
        self.cancelled_tokens.insert(token);
        
        if let Some(schedule_id) = self.token_to_schedule_id.get(&token) {
            match kind {
                ScheduleKind::Activity { .. } => {
                    self.cancelled_activity_ids.push(*schedule_id);
                }
                ScheduleKind::SubOrchestration { child_instance_id } => {
                    // Add to sub-orchestration cancellation list
                    self.cancelled_sub_orchestrations.push(child_instance_id.clone());
                }
                ScheduleKind::Timer | ScheduleKind::ExternalWait { .. } => {
                    // Virtual constructs - no external cancellation needed
                }
            }
        }
    }
    
    pub fn cancelled_sub_orchestrations(&self) -> &[String] {
        &self.cancelled_sub_orchestrations
    }
}
```

**execution.rs changes:**

```rust
// In run_single_execution_atomic, after handling cancelled_activity_ids:

// Handle dropped sub-orchestration futures
for child_instance_id in turn.cancelled_sub_orchestrations() {
    orchestrator_items.push(WorkItem::CancelInstance {
        instance: child_instance_id.clone(),
        reason: "parent dropped sub-orchestration future".to_string(),
    });
}
```

### API Changes

The `schedule_*` methods return `ScheduledFuture<T>` instead of `impl Future`:

```rust
impl OrchestrationContext {
    /// Schedule an activity - returns ScheduledFuture for cancellation support
    pub fn schedule_activity<I: Serialize>(
        &self,
        name: &str,
        input: I,
    ) -> ScheduledFuture<Result<String, String>> {
        let token = self.next_token();
        let inner = self.schedule_activity_inner(name, input);
        ScheduledFuture {
            token,
            kind: ScheduleKind::Activity { name: name.to_string() },
            ctx: self.clone(),
            inner: Box::pin(inner),
            completed: false,
        }
    }
    
    // Similar for schedule_timer, schedule_wait, schedule_sub_orchestration
}
```

## Implementation Plan

### Phase 1: Infrastructure

1. **Add ScheduleKind enum** - `src/lib.rs` or new module
   - `Activity { name: String }`
   - `Timer`
   - `ExternalWait { event_name: String }`
   - `SubOrchestration { child_instance_id: String }`

2. **Add ScheduledFuture wrapper** - `src/lib.rs` or new `src/scheduled_future.rs`
   - Token field (assigned at creation)
   - Kind field (ScheduleKind)
   - Context reference
   - Inner future
   - Completed flag (to skip cancellation on normal completion)

3. **Add token tracking to ReplayEngine** - `src/runtime/replay_engine.rs`
   - `cancelled_tokens: HashSet<u64>`
   - `token_to_schedule_id: HashMap<u64, u64>`
   - `cancelled_activity_ids: Vec<u64>` (existing, now populated)
   - `cancelled_sub_orchestrations: Vec<String>` (new)

4. **Add mark_token_cancelled method** - `src/runtime/replay_engine.rs`
   - Handle Activity → add to cancelled_activity_ids
   - Handle SubOrchestration → add to cancelled_sub_orchestrations
   - Handle Timer/ExternalWait → no-op (virtual constructs)

5. **Wire up OrchestrationContext** - `src/lib.rs`
   - Token counter
   - Forward cancellation calls to replay engine

**Estimated effort:** 2-4 hours

### Phase 2: Update schedule_* methods

1. **schedule_activity** - Return `ScheduledFuture<Result<String, String>>`
2. **schedule_timer** - Return `ScheduledFuture<()>`
3. **schedule_wait** - Return `ScheduledFuture<Option<String>>`
4. **schedule_sub_orchestration** - Return `ScheduledFuture<Result<String, String>>`
   - Capture child_instance_id in ScheduleKind

**Estimated effort:** 1-2 hours

### Phase 3: Consumption Path Updates

1. **execution.rs** - `run_single_execution_atomic`
   - Existing: iterate `cancelled_activity_ids` → ScheduledActivityIdentifier
   - New: iterate `cancelled_sub_orchestrations` → WorkItem::CancelInstance

2. **Verify sqlite.rs** - Provider already handles:
   - Activity cancellation via DELETE from worker_queue
   - CancelInstance enqueueing to orchestrator_queue

**Estimated effort:** 1 hour

### Phase 4: Documentation

1. Update `ORCHESTRATION-GUIDE.md` with cancellation model
2. Add section to `docs/activity-cancellation.md` (if exists) or create new doc
3. Update docstrings on `select2`/`select3`
4. Document sub-orchestration cancellation cascade behavior

**Estimated effort:** 1 hour

## Test Plan

### Test File Locations

| Category | File |
|----------|------|
| Unit Tests (Token, Select, Non-Awaited, Abandoned, Explicit Drop, Edge Cases) | `tests/replay_engine_tests.rs` |
| Sub-Orchestration Unit Tests | `tests/replay_engine_tests.rs` |
| Integration Scenarios (Activities) | `tests/scenarios/unobserved_future_cancellation.rs` |
| Integration Scenarios (Sub-Orchestrations) | `tests/scenarios/unobserved_future_cancellation.rs` |
| Replay Determinism | `tests/replay_engine_tests.rs` |
| Negative Tests | `tests/replay_engine_tests.rs` |

### Unit Tests (replay_engine_tests.rs)

#### Token Lifecycle Tests

| Test Name | Description |
|-----------|-------------|
| `token_cancelled_before_binding` | Drop ScheduledFuture before first poll - verify no action emitted |
| `token_cancelled_after_binding` | Drop ScheduledFuture after poll - verify schedule_id in cancelled list |
| `token_not_cancelled_on_completion` | Let future complete normally - verify NOT in cancelled list |
| `multiple_tokens_partial_cancel` | Create 3 futures, cancel 2 - verify correct ones in list |

#### Select Loser Tests

| Test Name | Description |
|-----------|-------------|
| `select2_timer_wins_activity_cancelled` | Timer wins select2 - verify activity in cancelled_activity_ids |
| `select2_activity_wins_timer_not_cancelled` | Activity wins - verify timer NOT in any cancelled list (timers don't need cancel) |
| `select3_one_winner_two_losers` | First completes - verify other two activities cancelled |
| `select2_nested_in_async_block` | `async { ctx.schedule_activity(...).await }` works with cancellation |

#### Non-Awaited Future Tests

| Test Name | Description |
|-----------|-------------|
| `non_awaited_future_never_emits_action` | Create future, never await, drop - no action should be emitted |
| `non_awaited_future_in_branch` | Conditional branch creates future but doesn't await - verify no orphan |
| `multiple_non_awaited_futures` | Create several futures, await none - all should be cancelled/not emitted |
| `non_awaited_sub_orchestration` | Create sub-orch future, never await - child should be cancelled |

#### Abandoned Future Tests

| Test Name | Description |
|-----------|-------------|
| `abandoned_after_first_poll` | Poll once then drop - activity should be cancelled |
| `abandoned_mid_orchestration` | Orchestration returns early, abandoning pending futures |
| `abandoned_in_loop_iteration` | Future created in loop, abandoned when loop continues |

#### Explicit Drop Tests

| Test Name | Description |
|-----------|-------------|
| `explicit_drop_before_poll` | `drop(ctx.schedule_activity(...))` immediately - no action emitted |
| `explicit_drop_after_poll` | Create, poll once, then `drop(fut)` - cancel schedule_id |
| `explicit_drop_activity_in_vec` | Create vec of futures, `drop()` some, await others |
| `explicit_drop_timer` | `drop(ctx.schedule_timer(...))` - timer should be cleaned up (no cancel needed) |
| `explicit_drop_sub_orchestration` | `drop(ctx.schedule_sub_orchestration(...))` after poll - child cancelled |
| `explicit_drop_external_wait` | `drop(ctx.schedule_wait(...))` after poll - wait cleaned up |

#### Sub-Orchestration Cancellation Tests

| Test Name | Description |
|-----------|-------------|
| `sub_orch_drop_before_poll` | Create sub-orch future, drop before poll - no SubOrchestrationScheduled event |
| `sub_orch_drop_after_poll` | Create, poll, drop - CancelInstance enqueued for child |
| `sub_orch_select_loser` | `select2(timer, sub_orch)` - timer wins, child receives CancelInstance |
| `sub_orch_cancel_propagates_to_grandchildren` | Parent drops child future, child's children also cancelled |
| `sub_orch_cancel_during_activity` | Child has in-flight activity when cancelled - both cancelled |
| `sub_orch_cancel_already_completed` | Drop future after child completed - no cancel (idempotent) |
| `sub_orch_cancel_already_failed` | Drop future after child failed - no cancel |
| `sub_orch_multiple_children_selective_cancel` | Start 3 children, drop 2 futures - only those 2 cancelled |

#### Pre-Binding Edge Cases

| Test Name | Description |
|-----------|-------------|
| `drop_before_any_poll` | Create future, drop immediately - no action should be emitted |
| `drop_between_action_emit_and_completion` | Standard case - action emitted, then dropped |
| `replay_with_cancelled_token` | Verify replay handles pre-cancelled tokens correctly |

### Integration Tests (tests/scenarios/)

#### Scenario: Select with Long-Running Activity

```rust
#[tokio::test]
async fn select_loser_activity_gets_cancel_signal() {
    // 1. Register activity that checks is_cancelled() in a loop
    // 2. Orchestration does select2(short_timer, long_activity)
    // 3. Timer wins
    // 4. Verify activity received cancellation signal
    // 5. Verify activity stopped executing
}
```

#### Scenario: Select Loser Sub-Orchestration

```rust
#[tokio::test]
async fn select_loser_sub_orchestration_cancelled() {
    // 1. Parent orchestration does select2(timer, sub_orchestration)
    // 2. Timer wins
    // 3. Verify CancelInstance work item enqueued for child
    // 4. Verify sub-orchestration status becomes "Cancelled"
}
```

#### Scenario: Dropped Sub-Orchestration Future Cancels Child (Basic)

```rust
#[tokio::test]
async fn dropped_sub_orch_future_cancels_child() {
    // Setup: parent and child orchestrations
    // 1. Parent creates sub-orchestration future, polls it (child starts)
    // 2. Parent explicitly drops the future (or it goes out of scope)
    // 3. Verify CancelInstance work item created for child instance
    // 4. Run dispatchers to process the CancelInstance
    // 5. Verify child orchestration status = "Cancelled"
    // 6. Verify parent orchestration can complete normally
}
```

#### Scenario: Dropped Sub-Orchestration with Grandchildren

```rust
#[tokio::test]
async fn dropped_sub_orch_cascades_to_grandchildren() {
    // 1. Parent starts child orchestration
    // 2. Child starts grandchild orchestration
    // 3. Parent drops child future (select loser or explicit drop)
    // 4. Verify child receives CancelInstance
    // 5. Verify grandchild also receives CancelInstance (cascade)
    // 6. All three orchestrations end in correct terminal states
}
```

#### Scenario: Sub-Orchestration with In-Flight Activity Cancelled

```rust
#[tokio::test]
async fn sub_orch_cancel_stops_child_activities() {
    // 1. Parent starts child orchestration
    // 2. Child starts long-running activity
    // 3. Parent drops child future
    // 4. Verify child receives CancelInstance
    // 5. Verify child's activity gets lock-stolen
    // 6. Verify activity worker sees is_cancelled() = true
}
```

#### Scenario: Non-Awaited Future Cleanup

```rust
#[tokio::test]
async fn non_awaited_future_no_orphan_activity() {
    // 1. Orchestration creates activity future but doesn't await it
    // 2. Orchestration completes via different path
    // 3. Verify no orphan activity running
    // 4. Verify no action was emitted for the non-awaited future
}
```

#### Scenario: Abandoned Future in Early Return

```rust
#[tokio::test]
async fn abandoned_future_early_return() {
    // 1. Orchestration starts activity, polls once
    // 2. Some condition triggers early return
    // 3. Verify activity receives cancellation
    // 4. Verify clean shutdown
}
```

#### Scenario: Rapid Select Drops

```rust
#[tokio::test]
async fn rapid_select_drops_no_leak() {
    // 1. In a loop, create select2 futures and drop them before completion
    // 2. Verify no memory leaks
    // 3. Verify no orphaned activities
}
```

### Replay Determinism Tests

| Test Name | Description |
|-----------|-------------|
| `replay_select_loser_same_outcome` | Replay orchestration that used select2 - same winner, same cancellations |
| `replay_cancelled_token_no_action` | Replay where token was cancelled before binding - still no action |
| `replay_with_cancelled_activity_completion` | Activity completes during replay but was cancelled - handle gracefully |

### Negative Tests

| Test Name | Description |
|-----------|-------------|
| `cancel_already_completed_no_op` | Dropping completed future doesn't add to cancelled list |
| `cancel_timer_no_provider_call` | Cancelled timer doesn't call provider (timers are virtual) |
| `double_drop_idempotent` | Somehow dropping twice (shouldn't happen) is safe |

## Ergonomics Evaluation: ScheduledFuture vs Regular Rust Futures

### What Matches Regular Future Expectations ✅

| Aspect | Behavior | Matches? |
|--------|----------|----------|
| `.await` works | Yes, implements `Future<Output = T>` | ✅ |
| Lazy evaluation | No work until first poll | ✅ |
| Can store in variables | `let fut = ctx.schedule_activity(...)` | ✅ |
| Can pass to combinators | Works with `select2`, `join`, etc. | ✅ |
| Can wrap in async block | `async { fut.await }` | ✅ |
| Can collect in Vec | `Vec<ScheduledFuture<T>>` | ✅ |

### What Differs From Regular Futures ⚠️

| Aspect | Regular Future | ScheduledFuture | Impact |
|--------|----------------|-----------------|--------|
| **Drop semantics** | No-op (memory cleanup) | Triggers cancellation | Semantic side effect |
| **Post-poll drop** | Inert | Cancels running work | Unexpected for Rust devs |
| **forget() safety** | Memory leak only | Cancellation leak | Work runs forever |
| **ManuallyDrop** | Just defers drop | Bypasses cancellation | Same as forget |
| **Clone** | Sometimes impl'd | Should NOT impl | Can't share ownership |
| **Type visibility** | Often `impl Future` | Concrete type exposed | Leakier abstraction |

### Detailed Concerns

#### 1. Drop With Side Effects

Regular Rust futures are inert on drop - dropping just frees memory. `ScheduledFuture` has a "meaningful destructor" that triggers external state changes (cancellation).

```rust
// Regular future: nothing happens externally
let fut = async { do_something().await };
drop(fut); // Just memory freed

// ScheduledFuture: external side effect
let fut = ctx.schedule_activity("Task", input);
drop(fut); // Cancellation triggered!
```

**Mitigation:** This is intentional and desired - we *want* unobserved futures to cancel. Document clearly.

#### 2. `std::mem::forget()` Bypass

```rust
let fut = ctx.schedule_activity("Task", input);
std::mem::forget(fut); // Drop never runs, no cancellation!
// Activity runs but result is never observed
```

**Mitigation:** This is a known Rust pattern limitation. Same issue exists with `MutexGuard`, file handles, etc. Document that `forget()` on ScheduledFuture causes resource leaks.

#### 3. No Clone

Regular futures sometimes implement `Clone` (e.g., `futures::future::Shared`). `ScheduledFuture` cannot - cloning would create two owners of the same cancellation token.

```rust
// This won't compile (good!)
let fut1 = ctx.schedule_activity("Task", input);
let fut2 = fut1.clone(); // Error: Clone not implemented
```

**Mitigation:** This matches most async patterns. Users who need shared futures can wrap in `Arc<Mutex<Option<ScheduledFuture<T>>>>` or use a channel.

#### 4. Collection Drop Semantics

```rust
let mut futures: Vec<_> = (0..10)
    .map(|i| ctx.schedule_activity("Task", i))
    .collect();

// Drop the whole vec - ALL 10 activities cancelled
drop(futures); 
```

**Mitigation:** This is probably the desired behavior. Document it.

### Comparison With Similar Patterns

| Type | Drop Behavior | Precedent |
|------|---------------|-----------|
| `tokio::task::JoinHandle` | Detaches task (keeps running) | Different - we cancel |
| `tokio::sync::OwnedMutexGuard` | Releases lock | Similar - external effect |
| `std::fs::File` | Closes file | Similar - external effect |
| `async_std::task::JoinHandle` | Detaches task | Different - we cancel |
| `ScheduledFuture` | Cancels work | Novel for futures |

### Recommendation

The semantic differences are **acceptable and intentional**:

1. **Drop-cancellation is the feature** - The whole point is to cancel unobserved work
2. **Matches RAII philosophy** - Resource (scheduled work) is cleaned up on drop
3. **Well-precedented** - File handles, mutex guards, etc. all have meaningful drop
4. **Explicit > implicit** - Better than silently leaking running activities

**Documentation requirements:**
- Clear docstrings on `schedule_*` methods explaining cancellation-on-drop
- Warning about `forget()` / `ManuallyDrop` in docs
- Examples showing intentional drop patterns

## Risks and Mitigations

### Risk 1: API Change Compatibility

**Impact:** `schedule_*` return type changes from `impl Future` to `ScheduledFuture<T>`

**Mitigation:** 
- `ScheduledFuture<T>` implements `Future<Output = T>`, so `.await` works identically
- **Existing tests require no changes** - all `.await` patterns work unchanged
- Async block wrapping (`async { ctx.schedule_activity(...).await }`) still works
- Only breaks code that explicitly annotates `impl Future` type (rare/unlikely)

### Risk 2: Performance Overhead

**Impact:** Extra allocation for wrapper, token tracking

**Mitigation:**
- Token is just a `u64` counter
- HashMap lookup is O(1)
- Only overhead when futures are dropped (rare path)

### Risk 3: Replay Compatibility

**Impact:** Old orchestrations replaying with new code

**Mitigation:**
- Cancellation is additive - old history without cancellation still replays
- New cancellation info is computed, not stored in history

## Alternatives Considered

### Alternative 1: Terminal-State-Only Cancellation

Keep current behavior where losers are only cancelled when orchestration completes.

**Pros:** Zero code change, already works  
**Cons:** Wasteful - long-running loser activities continue until orchestration ends

### Alternative 2: Explicit cancel_schedule() API

Add `ctx.cancel_schedule(schedule_id)` that users call manually.

**Pros:** Explicit, no magic  
**Cons:** Verbose, easy to forget, doesn't work with `impl Future` return types

### Alternative 3: Side-Channel via select2/3

Have `select2`/`select3` internally track which future lost and cancel it.

**Pros:** Doesn't change schedule_* API  
**Cons:** Only works for select combinators, not non-awaited/abandoned futures

## Success Criteria

1. ✅ Select losers appear in `cancelled_activity_ids` 
2. ✅ Non-awaited futures don't emit actions (cancelled before binding)
3. ✅ Abandoned futures trigger cancellation of in-flight work
4. ✅ Activities receive cancellation signal promptly (not just at terminal state)
5. ✅ **Sub-orchestration futures dropped → CancelInstance enqueued**
6. ✅ **Sub-orchestration cancellation cascades to grandchildren**
7. ✅ **Sub-orchestration cancellation stops child's in-flight activities**
8. ✅ All existing tests pass
9. ✅ New cancellation tests pass
10. ✅ Replay determinism preserved
11. ✅ Documentation updated

## Open Questions

1. **Should cancelled activities still have their results stored?** Currently yes (history is append-only). Document this.

2. **What if activity completes between cancel signal and processing?** Race condition - completed result should win over cancellation.

3. **Should we add a `with_cancel_token()` builder for opt-in cancellation?** Could make it opt-in initially for compatibility.
