# Troubleshooting Guide

This guide helps you diagnose and fix common issues when using Duroxide.

## Common Issues

### 1. "Activity not found" Error

**Symptom**: 
```
Activity 'MyActivity' not found in registry
```

**Causes & Solutions**:
- **Activity not registered**: Ensure you've registered the activity in the `ActivityRegistry`
  ```rust
  let activities = ActivityRegistry::builder()
      .register("MyActivity", |ctx: ActivityContext, input: String| async move {
          // Activity logic
          Ok(result)
      })
      .build();
  ```

- **Typo in activity name**: Activity names are case-sensitive. Check for typos.

- **Wrong registry passed to runtime**: Ensure you're passing the correct registry:
  ```rust
  let rt = Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;
  ```

### 2. "Orchestration not found" Error

**Symptom**:
```
Orchestration 'MyOrchestration' not found in registry
```

**Solutions**:
- Register the orchestration before starting the runtime
- Check for typos in the orchestration name
- Ensure the registry is properly built and passed to the runtime

### 3. Orchestration Hangs / Never Completes

**Common Causes**:

1. **Waiting for external event that never arrives**:
   ```rust
   // This will hang forever if "ApprovalEvent" is never raised
   let approval = ctx.schedule_wait("ApprovalEvent").into_event().await;
   ```
   
   **Solution**: Add timeouts:
   ```rust
   let timer = ctx.schedule_timer(30000); // 30 second timeout
   let event = ctx.schedule_wait("ApprovalEvent");
   let (_, result) = ctx.select2(timer, event).await;
   ```

2. **Activity implementation hangs**:
   ```rust
   // Bad: This blocks forever
   .register("BadActivity", |ctx: ActivityContext, _| async move {
       loop { tokio::time::sleep(Duration::from_secs(1)).await; }
       Ok("never happens".to_string())
   })
   ```
   
   **Solution**: Ensure activities complete or fail within reasonable time.

3. **Infinite loop without progress**:
   ```rust
   // Bad: Infinite loop without ContinueAsNew
   loop {
       ctx.schedule_activity("Process", "data").into_activity().await?;
       // History grows unbounded!
   }
   ```
   
   **Solution**: Use ContinueAsNew for long-running loops.

### 4. Nondeterminism Errors

**Symptom**:
```
Nondeterminism detected: expected Activity completion but found Timer
```

**Common Causes**:

1. **Using non-deterministic values**:
   ```rust
   // Bad: Non-deterministic
   let random = rand::random::<u32>();
   let now = SystemTime::now();
   
   // Good: Deterministic
   let guid = ctx.system_new_guid().await;
   let now = ctx.system_now_ms().await;
   ```

2. **Conditional logic based on external state**:
   ```rust
   // Bad: File might change between replays
   if std::path::Path::new("/tmp/flag").exists() {
       ctx.schedule_activity("A", "").into_activity().await?;
   } else {
       ctx.schedule_activity("B", "").into_activity().await?;
   }
   
   // Good: Use activity to check external state
   let flag_exists = ctx.schedule_activity("CheckFlag", "").into_activity().await?;
   if flag_exists == "true" {
       ctx.schedule_activity("A", "").into_activity().await?;
   } else {
       ctx.schedule_activity("B", "").into_activity().await?;
   }
   ```

3. **Changing orchestration logic between runs**:
   - Always version your orchestrations when making changes
   - Use feature flags passed as input, not compile-time changes

### 5. "History limit exceeded" Error

**Symptom**:
```
History limit exceeded (1024 events)
```

**Solution**: Use ContinueAsNew to reset history:
```rust
#[derive(Serialize, Deserialize)]
struct LoopState {
    iteration: u32,
    accumulated_result: String,
}

async fn long_running(ctx: OrchestrationContext, state_json: String) -> Result<String, String> {
    let mut state: LoopState = serde_json::from_str(&state_json)?;
    
    // Do work for this iteration
    let result = ctx.schedule_activity("Process", state.iteration.to_string())
        .into_activity().await?;
    
    state.accumulated_result.push_str(&result);
    state.iteration += 1;
    
    if state.iteration < 1000 {
        // Continue with fresh history
        ctx.continue_as_new(serde_json::to_string(&state)?);
        unreachable!()
    } else {
        Ok(state.accumulated_result)
    }
}
```

