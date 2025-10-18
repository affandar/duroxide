# Duroxide Macro System Design

## Overview

This document outlines the comprehensive macro system design for duroxide that simplifies activity and orchestration scheduling while maintaining type safety and deterministic execution.

## Core Macro System

### 1. Main Scheduling Macro

```rust
// Unified syntax for activities and sub-orchestrations
let greeting = schedule!(ctx, greet(name)).await?;
let order_result = schedule!(ctx, process_order(order)).await?;
```

### 2. Activity Definition

```rust
// Default naming (function name)
#[activity]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

// Explicit naming
#[activity(name = "CustomGreet")]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

// Typed version
#[activity(typed)]
async fn process_payment(amount: f64) -> Result<PaymentResult, String> {
    Ok(PaymentResult { transaction_id: "123".to_string() })
}
```

### 3. Orchestration Definition

```rust
// Default naming and versioning (function name, version 1.0.0)
#[orchestration]
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // ...
}

// Explicit naming and versioning
#[orchestration(name = "process_order", version = "2.0.0")]
async fn process_order_v2(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // ...
}

// Different name, same version
#[orchestration(name = "CustomOrder", version = "1.0.0")]
async fn custom_process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // ...
}
```

## Durable System Calls and Tracing

### 4. Durable System Calls

```rust
// Deterministic GUID generation
let order_id = durable_newguid!().await?;

// Deterministic timestamp
let created_at = durable_utcnow!().await?;
```

### 5. Durable Tracing

```rust
// Replay-safe logging
durable_trace_info!("Order {} created at {}", order_id, created_at);
durable_trace_warn!("Low inventory warning");
durable_trace_error!("Payment failed: {}", error);
durable_trace_debug!("Processing state: {:?}", state);
```

## Auto-Discovery System

### 6. Runtime Builder with Auto-Discovery

```rust
let rt = Runtime::builder()
    .store(store)
    .discover_activities()      // Discovers all #[activity] functions
    .discover_orchestrations()  // Discovers all #[orchestration] functions
    .start()
    .await?;
```

### 7. Workspace-Wide Discovery

```rust
// In main crate, import all activity/orchestration modules
use duroxide_core::activities::*;
use duroxide_app::activities::*;
use duroxide_main::orchestrations::*;

// Auto-discovery finds all imported functions
let rt = Runtime::builder()
    .store(store)
    .discover_activities()
    .discover_orchestrations()
    .start()
    .await?;
```

## Compatibility and Integration

### 8. Existing API Compatibility

```rust
// Works with existing select2 and join
let (winner, _) = ctx.select2(activity, timer).await;
let results = ctx.join(vec![payment, inventory, shipping]).await;

// Works with existing DurableOutput helpers
let payment_result = results[0].as_activity().unwrap()?;
```

### 9. External Events and Timers

```rust
// External events
let approval_event = ctx.schedule_wait("OrderApproval");
let approval_data = ctx.schedule_wait("OrderApproval").into_event().await;

// Timers
let timeout = ctx.schedule_timer(30000);
```

## Complete Example

