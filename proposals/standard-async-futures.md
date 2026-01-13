# Proposal: Standard Async/Await Support

**Status:** Draft  
**Author:** Copilot  
**Created:** 2026-01-13  
**Goal:** Enable standard Rust `async/await`, `futures::join!`, and `futures::select!` in orchestrations.

---

## 1. Summary

This proposal replaces duroxide's current poll-driven validation model with a schedule-time validation model. The key changes:

1. **Validation at schedule-time**: History matching happens when `ctx.schedule_*()` is called, not during `poll()`
2. **Separate future types**: `ActivityFuture`, `TimerFuture`, etc. with direct output types (no `.into_activity()`)
3. **Drop-based cancellation**: Dropping a future records `Action::Cancel`
4. **FIFO completion ordering**: `poll()` only returns `Ready` when this completion is next in history order
5. **Dehydration flag**: Prevents cancellation when orchestration suspends normally

These changes make standard `futures::select!` and `futures::join!` safe to use.

---

## 2. The Problem

### Current Architecture

```rust
// Today: DurableFuture::poll() does everything
fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<DurableOutput> {
    let mut inner = self.ctx.inner.lock().unwrap();
    
    // 1. Scan history for next unclaimed scheduling event
    // 2. Validate it matches our parameters
    // 3. Claim the event_id
    // 4. Scan for completion
    // 5. Check FIFO ordering
    // 6. Return Ready or Pending
    
    // ~100 lines per Kind variant
}
```

Problems:
- **Opaque futures**: Runtime can't see inside `async {}` blocks to cancel activities
- **Custom combinators required**: `select2`/`join` must manually inspect futures
- **Complex poll logic**: Each `Kind` variant has ~100 lines of history scanning

### Why Standard Select Seems Unsafe

`futures::select!` polls futures in pseudo-random order when multiple are ready. This appears non-deterministic, but with FIFO enforcement in `poll()`, the winner is always the one with the earliest completion—regardless of poll order.

---

## 3. Detailed Design

### 3.1. Extend `Action` for Cancellation

```rust
// src/lib.rs
pub enum Action {
    CallActivity { 
        scheduling_event_id: u64, 
        name: String, 
        input: String,
        retry_policy: Option<RetryPolicy>,
        options: Option<ActivityOptions>,
    },
    CreateTimer { scheduling_event_id: u64, fire_at_ms: u64 },
    WaitExternal { scheduling_event_id: u64, name: String },
    StartSubOrchestration { 
        scheduling_event_id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
    },
    ContinueAsNew { ... },
    SystemCall { ... },
    
    // NEW: Explicit cancellation (by scheduling_event_id)
    Cancel { 
        scheduling_event_id: u64,
    },
    
    // NEW: Cancel external event subscription (by name)
    CancelExternal {
        event_name: String,
    },
}
```

### 3.2. Context State Changes

```rust
// src/lib.rs
struct CtxInner {
    // === Scheduling Cursor ===
    /// Ordered list of scheduling events from history (extracted at turn start)
    scheduling_events: Vec<SchedulingEvent>,
    /// Current position in scheduling_events (advances with each schedule call)
    cursor: usize,
    
    // === Completion State ===
    /// Map: scheduling_event_id → completion result (populated at turn start)
    completions: HashMap<u64, CompletionResult>,
    /// Set of completion event_ids that have been consumed (for FIFO)
    consumed_completions: HashSet<u64>,
    
    // === External Event Tracking (name-based matching) ===
    /// Map: event_name → list of completion payloads in arrival order
    external_completions: HashMap<String, VecDeque<String>>,
    /// Map: event_name → consumption count (how many have been consumed)
    external_consumption_count: HashMap<String, usize>,
    
    // === Action Tracking ===
    /// Actions generated this turn (new schedules, cancels)
    pending_actions: Vec<Action>,
    /// Next event_id to assign for new events
    next_event_id: u64,
    
    // === Dehydration ===
    /// When true, Drop impls do not record cancellation
    dehydrating: bool,
    
    // === Metadata ===
    execution_id: u64,
    instance_id: String,
    // ... other existing fields ...
    
    // REMOVED: claimed_scheduling_events, cancelled_source_ids, wakers
}

struct SchedulingEvent {
    event_id: u64,
    kind: SchedulingKind,
}

enum SchedulingKind {
    Activity { 
        name: String, 
        input: String,
        retry_policy: Option<RetryPolicy>,  // For schedule_activity_with_retry
        options: Option<ActivityOptions>,    // For schedule_activity_with_options
    },
    Timer { fire_at_ms: u64 },
    External { name: String },
    SubOrchestration { 
        name: String, 
        version: Option<String>,
        instance: String, 
        input: String,
    },
    SystemCall { op: String, value: String },
}

enum CompletionResult {
    Activity(Result<String, String>),
    Timer,
    External(String),
    SubOrchestration(Result<String, String>),
}
```

