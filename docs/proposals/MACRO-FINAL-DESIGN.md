# Duroxide Macros: Final Design & Implementation Plan

## Overview

This document specifies the complete macro system for duroxide, providing syntactic sugar for:
- Auto-discovery of activities and orchestrations
- Type-safe durable function calls
- Clean tracing macros
- Progressive features (names, versions, types)

---

## Design Goals

1. **Clarity** - Clear distinction between inline and distributed execution
2. **Type Safety** - Compile-time validation of all calls
3. **Ergonomics** - Minimal boilerplate, clean syntax
4. **Performance** - Zero runtime overhead (compile-time transformation)
5. **Discoverability** - Auto-registration via distributed slices
6. **Flexibility** - Support versioning, custom names, typed I/O

---

## Core API

### Function Annotations

```rust
// Inline orchestration function (executes in orchestration turn)
#[orch_fn(typed)]
fn calculate_tax(amount: f64, state: String) -> Result<f64, String> {
    // Pure calculation or lightweight non-deterministic code
}

// Distributed activity (executes on worker)
#[activity(typed)]
async fn charge_payment(order: Order, amount: f64) -> Result<String, String> {
    // I/O operations allowed
}

// Orchestration
#[orchestration]
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // Coordinate activities
}
```

### Invocation Macros

```rust
// Inline execution (completes in same turn)
let tax = make_durable_inline!(calculate_tax(amount, state)).await?;

// Worker execution (completes in future turn)
let payment = make_durable!(charge_payment(order, amount)).await?;

// Tracing
trace_info!("Processing order: {}", order.id);
trace_warn!("Low inventory");
trace_error!("Payment failed: {}", error);
```

### Auto-Discovery

```rust
#[tokio::main]
async fn main() {
    let rt = Runtime::builder()
        .store(store)
        .discover_activities()      // Auto-registers all #[activity] functions
        .discover_orchestrations()   // Auto-registers all #[orchestration] functions
        .start()
        .await;
}
```

---

## Complete Example

```rust
use duroxide::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    total: f64,
    state: String,
}

// ============ Inline Functions ============

#[orch_fn(typed)]
fn calculate_tax(amount: f64, state: String) -> Result<f64, String> {
    let rate = match state.as_str() {
        "CA" => 0.0725,
        "NY" => 0.08875,
        _ => 0.05,
    };
    Ok(amount * rate)
}

#[orch_fn(typed)]
fn validate_order(order: Order) -> Result<bool, String> {
    Ok(!order.id.is_empty() && order.total > 0.0)
}

// ============ Distributed Activities ============

#[activity(typed)]
async fn charge_payment(order_id: String, amount: f64) -> Result<String, String> {
    // Call external payment API
    Ok(format!("TXN-{}", uuid::Uuid::new_v4()))
}

#[activity(typed)]
async fn send_email(email: String, body: String) -> Result<(), String> {
    // Send via SMTP
    Ok(())
}

// ============ Orchestration ============

#[orchestration]
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    trace_info!("Processing order: {}", order.id);
    
    // Inline validation
    let valid = make_durable_inline!(validate_order(order.clone())).await?;
    if !valid {
        return Err("Invalid order".to_string());
    }
    
    // Inline calculation
    let tax = make_durable_inline!(calculate_tax(order.total, order.state)).await?;
    let total = order.total + tax;
    
    trace_info!("Total with tax: ${:.2}", total);
    
    // Distributed work
    let txn = make_durable!(charge_payment(order.id.clone(), total)).await?;
    let _ = make_durable!(send_email("customer@example.com".into(), "Receipt".into())).await;
    
    trace_info!("Order complete: {}", txn);
    Ok(txn)
}

// ============ Main ============

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = setup_sqlite_provider().await?;
    
    // Auto-discover everything!
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    let order = Order { id: "ORD-1".into(), total: 100.0, state: "CA".into() };
    
    client.start_orchestration_typed("order-1", "process_order", &order).await?;
    
    let status = client.wait_for_orchestration_typed::<String>(
        "order-1",
        Duration::from_secs(30)
    ).await?;
    
    match status {
        OrchestrationStatus::Completed { output } => println!("✅ {}", output),
        OrchestrationStatus::Failed { error } => eprintln!("❌ {}", error),
        _ => {}
    }
    
    rt.shutdown().await;
    Ok(())
}
```

