# Duroxide Macro Naming Guide

## The `durable_*` Family

All durable/replay-safe macros use the **`durable_`** prefix for consistency.

### Complete Macro Reference

```rust
// === Durable Execution ===
durable!(fn(args))                   // Execute activity or sub-orchestration

// === Durable Tracing ===
durable_trace_info!("msg", ...)      // Info-level logging
durable_trace_warn!("msg", ...)      // Warning-level logging
durable_trace_error!("msg", ...)     // Error-level logging
durable_trace_debug!("msg", ...)     // Debug-level logging

// === Durable System Calls ===
durable_newguid!()                   // Deterministic GUID
durable_utcnow!()                    // Deterministic timestamp (milliseconds)
```

### Why All `durable_*`?

**Consistency:** Everything that's replay-safe uses the same prefix.

```rust
#[orchestration]
async fn my_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Everything durable starts with 'durable' - easy to remember!
    durable_trace_info!("Starting");
    let id = durable_newguid!().await?;
    let now = durable_utcnow!().await?;
    let result = durable!(my_activity(input)).await?;
    
    Ok(result)
}
```

---

## The Problem: Determinism

### What Goes Wrong Without Durable Macros

```rust
// ❌ DANGEROUS: Non-deterministic orchestration
#[orchestration]
async fn create_user(ctx: OrchestrationContext, email: String) -> Result<String, String> {
    tracing::info!("Creating user");         // ❌ Not in history
    let user_id = uuid::Uuid::new_v4();      // ❌ Different on replay!
    let created_at = SystemTime::now();      // ❌ Different on replay!
    
    // First execution:  user_id=abc-123, created_at=1000
    // Replay execution: user_id=xyz-789, created_at=2000  🐛 BUG!
    
    durable!(save_user(user_id.to_string(), email, created_at)).await?;
    Ok(user_id.to_string())
}
```

**Result:** Nondeterminism! Activity gets called with different parameters on replay.

### Solution: Durable Macros

```rust
// ✅ CORRECT: Deterministic orchestration
#[orchestration]
async fn create_user(ctx: OrchestrationContext, email: String) -> Result<String, String> {
    durable_trace_info!("Creating user");    // ✅ Recorded in history
    let user_id = durable_newguid!().await?; // ✅ Same on replay!
    let created_at = durable_utcnow!().await?; // ✅ Same on replay!
    
    // First execution:  user_id=abc-123, created_at=1000
    // Replay execution: user_id=abc-123, created_at=1000  ✅ Same!
    
    durable!(save_user(user_id, email, created_at.to_string())).await?;
    Ok(user_id)
}
```

---

## Naming Comparison

### Before (Confusing)

```rust
// Old naming - easy to make mistakes
ctx.trace_info("Message");           // Is this durable?
ctx.new_guid().await?;               // Is this deterministic?
ctx.utcnow_ms().await?;              // Is this replay-safe?
```

### After (Crystal Clear)

```rust
// New naming - impossible to confuse
durable_trace_info!("Message");      // ✅ Clearly durable
durable_newguid!().await?;           // ✅ Clearly deterministic
durable_utcnow!().await?;            // ✅ Clearly replay-safe
```

---

## Complete Example