### 3.3. Separate Future Types

```rust
// src/futures.rs

/// Future for activity completion
pub struct ActivityFuture {
    scheduling_event_id: u64,
    ctx: OrchestrationContext,
    consumed: Cell<bool>,
}

impl Future for ActivityFuture {
    type Output = Result<String, String>;  // Direct type, not DurableOutput
    
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // See §3.6 for implementation
    }
}

impl FusedFuture for ActivityFuture {
    fn is_terminated(&self) -> bool {
        self.consumed.get()
    }
}

impl Drop for ActivityFuture {
    fn drop(&mut self) {
        // See §3.7 for implementation
    }
}

/// Future for timer completion
pub struct TimerFuture {
    scheduling_event_id: u64,
    ctx: OrchestrationContext,
    consumed: Cell<bool>,
}

impl Future for TimerFuture {
    type Output = ();  // Timers return unit
    // ... similar impl
}

/// Future for external event
/// Note: Uses name-based matching with consumption order (not event_id)
pub struct ExternalFuture {
    event_name: String,              // Match by name, not event_id
    consumption_index: usize,        // Which occurrence (0 = first, 1 = second, etc.)
    ctx: OrchestrationContext,
    consumed: Cell<bool>,
}

impl Future for ExternalFuture {
    type Output = String;  // External events return payload
    
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            return Poll::Pending;  // FusedFuture behavior
        }
        
        let inner = self.ctx.inner.lock().unwrap();
        
        // Name-based matching with consumption order
        if let Some(completions) = inner.external_completions.get(&self.event_name) {
            if let Some(data) = completions.get(self.consumption_index) {
                self.consumed.set(true);
                return Poll::Ready(data.clone());
            }
        }
        
        Poll::Pending
    }
}

/// Future for sub-orchestration completion
pub struct SubOrchestrationFuture {
    scheduling_event_id: u64,
    ctx: OrchestrationContext,
    consumed: Cell<bool>,
}

impl Future for SubOrchestrationFuture {
    type Output = Result<String, String>;
    // Same pattern as ActivityFuture: lookup in completions map + FIFO check
}

/// Future for system call (trace, guid, utcnow)
pub struct SystemCallFuture {
    scheduling_event_id: u64,
    ctx: OrchestrationContext,
    consumed: Cell<bool>,
}

impl Future for SystemCallFuture {
    type Output = String;
    // Same pattern: lookup + FIFO
}
```

### 3.4. Schedule-Time Validation

Validation moves from `poll()` to `schedule_*()`. This is the critical change.