---

## Implementation Details

### Dependencies

```toml
# duroxide-macros/Cargo.toml
[dependencies]
proc-macro2 = "1.0"
quote = "1.0"
syn = { version = "2.0", features = ["full", "parsing", "extra-traits"] }

# duroxide/Cargo.toml
[dependencies]
linkme = "0.3"
duroxide-macros = { path = "./duroxide-macros" }
```

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│ User Code                                                         │
│                                                                   │
│  #[orch_fn(typed)]                                               │
│  fn calculate_tax(...) { ... }                                   │
│                                                                   │
│  #[activity(typed)]                                              │
│  async fn charge_payment(...) { ... }                            │
└────────────────────────┬────────────────────────────────────────┘
                         │ (macro expansion)
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│ Generated Code                                                    │
│                                                                   │
│  - Struct with .call() method                                    │
│  - Descriptor submitted to linkme distributed slice              │
│  - Auto-registration data                                        │
└────────────────────────┬────────────────────────────────────────┘
                         │ (link-time collection)
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│ Runtime Discovery                                                 │
│                                                                   │
│  Runtime::builder()                                              │
│    .discover_activities()    → iterates ACTIVITIES slice         │
│    .discover_orchestrations() → iterates ORCHESTRATIONS slice    │
└─────────────────────────────────────────────────────────────────┘
```

---

## Macro Implementation

### 1. `#[orch_fn]` - Inline Orchestration Function

```rust
// duroxide-macros/src/lib.rs

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, AttributeArgs, Meta, NestedMeta, Lit};

#[proc_macro_attribute]
pub fn orch_fn(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as AttributeArgs);
    let input = parse_macro_input!(item as ItemFn);
    
    // Parse attributes
    let attrs = parse_attributes(args);
    let fn_name = &input.sig.ident;
    let fn_name_str = attrs.name.unwrap_or_else(|| fn_name.to_string());
    
    // Validate: must be synchronous
    if input.sig.asyncness.is_some() {
        return syn::Error::new_spanned(
            &input.sig.asyncness,
            "#[orch_fn] functions must be synchronous (not async)"
        ).to_compile_error().into();
    }
    
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let block = &input.block;
    
    if attrs.typed {
        // Extract types
        let input_type = extract_first_param_type(&input.sig);
        let output_type = extract_result_ok_type(&input.sig);
        
        // Generate implementation name
        let impl_name = syn::Ident::new(&format!("__impl_{}", fn_name), fn_name.span());
        
        let expanded = quote! {
            // 1. Hidden implementation (user's code)
            #[doc(hidden)]
            #(#fn_attrs)*
            fn #impl_name(__input: #input_type) -> Result<#output_type, String> #block
            
            // 2. Public struct with .call() method
            #[doc = concat!("Inline orchestration function: ", #fn_name_str)]
            #[doc = ""]
            #[doc = "Executes synchronously within the orchestration turn."]
            #[doc = "Use with `make_durable_inline!()` macro."]
            #vis struct #fn_name;
            
            impl #fn_name {
                #[doc = "Execute this function inline and record result in history."]
                pub fn call(
                    &self,
                    __ctx: &::duroxide::OrchestrationContext,
                    __input: #input_type,
                ) -> impl ::std::future::Future<Output = Result<#output_type, String>> + Send {
                    async move {
                        // Lock context to access history
                        let mut __inner = __ctx.inner.lock().unwrap();
                        let __event_id = __inner.next_event_id;
                        
                        // Check for cached result (replay path)
                        let __cached = __inner.history.iter().find_map(|e| {
                            if let ::duroxide::Event::OrchFnCall { 
                                event_id, name, output, .. 
                            } = e {
                                if *event_id == __event_id && name == #fn_name_str {
                                    return Some(output.clone());
                                }
                            }
                            None
                        });
                        
                        if let Some(__output_json) = __cached {
                            // Replay: use cached result
                            __inner.next_event_id += 1;
                            drop(__inner);  // Release lock
                            
                            let __result: #output_type = ::serde_json::from_str(&__output_json)
                                .map_err(|e| format!("Deserialization error: {}", e))?;
                            return Ok(__result);
                        }
                        
                        // First execution: serialize input
                        let __input_json = ::serde_json::to_string(&__input)
                            .map_err(|e| format!("Serialization error: {}", e))?;
                        
                        // Release lock before executing user code
                        drop(__inner);
                        
                        // Execute the actual function
                        let __result = #impl_name(__input)?;
                        
                        // Serialize output
                        let __output_json = ::serde_json::to_string(&__result)
                            .map_err(|e| format!("Serialization error: {}", e))?;
                        
                        // Record in history
                        let mut __inner = __ctx.inner.lock().unwrap();
                        let __exec_id = __inner.execution_id;
                        __inner.history.push(::duroxide::Event::OrchFnCall {
                            event_id: __event_id,
                            execution_id: __exec_id,
                            name: #fn_name_str.to_string(),
                            input: __input_json,
                            output: __output_json.clone(),
                        });
                        __inner.next_event_id += 1;
                        
                        Ok(__result)
                    }
                }
            }
        };
        
        TokenStream::from(expanded)
    } else {
        // Non-typed version (string-based I/O)
        // Similar implementation but without type parameters
        // ...
        TokenStream::from(quote! { #input })
    }
}
```

