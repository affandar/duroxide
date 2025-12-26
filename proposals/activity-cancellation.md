# Activity Cancellation Design

**Status:** Proposed  
**Author:** Duroxide Team  
**Created:** 2024-12-22  
**Last Updated:** 2024-12-22

## Overview

This proposal introduces cooperative activity cancellation to Duroxide. When an orchestration reaches a terminal state (Completed, Failed, Cancelled) or is deleted, in-flight activities should be notified and given an opportunity to gracefully shut down.

### Problem Statement

Currently, when an orchestration is cancelled or completes:
- ✅ Sub-orchestrations receive `CancelInstance` work items
- ❌ **In-flight activities continue running** until completion
- ❌ **Pending activities in worker_queue remain** until picked up and executed
- ❌ Activities have **no awareness** of parent orchestration status

This leads to:
- **Worker slot starvation** - Long-running activities for terminated orchestrations hog worker dispatcher slots (default: only 2 concurrent workers), blocking legitimate work
- Wasted compute resources on activities whose results will never be observed
- Delayed cleanup of orchestration resources
- Poor user experience for long-running activities

**Example:** With default `worker_concurrency: 2`, if both slots are running 10-minute activities for orchestrations that were cancelled at minute 1, no other activities can execute for the remaining 9 minutes.

### Goals

1. **Minimal provider changes** - Piggyback on existing lock renewal mechanism
2. **Cooperative cancellation** - Activities can respond gracefully via cancellation token
3. **Bounded shutdown** - If an activity ignores cancellation beyond a grace period, the runtime aborts the activity task to free worker capacity
4. **Backward compatible** - Activities without cancellation awareness continue to work

### Non-Goals

1. Immediate/synchronous cancellation of activities
2. Automatic propagation to user-spawned tasks (users must handle this)
3. Cancellation of pending (not-yet-started) activities (future enhancement)

## Design

### High-Level Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  FETCH WORK ITEM                                                            │
│                                                                             │
│  When provider.fetch_work_item() is called:                                 │
│    1. Find next available work item                                         │
│    2. Check orchestration instance/execution state                          │
│    3. Return work item + orchestration state info                           │
│       - ExecutionState::Running                                         │
│       - ExecutionState::Terminal { status }                             │
│       - ExecutionState::Missing                                         │
│                                                                             │
│  Provider just reports state. Worker dispatcher decides what to do.         │
└─────────────────────────────────────────────────────────────────────────────┘
                                    ↓
┌─────────────────────────────────────────────────────────────────────────────┐
│  WORKER DISPATCHER (decision logic)                                         │
│                                                                             │
│  On fetch_work_item result:                                                 │
│    - If Running → execute activity normally                                 │
│    - If Terminal/Missing → skip activity, ack with None (drop), try next   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    ↓
┌─────────────────────────────────────────────────────────────────────────────┐
│  ACTIVITY MANAGER TASK (ongoing check for in-flight activities)             │
│                                                                             │
│  loop every ~25s (renewal interval):                                        │
│    1. Call provider.renew_work_item_lock(token, timeout)                    │
│    2. Provider returns:                                                     │
│       - Ok(ExecutionState::Running) → continue                          │
│       - Ok(ExecutionState::Terminal/Missing) → trigger cancellation     │
│       - Err(e) where e.retryable=true → log + keep trying                   │
│       - Err(e) where e.retryable=false → trigger cancellation + stop        │
│    3. On Terminal/Missing:                                                  │
│       a. Trigger cancellation token                                         │
│       b. Exit loop (stop renewing)                                          │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    ↓
┌─────────────────────────────────────────────────────────────────────────────┐
│  WORKER DISPATCHER (activity execution)                                     │
│                                                                             │
│  1. Spawn activity with CancellationToken in ActivityContext                │
│  2. Spawn activity manager task (evolved lock renewal)                      │
│  3. Wait for activity OR cancellation:                                      │
│                                                                             │
│     tokio::select! {                                                        │
│         result = activity_future => {                                       │
│             // Normal completion, ack result                                │
│         }                                                                   │
│         _ = cancel_token.cancelled() => {                                   │
│             // Orchestration terminated, give activity grace period         │
│             // Wait up to 10s for graceful shutdown                         │
│             // If timeout: log warning, don't abort, move on                │
│         }                                                                   │
│     }                                                                       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Cancellation Triggers

The provider should trigger cancellation (via error or skip) when:

| Condition | Detection | Reason |
|-----------|-----------|--------|
| Orchestration Completed | `execution.status = 'Completed'` | Activity result will not be observed |
| Orchestration Failed | `execution.status = 'Failed'` | Activity result will not be observed |
| Orchestration ContinuedAsNew | `execution.status = 'ContinuedAsNew'` | Execution is terminal; completions will be ignored |
| Orchestration Cancelled | `execution.status = 'Failed'` with Cancelled error | Activity result will not be observed |
| Instance Deleted | Instance row missing from `instances` table | Instance cleanup has occurred |
| Execution Deleted | Execution row missing from `executions` table | Execution cleanup has occurred |

**Note:** Both instance AND execution must exist and be `Running` for an activity to proceed.
If either is missing, or the execution status is anything other than `Running` (including `ContinuedAsNew`), the activity should be skipped and the worker item should be dropped (`ack_work_item(token, None)`).

### Grace Period Behavior

After cancellation is triggered:

1. **Wait for a grace period** (default: 10s) for the activity to observe cancellation and exit
2. **Regardless of whether the activity exits cleanly or not, the worker queue message MUST be dropped** using `ack_work_item(token, None)`.
    - This prevents retry loops and duplicate executions for already-terminal/missing orchestrations.
3. **If the activity does not complete within the grace period:**
    - Abort the spawned activity task (`JoinHandle::abort()`) to free worker capacity
    - Log a warning that the activity did not shut down gracefully

### Aborting Activities and User-Spawned Work

The runtime will abort the spawned activity task after the grace period.

**Important limitation:** `JoinHandle::abort()` cancels only that specific Tokio task. If an activity spawns additional tasks/threads, those will only stop if the activity propagates and honors the provided cancellation token.

**Contract for activity authors:**
- If you spawn child tasks (e.g. via `tokio::spawn()`), you MUST pass them the cancellation token and have them stop when it is cancelled.
- If you spawn OS threads (`std::thread::spawn`) or blocking work that does not observe cancellation, that work may outlive the activity task abort.

In other words: child work that ignores cancellation is **user error**; the runtime provides the cancellation signal, but cannot guarantee termination of arbitrary spawned work.

---

## Provider Contract Changes

### New Type: `ExecutionState`

```rust
/// State of the orchestration execution for an activity's parent instance.
/// Returned by provider to inform worker dispatcher of execution status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionState {
    /// Execution is running, activity should proceed.
    Running,
    
    /// Execution has reached a terminal state.
    /// Activity result will not be observed.
    Terminal {
        /// The terminal status for this execution.
        ///
        /// NOTE: For cancellation purposes, **terminal means any status that is NOT "Running"**.
        /// This includes (at minimum): "Completed", "Failed", and "ContinuedAsNew".
        status: String,
    },
    
    /// Orchestration instance or execution record is missing.
    /// This can happen after instance cleanup/deletion.
    /// Activity result will not be observed.
    Missing,
}
```

### Modified Method: `fetch_work_item`