```rust
// src/lib.rs
impl OrchestrationContext {
    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> ActivityFuture {
        let name = name.into();
        let input = input.into();
        let mut inner = self.inner.lock().unwrap();
        
        let scheduling_event_id = if inner.cursor < inner.scheduling_events.len() {
            // === REPLAY PATH ===
            let expected = &inner.scheduling_events[inner.cursor];
            
            // Determinism check
            match &expected.kind {
                SchedulingKind::Activity { name: hist_name, input: hist_input } => {
                    if *hist_name != name || *hist_input != input {
                        panic!(
                            "Non-determinism detected at cursor {}: \
                             history has Activity({:?}, {:?}) but code scheduled Activity({:?}, {:?})",
                            inner.cursor, hist_name, hist_input, name, input
                        );
                    }
                }
                other => {
                    panic!(
                        "Non-determinism detected at cursor {}: \
                         history has {:?} but code scheduled Activity",
                        inner.cursor, other
                    );
                }
            }
            
            inner.cursor += 1;
            expected.event_id
            
        } else {
            // === NEW EXECUTION PATH ===
            let event_id = inner.next_event_id;
            inner.next_event_id += 1;
            
            inner.pending_actions.push(Action::CallActivity {
                scheduling_event_id: event_id,
                name: name.clone(),
                input: input.clone(),
                retry_policy: None,
                options: None,
            });
            
            inner.cursor += 1;
            event_id
        };
        
        ActivityFuture {
            scheduling_event_id,
            ctx: self.clone(),
            consumed: Cell::new(false),
        }
    }
    
    /// Schedule activity with retry policy
    pub fn schedule_activity_with_retry(
        &self, 
        name: impl Into<String>, 
        input: impl Into<String>,
        retry_policy: RetryPolicy,
    ) -> ActivityFuture {
        // Same as schedule_activity but with retry_policy in Action
        // Determinism check also validates retry_policy matches history
        // ...
    }
    
    /// Schedule activity with options (timeout, cancellation grace period, etc.)
    pub fn schedule_activity_with_options(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        options: ActivityOptions,
    ) -> ActivityFuture {
        // Same as schedule_activity but with options in Action
        // Determinism check also validates options match history
        // ...
    }
    
    pub fn schedule_timer(&self, delay: Duration) -> TimerFuture {
        // Similar pattern: validate or create, return TimerFuture
    }
    
    pub fn schedule_wait(&self, name: impl Into<String>) -> ExternalFuture {
        let name = name.into();
        let mut inner = self.inner.lock().unwrap();
        
        // External events use name-based matching with consumption order
        // Track how many times we've scheduled this name
        let consumption_index = inner.external_consumption_count
            .entry(name.clone())
            .or_insert(0);
        let index = *consumption_index;
        *consumption_index += 1;
        
        // Cursor still advances for determinism (validates External in history)
        // ...validation similar to activity...
        
        inner.cursor += 1;
        
        ExternalFuture {
            event_name: name,
            consumption_index: index,
            ctx: self.clone(),
            consumed: Cell::new(false),
        }
    }
    
    pub fn schedule_sub_orchestration(
        &self,
        name: impl Into<String>,
        instance_id: impl Into<String>,
        input: impl Into<String>,
    ) -> SubOrchestrationFuture {
        // Same pattern as activity: validate or create, return SubOrchestrationFuture
        // ...
    }
    
    pub fn schedule_sub_orchestration_with_version(
        &self,
        name: impl Into<String>,
        version: impl Into<String>,
        instance_id: impl Into<String>,
        input: impl Into<String>,
    ) -> SubOrchestrationFuture {
        // Same with version field populated
        // ...
    }
}
```

### 3.5. External Event Name-Based Matching

External events are matched by **name + consumption order**, not by scheduling_event_id. This is because:

1. Multiple `schedule_wait("SameName")` calls may exist
2. External events arrive by name (e.g., `client.raise_event("instance", "ApprovalEvent", "approved")`)
3. First `schedule_wait("ApprovalEvent")` gets the first arrival, second gets the second, etc.

**Context initialization for external events:**
```rust
fn initialize_context(ctx: &OrchestrationContext) {
    // ... other initialization ...
    
    // Build external completions map: name → list of payloads in arrival order
    inner.external_completions.clear();
    for event in &inner.history {
        if let EventKind::ExternalEvent { name, data, .. } = &event.kind {
            inner.external_completions
                .entry(name.clone())
                .or_insert_with(VecDeque::new)
                .push_back(data.clone());
        }
    }
    
    // Reset consumption counters
    inner.external_consumption_count.clear();
}
```

**Determinism guarantee:**
- If code calls `schedule_wait("A")` twice, it gets consumption_index 0 and 1
- On replay, same calls → same indices → same matching
- Order of arrivals is preserved in history

### 3.6. Poll with FIFO Enforcement

Poll is now simple—just a lookup with FIFO ordering:

```rust
impl Future for ActivityFuture {
    type Output = Result<String, String>;
    
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.consumed.get() {
            // Already returned Ready, FusedFuture behavior
            return Poll::Pending;
        }
        
        let inner = self.ctx.inner.lock().unwrap();
        
        // Check if our completion exists
        let completion = match inner.completions.get(&self.scheduling_event_id) {
            Some(CompletionResult::Activity(result)) => result.clone(),
            _ => return Poll::Pending,  // No completion yet
        };
        
        // Get our completion's event_id from history
        let our_completion_event_id = inner.get_completion_event_id(self.scheduling_event_id);
        
        if let Some(our_event_id) = our_completion_event_id {
            // FIFO check: can we consume this completion?
            if inner.can_consume(our_event_id) {
                drop(inner);  // Release lock before mutating
                
                let mut inner = self.ctx.inner.lock().unwrap();
                inner.consumed_completions.insert(our_event_id);
                drop(inner);
                
                self.consumed.set(true);
                return Poll::Ready(completion);
            }
        }
        
        Poll::Pending
    }
}

impl CtxInner {
    /// Returns true if all completions with event_id < target have been consumed
    fn can_consume(&self, target_event_id: u64) -> bool {
        // Check all completion events in history
        for event in &self.completion_events {
            if event.event_id < target_event_id {
                if !self.consumed_completions.contains(&event.event_id) {
                    // An earlier completion hasn't been consumed yet
                    return false;
                }
            }
        }
        true
    }
}
```