### 2. `#[activity]` - Distributed Activity

```rust
#[proc_macro_attribute]
pub fn activity(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as AttributeArgs);
    let input = parse_macro_input!(item as ItemFn);
    
    let attrs = parse_attributes(args);
    let fn_name = &input.sig.ident;
    let activity_name = attrs.name.unwrap_or_else(|| fn_name.to_string());
    
    // Validate: must be async
    if input.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            &input.sig,
            "#[activity] functions must be async"
        ).to_compile_error().into();
    }
    
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let block = &input.block;
    
    if attrs.typed {
        let input_type = extract_first_param_type(&input.sig);
        let output_type = extract_result_ok_type(&input.sig);
        
        let impl_name = syn::Ident::new(&format!("__impl_{}", fn_name), fn_name.span());
        
        let expanded = quote! {
            // 1. Hidden implementation (user's code)
            #[doc(hidden)]
            #(#fn_attrs)*
            async fn #impl_name(__input: #input_type) -> Result<#output_type, String> #block
            
            // 2. Public struct with .call() method
            #[doc = concat!("Distributed activity: ", #activity_name)]
            #[doc = ""]
            #[doc = "Executes on a worker process."]
            #[doc = "Use with `make_durable!()` macro."]
            #vis struct #fn_name;
            
            impl #fn_name {
                #[doc = "Schedule this activity for execution on a worker."]
                pub fn call(
                    &self,
                    __ctx: &::duroxide::OrchestrationContext,
                    __input: #input_type,
                ) -> impl ::std::future::Future<Output = Result<#output_type, String>> + Send {
                    async move {
                        // Serialize input
                        let __input_json = ::serde_json::to_string(&__input)
                            .map_err(|e| format!("Failed to serialize input: {}", e))?;
                        
                        // Schedule activity
                        let __result_json = __ctx
                            .schedule_activity(#activity_name, __input_json)
                            .into_activity()
                            .await?;
                        
                        // Deserialize output
                        let __result: #output_type = ::serde_json::from_str(&__result_json)
                            .map_err(|e| format!("Failed to deserialize output: {}", e))?;
                        
                        Ok(__result)
                    }
                }
            }
            
            // 3. Submit to distributed slice for auto-discovery
            #[::linkme::distributed_slice(::duroxide::__internal::ACTIVITIES)]
            #[linkme(crate = ::linkme)]
            static ACTIVITY_DESCRIPTOR: ::duroxide::__internal::ActivityDescriptor = 
                ::duroxide::__internal::ActivityDescriptor {
                    name: #activity_name,
                    invoke: |input_json: String| {
                        Box::pin(async move {
                            let input: #input_type = ::serde_json::from_str(&input_json)
                                .map_err(|e| format!("Deserialization error: {}", e))?;
                            let result = #impl_name(input).await?;
                            ::serde_json::to_string(&result)
                                .map_err(|e| format!("Serialization error: {}", e))
                        })
                    },
                };
        };
        
        TokenStream::from(expanded)
    } else {
        // Non-typed version
        // ...
        TokenStream::from(quote! { #input })
    }
}
```

### 3. `#[orchestration]` - Orchestration Function

