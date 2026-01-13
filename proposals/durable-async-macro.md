# Proposal: `durable_async!()` Macro for Inline Task Scopes

**Status:** Draft  
**Author:** AI Assistant  
**Created:** 2026-01-12  
**Related:** [Activity Cancellation](activity-cancellation.md), [Durable Futures Internals](../docs/durable-futures-internals.md)

---

## Summary

Introduce a `durable_async!()` macro that wraps an async block into a `DurableFuture`, enabling it to be used in `select2()` and `join()` alongside other durable futures. This provides tokio-like ergonomics for racing groups of sequential operations without requiring pre-registered sub-orchestrations.

---

## Motivation

### The Problem

In tokio, you can race arbitrary async blocks:

```rust
tokio::select! {
    res = single_operation() => { ... }
    res = async {
        let a = step1().await;
        let b = step2(a).await;
        b
    } => { ... }
}
```

In duroxide, `select2()` only accepts `DurableFuture`:

```rust
// ❌ Won't compile - async block is impl Future, not DurableFuture
let (winner, _) = ctx.select2(
    ctx.schedule_activity("Fast", input),
    async {
        let a = ctx.schedule_activity("Step1", input).into_activity().await?;
        let b = ctx.schedule_activity("Step2", a).into_activity().await?;
        Ok(b)
    },
).await;
```

### Current Workaround

Users must create a sub-orchestration:

```rust
// 1. Register sub-orchestration
.register("SequentialSteps", |ctx, input| async move {
    let a = ctx.schedule_activity("Step1", input).into_activity().await?;
    let b = ctx.schedule_activity("Step2", a).into_activity().await?;
    Ok(b)
})

// 2. Use in select2
let (winner, _) = ctx.select2(
    ctx.schedule_activity("Fast", input.clone()),
    ctx.schedule_sub_orchestration("SequentialSteps", input),
).await;
```

This works but has downsides:
- **Verbose:** Requires separate registration
- **Scattered logic:** Workflow logic split across multiple locations
- **Overhead:** Sub-orchestrations create separate instances with their own history

### Proposed Solution

A `durable_async!()` macro that creates an inline task scope:

```rust
let (winner, _) = ctx.select2(
    ctx.schedule_activity("Fast", input.clone()),
    durable_async!(ctx, {
        let a = ctx.schedule_activity("Step1", input).into_activity().await?;
        let b = ctx.schedule_activity("Step2", a).into_activity().await?;
        Ok(b)
    }),
).await;
```

---

## Detailed Design

### API

```rust
/// Creates a DurableFuture from an async block.
/// 
/// The block runs within the same orchestration instance, with activities
/// grouped into a scope for cancellation purposes.
/// 
/// # Example
/// 
/// ```rust
/// let task = durable_async!(ctx, {
///     let a = ctx.schedule_activity("Step1", input).into_activity().await?;
///     let b = ctx.schedule_activity("Step2", a).into_activity().await?;
///     Ok(b)
/// });
/// 
/// let (winner, output) = ctx.select2(other_activity, task).await;
/// ```
#[macro_export]
macro_rules! durable_async {
    ($ctx:expr, $body:expr) => {{
        let __ctx = $ctx.clone();
        let __scope_id = __ctx.begin_scope();
        $crate::DurableFuture::new_task(
            __ctx,
            __scope_id,
            Box::pin(async move { $body }),
        )
    }};
}
```

### New `Kind::Task` Variant

```rust
pub(crate) enum Kind {
    Activity { name: String, input: String },
    Timer { delay_ms: u64 },
    External { name: String, result: RefCell<Option<String>> },
    SubOrch { name: String, version: Option<String>, instance: RefCell<String>, input: String },
    System { op: String, value: RefCell<Option<String>> },
    // NEW
    Task {
        scope_id: u64,
        future: RefCell<Option<Pin<Box<dyn Future<Output = Result<String, String>> + Send>>>>,
    },
}
```

### Scope Tracking in `CtxInner`

```rust
pub(crate) struct CtxInner {
    // ... existing fields ...
    