**Why FIFO makes random poll order safe:**

Given futures $F_1, F_2, \ldots, F_N$ with completions at event IDs $e_1, e_2, \ldots$:

1. Only the future with $e_{\min} = \min\{e_i\}$ can return `Ready`
2. All others are blocked by the FIFO check
3. Poll order is irrelevant—winner is determined by history

Mathematical proof: See discussion in design notes.

### 3.7. Drop-Based Cancellation with Dehydration Guard

```rust
impl Drop for ActivityFuture {
    fn drop(&mut self) {
        // Already completed - nothing to cancel
        if self.consumed.get() {
            return;
        }
        
        // Use try_lock to avoid panic if lock is poisoned (e.g., panic elsewhere)
        // If we can't get the lock, we skip cancellation recording.
        // This is acceptable: the orchestration is likely in a bad state anyway.
        let Ok(mut inner) = self.ctx.inner.try_lock() else {
            return;
        };
        
        // Dehydrating = normal suspension, not cancellation
        if inner.dehydrating {
            return;
        }
        
        // Check if completion exists (might have arrived but not consumed)
        if inner.completions.contains_key(&self.scheduling_event_id) {
            return;
        }
        
        // This is a real cancellation (e.g., select loser)
        inner.pending_actions.push(Action::Cancel {
            scheduling_event_id: self.scheduling_event_id,
        });
    }
}

impl Drop for ExternalFuture {
    fn drop(&mut self) {
        // ExternalFuture uses name-based matching, cancellation is simpler
        if self.consumed.get() {
            return;
        }
        
        let Ok(mut inner) = self.ctx.inner.try_lock() else {
            return;
        };
        
        if inner.dehydrating {
            return;
        }
        
        // For external events, we record cancellation by name
        // (No scheduling_event_id to reference)
        // This tells the runtime to stop waiting for this event
        inner.pending_actions.push(Action::CancelExternal {
            event_name: self.event_name.clone(),
        });
    }
}

// TimerFuture, SubOrchestrationFuture, SystemCallFuture follow same Drop pattern
```

### 3.8. Turn Execution with Dehydration Flag

```rust
// src/runtime/replay_engine.rs

pub fn run_turn<F, O>(
    ctx: OrchestrationContext,
    mut orchestration: F,
) -> TurnResult<O>
where
    F: Future<Output = O>,
{
    // Phase 1: Initialize context from history
    initialize_context(&ctx);
    
    // Phase 2: Ensure dehydrating = false during execution
    ctx.set_dehydrating(false);
    
    // Phase 3: Poll the orchestration
    let mut pinned = std::pin::pin!(orchestration);
    let poll_result = poll_once(pinned.as_mut());
    
    // Phase 4: Set dehydrating = true BEFORE the future is dropped
    ctx.set_dehydrating(true);
    
    // Phase 5: Extract results (future drops here with dehydrating=true)
    match poll_result {
        Poll::Ready(output) => {
            let actions = ctx.take_pending_actions();
            TurnResult::Completed { output, actions }
        }
        Poll::Pending => {
            let actions = ctx.take_pending_actions();
            TurnResult::Blocked { actions }
        }
    }
    // orchestration future dropped here, all DurableFuture drops are no-ops
}

fn initialize_context(ctx: &OrchestrationContext) {
    let mut inner = ctx.inner.lock().unwrap();
    
    // Extract scheduling events in order
    inner.scheduling_events = inner.history.iter()
        .filter_map(|e| match &e.kind {
            EventKind::ActivityScheduled { name, input } => Some(SchedulingEvent {
                event_id: e.event_id,
                kind: SchedulingKind::Activity { name: name.clone(), input: input.clone() },
            }),
            EventKind::TimerCreated { fire_at_ms } => Some(SchedulingEvent {
                event_id: e.event_id,
                kind: SchedulingKind::Timer { fire_at_ms: *fire_at_ms },
            }),
            EventKind::ExternalSubscribed { name } => Some(SchedulingEvent {
                event_id: e.event_id,
                kind: SchedulingKind::External { name: name.clone() },
            }),
            // ... other scheduling events
            _ => None,
        })
        .collect();
    
    // Build completion map: scheduling_event_id → result
    inner.completions = inner.history.iter()
        .filter_map(|e| {
            let source = e.source_event_id?;
            match &e.kind {
                EventKind::ActivityCompleted { result } => 
                    Some((source, CompletionResult::Activity(Ok(result.clone())))),
                EventKind::ActivityFailed { details } => 
                    Some((source, CompletionResult::Activity(Err(details.display_message())))),
                EventKind::TimerFired { .. } => 
                    Some((source, CompletionResult::Timer)),
                EventKind::ExternalEvent { data, .. } => 
                    Some((source, CompletionResult::External(data.clone()))),
                // ... other completions
                _ => None,
            }
        })
        .collect();
    
    // Extract completion event IDs for FIFO ordering
    inner.completion_events = inner.history.iter()
        .filter(|e| matches!(&e.kind,
            EventKind::ActivityCompleted { .. } |
            EventKind::ActivityFailed { .. } |
            EventKind::TimerFired { .. } |
            EventKind::ExternalEvent { .. } |
            EventKind::SubOrchestrationCompleted { .. } |
            EventKind::SubOrchestrationFailed { .. }
        ))
        .map(|e| CompletionEvent { 
            event_id: e.event_id, 
            source_event_id: e.source_event_id.unwrap() 
        })
        .collect();
    
    // Reset cursor
    inner.cursor = 0;
    inner.consumed_completions.clear();
    inner.pending_actions.clear();
}
```