```rust
#[proc_macro_attribute]
pub fn orchestration(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as AttributeArgs);
    let input = parse_macro_input!(item as ItemFn);
    
    let attrs = parse_attributes(args);
    let fn_name = &input.sig.ident;
    let orch_name = attrs.name.unwrap_or_else(|| fn_name.to_string());
    let version = attrs.version.unwrap_or_else(|| "1.0.0".to_string());
    
    // Validate: must be async
    if input.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            &input.sig,
            "#[orchestration] functions must be async"
        ).to_compile_error().into();
    }
    
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let block = &input.block;
    
    // Extract input type (second parameter after ctx)
    let input_type = extract_second_param_type(&input.sig);
    let output_type = extract_result_ok_type(&input.sig);
    
    let impl_name = syn::Ident::new(&format!("__impl_{}", fn_name), fn_name.span());
    
    let expanded = quote! {
        // Keep user function but rename
        #[doc(hidden)]
        #(#fn_attrs)*
        #vis #sig #block
        
        // Submit to distributed slice for auto-discovery
        #[::linkme::distributed_slice(::duroxide::__internal::ORCHESTRATIONS)]
        #[linkme(crate = ::linkme)]
        static ORCHESTRATION_DESCRIPTOR: ::duroxide::__internal::OrchestrationDescriptor = 
            ::duroxide::__internal::OrchestrationDescriptor {
                name: #orch_name,
                version: #version,
                invoke: |ctx: ::duroxide::OrchestrationContext, input_json: String| {
                    Box::pin(async move {
                        let input: #input_type = ::serde_json::from_str(&input_json)
                            .map_err(|e| format!("Deserialization error: {}", e))?;
                        let result = #fn_name(ctx, input).await?;
                        ::serde_json::to_string(&result)
                            .map_err(|e| format!("Serialization error: {}", e))
                    })
                },
            };
    };
    
    TokenStream::from(expanded)
}
```

### 4. Invocation Macros

```rust
/// Make an inline function call durable.
#[proc_macro]
pub fn make_durable_inline(input: TokenStream) -> TokenStream {
    let call = parse_macro_input!(input as syn::ExprCall);
    
    let func = &call.func;
    let args = &call.args;
    
    // Transform: fn(args) → fn.call(&ctx, args)
    let expanded = quote! {
        #func.call(&ctx, #args)
    };
    
    TokenStream::from(expanded)
}

/// Make an activity call durable (worker execution).
#[proc_macro]
pub fn make_durable(input: TokenStream) -> TokenStream {
    let call = parse_macro_input!(input as syn::ExprCall);
    
    let func = &call.func;
    let args = &call.args;
    
    // Transform: fn(args) → fn.call(&ctx, args)
    let expanded = quote! {
        #func.call(&ctx, #args)
    };
    
    TokenStream::from(expanded)
}
```

### 5. Trace Macros

```rust
#[proc_macro]
pub fn trace_info(input: TokenStream) -> TokenStream {
    trace_impl(input, "INFO")
}

#[proc_macro]
pub fn trace_warn(input: TokenStream) -> TokenStream {
    trace_impl(input, "WARN")
}

#[proc_macro]
pub fn trace_error(input: TokenStream) -> TokenStream {
    trace_impl(input, "ERROR")
}

#[proc_macro]
pub fn trace_debug(input: TokenStream) -> TokenStream {
    trace_impl(input, "DEBUG")
}

fn trace_impl(input: TokenStream, level: &str) -> TokenStream {
    let input = TokenStream2::from(input);
    
    let expanded = quote! {
        ctx.trace(#level, format!(#input))
    };
    
    TokenStream::from(expanded)
}
```

### Helper Functions