    /// Current scope stack (for nested durable_async! blocks)
    scope_stack: Vec<u64>,
    
    /// Next scope ID to allocate
    next_scope_id: u64,
    
    /// Map of scope_id -> activity event_ids scheduled within that scope
    scope_activities: HashMap<u64, Vec<u64>>,
}
```

### New Methods on `OrchestrationContext`

```rust
impl OrchestrationContext {
    /// Begin a new scope for grouping activities.
    /// Called by durable_async! macro.
    pub fn begin_scope(&self) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let scope_id = inner.next_scope_id;
        inner.next_scope_id += 1;
        inner.scope_stack.push(scope_id);
        inner.scope_activities.insert(scope_id, Vec::new());
        scope_id
    }
    
    /// End the current scope.
    /// Called when Task future completes or is cancelled.
    pub(crate) fn end_scope(&self, scope_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(pos) = inner.scope_stack.iter().position(|&s| s == scope_id) {
            inner.scope_stack.remove(pos);
        }
    }
    
    /// Get current scope (if any).
    pub(crate) fn current_scope(&self) -> Option<u64> {
        self.inner.lock().unwrap().scope_stack.last().copied()
    }
    
    /// Register an activity with the current scope.
    pub(crate) fn register_activity_in_scope(&self, event_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(&scope_id) = inner.scope_stack.last() {
            if let Some(activities) = inner.scope_activities.get_mut(&scope_id) {
                activities.push(event_id);
            }
        }
    }
    
    /// Get all activity event_ids for a scope (for cancellation).
    pub(crate) fn get_scope_activities(&self, scope_id: u64) -> Vec<u64> {
        self.inner.lock().unwrap()
            .scope_activities
            .get(&scope_id)
            .cloned()
            .unwrap_or_default()
    }
}
```

### Activity Scheduling Integration

When an activity is scheduled, it registers with the current scope:

```rust
// In Kind::Activity polling
let event_id = /* claim or create event_id */;