---

## 4. Replay Engine Walkthrough

### Example History

```
Event 1: OrchestrationStarted { input: "start" }
Event 2: ActivityScheduled { name: "A", input: "x" }
Event 3: TimerCreated { fire_at_ms: 1705000000 }
Event 4: ActivityCompleted { source: 2, result: "a_result" }
Event 5: TimerFired { source: 3 }
Event 6: ActivityScheduled { name: "B", input: "y" }
// B hasn't completed - that's why we're replaying
```

### Turn Initialization

```rust
scheduling_events = [
    { event_id: 2, kind: Activity("A", "x") },
    { event_id: 3, kind: Timer(1705000000) },
    { event_id: 6, kind: Activity("B", "y") },
]

completions = {
    2 → Activity(Ok("a_result")),
    3 → Timer,
}

completion_events = [
    { event_id: 4, source: 2 },
    { event_id: 5, source: 3 },
]

cursor = 0
consumed_completions = {}
dehydrating = false
```

### Orchestration Execution

```rust
async fn my_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Statement 1: Schedule activity A
    let a = ctx.schedule_activity("A", "x").await?;
```

| Step | Action |
|------|--------|
| `schedule_activity("A", "x")` | cursor=0, check scheduling_events[0] |
| | Match: Activity("A", "x") ✓ |
| | Return ActivityFuture { event_id: 2 } |
| | cursor → 1 |
| `.await` polls future | completions[2] exists |
| | completion_events[4].source = 2 |
| | can_consume(4)? No earlier completions → YES |
| | consumed_completions.insert(4) |
| | Return Ready(Ok("a_result")) |

```rust
    // Statement 2: Wait for timer
    ctx.schedule_timer(Duration::from_secs(60)).await;
```

| Step | Action |
|------|--------|
| `schedule_timer(60s)` | cursor=1, check scheduling_events[1] |
| | Match: Timer ✓ |
| | Return TimerFuture { event_id: 3 } |
| | cursor → 2 |
| `.await` polls future | completions[3] exists (Timer) |
| | can_consume(5)? event_id 4 consumed → YES |
| | consumed_completions.insert(5) |
| | Return Ready(()) |

```rust
    // Statement 3: Schedule activity B
    let b = ctx.schedule_activity("B", "y").await?;
```

| Step | Action |
|------|--------|
| `schedule_activity("B", "y")` | cursor=2, check scheduling_events[2] |
| | Match: Activity("B", "y") ✓ |
| | Return ActivityFuture { event_id: 6 } |
| | cursor → 3 |
| `.await` polls future | completions[6] does NOT exist |
| | Return Pending |

**Turn ends.** Orchestration blocked on B.

```rust
// Runtime sets dehydrating = true
// Orchestration future is dropped
// ActivityFuture for B is dropped
//   → consumed = false
//   → dehydrating = true
//   → NO cancellation recorded (correct!)
```

### Non-Determinism Detection

If code changes between deployments:

**History says:**
```
Event 2: ActivityScheduled { name: "A", input: "x" }
```

**New code does:**
```rust
let b = ctx.schedule_activity("B", "different").await?;
```

| Step | Result |
|------|--------|
| `schedule_activity("B", "different")` | cursor=0 |
| Check scheduling_events[0] | Activity("A", "x") |
| Code requested | Activity("B", "different") |
| **MISMATCH** | panic!("Non-determinism detected...") |