```rust
fn parse_attributes(args: AttributeArgs) -> MacroAttributes {
    let mut name = None;
    let mut version = None;
    let mut typed = false;
    
    for arg in args {
        match arg {
            NestedMeta::Meta(Meta::NameValue(nv)) => {
                if nv.path.is_ident("name") {
                    if let Lit::Str(lit) = nv.lit {
                        name = Some(lit.value());
                    }
                }
                if nv.path.is_ident("version") {
                    if let Lit::Str(lit) = nv.lit {
                        version = Some(lit.value());
                    }
                }
            }
            NestedMeta::Meta(Meta::Path(path)) => {
                if path.is_ident("typed") {
                    typed = true;
                }
            }
            _ => {}
        }
    }
    
    MacroAttributes { name, version, typed }
}

struct MacroAttributes {
    name: Option<String>,
    version: Option<String>,
    typed: bool,
}

fn extract_first_param_type(sig: &syn::Signature) -> syn::Type {
    // Extract first parameter type
    if let Some(syn::FnArg::Typed(pat_type)) = sig.inputs.first() {
        (*pat_type.ty).clone()
    } else {
        syn::parse_quote! { String }
    }
}

fn extract_second_param_type(sig: &syn::Signature) -> syn::Type {
    // Extract second parameter type (for orchestrations, after ctx)
    if let Some(syn::FnArg::Typed(pat_type)) = sig.inputs.iter().nth(1) {
        (*pat_type.ty).clone()
    } else {
        syn::parse_quote! { String }
    }
}

fn extract_result_ok_type(sig: &syn::Signature) -> syn::Type {
    // Extract T from Result<T, String>
    if let syn::ReturnType::Type(_, ty) = &sig.output {
        if let syn::Type::Path(type_path) = &**ty {
            if let Some(segment) = type_path.path.segments.last() {
                if segment.ident == "Result" {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(ok_type)) = args.args.first() {
                            return ok_type.clone();
                        }
                    }
                }
            }
        }
    }
    syn::parse_quote! { String }
}
```

---

## Runtime Support

### Distributed Slices

```rust
// duroxide/src/lib.rs

#[doc(hidden)]
pub mod __internal {
    use linkme::distributed_slice;
    use std::pin::Pin;
    use std::future::Future;
    
    pub type ActivityFn = fn(String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
    pub type OrchestrationFn = fn(crate::OrchestrationContext, String) 
        -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
    
    pub struct ActivityDescriptor {
        pub name: &'static str,
        pub invoke: ActivityFn,
    }
    
    pub struct OrchestrationDescriptor {
        pub name: &'static str,
        pub version: &'static str,
        pub invoke: OrchestrationFn,
    }
    
    #[distributed_slice]
    pub static ACTIVITIES: [ActivityDescriptor] = [..];
    
    #[distributed_slice]
    pub static ORCHESTRATIONS: [OrchestrationDescriptor] = [..];
}
```

### New Event Type

```rust
// duroxide/src/lib.rs

pub enum Event {
    // ... existing events ...
    
    /// Inline orchestration function call (single event for schedule+completion)
    OrchFnCall {
        event_id: u64,
        execution_id: u64,
        name: String,
        input: String,   // JSON serialized
        output: String,  // JSON serialized
    },
}
```

### Runtime Builder

```rust
// duroxide/src/runtime/mod.rs

pub struct RuntimeBuilder {
    store: Option<Arc<dyn Provider>>,
    activities: Option<ActivityRegistry>,
    orchestrations: Option<OrchestrationRegistry>,
    options: RuntimeOptions,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            store: None,
            activities: None,
            orchestrations: None,
            options: RuntimeOptions::default(),
        }
    }
    
    pub fn store(mut self, store: Arc<dyn Provider>) -> Self {
        self.store = Some(store);
        self
    }
    
    #[cfg(feature = "macros")]
    pub fn discover_activities(mut self) -> Self {
        let mut builder = ActivityRegistry::builder();
        
        for descriptor in crate::__internal::ACTIVITIES {
            // Wrap the function pointer as ActivityHandler
            let handler = ActivityFromFn {
                invoke: descriptor.invoke,
            };
            builder = builder.register(descriptor.name, handler);
        }
        
        self.activities = Some(builder.build());
        self
    }
    
    #[cfg(feature = "macros")]
    pub fn discover_orchestrations(mut self) -> Self {
        let mut builder = OrchestrationRegistry::builder();
        
        for descriptor in crate::__internal::ORCHESTRATIONS {
            let handler = OrchestrationFromFn {
                invoke: descriptor.invoke,
            };
            builder = builder.register_versioned(
                descriptor.name,
                descriptor.version,
                handler,
            );
        }
        
        self.orchestrations = Some(builder.build());
        self
    }
    
    pub fn activities(mut self, activities: ActivityRegistry) -> Self {
        self.activities = Some(activities);
        self
    }
    
    pub fn orchestrations(mut self, orchestrations: OrchestrationRegistry) -> Self {
        self.orchestrations = Some(orchestrations);
        self
    }
    
    pub fn options(mut self, options: RuntimeOptions) -> Self {
        self.options = options;
        self
    }
    
    pub async fn start(self) -> Arc<Runtime> {
        let store = self.store.expect("store is required");
        let activities = self.activities.expect("activities required - use discover_activities() or provide manually");
        let orchestrations = self.orchestrations.expect("orchestrations required - use discover_orchestrations() or provide manually");
        
        Runtime::start_with_options(
            store,
            Arc::new(activities),
            orchestrations,
            self.options
        ).await
    }
}

impl Runtime {
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }
}

// Helper structs
struct ActivityFromFn {
    invoke: crate::__internal::ActivityFn,
}

#[async_trait::async_trait]
impl crate::runtime::registry::ActivityHandler for ActivityFromFn {
    async fn invoke(&self, input: String) -> Result<String, String> {
        (self.invoke)(input).await
    }
}

struct OrchestrationFromFn {
    invoke: crate::__internal::OrchestrationFn,
}

#[async_trait::async_trait]
impl crate::OrchestrationHandler for OrchestrationFromFn {
    async fn invoke(&self, ctx: crate::OrchestrationContext, input: String) -> Result<String, String> {
        (self.invoke)(ctx, input).await
    }
}
```