// Register with current scope for cancellation tracking
this.ctx.register_activity_in_scope(event_id);
```

### Cancellation on `select2` Resolution

When `select2` picks a winner, loser scopes are cancelled:

```rust
// In AggregateDurableFuture::poll for Select mode
if let Some(winner_idx) = winner {
    // Phase 3: Cancel loser scopes
    for (idx, fut) in self.futures.iter().enumerate() {
        if idx != winner_idx {
            if let Kind::Task { scope_id, .. } = &fut.kind {
                // Get all activities in this scope
                let scope_activities = ctx.get_scope_activities(*scope_id);
                for activity_id in scope_activities {
                    inner.cancelled_source_ids.insert(activity_id);
                    inner.cancelled_activity_ids.insert(activity_id);
                }
                ctx.end_scope(*scope_id);
            }
            // ... existing loser handling for non-Task futures
        }
    }
}
```

### Task Polling Implementation

```rust
// In DurableFuture::poll for Kind::Task
Kind::Task { scope_id, future } => {
    // Ensure scope is active
    if !this.ctx.inner.lock().unwrap().scope_stack.contains(scope_id) {
        this.ctx.inner.lock().unwrap().scope_stack.push(*scope_id);
    }
    
    // Poll the inner future
    if let Some(fut) = future.borrow_mut().as_mut() {
        match fut.as_mut().poll(cx) {
            Poll::Ready(result) => {
                // Scope completed successfully
                this.ctx.end_scope(*scope_id);
                *future.borrow_mut() = None;
                Poll::Ready(DurableOutput::Task(result))
            }
            Poll::Pending => Poll::Pending,
        }
    } else {
        // Future already consumed (shouldn't happen)
        Poll::Ready(DurableOutput::Task(Err("Task already completed".to_string())))
    }
}
```

### New `DurableOutput` Variant

```rust
#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(Result<String, String>),
    Timer,
    External(String),
    SubOrchestration(Result<String, String>),
    Task(Result<String, String>),  // NEW
}
```

---

## Usage Examples

### Example 1: Racing Activity vs Sequential Pipeline

```rust
async fn order_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let order: Order = serde_json::from_str(&input)?;
    
    // Race: quick check vs full validation
    let quick_check = ctx.schedule_activity("QuickCheck", order.id.clone());
    
    let full_validation = durable_async!(ctx, {
        let customer = ctx.schedule_activity("ValidateCustomer", order.customer_id.clone())
            .into_activity().await?;
        let credit = ctx.schedule_activity("CreditCheck", customer)
            .into_activity().await?;
        let inventory = ctx.schedule_activity("ReserveInventory", order.sku.clone())
            .into_activity().await?;
        Ok(format!("{}:{}", credit, inventory))
    });
    
    let (winner, output) = ctx.select2(quick_check, full_validation).await;
    
    match (winner, output) {
        (0, DurableOutput::Activity(Ok(r))) => Ok(format!("quick:{}", r)),
        (1, DurableOutput::Task(Ok(r))) => Ok(format!("validated:{}", r)),
        (_, DurableOutput::Activity(Err(e))) => Err(e),
        (_, DurableOutput::Task(Err(e))) => Err(e),
        _ => unreachable!(),
    }
}
```

### Example 2: Timeout with Complex Cleanup

```rust
async fn provisioning_workflow(ctx: OrchestrationContext, config: String) -> Result<String, String> {
    let timeout = ctx.schedule_timer(Duration::from_secs(300));
    
    let provisioning = durable_async!(ctx, {
        let vm_id = ctx.schedule_activity("CreateVM", config.clone())
            .into_activity().await?;
        
        // Poll for readiness
        loop {
            let status = ctx.schedule_activity("CheckVMStatus", vm_id.clone())
                .into_activity().await?;
            
            if status == "ready" {
                break;
            }
            
            ctx.schedule_timer(Duration::from_secs(10)).into_timer().await;
        }
        
        // Install software
        for pkg in ["nginx", "postgres", "redis"] {
            ctx.schedule_activity("InstallPackage", format!("{}:{}", vm_id, pkg))
                .into_activity().await?;
        }
        
        Ok(vm_id)
    });
    
    let (winner, output) = ctx.select2(timeout, provisioning).await;
    
    match winner {
        0 => Err("Provisioning timed out".to_string()),
        1 => match output {
            DurableOutput::Task(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}
```

### Example 3: Racing Two Pipelines

```rust
async fn dual_path_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let path_a = durable_async!(ctx, {
        let a1 = ctx.schedule_activity("A1", input.clone()).into_activity().await?;
        let a2 = ctx.schedule_activity("A2", a1).into_activity().await?;
        let a3 = ctx.schedule_activity("A3", a2).into_activity().await?;
        Ok(format!("path_a:{}", a3))
    });
    
    let path_b = durable_async!(ctx, {
        let b1 = ctx.schedule_activity("B1", input.clone()).into_activity().await?;
        ctx.schedule_timer(Duration::from_secs(1)).into_timer().await;
        let b2 = ctx.schedule_activity("B2", b1).into_activity().await?;
        Ok(format!("path_b:{}", b2))
    });
    
    let (winner, output) = ctx.select2(path_a, path_b).await;
    
    ctx.trace_info(format!("Path {} won", if winner == 0 { "A" } else { "B" }));
    
    match output {
        DurableOutput::Task(result) => result,
        _ => unreachable!(),
    }
}
```

### Example 4: Full Control Flow

```rust
async fn batch_with_timeout(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let config: BatchConfig = serde_json::from_str(&input)?;
    
    let deadline = ctx.schedule_timer(Duration::from_secs(config.timeout_secs));
    
    let batch_work = durable_async!(ctx, {
        let mut results = Vec::new();
        let mut failed = 0;
        
        for (i, item) in config.items.iter().enumerate() {
            ctx.trace_info(format!("Processing {}/{}", i + 1, config.items.len()));
            
            // Conditional processing
            let result = match item.priority.as_str() {
                "high" => {
                    ctx.schedule_activity("PriorityProcess", item.id.clone())
                        .into_activity().await
                }
                "low" if config.skip_low_priority => {
                    continue;  // Skip low priority items
                }
                _ => {
                    ctx.schedule_activity("StandardProcess", item.id.clone())
                        .into_activity().await
                }
            };
            
            match result {
                Ok(r) => results.push(r),
                Err(e) => {
                    ctx.trace_warn(format!("Item {} failed: {}", item.id, e));
                    failed += 1;
                    
                    // Early exit if too many failures
                    if failed > config.max_failures {
                        return Err(format!("Too many failures: {}", failed));
                    }
                }
            }
            
            // Throttle between items
            if config.throttle_ms > 0 {
                ctx.schedule_timer(Duration::from_millis(config.throttle_ms))
                    .into_timer().await;
            }
        }
        
        Ok(serde_json::json!({
            "processed": results.len(),
            "failed": failed,
        }).to_string())
    });
    
    let (winner, output) = ctx.select2(deadline, batch_work).await;
    
    match (winner, output) {
        (0, _) => Err("Batch processing timed out".to_string()),
        (1, DurableOutput::Task(result)) => result,
        _ => unreachable!(),
    }
}
```

### Example 5: Nested `durable_async!()` (Advanced)

```rust
async fn nested_scopes(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let outer = durable_async!(ctx, {
        let setup = ctx.schedule_activity("Setup", input.clone())
            .into_activity().await?;
        
        // Nested race inside the outer block
        let inner_a = durable_async!(ctx, {
            let x = ctx.schedule_activity("InnerA1", setup.clone()).into_activity().await?;
            ctx.schedule_activity("InnerA2", x).into_activity().await
        });
        
        let inner_b = ctx.schedule_activity("InnerB", setup.clone());
        
        let (inner_winner, inner_output) = ctx.select2(inner_a, inner_b).await;
        
        let inner_result = match inner_output {
            DurableOutput::Task(r) | DurableOutput::Activity(r) => r?,
            _ => unreachable!(),
        };
        
        ctx.schedule_activity("Finalize", inner_result).into_activity().await
    });
    
    let timeout = ctx.schedule_timer(Duration::from_secs(60));
    
    let (winner, output) = ctx.select2(timeout, outer).await;
    
    match (winner, output) {
        (0, _) => Err("Timed out".to_string()),
        (1, DurableOutput::Task(r)) => r,
        _ => unreachable!(),
    }
}
```

### Example 6: With Helper Functions

```rust
// Helper async functions work inside durable_async!()
async fn validate_order(ctx: &OrchestrationContext, order: &Order) -> Result<String, String> {
    let customer = ctx.schedule_activity("ValidateCustomer", order.customer_id.clone())
        .into_activity().await?;
    let inventory = ctx.schedule_activity("CheckInventory", order.sku.clone())
        .into_activity().await?;
    Ok(format!("{}:{}", customer, inventory))
}

async fn process_payment(ctx: &OrchestrationContext, order: &Order) -> Result<String, String> {
    ctx.schedule_activity("ChargeCard", order.payment.clone())
        .into_activity().await
}

async fn order_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let order: Order = serde_json::from_str(&input)?;
    
    let timeout = ctx.schedule_timer(Duration::from_secs(30));
    
    let processing = durable_async!(ctx, {
        // Call helper functions - they run in this scope
        let validation = validate_order(&ctx, &order).await?;
        let payment = process_payment(&ctx, &order).await?;
        Ok(format!("{}|{}", validation, payment))
    });
    
    let (winner, output) = ctx.select2(timeout, processing).await;
    
    match (winner, output) {
        (0, _) => Err("Order processing timed out".to_string()),
        (1, DurableOutput::Task(r)) => r,
        _ => unreachable!(),
    }
}
```

---

## Direct Await Support

`durable_async!()` can also be awaited directly (not in `select2`):

```rust
async fn workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Direct await - works but no different from inline code
    let result = durable_async!(ctx, {
        let a = ctx.schedule_activity("Step1", input).into_activity().await?;
        let b = ctx.schedule_activity("Step2", a).into_activity().await?;
        Ok(b)
    }).into_task().await?;
    
    // Continue with result
    ctx.schedule_activity("Step3", result).into_activity().await
}
```

**Note:** Direct await provides no benefit over inline code—the macro is primarily useful for `select2`/`join` composition.

---

## Replay Semantics

### No New History Events for Task Itself

The `durable_async!()` scope is purely a runtime concept. No `TaskStarted` or `TaskCompleted` events are written to history. Only the inner operations (activities, timers, etc.) produce events.

### Determinism

The scope ID is deterministic—it's allocated in order during replay, just like event IDs. The same orchestration code will allocate the same scope IDs on each replay.

### History Example

```
Event 1: OrchestrationStarted
Event 2: ActivityScheduled "Fast"          <- select2 branch 1
Event 3: ActivityScheduled "Step1"         <- select2 branch 2 (scope 1)
Event 4: ActivityCompleted (for 3)
Event 5: ActivityScheduled "Step2"         <- still in scope 1
Event 6: ActivityCompleted (for 2)         <- Fast activity completes first!
                                           <- select2 returns winner=0
                                           <- scope 1 activities [3,5] marked cancelled
                                           <- Event 5 deleted from worker_queue