---

## 5. Select Race Example

```rust
futures::select! {
    result = ctx.schedule_activity("Slow", "data").fuse() => {
        return Ok(result?);
    }
    _ = ctx.schedule_timer(Duration::from_secs(5)).fuse() => {
        return Ok("timeout".into());
    }
}
```

### Scenario: Timer Fires First

**History:**
```
Event 2: ActivityScheduled { name: "Slow", input: "data" }
Event 3: TimerCreated { fire_at_ms: ... }
Event 4: TimerFired { source: 3 }
// Activity still running
```

**Execution:**

| Step | Action |
|------|--------|
| `schedule_activity("Slow")` | Match history, return ActivityFuture { event_id: 2 } |
| `schedule_timer(5s)` | Match history, return TimerFuture { event_id: 3 } |
| `select!` polls (random order) | |
| Activity.poll() | completions[2] = None → Pending |
| Timer.poll() | completions[3] = Timer, can_consume(4) → Ready |
| `select!` picks timer | |
| Activity future dropped | dehydrating=false, no completion → Cancel recorded |
| pending_actions | [Action::Cancel { scheduling_event_id: 2 }] |

**FIFO Guarantee:**

Even if `select!` polled timer first:
- Timer.poll() → Ready (event 4 is first completion)
- Activity never had a chance anyway

Even if `select!` polled activity first:
- Activity.poll() → Pending (no completion exists)
- Timer.poll() → Ready

Same winner, regardless of poll order.

---

## 6. API Changes

### Before

```rust
// Old: Unified type with conversion methods
let result = ctx.schedule_activity("Task", input).into_activity().await?;
ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;
let event = ctx.schedule_wait("Approval").into_event().await;

// Old: Custom combinators
let (winner, _) = ctx.select2(future_a, future_b).await;
let results = ctx.join(vec![f1, f2, f3]).await;
```

### After

```rust
// New: Direct await
let result = ctx.schedule_activity("Task", input).await?;
ctx.schedule_timer(Duration::from_secs(5)).await;
let event = ctx.schedule_wait("Approval").await;

// New: Standard combinators
futures::select! {
    a = future_a.fuse() => handle_a(a),
    b = future_b.fuse() => handle_b(b),
}
let (r1, r2, r3) = futures::join!(f1, f2, f3);
```

---

## 7. Migration Strategy

### Phase 1: Add Infrastructure

- Add `Action::Cancel` variant
- Add `dehydrating` flag to `CtxInner`
- Add scheduling cursor and completion map fields

### Phase 2: Create New Future Types

- Create `ActivityFuture`, `TimerFuture`, `ExternalFuture`, `SubOrchestrationFuture`
- Implement `Future`, `FusedFuture`, `Drop` for each
- Implement FIFO check in poll()

### Phase 3: Refactor Schedule Methods

- Move validation from `poll()` to `schedule_*()`
- Return specific future types instead of `DurableFuture`
- Initialize context state at turn start

### Phase 4: Update Turn Execution

- Set `dehydrating` flag around poll
- Remove old claiming/scanning logic

### Phase 5: Deprecate Old API

- Deprecate `select2`, `join` methods
- Deprecate `.into_activity()`, `.into_timer()`, etc.
- Remove `DurableOutput` enum
- Remove `DurableFuture` unified type
- Remove `AggregateDurableFuture`

---

## 8. Files Changed

| File | Changes |
|------|---------|
| `src/lib.rs` | Add `Action::Cancel`, update `CtxInner`, new schedule methods |
| `src/futures.rs` | Replace `DurableFuture` with separate types, remove `Kind` enum, remove `AggregateDurableFuture` |
| `src/runtime/replay_engine.rs` | Add context initialization, dehydration guard, remove old claiming logic |
| `src/runtime/execution.rs` | Update turn execution to use new model |
| `docs/ORCHESTRATION-GUIDE.md` | Update API examples |
| `tests/*.rs` | Update to new API |

---

## 9. Risks & Mitigations

### Risk: Accidental Non-Determinism

**Issue:** Users using `tokio::spawn` inside orchestrations.

**Mitigation:** This creates threads/tasks outside duroxide control. We can't prevent it at compile time. Runtime will detect divergence during replay and panic with clear error.

### Risk: History Bloat from Cancellation

**Issue:** Schedule + immediate Cancel in same turn creates two events.

**Mitigation:** Optimization: If `Action::Cancel` targets an action from the same turn that hasn't been persisted yet, remove both from pending_actions (no-op).