---

## Progressive Features

### Custom Names

```rust
// Override function name
#[orch_fn(typed, name = "CalculateTax")]
fn calculate_tax_v1(amount: f64, state: String) -> Result<f64, String> { ... }

#[activity(typed, name = "ChargePayment")]
async fn charge_payment_v1(order: Order) -> Result<String, String> { ... }
```

### Versioning

```rust
// Orchestrations support versioning
#[orchestration(version = "1.0.0")]
async fn process_order_v1(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // Version 1 logic
}

#[orchestration(name = "process_order", version = "2.0.0")]
async fn process_order_v2(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // Version 2 logic with new features
}

// Runtime resolves version based on policy
let rt = Runtime::builder()
    .store(store)
    .discover_orchestrations()  // Both versions registered
    .start()
    .await;

// Set version policy
rt.orchestration_registry
    .set_version_policy("process_order", VersionPolicy::Latest)
    .await;
```

### Type System Integration

```rust
// Full type safety with custom types
#[derive(Serialize, Deserialize)]
struct PaymentRequest {
    order_id: String,
    amount: f64,
    currency: String,
}

#[derive(Serialize, Deserialize)]
struct PaymentResponse {
    transaction_id: String,
    status: String,
}

#[activity(typed)]
async fn charge_payment(request: PaymentRequest) -> Result<PaymentResponse, String> {
    // Type-safe input and output
    Ok(PaymentResponse {
        transaction_id: format!("TXN-{}", uuid::Uuid::new_v4()),
        status: "SUCCESS".to_string(),
    })
}

#[orchestration]
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    let request = PaymentRequest {
        order_id: order.id.clone(),
        amount: order.total,
        currency: "USD".to_string(),
    };
    
    // Type-safe call - compiler validates types!
    let response = make_durable!(charge_payment(request)).await?;
    
    trace_info!("Payment {}: {}", response.transaction_id, response.status);
    Ok(response.transaction_id)
}
```

---

## Comparison: Before vs After

### Before (Verbose)

```rust
let greet = |name: String| async move { 
    Ok(format!("Hello, {}!", name)) 
};

let activities = ActivityRegistry::builder()
    .register("Greet", greet)
    .build();

let orchestration = |ctx: OrchestrationContext, name: String| async move {
    ctx.trace("INFO", format!("Starting: {}", name));
    
    let name_json = serde_json::to_string(&name)?;
    let result_json = ctx.schedule_activity("Greet", name_json)
        .into_activity()
        .await?;
    let greeting: String = serde_json::from_str(&result_json)?;
    
    Ok(greeting)
};

let orchestrations = OrchestrationRegistry::builder()
    .register("HelloWorld", orchestration)
    .build();

let rt = Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;
```

### After (Clean)

```rust
#[activity(typed)]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

#[orchestration]
async fn hello_world(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    trace_info!("Starting: {}", name);
    
    let greeting = make_durable!(greet(name)).await?;
    
    Ok(greeting)
}

let rt = Runtime::builder()
    .store(store)
    .discover_activities()
    .discover_orchestrations()
    .start()
    .await;
```

