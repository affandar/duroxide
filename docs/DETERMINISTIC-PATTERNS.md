# Deterministic Patterns: ctx.join() and ctx.select2()

## ⚠️ CRITICAL: Use Duroxide's Join/Select, Not Regular Futures!

### The Problem

```rust
// ❌ WRONG: Using futures::try_join_all
#[orchestration]
async fn bad_fan_out(ctx: OrchestrationContext, items: Vec<Item>) -> Result<Vec<Result>, String> {
    let futures: Vec<_> = items.into_iter()
        .map(|item| durable!(process(item)))
        .collect();
    
    // ❌ NON-DETERMINISTIC! Results consumed in completion order, not history order!
    let results = futures::future::try_join_all(futures).await?;
    
    Ok(results)
}
```

**Problem:** `futures::try_join_all()` consumes results in **completion order**, which is non-deterministic. On replay, activities might complete in a different order, causing nondeterminism!

---

### The Solution: Use ctx.join()

```rust
// ✅ CORRECT: Using ctx.join()
#[orchestration]
async fn good_fan_out(ctx: OrchestrationContext, users: Vec<User>) -> Result<String, String> {
    // Schedule all activities
    let futures: Vec<_> = users
        .into_iter()
        .map(|user| {
            let user_json = serde_json::to_string(&user).unwrap();
            ctx.schedule_activity("process_user", user_json)
        })
        .collect();
    
    // ✅ DETERMINISTIC! Results consumed in history order
    let results = ctx.join(futures).await;
    
    // Process results
    let mut outputs = Vec::new();
    for result in results {
        match result {
            DurableOutput::Activity(Ok(json)) => {
                let output: UserResult = serde_json::from_str(&json)?;
                outputs.push(output);
            }
            DurableOutput::Activity(Err(e)) => return Err(e),
            _ => unreachable!(),
        }
    }
    
    Ok(format!("Processed {} users", outputs.len()))
}
```

---

## Why ctx.join() is Required

### Deterministic Replay Guarantee

**First execution:**
```
Activity A completes at t=100ms → Result A
Activity B completes at t=50ms  → Result B
Activity C completes at t=75ms  → Result C

History order: [A, B, C] (scheduled order)
```

**On replay with ctx.join():**
```
Results consumed in history order: [A, B, C]
✅ Deterministic!
```

**On replay with futures::try_join_all():**
```
Activities "complete" instantly from history
futures::try_join_all might see them in any order
❌ Non-deterministic!
```

---

## Pattern: ctx.join() for Fan-Out/Fan-In

### Full Example

```rust
use duroxide::prelude::*;
use duroxide::DurableOutput;

#[activity(typed)]
async fn process_item(item: String) -> Result<String, String> {
    Ok(format!("Processed: {}", item))
}

#[orchestration]
async fn fan_out_fan_in(ctx: OrchestrationContext, items: Vec<String>) -> Result<Vec<String>, String> {
    durable_trace_info!("Processing {} items in parallel", items.len());
    
    // Fan-out: Schedule all items
    let futures: Vec<_> = items
        .into_iter()
        .map(|item| {
            // Current limitation: need to serialize manually for ctx.join()
            let item_json = serde_json::to_string(&item).unwrap();
            ctx.schedule_activity("process_item", item_json)
        })
        .collect();
    
    // Fan-in: Wait for all (deterministic!)
    let results = ctx.join(futures).await;
    
    // Extract typed results
    let mut outputs = Vec::new();
    for result in results {
        match result {
            DurableOutput::Activity(Ok(json)) => {
                let output: String = serde_json::from_str(&json)?;
                outputs.push(output);
            }
            DurableOutput::Activity(Err(e)) => {
                durable_trace_error!("Item failed: {}", e);
                return Err(e);
            }
            _ => unreachable!(),
        }
    }
    
    durable_trace_info!("✅ Processed {} items successfully", outputs.len());
    Ok(outputs)
}
```

---

## Pattern: ctx.select2() for Racing

### Correct Pattern

```rust
#[orchestration]
async fn timeout_pattern(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    durable_trace_info!("Starting operation with timeout");
    
    // Schedule both operations
    let operation = ctx.schedule_activity("slow_operation", 
        serde_json::to_string(&input).unwrap());
    let timeout = ctx.schedule_timer(5000);  // 5 second timeout
    
    // ✅ CORRECT: Use ctx.select2() for deterministic racing
    let (winner_index, result) = ctx.select2(operation, timeout).await;
    
    match (winner_index, result) {
        (0, DurableOutput::Activity(Ok(result_json))) => {
            let result: String = serde_json::from_str(&result_json)?;
            durable_trace_info!("✅ Operation completed: {}", result);
            Ok(result)
        }
        (0, DurableOutput::Activity(Err(e))) => {
            durable_trace_error!("Operation failed: {}", e);
            Err(e)
        }
        (1, DurableOutput::Timer) => {
            durable_trace_warn!("⏱️  Operation timed out");
            Err("Timeout".to_string())
        }
        _ => unreachable!(),
    }
}
```

### ❌ Wrong Pattern

```rust
// ❌ DON'T DO THIS:
let result = tokio::select! {
    r = durable!(operation(input)) => r,
    _ = tokio::time::sleep(Duration::from_secs(5)) => Err("timeout".into()),
};
// Non-deterministic! Won't replay correctly!
```

---

## Future Enhancement: Typed Join/Select

**Eventually, we want to support:**

```rust
// Future goal: Typed join that works with durable!()
let results: Vec<UserProfile> = ctx.join_typed(users.into_iter().map(|user| {
    durable!(fetch_user_profile(user))
})).await?;
```

**For now, use the pattern shown above with `ctx.join()` and manual serialization.**

---

## Key Rules

1. ✅ **Always use `ctx.join()`** for parallel work, never `futures::try_join_all()`
2. ✅ **Always use `ctx.select2()`** for racing, never `tokio::select!`
3. ✅ **Schedule activities with `ctx.schedule_activity()`** for join/select
4. ✅ **Use `durable!()` for sequential calls** only

---

## Quick Reference

```rust
// Sequential (use durable!())
let a = durable!(activity_a(input)).await?;
let b = durable!(activity_b(a)).await?;

// Parallel (use ctx.join())
let futures = vec![
    ctx.schedule_activity("a", input1),
    ctx.schedule_activity("b", input2),
];
let results = ctx.join(futures).await;

// Racing (use ctx.select2())
let operation = ctx.schedule_activity("op", input);
let timeout = ctx.schedule_timer(5000);
let (winner, result) = ctx.select2(operation, timeout).await;
```

**This ensures deterministic replay!** 🎯