### Risk: Breaking Change

**Issue:** Existing code uses `.into_activity()`, `select2`, etc.

**Mitigation:** 
- Phase deprecation over 2 releases
- Provide migration guide
- Keep old API as deprecated wrappers initially

### Risk: Subtle Semantic Changes

**Issue:** Currently dropping unawaited future might panic; now it cancels.

**Mitigation:** Add `#[must_use]` to all future types so compiler warns on unawaited futures.

---

## 10. Code Removal Checklist

### 10.1. `src/futures.rs` - Remove Entirely

| Item | Lines (approx) | Description |
|------|----------------|-------------|
| `Kind` enum | ~20 | Activity, Timer, External, SubOrch, System variants |
| `DurableFuture` struct | ~10 | Unified future type with `claimed_event_id`, `ctx`, `kind` |
| `DurableOutput` enum | ~10 | Activity, Timer, External, SubOrchestration variants |
| `impl Future for DurableFuture` | ~500 | Massive match on Kind with history scanning per variant |
| `can_consume_completion()` | ~30 | FIFO helper (replaced by simpler logic in new poll) |
| `AggregateDurableFuture` | ~200 | Custom select/join implementation |
| `AggregateMode` enum | ~5 | Select vs Join mode |
| `AggregateOutput` enum | ~10 | Select vs Join output wrapper |

### 10.2. `src/lib.rs` - Fields to Remove from `CtxInner`

```rust
// DELETE these fields
claimed_scheduling_events: HashSet<u64>,    // Replaced by cursor
cancelled_source_ids: HashSet<u64>,         // Re-derived each turn via Drop
cancelled_activity_ids: HashSet<u64>,       // Re-derived each turn via Drop
consumed_external_events: HashSet<String>,  // Merged into completions map
```

### 10.3. `src/lib.rs` - Methods to Remove

**Conversion methods on DurableFuture:**
```rust
// DELETE
pub fn into_activity(self) -> impl Future<Output = Result<String, String>>
pub fn into_timer(self) -> impl Future<Output = ()>
pub fn into_event(self) -> impl Future<Output = String>
pub fn into_sub_orchestration(self) -> impl Future<Output = Result<String, String>>
```

**Custom combinators on OrchestrationContext:**
```rust
// DELETE
pub fn select2(&self, a: DurableFuture, b: DurableFuture) -> SelectFuture
pub fn select3(...) // if exists
pub fn select4(...) // if exists
pub fn join(&self, futures: Vec<DurableFuture>) -> JoinFuture
```

**Helper methods:**
```rust
// DELETE
pub(crate) fn take_cancelled_activity_ids(&self) -> Vec<u64>
```

### 10.4. Summary

| Location | Lines Removed |
|----------|---------------|
| `src/futures.rs` | ~785 |
| `src/lib.rs` (CtxInner fields) | ~10 |
| `src/lib.rs` (methods) | ~70 |
| **Total Removed** | **~865** |
| **New Code Added** | ~350 |
| **Net Reduction** | **~515 lines** |

---

## 11. Test Migration Guide

### 11.1. Pattern Replacements

| Old Pattern | New Pattern |
|-------------|-------------|
| `ctx.schedule_activity("A", "x").into_activity().await?` | `ctx.schedule_activity("A", "x").await?` |
| `ctx.schedule_timer(Duration::from_secs(5)).into_timer().await` | `ctx.schedule_timer(Duration::from_secs(5)).await` |
| `ctx.schedule_wait("Event").into_event().await` | `ctx.schedule_wait("Event").await` |
| `ctx.schedule_sub_orchestration(...).into_sub_orchestration().await?` | `ctx.schedule_sub_orchestration(...).await?` |

### 11.2. Combinator Replacements

**select2 → futures::select!**
```rust
// OLD
let (winner_idx, output) = ctx.select2(
    ctx.schedule_activity("A", "x"),
    ctx.schedule_timer(Duration::from_secs(5)),
).await;
match output {
    DurableOutput::Activity(result) => ...,
    DurableOutput::Timer => ...,
}

// NEW
futures::select! {
    result = ctx.schedule_activity("A", "x").fuse() => {
        // result: Result<String, String>
    }
    _ = ctx.schedule_timer(Duration::from_secs(5)).fuse() => {
        // timeout
    }
}
```