**Current Signature:**
```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

**New Signature:**
```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError>;
```

**New Behavior:**

The provider MUST check the orchestration instance and execution status when fetching activity work items, and return the state alongside the work item.

**Expected Implementation Logic:**

```rust
async fn fetch_work_item(&self, lock_timeout: Duration, poll_timeout: Duration) 
    -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError> 
{
    // 1. Find and lock next available work item
    let (work_item, token, attempt_count) = /* existing logic */;
    
    // 2. For ActivityExecute, check orchestration state
    let orch_state = if let WorkItem::ActivityExecute { instance, execution_id, .. } = &work_item {
        let instance_exists = self.instance_exists(instance).await?;
        let execution = self.get_execution(instance, *execution_id).await?;
        
        match (instance_exists, execution) {
            (false, _) => ExecutionState::Missing,
            (true, None) => ExecutionState::Missing,
            // Terminal for cancellation purposes means: any execution status that is NOT Running.
            // This includes ContinuedAsNew.
            (true, Some(exec)) if exec.status != "Running" => {
                ExecutionState::Terminal { status: exec.status }
            }
            (true, Some(_)) => ExecutionState::Running,
        }
    } else {
        // Non-activity work items (shouldn't happen, but default to Running)
        ExecutionState::Running
    };
    
    Ok(Some((work_item, token, attempt_count, orch_state)))
}
```

### Modified Method: `renew_work_item_lock`

**Current Signature:**
```rust
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_by: Duration,
) -> Result<(), ProviderError>;
```

**New Signature:**
```rust
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_by: Duration,
) -> Result<ExecutionState, ProviderError>;
```

**New Behavior:**

The provider MUST check the orchestration execution status during lock renewal and return the state. The lock is only extended if the state is `Running`.

**Expected Implementation Logic:**

```rust
async fn renew_work_item_lock(&self, token: &str, extend_by: Duration) 
    -> Result<ExecutionState, ProviderError> 
{
    // 1. Find work item by lock token
    let work_item = self.find_work_item_by_token(token)?;
    
    if work_item.is_none() {
        return Err(ProviderError::LockNotFound);
    }
    
    // 2. Extract instance_id and execution_id from work item
    let (instance_id, execution_id) = match &work_item.unwrap() {
        WorkItem::ActivityExecute { instance, execution_id, .. } => (instance, execution_id),
        _ => return Err(ProviderError::InvalidWorkItem),
    };
    
    // 3. Check orchestration state
    let instance_exists = self.instance_exists(instance_id).await?;
    let execution = self.get_execution(instance_id, *execution_id).await?;
    
    let orch_state = match (instance_exists, execution) {
        (false, _) => ExecutionState::Missing,
        (true, None) => ExecutionState::Missing,
        // Terminal for cancellation purposes means: any execution status that is NOT Running.
        // This includes ContinuedAsNew.
        (true, Some(exec)) if exec.status != "Running" => {
            ExecutionState::Terminal { status: exec.status }
        }
        (true, Some(_)) => ExecutionState::Running,
    };
    
    // 4. Only extend lock if running
    if matches!(orch_state, ExecutionState::Running) {
        self.extend_lock(token, extend_by).await?;
    }
    
    Ok(orch_state)
}
```

### Modified Method: `ack_work_item`

**Current Signature:**
```rust
async fn ack_work_item(
    &self,
    token: &str,
    completion: WorkItem,
) -> Result<(), ProviderError>;
```

**New Signature:**
```rust
async fn ack_work_item(
    &self,
    token: &str,
    completion: Option<WorkItem>,
) -> Result<(), ProviderError>;
```

**New Behavior:**

The `completion` parameter is now optional.
- `Some(item)`: Deletes the worker item and enqueues the completion item to the orchestrator queue (standard behavior).
- `None`: Deletes the worker item but **does not enqueue anything** (used when orchestration is terminal or missing).

**Key Design Principle:**

Provider just reports state. Worker dispatcher decides what to do:
- `Running` → proceed normally
- `Terminal` → skip/cancel activity
- `Missing` → skip/cancel activity

## ActivityContext Changes

### New Fields

```rust
pub struct ActivityContext {
    // ... existing fields ...
    
    /// Cancellation token for cooperative cancellation.
    /// Triggered when the parent orchestration reaches a terminal state.
    cancellation_token: CancellationToken,
}
```

### New Methods

```rust
impl ActivityContext {
    /// Check if cancellation has been requested.
    ///
    /// Returns `true` if the parent orchestration has completed, failed,
    /// or been cancelled. Activities can use this for cooperative cancellation.
    ///
    /// # Example
    ///
    /// ```ignore
    /// for item in items {
    ///     if ctx.is_cancellation_requested() {
    ///         return Err("Activity cancelled".into());
    ///     }
    ///     process(item).await;
    /// }
    /// ```
    pub fn is_cancellation_requested(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }
    
    /// Returns a future that completes when cancellation is requested.
    ///
    /// Use with `tokio::select!` for interruptible activities.
    ///
    /// # Example
    ///
    /// ```ignore
    /// tokio::select! {
    ///     result = do_work() => return result,
    ///     _ = ctx.cancelled() => return Err("Cancelled".into()),
    /// }
    /// ```
    pub async fn cancelled(&self) {
        self.cancellation_token.cancelled().await
    }
    
