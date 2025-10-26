# Duroxide Quick Start Guide

This guide helps you get started with Duroxide in 5 minutes. Perfect for LLMs and developers who want to understand the framework quickly.

## What is Duroxide?

Duroxide is a framework for building **reliable, long-running workflows** that can survive failures and restarts. Think of it as "async Rust with superpowers" - your workflows can run for hours, days, or indefinitely.

## Core Concepts (30 seconds)

- **Orchestrations**: Your main workflow logic (async functions)
- **Activities**: Stateless functions that do actual work (NO delays/sleeps!)
- **Timers**: Use `ctx.schedule_timer(ms)` for delays, timeouts, scheduling
- **Deterministic Replay**: If something fails, Duroxide replays your code to get back to the same state
- **Durable Futures**: Special futures that remember their state across restarts

## ⚠️ Key Rules

### 1. Timers vs Activities
**For orchestration delays, use timers. Activities can use any async operations:**

```rust
// ✅ CORRECT: Use timers for orchestration delays
ctx.schedule_timer(5000).into_timer().await; // Wait 5 seconds

// ✅ ALSO CORRECT: Activities can use sleep, HTTP calls, database queries, etc.
// Activities are just regular async functions and can do anything
```

### 2. DurableFuture Conversion Pattern
**CRITICAL: You MUST call `.into_*().await`, not just `.await`!**

```rust
// ✅ CORRECT patterns:
let result = ctx.schedule_activity("Task", "input").into_activity().await?;
ctx.schedule_timer(5000).into_timer().await;
let event = ctx.schedule_wait("Event").into_event().await;

// ❌ WRONG - Missing conversion methods:
// let result = ctx.schedule_activity("Task", "input").await;  // Won't compile!
// ctx.schedule_timer(5000).await;                            // Won't compile!
```

## Minimal Example

```rust
use duroxide::*;
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

// 1. Define an activity (your business logic)
let activities = ActivityRegistry::builder()
    .register("Greet", |name: String| async move {
        Ok(format!("Hello, {}!", name))
    })
    .build();

// 2. Define an orchestration (your workflow)
let orchestration = |ctx: OrchestrationContext, name: String| async move {
    let greeting = ctx.schedule_activity("Greet", name)
        .into_activity().await?;
    Ok(greeting)
};

// 3. Set up and run
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await.unwrap());
let orchestrations = OrchestrationRegistry::builder()
    .register("HelloWorld", orchestration)
    .build();

let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
let client = Client::new(store);
client.start_orchestration("inst-1", "HelloWorld", "World").await?;
```

## Common Patterns

### 1. Function Chaining
```rust
async fn process_order(ctx: OrchestrationContext) -> Result<String, String> {
    let inventory = ctx.schedule_activity("ReserveInventory", "item1").into_activity().await?;
    let payment = ctx.schedule_activity("ProcessPayment", "card123").into_activity().await?;
    let shipping = ctx.schedule_activity("ShipItem", &inventory).into_activity().await?;
    Ok(shipping)
}
```

### 2. Parallel Processing (Fan-Out/Fan-In)
```rust
async fn process_multiple(ctx: OrchestrationContext) -> Vec<String> {
    let futures = vec![
        ctx.schedule_activity("Process", "item1"),
        ctx.schedule_activity("Process", "item2"),
        ctx.schedule_activity("Process", "item3"),
    ];
    let results = ctx.join(futures).await;
    // Process results...
}
```

### 3. Human-in-the-Loop (Approvals)
```rust
async fn approval_workflow(ctx: OrchestrationContext) -> String {
    let timer = ctx.schedule_timer(30000); // 30 second timeout
    let approval = ctx.schedule_wait("ApprovalEvent");
    
    let (_, result) = ctx.select2(timer, approval).await;
    match result {
        DurableOutput::External(data) => data, // Got approval
        DurableOutput::Timer => "timeout".to_string(), // Timeout
        _ => "error".to_string(),
    }
}
```

## Key APIs

### OrchestrationContext Methods
- `ctx.schedule_activity(name, input)` - Schedule an activity
- `ctx.schedule_timer(delay_ms)` - Create a timer
- `ctx.schedule_wait(event_name)` - Wait for external event
- `ctx.join(futures)` - Wait for all futures to complete
- `ctx.select2(future1, future2)` - Race between two futures
- `ctx.trace_info(message)` - Logging (replay-safe)

### DurableFuture Methods
- `.into_activity()` - Await activity result
- `.into_timer()` - Await timer completion
- `.into_event()` - Await external event

## Complete Working Example

```rust
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup
    let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);
    
    let activities = ActivityRegistry::builder()
        .register("ProcessOrder", |order_id: String| async move {
            println!("Processing order: {}", order_id);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(format!("Order {} processed", order_id))
        })
        .build();

    let orchestration = |ctx: OrchestrationContext, order_id: String| async move {
        ctx.trace_info("Starting order processing");
        
        // Process multiple orders in parallel
        let futures = vec![
            ctx.schedule_activity("ProcessOrder", format!("{}-1", order_id)),
            ctx.schedule_activity("ProcessOrder", format!("{}-2", order_id)),
        ];
        
        let results = ctx.join(futures).await;
        let success_count = results.len();
        
        Ok(format!("Processed {} orders successfully", success_count))
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("OrderProcessor", orchestration)
        .build();

    // Run
    let rt = runtime::Runtime::start_with_store(
        store.clone(), Arc::new(activities), orchestrations
    ).await;
    
    let client = Client::new(store);
    client.start_orchestration("order-1", "OrderProcessor", "ORDER-123").await?;
    
    match client.wait_for_orchestration("order-1", std::time::Duration::from_secs(5)).await? {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Success: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Failed: {}", error);
        }
        _ => println!("⏳ Still running..."),
    }

    rt.shutdown(None).await;  // Graceful shutdown
    Ok(())
}
```

## Next Steps

1. **Run the examples**: `cargo run --example hello_world`
2. **Read the full docs**: Check `examples/README.md` for comprehensive examples
3. **Explore patterns**: Look at `tests/e2e_samples.rs` for advanced scenarios
4. **Build something**: Start with a simple workflow and add complexity

## Why Use Duroxide?

- **Reliability**: Your workflows survive crashes, restarts, and failures
- **Simplicity**: Write normal async Rust code, Duroxide handles the complexity
- **Determinism**: Same input always produces the same output, even after failures
- **Composability**: Build complex workflows from simple, reusable pieces
- **Observability**: Built-in logging and tracing for debugging

## Common Use Cases

- **E-commerce**: Order processing, inventory management, payment workflows
- **DevOps**: Deployment pipelines, infrastructure provisioning, rollbacks
- **Data Processing**: ETL pipelines, batch processing, data validation
- **Business Processes**: Approval workflows, document processing, notifications
- **IoT**: Device management, data collection, alert processing

## Getting Help

- **Examples**: Start with `examples/` directory
- **Tests**: Look at `tests/e2e_samples.rs` for working code
- **Documentation**: Check `docs/` for detailed architecture info
- **Issues**: Open an issue if you find bugs or need help

---

**Ready to build reliable workflows? Start with `cargo run --example hello_world`!**