```rust
use duroxide::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    customer_id: String,
    total: f64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Receipt {
    order_id: String,
    transaction_id: String,
    correlation_id: String,
    processed_at: u64,
    processing_time_ms: u64,
}

#[activity(typed)]
async fn charge_payment(order_id: String, amount: f64, idempotency_key: String) -> Result<String, String> {
    // Activities can use regular functions (not replayed)
    tracing::info!("Charging ${:.2} for order {}", amount, order_id);
    
    // Regular UUID is fine here (activity isn't replayed)
    let transaction_id = uuid::Uuid::new_v4().to_string();
    
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    tracing::info!("Payment successful: {}", transaction_id);
    Ok(transaction_id)
}

#[orchestration]
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<Receipt, String> {
    // === Durable logging ===
    durable_trace_info!("🚀 Starting order processing: {}", order.id);
    durable_trace_debug!("Customer: {}, Total: ${:.2}", order.customer_id, order.total);
    
    // === Durable system calls ===
    let correlation_id = durable_newguid!().await?;
    durable_trace_info!("Generated correlation ID: {}", correlation_id);
    
    let order_received_at = durable_utcnow!().await?;
    durable_trace_info!("Order timestamp: {}", order_received_at);
    
    // === Validation with durable logging ===
    if order.total > 10000.0 {
        durable_trace_warn!("⚠️  High-value order: ${:.2}", order.total);
    }
    
    if order.total <= 0.0 {
        durable_trace_error!("❌ Invalid order amount: ${:.2}", order.total);
        return Err("Invalid amount".into());
    }
    
    // === Durable activity call ===
    durable_trace_info!("Processing payment...");
    let transaction_id = durable!(charge_payment(
        order.id.clone(),
        order.total,
        correlation_id.clone()
    )).await?;
    durable_trace_info!("✅ Payment completed: {}", transaction_id);
    
    let completed_at = durable_utcnow!().await?;
    let processing_time_ms = completed_at - order_received_at;
    
    durable_trace_info!("Order processing completed in {}ms", processing_time_ms);
    
    if processing_time_ms > 5000 {
        durable_trace_warn!("Slow processing: {}ms (>5s threshold)", processing_time_ms);
    }
    
    Ok(Receipt {
        order_id: order.id,
        transaction_id,
        correlation_id,
        processed_at: completed_at,
        processing_time_ms,
    })
}

#[duroxide::main]
async fn main() {
    let order = Order {
        id: "ORD-12345".into(),
        customer_id: "CUST-789".into(),
        total: 99.99,
    };
    
    println!("🚀 Processing order: {}\n", order.id);
    
    process_order::start(&client, "order-1", order).await?;
    
    let receipt = match process_order::wait(&client, "order-1", Duration::from_secs(30)).await? {
        OrchestrationStatus::Completed { output } => output,
        OrchestrationStatus::Failed { error } => {
            eprintln!("❌ Order failed: {}", error);
            return Err(error.into());
        }
        _ => return Err("Unexpected status".into()),
    };
    
    println!("✅ Order processed successfully!");
    println!("   Order ID: {}", receipt.order_id);
    println!("   Transaction ID: {}", receipt.transaction_id);
    println!("   Correlation ID: {}", receipt.correlation_id);
    println!("   Processing time: {}ms", receipt.processing_time_ms);
}
```

---

## Macro Family Reference

### All Durable Macros

| Macro | Purpose | Returns | Await? |
|-------|---------|---------|--------|
| `durable!(fn(args))` | Execute activity/sub-orch | `Result<T, String>` | ✅ Yes |
| `durable_trace_info!("msg", ...)` | Log info message | `()` | ❌ No |
| `durable_trace_warn!("msg", ...)` | Log warning | `()` | ❌ No |
| `durable_trace_error!("msg", ...)` | Log error | `()` | ❌ No |
| `durable_trace_debug!("msg", ...)` | Log debug | `()` | ❌ No |
| `durable_newguid!()` | Generate GUID | `Result<String, String>` | ✅ Yes |
| `durable_utcnow!()` | Get timestamp (ms) | `Result<u64, String>` | ✅ Yes |

### Consistency Benefits

**Everything durable starts with `durable_`:**
```rust
// Type 'durable_' and autocomplete shows:
durable_trace_info!
durable_trace_warn!
durable_trace_error!
durable_trace_debug!
durable_newguid!
durable_utcnow!
make_durable!  // (conceptually part of the family)
```

**Easy to grep:**
```bash
# Find all durable operations
rg "durable_"

# Find all durable tracing
rg "durable_trace_"

# Find all durable system calls
rg "durable_newguid!|durable_utcnow!"
```

---