```

---

## Comparison with Alternatives

| Approach | Ergonomics | Same Instance | Cancellation | Works Today |
|----------|------------|---------------|--------------|-------------|
| Sub-orchestration | Verbose | No (separate) | Cascading | ✅ |
| `durable_async!()` | Inline | Yes | Scoped | ❌ (this proposal) |
| Generic `select2` | N/A | N/A | ❌ (no tracking) | ❌ |
| `tokio::select!` | Inline | N/A | Drop | N/A (not durable) |

---

## Implementation Plan

### Phase 1: Core Infrastructure
1. Add `Kind::Task` variant to `DurableFuture`
2. Add scope tracking to `CtxInner`
3. Add `begin_scope()`, `end_scope()`, `register_activity_in_scope()` methods
4. Add `DurableOutput::Task` variant

### Phase 2: Macro Implementation
1. Implement `durable_async!` macro
2. Add `DurableFuture::new_task()` constructor
3. Implement `poll()` for `Kind::Task`

### Phase 3: Cancellation Integration
1. Update `AggregateDurableFuture` to handle `Kind::Task` losers
2. Collect scope activities into `cancelled_activity_ids`
3. Existing provider cancellation handles the rest

### Phase 4: Testing
1. Unit tests for scope tracking
2. Integration tests for `select2` with `durable_async!`
3. Cancellation tests
4. Nested scope tests
5. Replay determinism tests

---

## Open Questions

1. **Should `durable_async!()` require explicit capture syntax?**
   ```rust
   durable_async!(ctx, move |order| { ... })  // Explicit captures
   // vs
   durable_async!(ctx, { ... })  // Implicit captures (current proposal)
   ```

2. **Should there be a `durable_task!()` variant that returns `impl Future` for direct await?**
   This would avoid the `.into_task()` call but complicate the API.

3. **Should nested `durable_async!()` be supported?**
   The design supports it, but it adds complexity. Could defer to v2.

4. **Thread safety of `RefCell<Option<Pin<Box<dyn Future>>>>`?**
   Need to ensure the future is `Send` for multi-threaded runtimes.

---

## References

- [Durable Futures Internals](../docs/durable-futures-internals.md)
- [Activity Cancellation Proposal](activity-cancellation.md)
- [Sub-Orchestrations](../docs/sub-orchestrations.md)
- [Tokio select! macro](https://docs.rs/tokio/latest/tokio/macro.select.html)