**join → futures::join!**
```rust
// OLD
let futures = vec![
    ctx.schedule_activity("A", "1"),
    ctx.schedule_activity("B", "2"),
    ctx.schedule_activity("C", "3"),
];
let results = ctx.join(futures).await;
for output in results {
    match output {
        DurableOutput::Activity(Ok(s)) => ...,
        _ => ...,
    }
}

// NEW
let (a, b, c) = futures::join!(
    ctx.schedule_activity("A", "1"),
    ctx.schedule_activity("B", "2"),
    ctx.schedule_activity("C", "3"),
);
// a, b, c are each Result<String, String>
```

### 11.3. Files Requiring Test Updates

| Test File | Changes Needed |
|-----------|----------------|
| `tests/e2e_samples.rs` | Update all `.into_activity()` calls, replace `select2`/`join` |
| `tests/cancellation_tests.rs` | Replace `select2` with `futures::select!` |
| `tests/determinism_tests.rs` | Update API calls |
| `tests/replay_tests.rs` | Update API calls |
| `tests/scenarios/*.rs` | Update all orchestration patterns |
| `src/provider_stress_test/*.rs` | Update stress test orchestrations |
| `examples/*.rs` | Update all examples |

### 11.4. Import Changes

```rust
// OLD
use duroxide::{DurableFuture, DurableOutput, OrchestrationContext};

// NEW
use duroxide::{ActivityFuture, TimerFuture, ExternalFuture, OrchestrationContext};
use futures::{select, join};
use futures::FutureExt; // for .fuse()
```

---

## 12. Documentation Updates

### 12.1. README.md

| Section | Changes |
|---------|---------|
| "Key types" | Remove `DurableFuture`, `DurableOutput`. Add `ActivityFuture`, `TimerFuture`, etc. Remove mention of `.into_activity()`, `select2`, `join` |
| "Hello world" example | Remove `.into_activity()` from `ctx.schedule_activity(...).into_activity().await` |
| "Parallel fan-out" example | Replace `ctx.join(vec![...])` with `futures::join!()`. Remove `DurableOutput` match |
| "Control flow + timers" example | Replace `ctx.select2(a, b).await` with `futures::select!`. Remove `DurableOutput` match |
| "Error handling" example | Remove `.into_activity()` |
| "How it works" section | Update "Deterministic future aggregation: `ctx.select2`..." to mention `futures::select!` and FIFO enforcement |

### 12.2. docs/ORCHESTRATION-GUIDE.md

| Section | Changes |
|---------|---------|
| Quick Start example | Remove all `.into_activity()` calls |
| API Reference | Remove `into_activity()`, `into_timer()`, `into_event()`, `into_sub_orchestration()` |
| | Remove `select2`, `select3`, `select4`, `join` method docs |
| | Add guidance: "Use `futures::select!` and `futures::join!`" |
| Common Patterns | Update all patterns to use standard combinators |
| Anti-Patterns | Add note: "`tokio::spawn` breaks determinism" |
| Complete Examples | Update all examples |

### 12.3. docs/durable-futures-internals.md

**This document needs major rewrite.** Current content describes:
- `Kind` enum (removed)
- Claim system (replaced by cursor)
- `DurableFuture` unified type (replaced by separate types)
- Aggregate futures (removed, use standard combinators)

| Section | Changes |
|---------|---------|
| "The DurableFuture Type" | Rewrite to describe separate future types |
| "The Claim System" | Replace with "Scheduling Cursor" explanation |
| "Polling and Replay" | Simplify—poll is now just a map lookup + FIFO check |
| "Aggregate Futures (Select/Join)" | Remove entirely—explain using standard `futures::*` |
| Add new section | "Drop-Based Cancellation" |
| Add new section | "Dehydration Guard" |

### 12.4. docs/replay-engine.md

| Section | Changes |
|---------|---------|
| Turn Lifecycle | Update to show schedule-time validation instead of poll-time |
| Determinism Model | Explain cursor-based validation |
| Data Flow | Update `CtxInner` fields description |

### 12.5. Other Docs (Minor Updates)

| File | Changes |
|------|---------|
| `docs/external-events.md` | Remove `.into_event()` from examples |
| `docs/sub-orchestrations.md` | Remove `.into_sub_orchestration()` from examples |
| `docs/continue-as-new.md` | Update examples if any use old API |
| `docs/observability-guide.md` | Update examples if any use old API |
| `QUICK_START.md` | Update examples |
| `examples/README.md` | Update descriptions |

### 12.6. Docstrings in Code

| Location | Changes |
|----------|---------|
| `src/lib.rs` module docs | Update all examples in `//!` comments |
| `OrchestrationContext` docs | Update method docs, remove `select2`/`join` |
| `ActivityFuture` etc. | Add docs explaining FIFO ordering, Drop cancellation |