## Side-by-Side: Regular vs Durable

```rust
// ========== IN ACTIVITIES (non-replayed) ==========
#[activity(typed)]
async fn my_activity(input: String) -> Result<String, String> {
    // ✅ OK: Use regular functions
    tracing::info!("Activity executing");
    let id = uuid::Uuid::new_v4();
    let now = SystemTime::now();
    let random = rand::random::<u32>();
    
    Ok(format!("Done"))
}

// ========== IN ORCHESTRATIONS (replayed) ==========
#[orchestration]
async fn my_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // ✅ MUST: Use durable macros
    durable_trace_info!("Orchestration executing");
    let id = durable_newguid!().await?;
    let now = durable_utcnow!().await?;
    // No durable_random yet - would need to implement
    
    let result = durable!(my_activity(input)).await?;
    
    Ok(result)
}
```

---

## Why This Naming?

### Benefit 1: Impossible to Confuse

```rust
// These look nothing alike - good!
tracing::info!("Regular")           vs    durable_trace_info!("Durable")
uuid::Uuid::new_v4()                vs    durable_newguid!()
SystemTime::now()                   vs    durable_utcnow!()
```

### Benefit 2: Consistent Family

All durable operations have the same prefix:
- `durable_trace_*` - Logging
- `durable_newguid` - GUID
- `durable_utcnow` - Time
- `make_durable!` - Execution (conceptually `durable_execute!`)

### Benefit 3: Self-Documenting

```rust
let id = durable_newguid!().await?;  // Name tells you it's:
                                      // - durable (replay-safe)
                                      // - new guid (what it does)
                                      // - needs await (async)
```

### Benefit 4: Future Extensions

Easy to add more durable functions:
```rust
durable_random!()        // Deterministic random number
durable_hostname!()      // Current hostname (recorded)
durable_env_var!(key)    // Environment variable (recorded)
durable_process_id!()    // Process ID (recorded)
```

---

## Quick Reference Card

### What to Use Where

```rust
// ============ IN ORCHESTRATIONS ============
// (Everything must be deterministic/durable)

// Logging
durable_trace_info!("Info: {}", value);
durable_trace_warn!("Warning: {}", msg);
durable_trace_error!("Error: {}", err);
durable_trace_debug!("Debug: {:?}", state);

// System calls
let id = durable_newguid!().await?;
let ms = durable_utcnow!().await?;

// Execution
let result = durable!(my_activity(args)).await?;
let output = durable!(sub_orchestration(args)).await?;

// ============ IN ACTIVITIES ============
// (Can use regular functions - not replayed)

// Regular tracing (fine!)
tracing::info!("Activity running");
tracing::error!("Activity error: {}", err);

// Regular system functions (fine!)
let id = uuid::Uuid::new_v4();
let now = SystemTime::now();
let random = rand::random::<u32>();
```

---

## Migration Guide

### From Old Context Methods

```rust
// Old (still works!)
ctx.trace("INFO", "Message");
ctx.trace_info("Message");
ctx.new_guid().await?;
ctx.utcnow_ms().await?;

// New (preferred!)
durable_trace_info!("Message");
durable_newguid!().await?;
durable_utcnow!().await?;
```

Both work - macros are convenience wrappers around the context methods.

---

## Real-World Example