**~70% reduction in code!** 🎉

---

## Implementation Plan

### Phase 1: Foundation (Week 1-2)

**Goal:** Basic macro infrastructure and auto-discovery.

**Deliverables:**
- [ ] Create `duroxide-macros` proc-macro crate
- [ ] Implement basic `#[activity]` macro (non-typed)
- [ ] Implement basic `#[orchestration]` macro (non-typed)
- [ ] Add `linkme` integration
- [ ] Add `__internal` module with distributed slices
- [ ] Implement `Runtime::builder()` with `.discover_*()` methods
- [ ] Write basic integration test
- [ ] Update `Cargo.toml` dependencies

**Success Criteria:**
```rust
#[activity]
async fn greet(name: String) -> Result<String, String> { ... }

#[orchestration]
async fn hello(ctx: OrchestrationContext, name: String) -> Result<String, String> { ... }

let rt = Runtime::builder()
    .discover_activities()
    .discover_orchestrations()
    .start()
    .await;
```

### Phase 2: Typed Functions (Week 2-3)

**Goal:** Add `typed` parameter support for type-safe I/O.

**Deliverables:**
- [ ] Implement `#[activity(typed)]`
- [ ] Implement `#[orchestration(typed)]` (implicit for orchestrations)
- [ ] Add type extraction helpers
- [ ] Handle serialization/deserialization
- [ ] Write type safety tests
- [ ] Update examples

**Success Criteria:**
```rust
#[activity(typed)]
async fn charge(order: Order) -> Result<PaymentResult, String> { ... }

// Type-safe calls with custom types
```

### Phase 3: Invocation Macros (Week 3-4)

**Goal:** Add `make_durable!()` and `make_durable_inline!()` macros.

**Deliverables:**
- [ ] Implement `#[orch_fn]` macro
- [ ] Add `OrchFnCall` event type
- [ ] Implement `make_durable!()` macro
- [ ] Implement `make_durable_inline!()` macro
- [ ] Update `OrchestrationContext` to handle inline functions
- [ ] Write inline function tests
- [ ] Performance benchmarks (inline vs worker)

**Success Criteria:**
```rust
#[orch_fn(typed)]
fn calculate_tax(amount: f64) -> Result<f64, String> { ... }

let tax = make_durable_inline!(calculate_tax(100.0)).await?;
let payment = make_durable!(charge_payment(order)).await?;
```

### Phase 4: Trace Macros (Week 4)

**Goal:** Add clean tracing macros.

**Deliverables:**
- [ ] Implement `trace_info!()`
- [ ] Implement `trace_warn!()`
- [ ] Implement `trace_error!()`
- [ ] Implement `trace_debug!()`
- [ ] Update examples to use trace macros

**Success Criteria:**
```rust
trace_info!("Processing order: {}", order.id);
trace_warn!("Low inventory");
trace_error!("Payment failed: {}", error);
```

### Phase 5: Progressive Features (Week 5)

**Goal:** Custom names and versioning support.

**Deliverables:**
- [ ] Add `name` parameter parsing
- [ ] Add `version` parameter parsing (orchestrations only)
- [ ] Test custom names
- [ ] Test versioning with multiple versions
- [ ] Write migration guide

**Success Criteria:**
```rust
#[activity(typed, name = "CustomName")]
async fn my_activity(...) { ... }

#[orchestration(version = "2.0.0")]
async fn my_orch(...) { ... }
```

### Phase 6: Documentation & Polish (Week 6)

**Goal:** Complete documentation and examples.

**Deliverables:**
- [ ] API documentation for all macros
- [ ] Tutorial: "Getting Started with Macros"
- [ ] Migration guide from old style
- [ ] Update all examples to use macros
- [ ] Best practices guide
- [ ] Performance documentation
- [ ] Troubleshooting guide

---

## Testing Strategy

### Unit Tests (duroxide-macros)

