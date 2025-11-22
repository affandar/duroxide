# Duroxide Orchestration Writing Guide

**For:** LLMs and humans building durable workflows with Duroxide  
**Reference:** See `tests/e2e_samples.rs` and `examples/` for complete working examples

---

## Table of Contents

1. [Quick Start](#quick-start)
2. [Core Concepts](#core-concepts)
3. [Determinism Rules](#determinism-rules)
4. [API Reference](#api-reference)
5. [Client API Reference](#client-api-reference)
6. [Common Patterns](#common-patterns)
7. [Anti-Patterns (What to Avoid)](#anti-patterns)
8. [Complete Examples](#complete-examples)
9. [Testing Your Orchestrations](#testing)

---

## Quick Start

### Minimum Viable Orchestration

```rust
use duroxide::{ActivityContext, OrchestrationContext, OrchestrationRegistry, Client};
use duroxide::runtime::{self, registry::ActivityRegistry};
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create storage
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await?);
    
    // 2. Register activities (side-effecting work)
    let activities = ActivityRegistry::builder()
        .register("Greet", |ctx: ActivityContext, name: String| async move {
            Ok(format!("Hello, {}!", name))
        })
        .build();
    
    // 3. Define orchestration (deterministic coordination)
    let orchestration = |ctx: OrchestrationContext, name: String| async move {
        let greeting = ctx.schedule_activity("Greet", name)
            .into_activity().await?;
        Ok(greeting)
    };
    
    // 4. Register and start runtime
    let orchestrations = OrchestrationRegistry::builder()
        .register("HelloWorld", orchestration)
        .build();
    
    let rt = runtime::Runtime::start_with_store(
        store.clone(), 
        Arc::new(activities), 
        orchestrations
    ).await;
    
    // 5. Start an instance
    let client = Client::new(store);
    client.start_orchestration("my-instance-1", "HelloWorld", "World").await?;
    
    // 6. Wait for completion
    let status = client.wait_for_orchestration(
        "my-instance-1", 
        std::time::Duration::from_secs(5)
    ).await?;
    
    println!("Result: {:?}", status);
    
    rt.shutdown(None).await;
    Ok(())
}
```

---

## Core Concepts

### Orchestrations vs Activities

**Orchestrations** (deterministic coordination logic):
- Written as async functions: `|ctx: OrchestrationContext, input: T| async move { ... }`
- Must be **deterministic** (same input → same decisions)
- **Only** schedule work, don't do work themselves
- Can use: if/else, loops, match, any Rust control flow
- **CANNOT** use: random, time, I/O, non-deterministic APIs
- **Purpose:** Coordinate multi-step workflows, contain business logic

**Activities** (single-purpose execution units):
- Stateless async functions: `|ctx: ActivityContext, input: T| async move { ... }`
- Can do **anything**: database, HTTP, file I/O, polling, VM provisioning, etc.
- **CAN sleep/poll** internally as part of their work (e.g., waiting for VM to provision)
- Should be **idempotent** (tolerate retries)
- Should be **single-purpose** (do ONE thing well)
- Executed by worker dispatcher (can fail and retry)
- Return `Result<String, String>` (success or error)
- First parameter `ActivityContext` provides logging and metadata access

**Key Principle: Separation of Concerns**
- **Activities** = What to do (execution)
- **Orchestrations** = When and how to do it (coordination)

**Mental Model:**
- Orchestration = conductor (coordinates the orchestra, decides when each musician plays)
- Activity = musician (actually plays the music, focuses on playing well)
- Pull multi-step logic UP to orchestrations, keep activities focused

**Example:**
```rust
// ✅ GOOD: Activity polls for VM readiness (single-purpose: provision VM)
// Note: Long-running activities (minutes to hours) are fully supported via automatic lock renewal
activities.register("ProvisionVM", |ctx: ActivityContext, config| async move {
    let vm = create_vm(config).await?;
    while !vm_ready(&vm).await {
        tokio::time::sleep(Duration::from_secs(5)).await;  // ✅ Internal polling is fine
    }
    Ok(vm.id)
});

// ✅ GOOD: Orchestration coordinates multiple activities
async fn deploy_app(ctx: OrchestrationContext, config: String) -> Result<String, String> {
    let vm_id = ctx.schedule_activity("ProvisionVM", config.clone()).into_activity().await?;
    let app_id = ctx.schedule_activity("DeployApp", vm_id).into_activity().await?;
    let health = ctx.schedule_activity("HealthCheck", app_id.clone()).into_activity().await?;
    
    if health != "healthy" {
        // ✅ Business logic in orchestration
        ctx.schedule_activity("Rollback", app_id).into_activity().await?;
        return Err("Deployment unhealthy".to_string());
    }
    
    Ok(app_id)
}
```

### The Replay Model

**Why replay?**
Orchestrations can run for hours/days/months and must survive crashes.

**How it works:**
1. **First execution**: Orchestration runs, schedules activities, decisions recorded as events
2. **After crash/restart**: Orchestration code runs again, but replays from history
3. **Awaits resolve from history**: No duplicate side effects, fast forward to where we left off

**Example:**
```rust
async fn my_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Turn 1 (first execution):
    let result = ctx.schedule_activity("Step1", input).into_activity().await?;
    // ↑ Schedules Step1, waits for completion
    // [CRASH happens here]
    
    // Turn 2 (after restart - REPLAY):
    let result = ctx.schedule_activity("Step1", input).into_activity().await?;
    // ↑ Doesn't re-schedule! Reads Step1 completion from history
    
    let result2 = ctx.schedule_activity("Step2", result).into_activity().await?;
    // ↑ Schedules Step2 for FIRST time (not in history yet)
    
    Ok(result2)
}
```

### Durable Futures

All `schedule_*` methods return `DurableFuture`:
- Unified type that can represent any async operation
- **Must convert** before awaiting: `.into_activity()`, `.into_timer()`, `.into_event()`, `.into_sub_orchestration()`
- Can compose with `ctx.select2()` / `ctx.join()`

```rust
let fut = ctx.schedule_activity("Task", "input");
// fut is DurableFuture - can't await directly!

let result = fut.into_activity().await?;
// ✅ Now converted to awaitable future
```

---

## Determinism Rules

### ✅ DO (Safe in Orchestrations)

```rust
async fn safe_orchestration(ctx: OrchestrationContext, count: i32) -> Result<String, String> {
    // ✅ Control flow
    if count > 10 {
        return Ok("too many".to_string());
    }
    
    // ✅ Loops
    for i in 0..count {
        ctx.schedule_activity("Process", i.to_string()).into_activity().await?;
    }
    
    // ✅ Pattern matching
    let status = ctx.schedule_activity("GetStatus", "").into_activity().await?;
    match status.as_str() {
        "ready" => { /* ... */ }
        "pending" => { /* ... */ }
        _ => { /* ... */ }
    }
    
    // ✅ Timers for delays
    ctx.schedule_timer(std::time::Duration::from_secs(5)).into_timer().await;
    
    // ✅ External events for signals
    let approval = ctx.schedule_wait("ApprovalEvent").into_event().await;
    
    // ✅ Logging (replay-safe)
    ctx.trace_info("Step completed");
    
    // ✅ Deterministic GUIDs
    let id = ctx.new_guid().await?;
    
    // ✅ Deterministic timestamps
    let now_ms = ctx.utcnow_ms().await?;
    
    Ok("done".to_string())
}
```

### ❌ DON'T (Non-Deterministic)

```rust
async fn unsafe_orchestration(ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    // ❌ Random numbers
    let random = rand::random::<u64>();  // Different on each replay!
    
    // ❌ System time
    let now = std::time::SystemTime::now();  // Different on each replay!
    
    // ❌ I/O in orchestration
    let file_contents = std::fs::read_to_string("data.txt")?;  // Side effect!
    
    // ❌ HTTP in orchestration
    let response = reqwest::get("https://api.example.com").await?;  // Side effect!
    
    // ❌ Sleep in orchestration
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;  // Use ctx.schedule_timer() instead!
    
    // ❌ Direct await on activity (wrong type!)
    let result = ctx.schedule_activity("Task", "input").await;  // Missing .into_activity()!
    
    Ok("unsafe".to_string())
}
```

**Instead, use:**
```rust
async fn safe_version(ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    // ✅ Random → Use ctx.new_guid() or activity
    let id = ctx.new_guid().await?;
    
    // ✅ Time → Use ctx.utcnow_ms()
    let now = ctx.utcnow_ms().await?;
    
    // ✅ I/O → Use activities
    let file_contents = ctx.schedule_activity("ReadFile", "data.txt").into_activity().await?;
    
    // ✅ HTTP → Use activities
    let response = ctx.schedule_activity("HttpGet", "https://api.example.com").into_activity().await?;
    
    // ✅ Sleep → Use timers
    ctx.schedule_timer(5000).into_timer().await;
    
    // ✅ Activity → Use .into_activity()
    let result = ctx.schedule_activity("Task", "input").into_activity().await?;
    
    Ok(result)
}
```

---

## API Reference

### OrchestrationContext Methods

#### Scheduling Activities

```rust
// String I/O (basic)
fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture

// Typed I/O (with serde)
fn schedule_activity_typed<In: Serialize, Out: DeserializeOwned>(
    &self, 
    name: impl Into<String>, 
    input: &In
) -> DurableFuture

// Usage:
let result = ctx.schedule_activity("ProcessOrder", order_json)
    .into_activity().await?;

let result: OrderResult = ctx.schedule_activity_typed("ProcessOrder", &order_data)
    .into_activity_typed().await?;
```

#### Scheduling Timers

```rust
// Delay in milliseconds
fn schedule_timer(&self, delay: Duration) -> DurableFuture

// Usage:
ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;  // Wait 5 seconds
ctx.schedule_timer(Duration::from_secs(60)).into_timer().await;  // Wait 1 minute
```

#### External Events

```rust
// Wait for external signal by name
fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture

// Usage:
let approval_data = ctx.schedule_wait("ApprovalEvent").into_event().await;

// Typed version
let approval: ApprovalData = ctx.schedule_wait("ApprovalEvent")
    .into_event_typed().await;
```

#### Sub-Orchestrations

```rust
// Start child orchestration
fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture

// Usage:
let child_result = ctx.schedule_sub_orchestration("ChildWorkflow", input_json)
    .into_sub_orchestration().await?;

// Typed version
let child_result: ChildOutput = ctx.schedule_sub_orchestration_typed("ChildWorkflow", &child_input)
    .into_sub_orchestration_typed().await?;
```

#### Detached Orchestrations (Fire-and-Forget)

```rust
// Start orchestration without waiting for result
fn schedule_orchestration(&self, name: impl Into<String>, instance: impl Into<String>, input: impl Into<String>)

// Usage:
ctx.schedule_orchestration("EmailNotification", "email-123", email_json);
// Returns immediately, doesn't wait for completion
```

#### Composition (Select/Join)

```rust
// Select: first to complete wins
fn select2(&self, a: DurableFuture, b: DurableFuture) -> SelectFuture
fn select(&self, futures: Vec<DurableFuture>) -> SelectFuture

// Join: wait for all
fn join(&self, futures: Vec<DurableFuture>) -> JoinFuture

// Usage:
let timer = ctx.schedule_timer(Duration::from_secs(30));
let approval = ctx.schedule_wait("Approval");
let (winner, output) = ctx.select2(timer, approval).await;

match winner {
    0 => println!("Timed out"),
    1 => println!("Approved: {:?}", output),
    _ => unreachable!(),
}

// Join example
let futs = vec![
    ctx.schedule_activity("Task1", "a"),
    ctx.schedule_activity("Task2", "b"),
    ctx.schedule_activity("Task3", "c"),
];
let results = ctx.join(futs).await;  // Wait for all 3
```

#### Continue As New

```rust
// End current execution, start fresh with new input
fn continue_as_new(&self, input: impl Into<String>)

// Usage:
async fn pagination(ctx: OrchestrationContext, cursor: String) -> Result<(), String> {
    let next_cursor = ctx.schedule_activity("ProcessPage", cursor).into_activity().await?;
    
    if next_cursor == "EOF" {
        Ok(())
    } else {
        ctx.continue_as_new(next_cursor);  // Start new execution
        Ok(())
    }
}
```

Note: Each Continue As New starts a new execution with an empty history. The runtime stamps a fresh `OrchestrationStarted { event_id: 1 }` for the new execution; activities, timers, and external events always target the currently active execution only.

#### Logging (Replay-Safe)

```rust
fn trace_info(&self, message: impl Into<String>)
fn trace_warn(&self, message: impl Into<String>)
fn trace_error(&self, message: impl Into<String>)
fn trace_debug(&self, message: impl Into<String>)

// Usage:
ctx.trace_info("Processing order 12345");
ctx.trace_warn(format!("Retry attempt {}", attempt));
ctx.trace_error("Payment failed");
```

> **Default log targets**
> The runtime installs a filter equivalent to `error,duroxide::orchestration={level},duroxide::activity={level}` (where `{level}` comes from `ObservabilityConfig.log_level`). This keeps stdout focused on orchestration and activity trace helpers. To opt into additional targets—such as the internal runtime dispatcher logs—override `RUST_LOG` before starting your app:
>
> ```bash
> RUST_LOG=error,duroxide::orchestration=info,duroxide::activity=info,duroxide::runtime=debug cargo run --example with_observability
> ```
>
> You can add any other targets in the same way (for example `duroxide::providers::sqlite=trace`). Setting `RUST_LOG` always takes precedence over the runtime defaults.

#### System Calls (Deterministic Non-Determinism)

```rust
// Generate deterministic GUID
async fn new_guid(&self) -> Result<String, String>

// Get deterministic UTC timestamp
async fn utcnow_ms(&self) -> Result<u64, String>

// Usage:
let correlation_id = ctx.new_guid().await?;
let created_at = ctx.utcnow_ms().await?;
```

---

## Client API Reference

The `Client` provides control-plane operations for managing orchestration instances. It communicates with the runtime through the shared `Provider` (no direct coupling).

### Creating a Client

```rust
use duroxide::Client;
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
let client = Client::new(store);
```

**Key Points:**
- Client shares the same Provider instance as the Runtime
- Client is `Clone` and thread-safe
- Can be used from any process, even without a running Runtime

### Core Operations

#### Start an Orchestration

```rust
// Basic string input
client.start_orchestration("order-123", "ProcessOrder", r#"{"customer_id": "c1"}"#).await?;

// With explicit version
client.start_orchestration_versioned("order-456", "ProcessOrder", "2.0.0", "{}").await?;

// With typed input
#[derive(Serialize)]
struct OrderInput {
    customer_id: String,
    amount: f64,
}

let input = OrderInput {
    customer_id: "c1".to_string(),
    amount: 99.99,
};
client.start_orchestration_typed("order-789", "ProcessOrder", &input).await?;
```

**Behavior:**
- Enqueues a `StartOrchestration` work item
- Runtime picks it up and begins execution
- Returns immediately (non-blocking)
- Use `wait_for_orchestration()` to wait for completion

#### Raise External Event

```rust
// String payload
client.raise_event("order-123", "PaymentReceived", r#"{"amount": 99.99}"#).await?;

// Typed payload
#[derive(Serialize)]
struct PaymentEvent {
    transaction_id: String,
    amount: f64,
}

let event = PaymentEvent {
    transaction_id: "tx-456".to_string(),
    amount: 99.99,
};
client.raise_event_typed("order-123", "PaymentReceived", &event).await?;
```

**Use Cases:**
- Human approvals
- Webhook callbacks
- External system notifications
- Timer-based triggers from external schedulers

#### Cancel an Orchestration

```rust
client.cancel_instance("order-123", "Customer requested cancellation").await?;
```

**Behavior:**
- Enqueues a `CancelInstance` work item
- Runtime processes it on next turn
- Orchestration receives cancellation and can handle gracefully

#### Check Status

```rust
use duroxide::OrchestrationStatus;

let status = client.get_orchestration_status("order-123").await?; // Returns Result<OrchestrationStatus, ClientError>

match status {
    OrchestrationStatus::Running => println!("Still processing..."),
    OrchestrationStatus::Completed { output } => println!("Done: {}", output),
    OrchestrationStatus::Failed { details } => {
        eprintln!("Failed: {}", details.display_message());
        eprintln!("Category: {}", details.category());
    }
    OrchestrationStatus::NotFound => println!("Instance doesn't exist"),
}
```

**Returns:**
- `Running` - Orchestration in progress
- `Completed { output }` - Successful completion with result
- `Failed { details }` - Failed with error details
- `NotFound` - Instance doesn't exist or no history

#### Wait for Completion

```rust
use std::time::Duration;

// Wait up to 30 seconds
let status = client.wait_for_orchestration("order-123", Duration::from_secs(30)).await?;

match status {
    OrchestrationStatus::Completed { output } => {
        println!("Success: {}", output);
    }
    OrchestrationStatus::Failed { details } => {
        eprintln!("Failed: {}", details.display_message());
    }
    _ => unreachable!("wait_for_orchestration only returns terminal states"),
}

// Typed output
#[derive(Deserialize)]
struct OrderResult {
    order_id: String,
    status: String,
}

let result: OrderResult = client
    .wait_for_orchestration_typed("order-123", Duration::from_secs(30))
    .await?;
```

**Behavior:**
- Polls status with exponential backoff (5ms → 100ms)
- Returns when terminal state reached or timeout
- Only returns `Completed` or `Failed`, never `Running` or `NotFound`

### Management Operations (Optional)

These methods require a provider that implements `ProviderAdmin`. Check availability first:

```rust
if client.has_management_capability() {
    // Management operations available
} else {
    // Provider doesn't support management
}
```

#### List Instances

```rust
// List all instances
let instances = client.list_all_instances().await?;
for instance in instances {
    println!("Instance: {}", instance);
}

// Filter by status
let completed = client.list_instances_by_status("Completed").await?;
let running = client.list_instances_by_status("Running").await?;
let failed = client.list_instances_by_status("Failed").await?;
```

#### Get Instance Metadata

```rust
let info = client.get_instance_info("order-123").await?;

println!("Instance: {}", info.instance_id);
println!("Orchestration: {} v{}", info.orchestration_name, info.orchestration_version);
println!("Status: {}", info.status);
println!("Current Execution: {}", info.current_execution_id);
if let Some(output) = info.output {
    println!("Output: {}", output);
}
```

#### Inspect Executions

```rust
// List all execution IDs for an instance
let executions = client.list_executions("order-123").await?;
println!("Executions: {:?}", executions);  // [1, 2, 3] if ContinueAsNew used

// Get execution metadata
let exec_info = client.get_execution_info("order-123", 1).await?;
println!("Status: {}", exec_info.status);
println!("Events: {}", exec_info.event_count);

// Read execution history
let history = client.read_execution_history("order-123", 1).await?;
for event in history {
    println!("Event: {:?}", event);
}
```

#### System Metrics

```rust
let metrics = client.get_system_metrics().await?;
println!("Total instances: {}", metrics.total_instances);
println!("Running: {}", metrics.running_instances);
println!("Completed: {}", metrics.completed_instances);
println!("Failed: {}", metrics.failed_instances);

let queues = client.get_queue_depths().await?;
println!("Orchestrator queue: {}", queues.orchestrator_queue);
println!("Worker queue: {}", queues.worker_queue);
```

### Client Usage Patterns

#### Fire-and-Forget

```rust
// Start orchestration and don't wait
client.start_orchestration("background-job-1", "ProcessData", "input").await?;
// Continue with other work immediately
```

#### Synchronous Wait

```rust
// Start and wait for completion
client.start_orchestration("sync-job-1", "ProcessData", "input").await?;
let status = client.wait_for_orchestration("sync-job-1", Duration::from_secs(30)).await?;
```

#### Polling for Progress

```rust
client.start_orchestration("long-job-1", "ProcessData", "input").await?;

loop {
    let status = client.get_orchestration_status("long-job-1").await?; // Returns Result<OrchestrationStatus, ClientError>
    match status {
        OrchestrationStatus::Running => {
            println!("Still running...");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        OrchestrationStatus::Completed { output } => {
            println!("Completed: {}", output);
            break;
        }
        OrchestrationStatus::Failed { details } => {
            eprintln!("Failed: {}", details.display_message());
            break;
        }
        _ => break,
    }
}
```

#### Human-in-the-Loop

```rust
// In your orchestration:
async fn approval_workflow(ctx: OrchestrationContext, order_id: String) -> Result<String, String> {
    let result = ctx.schedule_activity("SubmitForApproval", order_id).into_activity().await?;
    
    // Wait for approval event
    let approval = ctx.schedule_wait("ManagerApproval").into_event().await;
    
    Ok(approval)
}

// External system receives approval and raises event:
async fn handle_approval(client: &Client, order_id: &str, approved: bool) {
    let event_data = serde_json::json!({
        "approved": approved,
        "approver": "manager@company.com",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    
    client.raise_event(
        &format!("order-{}", order_id),
        "ManagerApproval",
        event_data.to_string()
    ).await.unwrap();
}
```

#### Dashboard / Monitoring

```rust
async fn show_dashboard(client: &Client) {
    if !client.has_management_capability() {
        println!("Management features not available");
        return;
    }
    
    // System overview
    let metrics = client.get_system_metrics().await.unwrap();
    println!("=== System Status ===");
    println!("Total: {} | Running: {} | Completed: {} | Failed: {}",
        metrics.total_instances,
        metrics.running_instances,
        metrics.completed_instances,
        metrics.failed_instances
    );
    
    // Queue health
    let queues = client.get_queue_depths().await.unwrap();
    println!("\n=== Queue Depths ===");
    println!("Orchestrator: {} | Worker: {}", 
        queues.orchestrator_queue, 
        queues.worker_queue
    );
    
    // Recent failures
    let failed = client.list_instances_by_status("Failed").await.unwrap();
    println!("\n=== Recent Failures ===");
    for instance in failed.iter().take(10) {
        let info = client.get_instance_info(instance).await.unwrap();
        println!("{}: {} ({})", instance, info.orchestration_name, info.status);
    }
}
```

### Error Handling

```rust
use duroxide::OrchestrationStatus;

match client.start_orchestration("test", "MyOrch", "input").await {
    Ok(()) => println!("Started successfully"),
    Err(e) => eprintln!("Failed to start: {}", e),
}

// Wait with timeout handling
match client.wait_for_orchestration("test", Duration::from_secs(10)).await {
    Ok(OrchestrationStatus::Completed { output }) => println!("Done: {}", output),
    Ok(OrchestrationStatus::Failed { details }) => {
        eprintln!("Orchestration failed: {}", details.display_message());
        
        // Check error category for handling
        match details.category() {
            "application" => {
                // Business logic error - may want to retry with different input
            }
            "configuration" => {
                // Unregistered orchestration/activity - needs deployment fix
            }
            "infrastructure" => {
                // Provider/database error - may be transient
            }
            _ => {}
        }
    }
    Err(e) => eprintln!("Timeout or error: {:?}", e),
    _ => {}
}
```

### Client API Summary

| Method | Purpose | Returns |
|--------|---------|---------|
| `start_orchestration()` | Start new instance | `Result<(), ClientError>` |
| `raise_event()` | Send external event | `Result<(), ClientError>` |
| `cancel_instance()` | Request cancellation | `Result<(), ClientError>` |
| `get_orchestration_status()` | Check status | `Result<OrchestrationStatus, ClientError>` |
| `wait_for_orchestration()` | Wait for completion | `Result<OrchestrationStatus, ClientError>` |
| `has_management_capability()` | Check feature availability | `bool` |
| `list_all_instances()` | List instances | `Result<Vec<String>, ClientError>` |
| `list_instances_by_status()` | Filter by status | `Result<Vec<String>, ClientError>` |
| `get_instance_info()` | Instance metadata | `Result<InstanceInfo, ClientError>` |
| `get_execution_info()` | Execution metadata | `Result<ExecutionInfo, ClientError>` |
| `list_executions()` | List execution IDs | `Result<Vec<u64>, ClientError>` |
| `read_execution_history()` | Read history | `Result<Vec<Event>, ClientError>` |
| `get_system_metrics()` | System stats | `Result<SystemMetrics, ClientError>` |
| `get_queue_depths()` | Queue depths | `Result<QueueDepths, ClientError>` |

---

## Common Patterns

### 1. Function Chaining (Sequential Steps)

```rust
async fn process_order(ctx: OrchestrationContext, order_json: String) -> Result<String, String> {
    ctx.trace_info("Starting order processing");
    
    // Step 1: Validate
    let validated = ctx.schedule_activity("ValidateOrder", order_json)
        .into_activity().await?;
    
    // Step 2: Charge payment
    let payment_result = ctx.schedule_activity("ChargePayment", validated)
        .into_activity().await?;
    
    // Step 3: Ship order
    let tracking = ctx.schedule_activity("ShipOrder", payment_result)
        .into_activity().await?;
    
    // Step 4: Send confirmation
    let confirmation = ctx.schedule_activity("SendConfirmation", tracking)
        .into_activity().await?;
    
    ctx.trace_info("Order processing completed");
    Ok(confirmation)
}
```

### 2. Fan-Out/Fan-In (Parallel Processing)

```rust
async fn process_users(ctx: OrchestrationContext, user_ids_json: String) -> Result<String, String> {
    // Parse input
    let user_ids: Vec<String> = serde_json::from_str(&user_ids_json)?;
    
    // Fan-out: Schedule all tasks in parallel
    let futures: Vec<_> = user_ids
        .into_iter()
        .map(|user_id| ctx.schedule_activity("ProcessUser", user_id))
        .collect();
    
    // Fan-in: Wait for all to complete (deterministic order)
    let results = ctx.join(futures).await;
    
    // Process results
    let mut successes = 0;
    for result in results {
        match result {
            DurableOutput::Activity(Ok(_)) => successes += 1,
            DurableOutput::Activity(Err(e)) => {
                ctx.trace_warn(format!("User processing failed: {}", e));
            }
            _ => {}
        }
    }
    
    Ok(format!("Processed {} users successfully", successes))
}
```

### 3. Human-in-the-Loop with Timeout

```rust
async fn approval_workflow(ctx: OrchestrationContext, request_json: String) -> Result<String, String> {
    // Submit approval request
    let request_id = ctx.schedule_activity("SubmitApprovalRequest", request_json)
        .into_activity().await?;
    
    // Wait for approval or timeout
    let timeout = ctx.schedule_timer(std::time::Duration::from_millis(86_400_000));  // 24 hours
    let approval = ctx.schedule_wait("ApprovalEvent");
    
    let (winner, output) = ctx.select2(timeout, approval).await;
    
    match winner {
        0 => {
            // Timed out - escalate
            ctx.trace_warn("Approval timed out, escalating");
            ctx.schedule_activity("EscalateApproval", request_id)
                .into_activity().await
        }
        1 => {
            // Approved - extract data
            if let DurableOutput::External(data) = output {
                ctx.trace_info("Approval received");
                Ok(data)
            } else {
                Err("Unexpected output type".to_string())
            }
        }
        _ => unreachable!(),
    }
}
```

### 4. Error Handling and Compensation (Saga Pattern)

```rust
async fn saga_orchestration(ctx: OrchestrationContext, order_json: String) -> Result<String, String> {
    // Step 1: Reserve inventory
    let reservation = match ctx.schedule_activity("ReserveInventory", order_json.clone())
        .into_activity().await 
    {
        Ok(r) => r,
        Err(e) => {
            ctx.trace_error(format!("Reservation failed: {}", e));
            return Err("Reservation failed".to_string());
        }
    };
    
    // Step 2: Charge payment
    match ctx.schedule_activity("ChargePayment", reservation.clone())
        .into_activity().await 
    {
        Ok(payment) => Ok(payment),
        Err(e) => {
            // Compensation: Release inventory
            ctx.trace_warn(format!("Payment failed: {}, releasing inventory", e));
            ctx.schedule_activity("ReleaseInventory", reservation)
                .into_activity().await?;
            Err("Payment failed, order cancelled".to_string())
        }
    }
}
```

### 5. Retry with Exponential Backoff

```rust
async fn retry_orchestration(ctx: OrchestrationContext, task_input: String) -> Result<String, String> {
    let max_attempts = 5;
    let mut delay = std::time::Duration::from_secs(1);  // Start with 1 second
    
    for attempt in 1..=max_attempts {
        ctx.trace_info(format!("Attempt {} of {}", attempt, max_attempts));
        
        match ctx.schedule_activity("UnreliableTask", task_input.clone())
            .into_activity().await 
        {
            Ok(result) => {
                ctx.trace_info("Task succeeded");
                return Ok(result);
            }
            Err(e) => {
                ctx.trace_warn(format!("Attempt {} failed: {}", attempt, e));
                
                if attempt < max_attempts {
                    // Exponential backoff
                    ctx.schedule_timer(delay).into_timer().await;
                    delay *= 2;  // Double the delay
                } else {
                    ctx.trace_error("All attempts failed");
                    return Err(format!("Failed after {} attempts", max_attempts));
                }
            }
        }
    }
    
    unreachable!()
}
```

### 6. Understanding Error Types

Duroxide classifies errors into three categories for proper handling and metrics:

**Application Errors** - Business logic failures that your orchestration code sees and handles:
```rust
async fn order_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    match ctx.schedule_activity("ProcessPayment", input).into_activity().await {
        Ok(txn_id) => Ok(format!("paid:{txn_id}")),
        Err(e) => {
            // This is an Application error - you handle it
            // Examples: "insufficient_funds", "invalid_card"
            ctx.trace_warn(format!("Payment failed: {e}"));
            Err(format!("payment_error:{e}"))
        }
    }
}
```

**Configuration Errors** - Deployment issues that abort orchestration execution before your code runs:
- Unregistered orchestrations/activities
- Nondeterminism (code changed between replays)
- Missing versions

Your orchestration code **never sees** these errors. The runtime:
1. Records the failure in history (ActivityFailed + OrchestrationFailed events)
2. Marks the orchestration as failed with ErrorDetails::Configuration
3. Requires deployment fix (register missing activity, fix nondeterministic code)

**Infrastructure Errors** - Provider/system failures that abort execution:
- Database connection failures
- Data corruption
- Queue operation failures

These also abort before your code runs, may be retryable by the runtime.

**Checking Error Category** (for metrics/logging):
```rust
match client.get_orchestration_status("inst-1").await? { // Returns Result<OrchestrationStatus, ClientError>
    OrchestrationStatus::Failed { details } => {
        match details.category() {
            "infrastructure" => alert_ops_team(),      // System issue
            "configuration" => alert_dev_team(),       // Deployment issue  
            "application" => log_business_error(),     // Expected failures
        }
    }
    _ => {}
}
```

### 7. Loop and Accumulate

```rust
async fn accumulate_results(ctx: OrchestrationContext, items_json: String) -> Result<String, String> {
    let items: Vec<String> = serde_json::from_str(&items_json)?;
    let mut accumulator = Vec::new();
    
    for (i, item) in items.iter().enumerate() {
        ctx.trace_info(format!("Processing item {} of {}", i + 1, items.len()));
        
        let result = ctx.schedule_activity("ProcessItem", item.clone())
            .into_activity().await?;
        
        accumulator.push(result);
    }
    
    let summary = serde_json::to_string(&accumulator).unwrap();
    Ok(summary)
}
```

### 7. Conditional Workflow

```rust
async fn conditional_workflow(ctx: OrchestrationContext, order_json: String) -> Result<String, String> {
    // Get order value
    let order_value: f64 = ctx.schedule_activity("GetOrderValue", order_json.clone())
        .into_activity_typed().await?;
    
    if order_value > 1000.0 {
        // High value - require approval
        ctx.trace_info("High-value order, requiring approval");
        
        let approval_request = ctx.schedule_activity("CreateApprovalRequest", order_json.clone())
            .into_activity().await?;
        
        let timeout = ctx.schedule_timer(std::time::Duration::from_millis(3600_000));  // 1 hour
        let approval = ctx.schedule_wait("ManagerApproval");
        
        let (winner, _) = ctx.select2(timeout, approval).await;
        if winner == 0 {
            ctx.trace_warn("Approval timeout");
            return Err("Approval timeout".to_string());
        }
    }
    
    // Process the order
    ctx.schedule_activity("ProcessOrder", order_json).into_activity().await
}
```

### 8. Sub-Orchestrations (Composition)

```rust
async fn parent_workflow(ctx: OrchestrationContext, batch_json: String) -> Result<String, String> {
    let batches: Vec<String> = serde_json::from_str(&batch_json)?;
    
    // Process each batch as a sub-orchestration
    let mut results = Vec::new();
    for batch in batches {
        let result = ctx.schedule_sub_orchestration("ProcessBatch", batch)
            .into_sub_orchestration().await?;
        results.push(result);
    }
    
    Ok(serde_json::to_string(&results).unwrap())
}

async fn child_workflow(ctx: OrchestrationContext, batch: String) -> Result<String, String> {
    // Child has its own event history, isolated from parent
    let processed = ctx.schedule_activity("ProcessBatchItems", batch)
        .into_activity().await?;
    
    ctx.trace_info("Batch processing completed");
    Ok(processed)
}
```

### 9. Continue As New (Long-Running Workflows)

```rust
async fn eternal_monitor(ctx: OrchestrationContext, state_json: String) -> Result<(), String> {
    let state: MonitorState = serde_json::from_str(&state_json)?;
    
    // Do one iteration of work
    let check_result = ctx.schedule_activity("CheckSystem", state_json)
        .into_activity().await?;
    
    // Wait before next check
    ctx.schedule_timer(std::time::Duration::from_millis(60_000)).into_timer().await;  // 1 minute
    
    // Continue with updated state
    ctx.continue_as_new(check_result);
    
    Ok(())  // This execution ends, new one begins
}
```

---

## Anti-Patterns (What to Avoid)

### ❌ Anti-Pattern 1: Business Logic / Control Flow in Activities

```rust
// WRONG - Activity contains business logic and branching
activities.register("ProcessOrderComplex", |ctx: ActivityContext, order_json: String| async move {
    let order: Order = serde_json::from_str(&order_json)?;
    
    // ❌ Business logic in activity
    if order.total > 1000.0 {
        // Send for approval
        send_approval_request(&order).await?;
        wait_for_approval(&order.id).await?;  // Polling/waiting in activity
    }
    
    // ❌ Control flow in activity
    if order.customer_type == "premium" {
        process_premium(&order).await?;
    } else {
        process_standard(&order).await?;
    }
    
    Ok("processed".to_string())
});
```

```rust
// CORRECT - Business logic in orchestration, activities are single-purpose
// Activities: single-purpose, can sleep/poll internally if needed
activities.register("GetOrderValue", |ctx: ActivityContext, order_json: String| async move {
    let order: Order = serde_json::from_str(&order_json)?;
    Ok(order.total.to_string())
});

activities.register("SendApprovalRequest", |ctx: ActivityContext, order_json: String| async move {
    let order: Order = serde_json::from_str(&order_json)?;
    send_to_approval_system(&order).await?;
    Ok(order.id)
});

activities.register("ProcessPremiumOrder", |ctx: ActivityContext, order_json: String| async move {
    let order: Order = serde_json::from_str(&order_json)?;
    // Activity can poll/sleep internally - that's fine!
    let vm = provision_vm(&order).await?;
    while !vm_ready(&vm.id).await {
        tokio::time::sleep(Duration::from_secs(5)).await;  // ✅ OK in activity
    }
    process_on_vm(&order, &vm).await?;
    Ok("processed".to_string())
});

// Orchestration: contains business logic and control flow
async fn good_orch(ctx: OrchestrationContext, order_json: String) -> Result<String, String> {
    // Get order value
    let value_str = ctx.schedule_activity("GetOrderValue", order_json.clone())
        .into_activity().await?;
    let value: f64 = value_str.parse().unwrap();
    
    // ✅ Business logic in orchestration
    if value > 1000.0 {
        // Send for approval
        ctx.schedule_activity("SendApprovalRequest", order_json.clone())
            .into_activity().await?;
        
        // ✅ Orchestration-level timeout control
        let timeout = ctx.schedule_timer(std::time::Duration::from_millis(3600_000));  // 1 hour
        let approval = ctx.schedule_wait("ApprovalEvent");
        let (winner, _) = ctx.select2(timeout, approval).await;
        
        if winner == 0 {
            return Err("Approval timeout".to_string());
        }
    }
    
    // ✅ Control flow in orchestration
    let customer_type = ctx.schedule_activity("GetCustomerType", order_json.clone())
        .into_activity().await?;
    
    if customer_type == "premium" {
        ctx.schedule_activity("ProcessPremiumOrder", order_json).into_activity().await
    } else {
        ctx.schedule_activity("ProcessStandardOrder", order_json).into_activity().await
    }
}
```

**Why:** 
- Activities are **execution units** - they do ONE thing well
- Orchestrations are **coordination units** - they contain business logic and control flow
- Control flow in activities breaks composability and replay benefits
- An activity CAN sleep/poll internally (e.g., provisioning a VM) - that's perfectly fine!
- What matters: keep activities single-purpose, pull multi-step logic into orchestrations

### ❌ Anti-Pattern 2: Non-Deterministic Branching

```rust
// WRONG - Random branching
async fn bad_orch(ctx: OrchestrationContext) -> Result<String, String> {
    if rand::random::<bool>() {  // Different on each replay!
        ctx.schedule_activity("PathA", "").into_activity().await
    } else {
        ctx.schedule_activity("PathB", "").into_activity().await
    }
}
```

```rust
// CORRECT - Deterministic branching based on activity result
async fn good_orch(ctx: OrchestrationContext) -> Result<String, String> {
    let decision = ctx.schedule_activity("MakeDecision", "").into_activity().await?;
    
    if decision == "A" {
        ctx.schedule_activity("PathA", "").into_activity().await
    } else {
        ctx.schedule_activity("PathB", "").into_activity().await
    }
}
```

### ❌ Anti-Pattern 3: Forgetting .into_*() Conversion

```rust
// WRONG - Missing conversion (won't compile)
async fn bad_orch(ctx: OrchestrationContext) -> Result<String, String> {
    let result = ctx.schedule_activity("Task", "input").await?;  // Error!
    Ok(result)
}
```

```rust
// CORRECT - With conversion
async fn good_orch(ctx: OrchestrationContext) -> Result<String, String> {
    let result = ctx.schedule_activity("Task", "input").into_activity().await?;
    Ok(result)
}
```

### ❌ Anti-Pattern 4: Using Activity for Pure Delays/Timeouts

```rust
// WRONG - Activity that only sleeps (no business logic)
activities.register("Wait30Seconds", |ctx: ActivityContext, _: String| async move {
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    Ok("done".to_string())
});

async fn bad_delay(ctx: OrchestrationContext) -> Result<String, String> {
    ctx.schedule_activity("Wait30Seconds", "").into_activity().await?;
    // Just wasted a worker slot for 30 seconds!
    Ok("done".to_string())
}
```

```rust
// CORRECT - Use timer for pure delays
async fn good_delay(ctx: OrchestrationContext) -> Result<String, String> {
    ctx.schedule_timer(std::time::Duration::from_millis(30_000)).into_timer().await;
    // Timer doesn't block workers, handled by timer dispatcher
    Ok("done".to_string())
}
```

**Why:**
- If the ONLY purpose is to delay, use `schedule_timer()` - it's designed for that
- Activities are for doing work (DB, API, computation), not just waiting
- **BUT:** It's perfectly fine for an activity to sleep/poll AS PART of doing work:
  ```rust
  // ✅ FINE - Activity polls as part of provisioning work
  activities.register("ProvisionVM", |ctx: ActivityContext, config: String| async move {
      let vm_id = start_vm_creation(config).await?;
      
      // Polling is part of the provisioning logic
      while !vm_ready(&vm_id).await {
          tokio::time::sleep(Duration::from_secs(5)).await;  // ✅ OK - part of work
      }
      
      Ok(vm_id)
  });
  ```
- The rule: Use timers for orchestration-level delays, not for activity-internal polling

### ❌ Anti-Pattern 5: Infinite Loop Without Delay

```rust
// WRONG - Infinite loop hammering activities
async fn bad_poll(ctx: OrchestrationContext, task_id: String) -> Result<String, String> {
    loop {
        let status = ctx.schedule_activity("CheckStatus", task_id.clone())
            .into_activity().await?;
        
        if status == "complete" {
            return Ok(status);
        }
        // No delay - creates unlimited activities!
    }
}
```

```rust
// CORRECT - Loop with timer delays
async fn good_poll(ctx: OrchestrationContext, task_id: String) -> Result<String, String> {
    for _ in 0..100 {  // Max iterations to prevent infinite history
        let status = ctx.schedule_activity("CheckStatus", task_id.clone())
            .into_activity().await?;
        
        if status == "complete" {
            return Ok(status);
        }
        
        ctx.schedule_timer(std::time::Duration::from_millis(5000)).into_timer().await;  // Wait 5s before retry
    }
    
    Err("Polling timeout".to_string())
}
```

**Better:** Use Continue As New for eternal polling:
```rust
async fn eternal_poll(ctx: OrchestrationContext, task_id: String) -> Result<String, String> {
    let status = ctx.schedule_activity("CheckStatus", task_id.clone())
        .into_activity().await?;
    
    if status == "complete" {
        Ok(status)
    } else {
        ctx.schedule_timer(std::time::Duration::from_millis(5000)).into_timer().await;
        ctx.continue_as_new(task_id);  // Fresh history for next iteration
        Ok(())
    }
}
```

---

## Complete Examples

### E-Commerce Order Processing

```rust
use duroxide::{OrchestrationContext, DurableOutput};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Order {
    order_id: String,
    customer_id: String,
    items: Vec<OrderItem>,
    total: f64,
}

#[derive(Serialize, Deserialize)]
struct OrderItem {
    product_id: String,
    quantity: u32,
}

async fn process_order_orchestration(
    ctx: OrchestrationContext,
    order_json: String,
) -> Result<String, String> {
    let order: Order = serde_json::from_str(&order_json)
        .map_err(|e| format!("Invalid order JSON: {}", e))?;
    
    ctx.trace_info(format!("Processing order {}", order.order_id));
    
    // 1. Validate order
    let validation_result = ctx.schedule_activity("ValidateOrder", order_json.clone())
        .into_activity().await?;
    
    // 2. Reserve inventory for all items (fan-out)
    let reservation_futures: Vec<_> = order.items.iter()
        .map(|item| {
            let item_json = serde_json::to_string(item).unwrap();
            ctx.schedule_activity("ReserveInventory", item_json)
        })
        .collect();
    
    let reservation_results = ctx.join(reservation_futures).await;
    
    // Check all reservations succeeded
    let all_reserved = reservation_results.iter().all(|r| {
        matches!(r, DurableOutput::Activity(Ok(_)))
    });
    
    if !all_reserved {
        ctx.trace_error("Inventory reservation failed");
        return Err("Insufficient inventory".to_string());
    }
    
    // 3. Charge payment
    let payment_result = ctx.schedule_activity("ChargePayment", order_json.clone())
        .into_activity().await;
    
    match payment_result {
        Ok(payment_id) => {
            // 4. Fulfill order
            ctx.trace_info("Payment successful, fulfilling order");
            let fulfillment = ctx.schedule_activity("FulfillOrder", order_json)
                .into_activity().await?;
            
            Ok(fulfillment)
        }
        Err(payment_error) => {
            // Compensation: Release all reservations
            ctx.trace_warn("Payment failed, compensating");
            
            let release_futures: Vec<_> = order.items.iter()
                .map(|item| {
                    let item_json = serde_json::to_string(item).unwrap();
                    ctx.schedule_activity("ReleaseInventory", item_json)
                })
                .collect();
            
            ctx.join(release_futures).await;
            
            Err(format!("Payment failed: {}", payment_error))
        }
    }
}
```

### Document Approval Workflow

```rust
async fn document_approval(
    ctx: OrchestrationContext,
    document_id: String,
) -> Result<String, String> {
    ctx.trace_info(format!("Starting approval for document {}", document_id));
    
    // 1. Request manager approval
    let approval_request = ctx.schedule_activity(
        "SendApprovalRequest",
        format!("{{\"document_id\": \"{}\", \"level\": \"manager\"}}", document_id)
    ).into_activity().await?;
    
    // 2. Wait for approval or timeout (24 hours)
    let manager_timeout = ctx.schedule_timer(std::time::Duration::from_millis(86_400_000));
    let manager_approval = ctx.schedule_wait(format!("ManagerApproval_{}", document_id));
    
    let (winner, output) = ctx.select2(manager_timeout, manager_approval).await;
    
    let approval_data = match winner {
        0 => {
            // Manager didn't respond - auto-escalate to director
            ctx.trace_warn("Manager approval timeout, escalating to director");
            
            ctx.schedule_activity(
                "SendApprovalRequest",
                format!("{{\"document_id\": \"{}\", \"level\": \"director\"}}", document_id)
            ).into_activity().await?;
            
            // Wait for director (no timeout)
            ctx.schedule_wait(format!("DirectorApproval_{}", document_id))
                .into_event().await
        }
        1 => {
            // Manager approved
            if let DurableOutput::External(data) = output {
                ctx.trace_info("Manager approved document");
                data
            } else {
                return Err("Unexpected output type".to_string());
            }
        }
        _ => unreachable!(),
    };
    
    // 3. Finalize document
    let finalize_input = format!("{{\"document_id\": \"{}\", \"approval\": {}}}", 
        document_id, approval_data);
    
    ctx.schedule_activity("FinalizeDocument", finalize_input)
        .into_activity().await
}
```

### Batch Processing with Continue As New

```rust
async fn batch_processor(
    ctx: OrchestrationContext,
    cursor: String,
) -> Result<String, String> {
    ctx.trace_info(format!("Processing batch starting at cursor: {}", cursor));
    
    // Fetch next batch
    let batch_result = ctx.schedule_activity("FetchBatch", cursor)
        .into_activity().await?;
    
    let batch: BatchResult = serde_json::from_str(&batch_result)?;
    
    // Process items in this batch (fan-out)
    let process_futures: Vec<_> = batch.items.iter()
        .map(|item| ctx.schedule_activity("ProcessItem", item.clone()))
        .collect();
    
    let results = ctx.join(process_futures).await;
    
    // Count successes
    let success_count = results.iter()
        .filter(|r| matches!(r, DurableOutput::Activity(Ok(_))))
        .count();
    
    ctx.trace_info(format!("Processed {} of {} items", success_count, batch.items.len()));
    
    // Continue to next batch or complete
    if let Some(next_cursor) = batch.next_cursor {
        ctx.trace_info("More batches to process, continuing");
        ctx.continue_as_new(next_cursor);  // Start fresh with new cursor
        Ok(())
    } else {
        ctx.trace_info("All batches processed");
        Ok(format!("Batch processing complete"))
    }
}

#[derive(Deserialize)]
struct BatchResult {
    items: Vec<String>,
    next_cursor: Option<String>,
}
```

---

## Testing Your Orchestrations

### Unit Testing (Without Runtime)

```rust
use duroxide::{OrchestrationContext, Event, run_turn};

#[tokio::test]
async fn test_my_orchestration_logic() {
    // Define orchestration
    let orch = |ctx: OrchestrationContext, input: String| async move {
        let result = ctx.schedule_activity("Task", input).into_activity().await?;
        Ok(result)
    };
    
    // Turn 1: Schedule activity
    let history1 = vec![Event::OrchestrationStarted {
        event_id: 1,
        name: "TestOrch".to_string(),
        version: "1.0.0".to_string(),
        input: "test".to_string(),
        parent_instance: None,
        parent_id: None,
    }];
    
    let (history2, actions1, output1) = run_turn(history1, orch);
    
    assert!(output1.is_none());  // Pending on activity
    assert_eq!(actions1.len(), 1);  // Scheduled 1 activity
    
    // Simulate activity completion
    let mut history2 = history2;
    history2.push(Event::ActivityCompleted {
        event_id: 3,
        source_event_id: 2,
        result: "success".to_string(),
    });
    
    // Turn 2: Resume from completion
    let (_history3, _actions2, output2) = run_turn(history2, orch);
    
    assert!(output2.is_some());
    assert_eq!(output2.unwrap(), Ok("success".to_string()));
}
```

### Integration Testing (With Runtime)

```rust
#[tokio::test]
async fn test_full_workflow() {
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    
    let activities = ActivityRegistry::builder()
        .register("Task", |ctx: ActivityContext, input: String| async move {
            Ok(format!("processed: {}", input))
        })
        .build();
    
    let orch = |ctx: OrchestrationContext, input: String| async move {
        ctx.schedule_activity("Task", input).into_activity().await
    };
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestOrch", orch)
        .build();
    
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    ).await;
    
    let client = Client::new(store);
    client.start_orchestration("test-1", "TestOrch", "input").await.unwrap();
    
    let status = client.wait_for_orchestration("test-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    
    match status {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "processed: input");
        }
        _ => panic!("Expected completion"),
    }
    
    rt.shutdown(None).await;
}
```

---

## Best Practices

### 1. Keep Activities Idempotent

```rust
// Good: Idempotent database update
activities.register("UpdateUserStatus", |ctx: ActivityContext, user_json: String| async move {
    let user: User = serde_json::from_str(&user_json)?;
    
    // Upsert is idempotent
    db.execute("INSERT INTO users (id, status) VALUES (?, ?) ON CONFLICT (id) DO UPDATE SET status = ?",
        [&user.id, &user.status, &user.status])?;
    
    Ok(user.id)
});
```

### 2. Use Correlation IDs

```rust
async fn order_with_tracking(ctx: OrchestrationContext, order_json: String) -> Result<String, String> {
    // Generate correlation ID for entire workflow
    let correlation_id = ctx.new_guid().await?;
    
    // Pass correlation ID to all activities
    let validated = ctx.schedule_activity(
        "ValidateOrder",
        format!("{{\"order\": {}, \"correlation_id\": \"{}\"}}", order_json, correlation_id)
    ).into_activity().await?;
    
    // Activities can log with correlation ID for tracing
    // ...
    
    Ok(correlation_id)
}
```

### 3. Handle Partial Failures

```rust
async fn robust_batch_processing(ctx: OrchestrationContext, items_json: String) -> Result<String, String> {
    let items: Vec<String> = serde_json::from_str(&items_json)?;
    
    // Process all items (don't stop on first failure)
    let futures: Vec<_> = items.iter()
        .map(|item| ctx.schedule_activity("ProcessItem", item.clone()))
        .collect();
    
    let results = ctx.join(futures).await;
    
    // Collect successes and failures
    let mut successes = Vec::new();
    let mut failures = Vec::new();
    
    for (i, result) in results.iter().enumerate() {
        match result {
            DurableOutput::Activity(Ok(output)) => {
                successes.push((i, output.clone()));
            }
            DurableOutput::Activity(Err(error)) => {
                failures.push((i, error.clone()));
                ctx.trace_warn(format!("Item {} failed: {}", i, error));
            }
            _ => {}
        }
    }
    
    if failures.is_empty() {
        Ok(format!("All {} items succeeded", successes.len()))
    } else {
        Ok(format!("{} succeeded, {} failed", successes.len(), failures.len()))
    }
}
```

### 4. Use Typed APIs When Possible

```rust
#[derive(Serialize, Deserialize)]
struct OrderInput {
    customer_id: String,
    items: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct OrderOutput {
    order_id: String,
    status: String,
}

async fn typed_workflow(
    ctx: OrchestrationContext,
    order: OrderInput,
) -> Result<OrderOutput, String> {
    // Compile-time type safety
    let validation: ValidationResult = ctx
        .schedule_activity_typed("ValidateOrder", &order)
        .into_activity_typed()
        .await?;
    
    let order_id = ctx.new_guid().await?;
    
    Ok(OrderOutput {
        order_id,
        status: "completed".to_string(),
    })
}
```

---

## Debugging Tips

### 1. Use Trace Statements

```rust
async fn debuggable_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    ctx.trace_info(format!("Starting with input: {}", input));
    
    let step1 = ctx.schedule_activity("Step1", input).into_activity().await?;
    ctx.trace_info(format!("Step1 completed: {}", step1));
    
    let step2 = ctx.schedule_activity("Step2", step1).into_activity().await?;
    ctx.trace_info(format!("Step2 completed: {}", step2));
    
    ctx.trace_info("Workflow completed successfully");
    Ok(step2)
}
```

### 2. Inspect History

```rust
// Get orchestration history (requires management capability)
let client = Client::new(store);
let history = client.read_execution_history("my-instance", 1).await.unwrap();

for event in history {
    println!("{:?}", event);
}
```

### 3. Inspect Deterministic Flow

Use trace statements and history inspection to understand orchestration progress across turns. Each turn is triggered by new completions and is replay-safe; there is no public turn index.

---

## Advanced Patterns

### Dynamic Activity Scheduling

```rust
async fn dynamic_workflow(ctx: OrchestrationContext, config_json: String) -> Result<String, String> {
    let config: WorkflowConfig = serde_json::from_str(&config_json)?;
    
    // Determine activities to run based on input
    let activity_names = match config.workflow_type.as_str() {
        "standard" => vec!["Step1", "Step2", "Step3"],
        "expedited" => vec!["FastStep1", "FastStep2"],
        "manual" => vec!["ManualReview"],
        _ => return Err("Unknown workflow type".to_string()),
    };
    
    // Schedule determined activities
    let mut result = config_json;
    for activity_name in activity_names {
        result = ctx.schedule_activity(activity_name, result)
            .into_activity().await?;
    }
    
    Ok(result)
}
```

### Actor-Style Orchestrations

```rust
// Orchestration as a long-lived actor
async fn order_actor(ctx: OrchestrationContext, state_json: String) -> Result<(), String> {
    let mut state: OrderState = serde_json::from_str(&state_json)?;
    
    // Wait for next command
    let command = ctx.schedule_wait("OrderCommand").into_event().await;
    let cmd: Command = serde_json::from_str(&command)?;
    
    match cmd.action.as_str() {
        "cancel" => {
            state.status = "cancelled".to_string();
            ctx.schedule_activity("SendCancellationEmail", state_json).into_activity().await?;
            Ok(())  // Terminal
        }
        "ship" => {
            state.status = "shipped".to_string();
            ctx.schedule_activity("ShipOrder", serde_json::to_string(&state)?).into_activity().await?;
            
            // Continue waiting for more commands
            ctx.continue_as_new(serde_json::to_string(&state)?);
            Ok(())
        }
        _ => {
            // Unknown command, continue waiting
            ctx.continue_as_new(state_json);
            Ok(())
        }
    }
}
```

---

## Performance Considerations

### 1. Minimize History Size

Each orchestration turn appends events. Keep history manageable:

```rust
// ❌ Bad - Creates millions of events
async fn bad_loop(ctx: OrchestrationContext, _input: String) -> Result<String, String> {
    for i in 0..1_000_000 {  // Too many!
        ctx.schedule_activity("Process", i.to_string()).into_activity().await?;
    }
    Ok(())
}
```

```rust
// ✅ Good - Use Continue As New to reset history
async fn good_loop(ctx: OrchestrationContext, state_json: String) -> Result<String, String> {
    let state: State = serde_json::from_str(&state_json)?;
    
    // Process batch of 100
    for i in 0..100 {
        ctx.schedule_activity("Process", (state.offset + i).to_string())
            .into_activity().await?;
    }
    
    state.offset += 100;
    
    if state.offset < state.total {
        ctx.continue_as_new(serde_json::to_string(&state)?);  // Fresh history
        Ok(())
    } else {
        Ok("All processed".to_string())
    }
}
```

### 2. Batch Operations

```rust
// ❌ Slow - One DB call per item
async fn slow_updates(ctx: OrchestrationContext, items: Vec<String>) -> Result<(), String> {
    for item in items {
        ctx.schedule_activity("UpdateItem", item).into_activity().await?;
    }
    Ok(())
}
```

```rust
// ✅ Fast - Batch update in single activity
async fn fast_updates(ctx: OrchestrationContext, items_json: String) -> Result<(), String> {
    ctx.schedule_activity("UpdateItemsBatch", items_json).into_activity().await?;
    Ok(())
}
```

---

## Common Gotchas

### 1. Forgetting .into_*() Conversion

**Error:**
```
the trait `From<DurableFuture>` is not implemented for `Result<String, String>`
```

**Fix:** Add `.into_activity()` / `.into_timer()` / `.into_event()` / `.into_sub_orchestration()`

### 2. Using await on DurableFuture Directly

**Won't Compile:**
```rust
let result = ctx.schedule_activity("Task", "input").await?;
```

**Must Convert First:**
```rust
let result = ctx.schedule_activity("Task", "input").into_activity().await?;
```

### 3. Non-Deterministic Control Flow

**Symptom:** Orchestration replays differently, causes nondeterminism errors

**Fix:** Base all decisions on activity results or external events, never on time/random

### 4. Activities That Only Sleep

**Symptom:** Activity that only sleeps without doing actual work

**Fix:** Use `ctx.schedule_timer()` in orchestration; activities can sleep/poll as part of their work (e.g., provisioning resources)

---

## Checklist for Production Orchestrations

- [ ] All side effects in activities (no I/O in orchestration code)
- [ ] Orchestration delays use `schedule_timer()` (activities can sleep/poll as needed)
- [ ] All branching is deterministic (based on activity results)
- [ ] Activities are idempotent (can retry safely)
- [ ] Error handling for all activities (match on Result)
- [ ] Trace statements for observability (`ctx.trace_*()` in both orchestrations and activities)
- [ ] Continue As New for long-running workflows (avoid unbounded history)
- [ ] Timeouts on external events (use select2 with timer)
- [ ] Compensation logic for failures (saga pattern)
- [ ] Tests written (unit + integration)

---

## Quick Reference

### OrchestrationContext API

| Method | Returns | Use For |
|--------|---------|---------|
| `schedule_activity(name, input)` | `DurableFuture` | Database, HTTP, file I/O |
| `schedule_timer(delay)` | `DurableFuture` | Delays, timeouts, scheduling |
| `schedule_wait(event_name)` | `DurableFuture` | External signals, human input |
| `schedule_sub_orchestration(name, input)` | `DurableFuture` | Child workflows |
| `schedule_orchestration(name, instance, input)` | `()` | Fire-and-forget |
| `select2(a, b)` / `select(vec)` | `SelectFuture` | First to complete |
| `join(vec)` | `JoinFuture` | Wait for all |
| `continue_as_new(input)` | `()` | Reset history, keep running |
| `new_guid()` | `Future<String>` | Correlation IDs |
| `utcnow_ms()` | `Future<u64>` | Timestamps |
| `trace_info/warn/error(msg)` | `()` | Logging |

### DurableFuture Conversions

| Conversion | Return Type | Use After |
|------------|-------------|-----------|
| `.into_activity()` | `impl Future<Output = Result<String, String>>` | `schedule_activity` |
| `.into_activity_typed<T>()` | `impl Future<Output = Result<T, String>>` | `schedule_activity_typed` |
| `.into_timer()` | `impl Future<Output = ()>` | `schedule_timer` |
| `.into_event()` | `impl Future<Output = String>` | `schedule_wait` |
| `.into_event_typed<T>()` | `impl Future<Output = T>` | `schedule_wait_typed` |
| `.into_sub_orchestration()` | `impl Future<Output = Result<String, String>>` | `schedule_sub_orchestration` |

---

## Getting Help

- **Examples**: `examples/` directory - runnable samples
- **Tests**: `tests/e2e_samples.rs` - comprehensive patterns
- **API Docs**: Run `cargo doc --open`
- **Replay Debugging**: Check history with `client.read_execution_history()` (management capability required)
- **Cross-Crate Composition**: See `docs/cross-crate-registry-pattern.md` for organizing orchestrations across multiple crates

---

**You now have everything needed to build production-grade durable workflows!** 🎉