```rust
use duroxide::{OrchestrationContext, Client};
use duroxide::macros::*;

// Activities
#[activity(typed)]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

#[activity(typed)]
async fn process_payment(amount: f64) -> Result<String, String> {
    Ok(format!("Payment processed: ${}", amount))
}

// Orchestrations with versioning
#[orchestration(name = "process_order", version = "1.0.0")]
async fn process_order_v1(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    ctx.trace_info("Processing order v1.0.0");
    
    let order_id = durable_newguid!().await?;
    let created_at = durable_utcnow!().await?;
    
    durable_trace_info!("Order {} created at {}", order_id, created_at);
    
    let payment_result = schedule!(ctx, process_payment(order.amount)).await?;
    
    Ok(format!("Order {} processed: {}", order_id, payment_result))
}

#[orchestration(name = "process_order", version = "2.0.0")]
async fn process_order_v2(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    ctx.trace_info("Processing order v2.0.0");
    
    let order_id = durable_newguid!().await?;
    let created_at = durable_utcnow!().await?;
    
    durable_trace_info!("Order {} created at {}", order_id, created_at);
    
    // Parallel processing with join
    let payment = schedule!(ctx, process_payment(order.amount));
    let inventory = schedule!(ctx, check_inventory(order.product_id.clone()));
    let shipping = schedule!(ctx, calculate_shipping(order.address.clone()));
    
    let results = ctx.join(vec![payment, inventory, shipping]).await;
    
    let payment_result = results[0].as_activity().unwrap()?;
    let inventory_result = results[1].as_activity().unwrap()?;
    let shipping_result = results[2].as_activity().unwrap()?;
    
    Ok(format!("Order {} processed: {} | {} | {}", 
               order_id, payment_result, inventory_result, shipping_result))
}

// Main orchestration
#[orchestration]
async fn order_workflow(ctx: OrchestrationContext, customer: String) -> Result<String, String> {
    ctx.trace_info("Starting order workflow");
    
    let order = Order {
        id: "123".to_string(),
        customer: customer.clone(),
        amount: 100.0,
        product_id: "PROD-001".to_string(),
        address: "123 Main St".to_string(),
    };
    
    // Call sub-orchestration (auto-detects version)
    let order_result = schedule!(ctx, process_order_v2(order)).await?;
    
    // Wait for external event with timeout
    let approval_event = ctx.schedule_wait("OrderApproval");
    let timeout = ctx.schedule_timer(30000);
    
    let (winner, _) = ctx.select2(approval_event, timeout).await;
    
    match winner {
        0 => {
            let approval_data = ctx.schedule_wait("OrderApproval").into_event().await;
            durable_trace_info!("Order approved: {}", approval_data);
            Ok(format!("Order workflow completed: {} | Approved: {}", 
                      order_result, approval_data))
        }
        1 => {
            durable_trace_warn!("Order approval timed out");
            Ok(format!("Order workflow completed: {} | Timeout", order_result))
        }
        _ => unreachable!(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("order_workflow.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // Auto-discovery
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await?;
    
    let client = Client::new(store);
    
    // Start orchestration (using existing client API for now)
    let instance_id = "order-workflow-1";
    client
        .start_orchestration(instance_id, "order_workflow", "John Doe")
        .await?;
    
    // Simulate external event
    tokio::time::sleep(Duration::from_secs(2)).await;
    client.raise_event(instance_id, "OrderApproval", "APPROVED").await?;
    
    // Wait for completion
    match client
        .wait_for_orchestration(instance_id, Duration::from_secs(30))
        .await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            println!("✅ Order workflow completed: {}", output);
        }
        duroxide::OrchestrationStatus::Failed { error } => {
            println!("❌ Order workflow failed: {}", error);
        }
        _ => {
            println!("⏳ Order workflow still running");
        }
    }
    
    rt.shutdown().await;
    Ok(())
}
```

## Macro Implementation Details

### Main Scheduling Macro

```rust
#[macro_export]
macro_rules! schedule {
    ($ctx:expr, $func:ident($($args:expr),*)) => {
        {
            let func_name = stringify!($func);
            let args = ($($args),*);
            let serialized_args = serde_json::to_string(&args).expect("Failed to serialize args");
            
            // Auto-detect based on registration
            if crate::__internal::is_orchestration(func_name) {
                let instance_id = format!("{}-{}", func_name, uuid::Uuid::new_v4());
                $ctx.schedule_sub_orchestration(func_name, &instance_id, serialized_args).into_sub_orchestration()
            } else {
                $ctx.schedule_activity(func_name, serialized_args).into_activity()
            }
        }
    };
}
```

### Durable System Calls

```rust
#[macro_export]
macro_rules! durable_newguid {
    () => {
        {
            let ctx = crate::__internal::get_current_context();
            ctx.new_guid().await
        }
    };
}

#[macro_export]
macro_rules! durable_utcnow {
    () => {
        {
            let ctx = crate::__internal::get_current_context();
            ctx.utc_now().await
        }
    };
}
```

### Durable Tracing

```rust
#[macro_export]
macro_rules! durable_trace_info {
    ($($arg:tt)*) => {
        {
            let ctx = crate::__internal::get_current_context();
            ctx.trace_info(format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! durable_trace_warn {
    ($($arg:tt)*) => {
        {
            let ctx = crate::__internal::get_current_context();
            ctx.trace_warn(format!($($arg)*));
        }
    };
}
```

### Activity Macro Implementation

```rust
#[proc_macro_attribute]
pub fn activity(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as AttributeArgs);
    let input_fn = parse_macro_input!(input as ItemFn);
    
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    
    // Extract explicit name if provided
    let activity_name = args.iter()
        .find_map(|arg| {
            if let NestedMeta::Meta(Meta::NameValue(nv)) = arg {
                if nv.path.is_ident("name") {
                    if let syn::Lit::Str(s) = &nv.lit {
                        Some(s.value())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| fn_name_str.clone());  // Default to function name
    
    let registration = quote! {
        #[cfg(feature = "macros")]
        crate::__internal::ACTIVITIES.get_or_init(|| Vec::new()).push(crate::__internal::ActivityDescriptor {
            name: #activity_name,
            invoke: #fn_name,
        });
    };
    
    quote! {
        #input_fn
        
        #registration
    }.into()
}
```