```rust
#[test]
fn test_activity_expansion() {
    // Test macro expansion with trybuild
}

#[test]
fn test_orch_fn_must_be_sync() {
    // Verify compile error for async orch_fn
}

#[test]
fn test_activity_must_be_async() {
    // Verify compile error for sync activity
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_auto_discovery() {
    #[activity(typed)]
    async fn test_activity(input: i32) -> Result<i32, String> {
        Ok(input * 2)
    }
    
    #[orchestration]
    async fn test_orch(ctx: OrchestrationContext, input: i32) -> Result<i32, String> {
        let result = make_durable!(test_activity(input)).await?;
        Ok(result)
    }
    
    let (store, _temp) = create_sqlite_store_disk().await;
    
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    client.start_orchestration_typed("test-1", "test_orch", &5).await.unwrap();
    
    let status = client.wait_for_orchestration_typed::<i32>("test-1", Duration::from_secs(5))
        .await
        .unwrap();
    
    match status {
        OrchestrationStatus::Completed { output } => assert_eq!(output, 10),
        _ => panic!("Expected completed"),
    }
    
    rt.shutdown().await;
}
```

### Performance Tests

```rust
#[tokio::test]
async fn benchmark_inline_vs_worker() {
    // Measure latency difference between inline and worker execution
}
```

---

## Migration Guide

### Gradual Migration

You can mix old and new styles:

```rust
// Old style (still works)
let old_activity = |input: String| async move { Ok(input) };
let activities = ActivityRegistry::builder()
    .register("OldActivity", old_activity)
    .build();

// New style
#[activity(typed)]
async fn new_activity(input: String) -> Result<String, String> {
    Ok(input)
}

// Mix both
let rt = Runtime::builder()
    .store(store)
    .activities(activities)  // Manual registration
    .discover_orchestrations()  // Auto-discovery
    .start()
    .await;
```

### Step-by-Step

1. **Add macros dependency**
   ```toml
   [dependencies]
   duroxide = { version = "0.x", features = ["macros"] }
   ```

2. **Convert activities one-by-one**
   ```rust
   // Before
   let greet = |name: String| async move { ... };
   
   // After
   #[activity(typed)]
   async fn greet(name: String) -> Result<String, String> { ... }
   ```

3. **Update orchestrations**
   ```rust
   // Before
   let orch = |ctx: OrchestrationContext, input: String| async move { ... };
   
   // After
   #[orchestration]
   async fn my_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> { ... }
   ```

4. **Switch to auto-discovery**
   ```rust
   // Before
   let rt = Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;
   
   // After
   let rt = Runtime::builder()
       .store(store)
       .discover_activities()
       .discover_orchestrations()
       .start()
       .await;
   ```

---

## Prelude

```rust
// duroxide/src/prelude.rs

pub use crate::{
    Client,
    OrchestrationContext,
    OrchestrationRegistry,
    OrchestrationStatus,
    Event,
};

pub use crate::runtime::{
    Runtime,
    RuntimeOptions,
    registry::{ActivityRegistry, OrchestrationRegistry as OrcheRegistry},
};

pub use crate::providers::sqlite::SqliteProvider;

// Re-export all macros
#[cfg(feature = "macros")]
pub use duroxide_macros::{
    activity,
    orch_fn,
    orchestration,
    make_durable,
    make_durable_inline,
    trace_info,
    trace_warn,
    trace_error,
    trace_debug,
};

pub use std::sync::Arc;
pub use std::time::Duration;
```

---

## Open Questions

1. **Cross-crate activities:** How to handle activities defined in separate crates? Linkme has limitations for cross-crate distributed slices.

2. **Generic functions:** Should we support generic activities/orch_fns? E.g., `fn process<T: Serialize>(item: T)`?

3. **Async traits:** Should activities implement a trait for runtime polymorphism?

4. **Error messages:** How to provide helpful compile errors when macros fail?

5. **IDE support:** Test with rust-analyzer to ensure good autocomplete and navigation.

---

## Success Metrics

- ✅ **70%+ reduction** in registration boilerplate
- ✅ **Zero runtime overhead** - all compile-time transformation
- ✅ **Full IDE support** - autocomplete, go-to-definition, refactoring
- ✅ **Type safety** - compile-time validation of all calls
- ✅ **100% backward compatible** - old style still works
- ✅ **All examples migrated** - demonstrate best practices

---

## Next Steps

1. **Review this proposal** - gather feedback
2. **Start Phase 1** - basic infrastructure
3. **Iterate based on usage** - refine ergonomics
4. **Document as we go** - keep docs up to date
5. **Migrate examples** - dogfood the new API

**Ready to implement!** 🚀