### 6. External Events Not Received

**Common Issues**:

1. **Event sent before subscription**:
   ```rust
   // Bad: Race condition
   // External system might send event before we subscribe
   notify_external_system(&instance_id).await;
   let event = ctx.schedule_wait("Event").into_event().await;
   
   // Good: Subscribe first
   let event_future = ctx.schedule_wait("Event");
   notify_external_system(&instance_id).await;
   let event = event_future.into_event().await;
   ```

2. **Wrong instance ID**:
   ```rust
   // Ensure you're raising events to the correct instance
   runtime.raise_event("wrong-instance-id", "EventName", data).await; // Won't work
   runtime.raise_event(&correct_instance_id, "EventName", data).await; // Works
   ```

3. **Event name mismatch**:
   - Event names are case-sensitive
   - Must match exactly between `schedule_wait` and `raise_event`

### 7. Memory Issues

**Symptom**: High memory usage or OOM errors

**Solutions**:

1. **Use ContinueAsNew for long-running orchestrations**
2. **Don't accumulate large amounts of data in orchestration state**:
   ```rust
   // Bad: Accumulating everything in memory
   let mut all_results = Vec::new();
   for i in 0..10000 {
       let result = ctx.schedule_activity("Process", i.to_string())
           .into_activity().await?;
       all_results.push(result); // Memory grows!
   }
   
   // Good: Process in batches with ContinueAsNew
   // Or use activities to store results externally
   ```

3. **Limit parallel operations**:
   ```rust
   // Process in chunks instead of all at once
   for chunk in items.chunks(100) {
       let futures: Vec<_> = chunk.iter()
           .map(|item| ctx.schedule_activity("Process", item))
           .collect();
       let results = ctx.join(futures).await;
       // Process results and continue
   }
   ```

### 8. Debugging Tips

1. **Enable detailed logging**:
   ```bash
   RUST_LOG=debug cargo run
   ```

2. **Use ctx.trace_* for orchestration logging**:
   ```rust
   ctx.trace_debug("Starting processing");
   ctx.trace_info(format!("Processing item: {}", item));
   ctx.trace_warn("Retrying after failure");
   ctx.trace_error(format!("Failed after retries: {}", error));
   ```

3. **Inspect history**:
   ```rust
   // For debugging, you can inspect history in tests
   let history = store.read(&instance_id).await;
   for event in history {
       println!("{:?}", event);
   }
   ```

4. **Check orchestration status**:
   ```rust
   let status = runtime.get_orchestration_status(&instance_id).await;
   match status {
       OrchestrationStatus::Running => println!("Still running"),
       OrchestrationStatus::Completed { output } => println!("Output: {}", output),
       OrchestrationStatus::Failed { error } => println!("Error: {}", error),
       OrchestrationStatus::NotFound => println!("Instance not found"),
   }
   ```

## Performance Optimization

### 1. Minimize History Size
- Use ContinueAsNew for long-running workflows
- Avoid scheduling unnecessary activities
- Batch operations when possible

### 2. Optimize Activity Granularity
- Too fine: Overhead from scheduling many small activities
- Too coarse: Less parallelism and longer failure recovery
- Find the right balance for your use case

### 3. Use Parallel Processing
```rust
// Sequential (slow)
for item in items {
    ctx.schedule_activity("Process", item).into_activity().await?;
}

// Parallel (fast)
let futures: Vec<_> = items.into_iter()
    .map(|item| ctx.schedule_activity("Process", item))
    .collect();
let results = ctx.join(futures).await;
```

### 4. Cache Computation in Activities
- Activities should be idempotent
- Cache expensive computations externally
- Use correlation IDs for cache keys

## Getting Help

If you're still stuck:

1. Check the [examples](../examples/) for working code
2. Review the test files for similar scenarios
3. Enable debug logging to see what's happening
4. Open an issue with:
   - Minimal reproduction code
   - Expected vs actual behavior
   - Relevant log output
   - Duroxide version