### Orchestration Macro Implementation

```rust
#[proc_macro_attribute]
pub fn orchestration(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as AttributeArgs);
    let input_fn = parse_macro_input!(input as ItemFn);
    
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    
    // Extract explicit name and version
    let orchestration_name = args.iter()
        .find_map(|arg| {
            if let NestedMeta::Meta(Meta::NameValue(nv)) = arg {
                if nv.path.is_ident("name") {
                    if let syn::Lit::Str(s) = &nv.lit {
                        Some(s.value())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| fn_name_str.clone());  // Default to function name
    
    let version = args.iter()
        .find_map(|arg| {
            if let NestedMeta::Meta(Meta::NameValue(nv)) = arg {
                if nv.path.is_ident("version") {
                    if let syn::Lit::Str(s) = &nv.lit {
                        Some(s.value())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| "1.0.0".to_string());  // Default to 1.0.0
    
    // Validate semver format
    if let Err(_) = semver::Version::parse(&version) {
        panic!("Invalid semantic version: {}", version);
    }
    
    let registration = quote! {
        #[cfg(feature = "macros")]
        crate::__internal::ORCHESTRATIONS.get_or_init(|| Vec::new()).push(crate::__internal::OrchestrationDescriptor {
            name: #orchestration_name,
            version: #version,
            invoke: #fn_name,
        });
    };
    
    quote! {
        #input_fn
        
        #registration
    }.into()
}
```

### Runtime Builder with Auto-Discovery

```rust
impl RuntimeBuilder {
    #[cfg(feature = "macros")]
    pub fn discover_activities(mut self) -> Self {
        let mut builder = ActivityRegistry::builder();
        
        if let Some(activities) = crate::__internal::ACTIVITIES.get() {
            for descriptor in activities {
                builder = builder.register(descriptor.name, descriptor.invoke);
            }
        }
        
        self.activities = Some(builder.build());
        self
    }
    
    #[cfg(feature = "macros")]
    pub fn discover_orchestrations(mut self) -> Self {
        let mut builder = OrchestrationRegistry::builder();
        
        if let Some(orchestrations) = crate::__internal::ORCHESTRATIONS.get() {
            for descriptor in orchestrations {
                builder = builder.register_versioned(
                    descriptor.name,
                    descriptor.version,
                    descriptor.invoke,
                );
            }
        }
        
        self.orchestrations = Some(builder.build());
        self
    }
}
```

## Benefits

1. **Clean Syntax**: `schedule!(ctx, function_name(args))` replaces verbose `ctx.schedule_activity("name", input).into_activity().await?`
2. **Type Safety**: Compile-time validation of function calls
3. **Auto-Discovery**: `#[activity]` and `#[orchestration]` attributes automatically register functions
4. **Explicit Context**: `ctx` parameter makes durable operations explicit
5. **Semantic Versioning**: Orchestration versions follow semver standards
6. **Flexible Naming**: Default to function names with explicit overrides
7. **Workspace Support**: Cross-crate discovery via explicit imports
8. **Compatibility**: Works with existing `select2`, `join`, and other APIs
9. **Deterministic**: Replay-safe system calls and tracing
10. **Unified**: Same syntax for activities and sub-orchestrations

## Implementation Status

✅ **Core Macro System**: `schedule!(ctx, function_name(args))`  
✅ **Activity/Orchestration Attributes**: `#[activity]`, `#[orchestration]`  
✅ **Auto-Discovery**: `Runtime::builder().discover_activities().discover_orchestrations()`  
✅ **Durable System Calls**: `durable_newguid!()`, `durable_utcnow!()`  
✅ **Durable Tracing**: `durable_trace_info!()`, `durable_trace_warn!()`, etc.  
✅ **Versioning**: Semantic versioning with defaults  
✅ **Naming**: Function name defaults with explicit overrides  
✅ **Workspace Support**: Import-based discovery  
✅ **Compatibility**: Existing API integration  

## Next Steps

- **Client Helpers**: Type-safe orchestration start/wait methods
- **Error Handling**: Improved error types and messages
- **Documentation**: Comprehensive examples and guides
- **Testing**: Complete test suite for macro system
