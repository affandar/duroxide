# Common Orchestration Patterns

This guide covers common patterns and best practices for building orchestrations with Duroxide.

## Table of Contents

1. [Function Chaining](#function-chaining)
2. [Fan-Out/Fan-In](#fan-outfan-in)
3. [Human-in-the-Loop](#human-in-the-loop)
4. [Saga Pattern](#saga-pattern)
5. [Monitor Pattern](#monitor-pattern)
6. [Aggregator Pattern](#aggregator-pattern)
7. [Retry Patterns](#retry-patterns)
8. [State Management](#state-management)

## Function Chaining

Sequential execution where each step depends on the previous result.

```rust
async fn order_processing(ctx: OrchestrationContext, order_json: String) -> Result<String, String> {
    // Step 1: Validate order
    let validated_order = ctx.schedule_activity("ValidateOrder", order_json)
        .into_activity().await?;
    
    // Step 2: Reserve inventory
    let reservation = ctx.schedule_activity("ReserveInventory", &validated_order)
        .into_activity().await?;
    
    // Step 3: Process payment
    let payment_result = ctx.schedule_activity("ProcessPayment", &validated_order)
        .into_activity().await?;
    
    // Step 4: Ship order
    let shipment = ctx.schedule_activity("ShipOrder", &reservation)
        .into_activity().await?;
    
    Ok(format!("Order complete: {}", shipment))
}
```

## Fan-Out/Fan-In

Process multiple items in parallel and aggregate results.

```rust
async fn batch_processor(ctx: OrchestrationContext, items_json: String) -> Result<String, String> {
    let items: Vec<String> = serde_json::from_str(&items_json)
        .map_err(|e| e.to_string())?;
    
    // Fan-out: Schedule all processing in parallel
    let futures: Vec<_> = items
        .into_iter()
        .map(|item| ctx.schedule_activity("ProcessItem", item))
        .collect();
    
    // Fan-in: Wait for all to complete
    let results = ctx.join(futures).await;
    
    // Aggregate results
    let mut successful = 0;
    let mut failed = 0;
    
    for result in results {
        match result {
            DurableOutput::Activity(Ok(_)) => successful += 1,
            DurableOutput::Activity(Err(_)) => failed += 1,
            _ => {}
        }
    }
    
    Ok(format!("Processed {} successfully, {} failed", successful, failed))
}
```

### Dynamic Fan-Out

When the number of parallel operations is determined at runtime:

```rust
async fn dynamic_fanout(ctx: OrchestrationContext, count: String) -> Result<String, String> {
    let n: usize = count.parse().map_err(|e| format!("Invalid count: {}", e))?;
    
    // Dynamically create futures
    let mut futures = Vec::new();
    for i in 0..n {
        let future = ctx.schedule_activity("ProcessTask", format!("task-{}", i));
        futures.push(future);
    }
    
    // Wait for all
    let results = ctx.join(futures).await;
    Ok(format!("Completed {} tasks", results.len()))
}
```

## Human-in-the-Loop

Wait for human approval or external system callbacks.

```rust
async fn approval_workflow(ctx: OrchestrationContext, request_json: String) -> Result<String, String> {
    // Submit for approval
    ctx.schedule_activity("SendApprovalRequest", &request_json)
        .into_activity().await?;
    
    // Wait for approval with timeout
    let timeout = ctx.schedule_timer(86400000); // 24 hours
    let approval = ctx.schedule_wait("ApprovalResponse");
    
    let (winner_idx, result) = ctx.select2(timeout, approval).await;
    
    match (winner_idx, result) {
        (0, DurableOutput::Timer) => {
            // Timeout - escalate
            ctx.trace_warn("Approval timeout, escalating");
            ctx.schedule_activity("EscalateApproval", &request_json)
                .into_activity().await?;
            
            // Wait again with shorter timeout
            ctx.schedule_wait("ApprovalResponse")
                .into_event().await
        }
        (1, DurableOutput::External(response)) => {
            // Got approval
            ctx.trace_info("Approval received");
            response
        }
        _ => return Err("Unexpected result".to_string()),
    }
}
```

## Saga Pattern

Implement compensating transactions for rollback on failure.

```rust
async fn saga_transaction(ctx: OrchestrationContext, data: String) -> Result<String, String> {
    let mut completed_steps = Vec::new();
    
    // Step 1: Create resource A
    match ctx.schedule_activity("CreateResourceA", &data).into_activity().await {
        Ok(resource_a) => {
            completed_steps.push(("ResourceA", resource_a.clone()));
            
            // Step 2: Create resource B
            match ctx.schedule_activity("CreateResourceB", &resource_a).into_activity().await {
                Ok(resource_b) => {
                    completed_steps.push(("ResourceB", resource_b.clone()));
                    
                    // Step 3: Link resources
                    match ctx.schedule_activity("LinkResources", &format!("{},{}", resource_a, resource_b))
                        .into_activity().await {
                        Ok(link) => {
                            // Success path
                            Ok(format!("Transaction complete: {}", link))
                        }
                        Err(e) => {
                            // Compensate in reverse order
                            compensate(ctx, completed_steps).await;
                            Err(format!("Link failed: {}", e))
                        }
                    }
                }
                Err(e) => {
                    compensate(ctx, completed_steps).await;
                    Err(format!("Resource B creation failed: {}", e))
                }
            }
        }
        Err(e) => Err(format!("Resource A creation failed: {}", e))
    }
}

async fn compensate(ctx: &OrchestrationContext, steps: Vec<(&str, String)>) {
    // Compensate in reverse order
    for (resource_type, resource_id) in steps.into_iter().rev() {
        ctx.trace_info(format!("Compensating {}", resource_type));
        let _ = ctx.schedule_activity("DeleteResource", resource_id)
            .into_activity().await;
    }
}
```

## Monitor Pattern

Periodic monitoring with ContinueAsNew for eternal orchestrations.

```rust
#[derive(Serialize, Deserialize)]
struct MonitorState {
    check_count: u32,
    last_status: String,
    start_time: u128,
}

async fn monitor_service(ctx: OrchestrationContext, state_json: String) -> Result<String, String> {
    let mut state: MonitorState = if state_json.is_empty() {
        MonitorState {
            check_count: 0,
            last_status: "unknown".to_string(),
            start_time: ctx.system_now_ms().await,
        }
    } else {
        serde_json::from_str(&state_json).map_err(|e| e.to_string())?
    };
    
    // Perform health check
    let status = ctx.schedule_activity("CheckServiceHealth", "service-endpoint")
        .into_activity().await?;
    
    state.check_count += 1;
    
    // Alert if status changed
    if status != state.last_status {
        ctx.trace_warn(format!("Status changed from {} to {}", state.last_status, status));
        ctx.schedule_activity("SendAlert", &format!("Status changed to {}", status))
            .into_activity().await?;
    }
    
    state.last_status = status;
    
    // Wait before next check
    ctx.schedule_timer(300000).into_timer().await; // 5 minutes
    
    // Check if should continue monitoring
    let elapsed = ctx.system_now_ms().await - state.start_time;
    if elapsed < 86400000 { // 24 hours
        // Continue monitoring
        ctx.continue_as_new(serde_json::to_string(&state).unwrap());
        unreachable!()
    } else {
        Ok(format!("Monitoring complete after {} checks", state.check_count))
    }
}
```

## Aggregator Pattern

Collect data from multiple sources and aggregate.

```rust
async fn data_aggregator(ctx: OrchestrationContext, sources_json: String) -> Result<String, String> {
    let sources: Vec<String> = serde_json::from_str(&sources_json)
        .map_err(|e| e.to_string())?;
    
    // Fetch from all sources in parallel with individual timeouts
    let mut futures = Vec::new();
    
    for source in sources {
        // Create a timeout for each source
        let fetch = ctx.schedule_activity("FetchData", source.clone());
        let timeout = ctx.schedule_timer(5000); // 5 second timeout per source
        
        // Race fetch against timeout
        futures.push(async move {
            let (idx, result) = ctx.select2(fetch, timeout).await;
            match (idx, result) {
                (0, DurableOutput::Activity(Ok(data))) => Some(data),
                _ => {
                    ctx.trace_warn(format!("Failed to fetch from {}", source));
                    None
                }
            }
        });
    }
    
    // Collect all results
    let results: Vec<Option<String>> = futures::future::join_all(futures).await;
    
    // Aggregate non-null results
    let valid_data: Vec<String> = results.into_iter().flatten().collect();
    
    if valid_data.is_empty() {
        Err("No data sources responded".to_string())
    } else {
        ctx.schedule_activity("AggregateData", serde_json::to_string(&valid_data).unwrap())
            .into_activity().await
    }
}
```

## Retry Patterns

### Simple Retry with Backoff

```rust
async fn retry_with_backoff(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let max_attempts = 3;
    let mut attempt = 0;
    let mut delay_ms = 1000; // Start with 1 second
    
    loop {
        attempt += 1;
        ctx.trace_info(format!("Attempt {} of {}", attempt, max_attempts));
        
        match ctx.schedule_activity("UnreliableActivity", &input).into_activity().await {
            Ok(result) => return Ok(result),
            Err(e) if attempt < max_attempts => {
                ctx.trace_warn(format!("Attempt {} failed: {}, retrying in {}ms", attempt, e, delay_ms));
                ctx.schedule_timer(delay_ms).into_timer().await;
                delay_ms *= 2; // Exponential backoff
            }
            Err(e) => {
                ctx.trace_error(format!("All {} attempts failed", max_attempts));
                return Err(e);
            }
        }
    }
}
```

### Circuit Breaker Pattern

```rust
#[derive(Serialize, Deserialize)]
struct CircuitState {
    failures: u32,
    last_failure_time: u128,
    state: String, // "closed", "open", "half-open"
}

async fn circuit_breaker_orchestration(
    ctx: OrchestrationContext, 
    request_json: String
) -> Result<String, String> {
    let mut circuit: CircuitState = CircuitState {
        failures: 0,
        last_failure_time: 0,
        state: "closed".to_string(),
    };
    
    let now = ctx.system_now_ms().await;
    
    // Check circuit state
    match circuit.state.as_str() {
        "open" => {
            // Check if enough time has passed to try again
            if now - circuit.last_failure_time > 60000 { // 1 minute
                circuit.state = "half-open".to_string();
            } else {
                return Err("Circuit breaker is open".to_string());
            }
        }
        _ => {}
    }
    
    // Try the operation
    match ctx.schedule_activity("ProtectedOperation", request_json).into_activity().await {
        Ok(result) => {
            // Success - reset circuit
            circuit.failures = 0;
            circuit.state = "closed".to_string();
            Ok(result)
        }
        Err(e) => {
            circuit.failures += 1;
            circuit.last_failure_time = now;
            
            if circuit.failures >= 3 {
                circuit.state = "open".to_string();
                ctx.trace_error("Circuit breaker opened after 3 failures");
            }
            
            Err(format!("Operation failed: {}", e))
        }
    }
}
```

## State Management

### Using ContinueAsNew for Long-Running Workflows

```rust
#[derive(Serialize, Deserialize)]
struct WorkflowState {
    items_processed: u32,
    total_items: u32,
    batch_size: u32,
    results: Vec<String>,
}

async fn batch_workflow(ctx: OrchestrationContext, state_json: String) -> Result<String, String> {
    let mut state: WorkflowState = serde_json::from_str(&state_json)
        .map_err(|e| e.to_string())?;
    
    // Process next batch
    let batch_end = (state.items_processed + state.batch_size).min(state.total_items);
    let batch_items: Vec<u32> = (state.items_processed..batch_end).collect();
    
    // Process batch in parallel
    let futures: Vec<_> = batch_items
        .iter()
        .map(|i| ctx.schedule_activity("ProcessItem", i.to_string()))
        .collect();
    
    let batch_results = ctx.join(futures).await;
    
    // Update state
    for result in batch_results {
        if let DurableOutput::Activity(Ok(res)) = result {
            state.results.push(res);
        }
    }
    state.items_processed = batch_end;
    
    // Check if done
    if state.items_processed < state.total_items {
        // Continue with next batch
        ctx.continue_as_new(serde_json::to_string(&state).unwrap());
        unreachable!()
    } else {
        // Complete
        Ok(format!("Processed {} items with {} results", 
                   state.total_items, state.results.len()))
    }
}
```

## Best Practices

1. **Keep orchestrations focused**: Each orchestration should have a single, clear purpose
2. **Use activities for I/O**: Never perform I/O directly in orchestrations
3. **Handle all error cases**: Explicit error handling makes workflows more robust
4. **Use meaningful names**: Activity and orchestration names should be self-documenting
5. **Limit history size**: Use ContinueAsNew for long-running workflows to prevent history growth
6. **Test determinism**: Ensure your orchestrations are replay-safe
7. **Version carefully**: Plan for orchestration evolution with proper versioning

## Anti-Patterns to Avoid

1. ❌ **Blocking operations in orchestrations**: Use activities instead
2. ❌ **Non-deterministic operations**: Avoid random numbers, current time, etc.
3. ❌ **Infinite loops without ContinueAsNew**: History will grow unbounded
4. ❌ **Ignoring errors**: Always handle activity failures explicitly
5. ❌ **Shared mutable state**: Each orchestration instance should be independent
6. ❌ **Direct database access**: Use activities for all external interactions

## See Also

- [Examples](../examples/) - Complete runnable examples
- [API Reference](api.md) - Detailed API documentation
- [Architecture](architecture.md) - How Duroxide works internally