    /// Get a clone of the cancellation token for use in spawned tasks.
    ///
    /// If your activity spawns child tasks with `tokio::spawn()`, you should
    /// pass them this token so they can also respond to cancellation.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let token = ctx.cancellation_token();
    /// let handle = tokio::spawn(async move {
    ///     loop {
    ///         tokio::select! {
    ///             _ = do_work() => {}
    ///             _ = token.cancelled() => break,
    ///         }
    ///     }
    /// });
    /// ```
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation_token.clone()
    }
}
```

---

## Worker Dispatcher Changes

### Renamed: Lock Renewal Task → Activity Manager Task

The existing `spawn_lock_renewal_task` function is renamed and enhanced:

```rust
fn spawn_activity_manager(
    store: Arc<dyn Provider>,
    token: String,
    lock_timeout: Duration,
    buffer: Duration,
    shutdown: Arc<AtomicBool>,
    cancel_token: CancellationToken,
) -> JoinHandle<()> {
    let renewal_interval = calculate_renewal_interval(lock_timeout, buffer);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // Skip first immediate tick

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::debug!(lock_token = %token, "Activity manager stopping due to shutdown");
                        break;
                    }

                    match store.renew_work_item_lock(&token, lock_timeout).await {
                        Ok(ExecutionState::Running) => {
                            tracing::trace!(lock_token = %token, "Lock renewed");
                        }
                        Ok(ExecutionState::Terminal { status }) => {
                            tracing::info!(
                                lock_token = %token,
                                status = %status,
                                "Orchestration terminal, triggering activity cancellation"
                            );
                            cancel_token.cancel();
                            break;
                        }
                        Ok(ExecutionState::Missing) => {
                            tracing::info!(
                                lock_token = %token,
                                "Orchestration missing (deleted), triggering activity cancellation"
                            );
                            cancel_token.cancel();
                            break;
                        }
                        Err(e) if e.is_retryable() => {
                            // Transient provider error (DB busy/locked, etc.)
                            // Keep trying so we don't self-induce lock loss under load.
                            tracing::warn!(
                                lock_token = %token,
                                error = %e,
                                "Lock renewal failed (retryable), will retry"
                            );
                            continue;
                        }
                        Err(e) => {
                            // Non-retryable implies: token invalid/expired/acked or other permanent failure.
                            // Treat as "lost lease" to avoid wasted/duplicate work.
                            tracing::warn!(
                                lock_token = %token,
                                error = %e,
                                "Lock renewal failed (non-retryable), triggering cancellation"
                            );
                            cancel_token.cancel();
                            break;
                        }
                    }
                }
            }
        }
        
        tracing::debug!(lock_token = %token, "Activity manager stopped");
    })
}
```

### Activity Execution with Cancellation Handling

```rust
// Fetch work item - now includes orchestration state
match fetch_result {
    Ok(Some((item, token, attempt_count, orch_state))) => {
        match item {
            WorkItem::ActivityExecute { instance, execution_id, id, name, input } => {
                // Check orchestration state BEFORE starting activity
                match orch_state {
                    ExecutionState::Terminal { status } => {
                        tracing::debug!(
                            instance = %instance,
                            activity_id = %id,
                            status = %status,
                            "Skipping activity: orchestration already terminal"
                        );
                        // Ack with None - deletes work item without notifying orchestrator
                        let _ = rt.history_store.ack_work_item(&token, None).await;
                        continue;
                    }
                    ExecutionState::Missing => {
                        tracing::debug!(
                            instance = %instance,
                            activity_id = %id,
                            "Skipping activity: orchestration instance/execution missing"
                        );
                        // Ack with None - deletes work item without notifying orchestrator
                        let _ = rt.history_store.ack_work_item(&token, None).await;
                        continue;
                    }
                    ExecutionState::Running => {
                        // Proceed with activity execution
                    }
                }
                
                // Create cancellation token for this activity
                let cancel_token = CancellationToken::new();

                // Spawn activity manager
                let manager_handle = spawn_activity_manager(
                    Arc::clone(&rt.history_store),
                    token.clone(),
                    rt.options.worker_lock_timeout,
                    rt.options.worker_lock_renewal_buffer,
                    Arc::clone(&shutdown),
                    cancel_token.clone(),
                );

                // Create context with cancellation token
                let activity_ctx = ActivityContext::new_with_cancellation(
                    instance.clone(),
                    execution_id,
                    orch_name,
                    orch_version,
                    name.clone(),
                    id,
                    worker_id.clone(),
                    cancel_token.clone(),
                );

                // Spawn activity execution
                let activity_handle = tokio::spawn({
                    let handler = handler.clone();
                    let input = input.clone();
                    async move { handler.invoke(activity_ctx, input).await }
                });

                // Wait for completion OR cancellation
                tokio::select! {
                    result = &mut activity_handle => {
                        // Normal completion
                        manager_handle.abort();
                        
                        match result {
                            Ok(Ok(output)) => {
                                // Success - ack with ActivityCompleted
                            }
                            Ok(Err(error)) => {
                                // Application error - ack with ActivityFailed
                            }
                            Err(join_error) => {
                                // Panic - ack with ActivityFailed
                            }
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        // Orchestration terminated during execution
                        manager_handle.abort();
                        
                        tracing::info!(
                            instance = %instance,
                            activity_id = %id,
                            activity_name = %name,
                            "Orchestration terminated, waiting up to 10s for activity to complete"
                        );
                        
                        // Grace period for graceful shutdown
                        match tokio::time::timeout(
                            rt.options.activity_cancellation_grace_period,
                            activity_handle
                        ).await {
                            Ok(Ok(Ok(output))) => {
                                tracing::info!(
                                    instance = %instance,
                                    activity_id = %id,
                                    "Activity completed gracefully after cancellation"
                                );
                                // Drop worker message without notifying orchestrator
                                let _ = rt.history_store.ack_work_item(&token, None).await;
                            }
                            Ok(Ok(Err(error))) => {
                                tracing::info!(
                                    instance = %instance,
                                    activity_id = %id,
                                    error = %error,
                                    "Activity failed gracefully after cancellation"
                                );
                                // Drop worker message without notifying orchestrator
                                let _ = rt.history_store.ack_work_item(&token, None).await;
                            }
                            Ok(Err(join_error)) => {
                                tracing::warn!(
                                    instance = %instance,
                                    activity_id = %id,
                                    error = %join_error,
                                    "Activity panicked during cancellation grace period"
                                );
                                // Drop worker message without notifying orchestrator
                                let _ = rt.history_store.ack_work_item(&token, None).await;
                            }
                            Err(_timeout) => {
                                tracing::warn!(
                                    instance = %instance,
                                    activity_id = %id,
                                    activity_name = %name,
                                    "Activity did not complete within grace period; aborting activity task"
                                );
                                // Abort the activity task to free worker capacity
                                // NOTE: child tasks/threads spawned by the activity may outlive this abort
                                // if they do not observe the cancellation token (user error).
                                // Drop worker message without notifying orchestrator
                                // (prevents retries/duplicates for terminal/missing orchestrations).
                                activity_handle.abort();
                                let _ = rt.history_store.ack_work_item(&token, None).await;
                            }
                        }
                    }
                }
            }
            // ... other work items ...
        }
    }
    // ... error handling ...
}
```

---

## Configuration Options

### New RuntimeOptions Field (Optional)

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    /// Grace period for activity cancellation.
    ///
    /// When an orchestration reaches a terminal state, in-flight activities
    /// are notified via their cancellation token. This setting controls how
    /// long to wait for activities to complete gracefully before moving on.
    ///
    /// Default: 10 seconds
    pub activity_cancellation_grace_period: Duration,
}
```

---

## Test Plan

### Unit Tests

#### 1. Provider Tests (provider_validations)

**Test: `fetch_returns_running_state_for_active_orchestration`**
```
Given: An activity work item in queue, orchestration status = "Running"
When: fetch_work_item is called
Then: Returns (work_item, token, count, ExecutionState::Running)
```

**Test: `fetch_returns_terminal_state_when_orchestration_completed`**
```
Given: An activity work item in queue, orchestration status = "Completed"
When: fetch_work_item is called
Then: Returns (work_item, token, count, ExecutionState::Terminal { status: "Completed" })
```

**Test: `fetch_returns_terminal_state_when_orchestration_failed`**
```
Given: An activity work item in queue, orchestration status = "Failed"
When: fetch_work_item is called
Then: Returns (work_item, token, count, ExecutionState::Terminal { status: "Failed" })
```

**Test: `fetch_returns_terminal_state_when_orchestration_continued_as_new`**
```
Given: An activity work item in queue, orchestration status = "ContinuedAsNew"
When: fetch_work_item is called
Then: Returns (work_item, token, count, ExecutionState::Terminal { status: "ContinuedAsNew" })
```

**Test: `fetch_treats_any_non_running_status_as_terminal`**
```
Given: An activity work item in queue, orchestration status = "<anything other than Running>"
When: fetch_work_item is called
Then: Returns ExecutionState::Terminal { status: <that status> }
```

**Test: `fetch_returns_missing_state_when_instance_deleted`**
```
Given: An activity work item in queue, instance row missing
When: fetch_work_item is called
Then: Returns (work_item, token, count, ExecutionState::Missing)
```

**Test: `fetch_returns_missing_state_when_execution_deleted`**
```
Given: An activity work item in queue, execution row missing but instance exists
When: fetch_work_item is called
Then: Returns (work_item, token, count, ExecutionState::Missing)
```

**Test: `renew_returns_running_when_orchestration_active`**
```
Given: An activity work item is locked, orchestration status = "Running"
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Running)
And: Lock is extended
```

**Test: `renew_returns_terminal_when_orchestration_completed`**
```
Given: An activity work item is locked, orchestration status = "Completed"
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Terminal { status: "Completed" })
And: Lock is NOT extended
```

**Test: `renew_returns_terminal_when_orchestration_failed`**
```
Given: An activity work item is locked, orchestration status = "Failed"
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Terminal { status: "Failed" })
And: Lock is NOT extended
```

**Test: `renew_returns_terminal_when_orchestration_continued_as_new`**
```
Given: An activity work item is locked, orchestration status = "ContinuedAsNew"
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Terminal { status: "ContinuedAsNew" })
And: Lock is NOT extended
```

**Test: `renew_returns_missing_when_instance_deleted`**
```
Given: An activity work item is locked, instance row deleted from instances table
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Missing)
And: Lock is NOT extended
```

**Test: `renew_returns_missing_when_execution_deleted`**
```
Given: An activity work item is locked, execution row deleted but instance exists
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Missing)
And: Lock is NOT extended
```

**Test: `renew_succeeds_when_orchestration_running`**
```
Given: An activity work item is locked, orchestration status = "Running"
When: renew_work_item_lock is called
Then: Returns Ok(ExecutionState::Running)
And: Lock is extended
```

**Test: `ack_work_item_none_deletes_without_enqueue`**
```
Given: A locked worker_queue item with lock_token = token
When: ack_work_item(token, None) is called
Then: worker_queue row is deleted
And: no message is enqueued into orchestrator_queue
```

**Test: `ack_work_item_some_deletes_and_enqueues`**
```
Given: A locked worker_queue item with lock_token = token
When: ack_work_item(token, Some(ActivityCompleted|ActivityFailed)) is called
Then: worker_queue row is deleted
And: exactly one message is enqueued into orchestrator_queue for that instance
```

### Integration Tests

#### 2. Cancellation Token Tests

**Test: `activity_receives_cancellation_token`**
```
Given: An orchestration that schedules an activity
When: Activity executes
Then: ActivityContext.is_cancellation_requested() returns false initially
```

**Test: `cancellation_token_triggered_on_orchestration_cancel`**
```
Given: An orchestration with a long-running activity (checks cancellation in loop)
When: Orchestration is cancelled via client.cancel_instance()
Then: Activity's ctx.is_cancellation_requested() becomes true within 2 renewal cycles
And: Activity can exit early
```

**Test: `cancellation_token_triggered_on_orchestration_complete`**
```
Given: An orchestration that completes before activity finishes
When: Orchestration completes (e.g., select2 where activity loses)
Then: Activity's cancellation token is triggered within 2 renewal cycles
```

**Test: `cancellation_token_triggered_on_continue_as_new`**
```
Given: An orchestration execution transitions to ContinuedAsNew while an activity is in-flight
When: The next renewal observes execution.status = "ContinuedAsNew"
Then: Activity cancellation token is triggered
```

#### 3. Grace Period Tests

**Test: `activity_completes_within_grace_period`**
```
Given: An activity that responds to cancellation within 5s
When: Orchestration is cancelled
Then: Activity completes gracefully
And: No warnings about grace period timeout
```

**Test: `activity_exceeds_grace_period`**
```
Given: An activity that ignores cancellation (sleeps for 30s)
When: Orchestration is cancelled
Then: Warning is logged about grace period timeout
And: Worker aborts the activity task
And: worker_queue message is dropped via ack_work_item(token, None)
And: Worker moves on to next work item
```

**Test: `activity_completes_or_fails_after_cancellation_always_drops_message`**
```
Given: An activity is cancelled mid-flight
When: Activity completes successfully OR returns an error OR panics during the grace window
Then: worker_queue message is dropped via ack_work_item(token, None)
And: no completion is enqueued to orchestrator_queue
```

#### 4. Edge Case Tests

**Test: `activity_completes_before_cancellation_check`**
```
Given: An activity that completes in 100ms
When: Orchestration is cancelled at t=50ms
Then: Activity completion is processed normally
And: Cancellation is a no-op
```

**Test: `terminal_or_missing_activity_is_never_started`**
```
Given: A pending ActivityExecute message exists
And: ExecutionState is Terminal or Missing at fetch time
When: Worker dispatcher processes the message
Then: Activity handler is NOT invoked
And: worker_queue message is dropped via ack_work_item(token, None)
And: no completion is enqueued
```

**Test: `renew_retryable_errors_do_not_cancel`**
```
Given: An in-flight activity with a renewal manager
When: renew_work_item_lock intermittently returns ProviderError { retryable: true }
Then: Manager keeps retrying renewals
And: cancellation token is NOT triggered by the retryable error alone
```

**Test: `renew_non_retryable_error_cancels_and_aborts`**
```
Given: An in-flight activity with a renewal manager
When: renew_work_item_lock returns ProviderError { retryable: false } (lost/invalid lease)
Then: cancellation token is triggered
And: after grace period the activity task is aborted
And: worker_queue message is dropped via ack_work_item(token, None)
```

**Test: `multiple_activities_cancelled_together`**
```
Given: An orchestration with 3 parallel activities
When: Orchestration is cancelled
Then: All 3 activities receive cancellation token trigger
And: Each completes or times out independently
```

**Test: `activity_spawns_child_tasks_with_cancellation`**
```
Given: An activity that spawns child tasks and passes cancellation token
When: Orchestration is cancelled
Then: Child tasks also observe cancellation
And: All tasks exit gracefully
```

**Test: `activity_spawns_child_tasks_without_cancellation_is_user_error`**
```
Given: An activity spawns child tasks/threads without passing cancellation token
When: Orchestration is cancelled
Then: Activity task may be aborted after grace period
And: child work may continue running (documented as user error)
```

### Stress Tests

#### 5. Concurrency Tests

**Test: `high_concurrency_cancellation`**
```
Given: 100 orchestrations each with 5 activities
When: All 100 orchestrations are cancelled simultaneously
Then: All activities receive cancellation within expected time
And: No deadlocks or resource leaks
```

**Test: `cancellation_during_high_load`**
```
Given: Worker dispatcher at full concurrency (all workers busy)
When: Orchestrations are cancelled
Then: Cancellation propagates despite load
And: Worker throughput recovers after cancellations complete
```

---

## Migration Guide

### For Provider Implementers

1. **Update `renew_work_item_lock`** to check execution status
2. **Update `ack_work_item`** to accept `Option<WorkItem>` (handle `None` by deleting without enqueue)
3. **Add `OrchestrationTerminal` error handling** to error type
4. **Optimize query** to do status check in same database round-trip as lock renewal

### For Activity Authors

Activities continue to work without changes. To benefit from cancellation:

1. **Check cancellation in loops:**
   ```rust
   for item in items {
       if ctx.is_cancellation_requested() {
           return Err("Cancelled".into());
       }
       process(item).await;
   }
   ```

2. **Use select for interruptible waits:**
   ```rust
   tokio::select! {
       result = long_operation() => result,
       _ = ctx.cancelled() => Err("Cancelled".into()),
   }
   ```

3. **Propagate token to spawned tasks:**
   ```rust
   let token = ctx.cancellation_token();
   tokio::spawn(async move {
       while !token.is_cancelled() {
           do_work().await;
       }
   });
   ```

**Important:** If you spawn additional tasks/threads and do not pass them the cancellation token, they may outlive the activity's cancellation/abort. This is user error.

---

## Documentation Follow-ups (Post-Implementation)

Once this proposal is implemented, we should update the docs to reflect the new provider/runtime behavior.

### Provider Docs

- Update [docs/provider-implementation-guide.md](docs/provider-implementation-guide.md) to cover:
    - `ExecutionState` checks during worker dequeue/renewal
    - `ack_work_item(token, None)` semantics (drop worker message without enqueuing)
    - `ProviderError.retryable` expectations for renewal behavior
    - `status != "Running"` treated as terminal (includes `ContinuedAsNew`)

### Provider Testing Docs

- Update [docs/provider-testing-guide.md](docs/provider-testing-guide.md) to include:
    - New/updated validation cases for `ExecutionState` and `ack_work_item(None)`
    - Renewal retry behavior: retryable errors must not induce cancellation
    - Abort-after-grace semantics (and what can/can't be guaranteed)

### User Guide

- Add a dedicated section to the user guide (e.g. [docs/observability-guide.md](docs/observability-guide.md) or a new doc under `docs/`) on **how to write cancellable activities**:
    - Using `tokio::select!` with `ctx.cancelled()` for interruptible waits
    - Propagating the cancellation token to spawned tasks
    - Avoiding non-cancellable blocking work, or structuring it with periodic cancellation checks
    - Explicitly stating that spawned work that ignores cancellation may outlive abort (user responsibility)

---

## Concurrency Considerations

### Execution State Check is Eventually Consistent

The execution state check in both `fetch_work_item` and `renew_work_item_lock` is **not transactional** with respect to orchestration state changes. This is intentional.

### Race Scenarios

**1. Race on `fetch_work_item`:**
```
Timeline:
  T0: Provider checks execution status → Running
  T1: Orchestration completes/fails (status → Completed)
  T2: Provider returns (WorkItem, token, count, ExecutionState::Running)
  T3: Worker dispatcher receives Running, starts activity
```
**Impact:** Activity starts, runs for up to one renewal cycle (~25s) before next check catches it.

**2. Race on `renew_work_item_lock`:**
```
Timeline:
  T0: Provider checks execution status → Running, extends lock
  T1: Orchestration completes/fails (status → Completed)  
  T2: Provider returns Ok(ExecutionState::Running)
  T3: Activity manager continues, schedules next renewal
```
**Impact:** Activity continues for one more renewal cycle (~25s).

**3. Race with deletion:**
```
Timeline:
  T0: Provider checks instance/execution exists → true
  T1: Instance/execution deleted (cleanup job)
  T2: Provider returns ExecutionState::Running
```
**Impact:** Same - one extra cycle before `Missing` is detected.

### Why This Is Safe

**Terminal states are final.** Once an execution is `Completed` or `Failed`, it never transitions back to `Running`. The race only goes one direction:

| Actual State Transition | What Provider Sees | Impact |
|-------------------------|-------------------|--------|
| Running → Running | ✅ Correct | None |
| Terminal → Terminal | ✅ Correct | None |
| **Running → Terminal (race)** | Sees Running | Miss one cycle (~25s) |
| Terminal → Running | ❌ Impossible | N/A |

### Worst Case Delay

```
Renewal interval: ~25s (default)
Worst case: Orchestration terminates 1ms after renewal check
Additional delay: ~25 seconds

Total time to cancellation: up to ~25s instead of ~0s
```

### Why Not Use Locking?

A transactional approach would require locking:
```sql
BEGIN;
  SELECT status FROM executions WHERE ... FOR UPDATE;  -- Lock row
  UPDATE worker_queue SET locked_until = ... WHERE ...;
COMMIT;
```

This would be worse because:
- **Contention:** Orchestration dispatcher and worker dispatcher would contend for the same execution row lock
- **Deadlock risk:** Multiple dispatchers acquiring locks in different orders
- **Performance:** Every lock renewal becomes a heavier operation
- **Marginal benefit:** All this complexity to save ~25s in a rare race condition

### Conclusion

**Acceptable trade-off:**
- Eventually consistent (within one renewal cycle)
- No false positives (never see Terminal when actually Running)
- No locking/contention overhead
- Simpler implementation
- Worst case: ~25s additional delay before cancellation is detected

---

## Limitations & Future Work

### Current Limitations

1. **Cancellation latency:** Up to one renewal interval (~25s with defaults)
2. **Best-effort termination:** The runtime aborts the activity task after the grace period, but user-spawned work that ignores the cancellation token may outlive the abort
3. **Pending activities not cancelled:** Only in-flight activities are notified

### Future Enhancements

1. **Cancel pending activities in queue:** Mark queued activities as cancelled before they start
2. **Shorter cancellation check interval:** Separate cancellation poll from lock renewal
3. **Activity-level cancellation messages:** Explicit cancel messages for immediate notification

---

## Appendix: Cancellation Latency Analysis

With default settings:
- `worker_lock_timeout`: 30 seconds
- `worker_lock_renewal_buffer`: 5 seconds
- Renewal interval: 25 seconds

**Worst case latency:** 25 seconds (orchestration terminates right after renewal)
**Best case latency:** 0 seconds (orchestration terminates right before renewal)
**Average latency:** ~12.5 seconds

To reduce latency, users can configure shorter timeouts:
```rust
RuntimeOptions {
    worker_lock_timeout: Duration::from_secs(10),
    worker_lock_renewal_buffer: Duration::from_secs(2),
    // Renewal every 8 seconds → avg latency ~4 seconds
    ..Default::default()
}
```

Trade-off: Shorter intervals = more database queries for lock renewal.