```rust
use duroxide::prelude::*;

#[activity(typed)]
async fn create_vm(config: VmConfig) -> Result<VmInstance, String> {
    // Regular tracing in activities - OK!
    tracing::info!("Creating VM: {}", config.name);
    
    let start = std::time::Instant::now();
    
    // Simulate API call
    tokio::time::sleep(Duration::from_millis(200)).await;
    
    tracing::info!("VM created in {:?}", start.elapsed());
    
    Ok(VmInstance {
        id: format!("vm-{}", uuid::Uuid::new_v4()),  // OK in activities!
        ip: "10.0.1.10".into(),
        status: "running".into(),
    })
}

#[orchestration]
async fn provision_vm_with_monitoring(
    ctx: OrchestrationContext,
    config: VmConfig,
) -> Result<VmProvisionResult, String> {
    // === Durable logging ===
    durable_trace_info!("🖥️  Starting VM provisioning: {}", config.name);
    
    // === Generate correlation ID (durable) ===
    let correlation_id = durable_newguid!().await?;
    durable_trace_info!("Correlation ID: {}", correlation_id);
    
    // === Record start time (durable) ===
    let started_at = durable_utcnow!().await?;
    durable_trace_debug!("Started at timestamp: {}", started_at);
    
    // === Create VM (durable activity call) ===
    durable_trace_info!("Creating VM instance...");
    let vm = durable!(create_vm(config.clone())).await?;
    durable_trace_info!("✅ VM created: {} ({})", vm.id, vm.ip);
    
    // === Wait for readiness ===
    durable_trace_info!("Waiting for VM to be ready...");
    durable!(wait_for_vm_ready(vm.id.clone())).await?;
    durable_trace_info!("✅ VM is ready");
    
    // === Calculate duration (durable) ===
    let completed_at = durable_utcnow!().await?;
    let duration_ms = completed_at - started_at;
    
    durable_trace_info!("VM provisioning completed in {}ms", duration_ms);
    
    if duration_ms > 60000 {
        durable_trace_warn!("⚠️  Slow provisioning: {}ms (>1min)", duration_ms);
    }
    
    Ok(VmProvisionResult {
        vm,
        correlation_id,
        provisioned_at: completed_at,
        duration_ms,
    })
}
```

---

## Pattern: When to Use Each

### Use `durable_trace_*!` When:
- ✅ Logging in orchestrations
- ✅ Need logs to be part of durable history
- ✅ Want logs to replay consistently
- ✅ Debugging orchestration logic

### Use Regular `tracing::*!` When:
- ✅ Logging in activities
- ✅ Logging in regular helper functions
- ✅ Runtime/provider internal logging
- ✅ Performance-sensitive logging (not recorded)

### Use `durable_newguid!()` When:
- ✅ Need unique ID that's deterministic on replay
- ✅ Generating correlation IDs
- ✅ Creating entity IDs within orchestration
- ✅ Idempotency keys

### Use `durable_utcnow!()` When:
- ✅ Need timestamp that's deterministic on replay
- ✅ Recording when something happened in orchestration
- ✅ Calculating durations/timeouts
- ✅ Creating time-based identifiers

---

## Cheat Sheet

```rust
// ===== ALWAYS USE IN ORCHESTRATIONS =====

// Logging (no await needed)
durable_trace_info!("Message: {}", value);
durable_trace_warn!("Warning: {}", msg);
durable_trace_error!("Error: {}", err);
durable_trace_debug!("Debug: {:?}", state);

// System calls (await needed)
let guid = durable_newguid!().await?;
let timestamp = durable_utcnow!().await?;

// Execution (await needed)
let result = make_durable!(activity(args)).await?;

// ===== NEVER USE IN ORCHESTRATIONS =====

// ❌ Don't use these - they're non-deterministic!
tracing::info!("...")        // Use durable_trace_info! instead
uuid::Uuid::new_v4()         // Use durable_newguid! instead
SystemTime::now()            // Use durable_utcnow! instead
rand::random()               // No durable version yet
```

---

## Summary

**All durable operations use `durable` prefix:**
- `durable!()` - Durable execution
- `durable_trace_*!()` - Durable logging
- `durable_newguid!()` - Durable GUID
- `durable_utcnow!()` - Durable time

**Benefits:**
- ✅ Consistent naming family
- ✅ Impossible to confuse with non-durable functions
- ✅ Self-documenting code
- ✅ Easy to grep/search
- ✅ Clear IDE autocomplete grouping
- ✅ Prevents determinism bugs

**The `durable_` prefix is your guarantee of replay-safety!** 🛡️
