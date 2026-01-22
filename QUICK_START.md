# Duroxide Quick Start Guide

This guide helps you get started with Duroxide in 5 minutes. Perfect for LLMs and developers who want to understand the framework quickly.

## What is Duroxide?

Duroxide is a framework for building **reliable, long-running workflows** that can survive failures and restarts. Think of it as "async Rust with superpowers" - your workflows can run for hours, days, or indefinitely.

## Core Concepts (30 seconds)

- **Orchestrations**: Your main workflow logic (async functions)
- **Activities**: Stateless functions that do actual work (can use sleep/polling/HTTP internally)
- **Timers**: Use `ctx.schedule_timer(ms)` for orchestration-level delays
- **Deterministic Replay**: If something fails, Duroxide replays your code to get back to the same state
- **Durable Futures**: Special futures that remember their state across restarts

## ⚠️ Key Rules

### 1. Timers vs Activities
**For orchestration delays, use timers. Activities can use any async operations:**

```rust
// ✅ CORRECT: Use timers for orchestration delays
use std::time::Duration;
ctx.schedule_timer(Duration::from_secs(5)).await; // Wait 5 seconds

// ✅ ALSO CORRECT: Activities can use sleep, HTTP calls, database queries, etc.
// Activities are just regular async functions and can do anything
```

### 2. Direct Await Pattern
**schedule_* methods return futures that can be awaited directly:**

```rust
// ✅ CORRECT patterns - await directly:
let result = ctx.schedule_activity("Task", "input").await?;
ctx.schedule_timer(Duration::from_secs(5)).await;
let event = ctx.schedule_wait("Event").await;
let sub_result = ctx.schedule_sub_orchestration("Child", "input").await?;
```

## Minimal Example

> **Note**: This example uses the bundled SQLite provider. Add to your `Cargo.toml`:
> ```toml
> duroxide = { version = "0.1", features = ["sqlite"] }
> ```

```rust
use duroxide::*;
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

// 1. Define an activity (your business logic)
let activities = ActivityRegistry::builder()
    .register("Greet", |ctx: ActivityContext, name: String| async move {
        Ok(format!("Hello, {}!", name))
    })
    .build();

// 2. Define an orchestration (your workflow)
let orchestration = |ctx: OrchestrationContext, name: String| async move {
    let greeting = ctx.schedule_activity("Greet", name).await?;
    Ok(greeting)
};

// 3. Set up and run
let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await.unwrap());
let orchestrations = OrchestrationRegistry::builder()
    .register("HelloWorld", orchestration)
    .build();

let rt = Runtime::start_with_store(store.clone(), activities, orchestrations).await;
let client = Client::new(store);
client.start_orchestration("inst-1", "HelloWorld", "World").await?; // Returns Result<(), ClientError>
```

## Common Patterns

### 1. Function Chaining
```rust
async fn process_order(ctx: OrchestrationContext) -> Result<String, String> {
    let inventory = ctx.schedule_activity("ReserveInventory", "item1").await?;
    let payment = ctx.schedule_activity("ProcessPayment", "card123").await?;
    let shipping = ctx.schedule_activity("ShipItem", &inventory).await?;
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
use duroxide::Either2;

async fn approval_workflow(ctx: OrchestrationContext) -> String {
    let timer = ctx.schedule_timer(std::time::Duration::from_secs(30)); // 30 second timeout
    let approval = ctx.schedule_wait("ApprovalEvent");
    
    match ctx.select2(timer, approval).await {
        Either2::First(()) => "timeout".to_string(), // Timeout
        Either2::Second(data) => data, // Got approval
    }
}
```

## Key APIs

### OrchestrationContext Methods
- `ctx.schedule_activity(name, input)` - Schedule an activity (returns `impl Future<Output = Result<String, String>>`)
- `ctx.schedule_timer(delay)` - Create a timer (returns `impl Future<Output = ()>`)
- `ctx.schedule_wait(event_name)` - Wait for external event (returns `impl Future<Output = String>`)
- `ctx.schedule_sub_orchestration(name, input)` - Start child orchestration (returns `impl Future<Output = Result<String, String>>`)
- `ctx.join(futures)` - Wait for all futures to complete
- `ctx.select2(future1, future2)` - Race between two futures (returns `Either2<T1, T2>`)
- `ctx.trace_info(message)` - Logging (replay-safe)

## Complete Working Example

```rust
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup
    let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
    
    let activities = ActivityRegistry::builder()
        .register("ProcessOrder", |ctx: ActivityContext, order_id: String| async move {
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
        store.clone(), activities, orchestrations
    ).await;
    
    let client = Client::new(store);
    client.start_orchestration("order-1", "OrderProcessor", "ORDER-123").await?; // Returns Result<(), ClientError>
    
    match client.wait_for_orchestration("order-1", std::time::Duration::from_secs(5)).await? { // Returns Result<OrchestrationStatus, ClientError>
        OrchestrationStatus::Completed { output } => {
            println!("✅ Success: {}", output);
        }
        OrchestrationStatus::Failed { details } => {
            println!("❌ Failed ({}): {}", details.category(), details.display_message());
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
