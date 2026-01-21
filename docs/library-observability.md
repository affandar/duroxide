# Library Observability Guide

This guide is for developers building reusable orchestrations and activities as library crates. It covers best practices for making your workflows observable.

## Overview

When building reusable orchestrations/activities, proper observability helps users:
- Debug issues in their deployments
- Monitor performance in production
- Understand workflow behavior
- Correlate logs across complex workflows

## Activity Naming Conventions

Activity names appear in metrics and logs. Use clear, descriptive names:

### ✅ Good Names
```rust
ActivityRegistry::builder()
    .register("ValidatePayment", validate_payment)
    .register("SendConfirmationEmail", send_email)
    .register("UpdateInventory", update_inventory)
    .build()
```

Metrics will show:
- `duroxide.activity.executions{activity_name="ValidatePayment"}`
- `duroxide.activity.duration_ms{activity_name="SendConfirmationEmail"}`

### ❌ Bad Names
```rust
// Too generic
.register("Process", process)
.register("Handler1", handler1)

// Too cryptic
.register("vpmt", validate_payment)
```

## Orchestration Naming

Orchestration names should be:
- **Descriptive**: `ProcessOrder` not `Handler`
- **Consistent**: Use consistent naming across versions
- **Versioned**: Use semver for breaking changes

```rust
OrchestrationRegistry::builder()
    .register_versioned("ProcessOrder", "1.0.0", process_order_v1)
    .register_versioned("ProcessOrder", "2.0.0", process_order_v2)
    .build()
```

## Effective Logging in Orchestrations

### Use ctx.trace_*() Methods

Always use the context methods for replay-safe logging:

```rust
async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    ctx.trace_info("Order processing started");
    
    let validation_result = ctx.schedule_activity("ValidateOrder", order.id.clone())
        .await?;
    
    ctx.trace_info(format!("Validation: {}", validation_result));
    
    // More processing...
    
    ctx.trace_info("Order processed successfully");
    Ok(order.id)
}
```

**Benefits**:
- Automatically includes instance_id, execution_id, orchestration_name
- Replay-safe (only logged on first execution)
- Searchable in log analytics platforms

### What to Log

#### ✅ Do Log:
- Orchestration milestones: "Started processing", "Validation complete"
- Decision points: "Approved", "Retry attempt 3"
- External event receipts: "Approval received from manager@company.com"
- Errors and recovery: "Payment failed, initiating refund"

#### ❌ Don't Log:
- Sensitive data: Credit cards, passwords, PII
- Every minor step: Over-logging creates noise
- Large payloads: Summarize instead of logging full JSON
- Implementation details: Leave those to framework logs

### Example: Good Logging

```rust
async fn payment_workflow(
    ctx: OrchestrationContext,
    request: PaymentRequest
) -> Result<String, String> {
    ctx.trace_info(format!("Processing payment for order {}", request.order_id));
    
    // Process payment
    match ctx.schedule_activity("ChargeCard", request.amount).await {
        Ok(transaction_id) => {
            ctx.trace_info(format!("Payment successful, transaction: {}", transaction_id));
            Ok(transaction_id)
        }
        Err(e) => {
            ctx.trace_warn(format!("Payment failed: {}, initiating refund flow", e));
            // Compensation logic
            ctx.schedule_activity("RefundOrder", request.order_id).await?;
            Err("payment_failed_refunded".to_string())
        }
    }
}
```

## Error Handling for Observability

### Application Errors

Return descriptive errors that will be logged and metered:

```rust
async fn validate_order(order: Order) -> Result<String, String> {
    if order.items.is_empty() {
        // This becomes an app_error metric
        return Err("validation_failed: empty order".to_string());
    }
    
    if order.total < 0.0 {
        return Err("validation_failed: negative total".to_string());
    }
    
    Ok("valid".to_string())
}
```

Metrics: `duroxide.activity.executions{activity_name="ValidateOrder", outcome="app_error"}`

### Don't Panic in Activities

Panics are caught but harder to debug:

#### ❌ Bad:
```rust
async fn process(data: String) -> Result<String, String> {
    let value: i32 = data.parse().unwrap(); // Panic if invalid!
    Ok(value.to_string())
}
```

#### ✅ Good:
```rust
async fn process(data: String) -> Result<String, String> {
    let value: i32 = data.parse()
        .map_err(|e| format!("parse_error: {}", e))?;
    Ok(value.to_string())
}
```

## Versioning and Metrics

Metrics include `version` label for orchestrations. Use semantic versioning:

### Major Version Changes
Breaking changes in orchestration logic:
```rust
// v1.0.0 - original
.register_versioned("ProcessOrder", "1.0.0", process_order_v1)

// v2.0.0 - incompatible changes
.register_versioned("ProcessOrder", "2.0.0", process_order_v2)
```

Metrics will separate:
- `duroxide.orchestration.completions{orchestration_name="ProcessOrder", version="1.0.0"}`
- `duroxide.orchestration.completions{orchestration_name="ProcessOrder", version="2.0.0"}`

This allows monitoring both versions during rollout.

### Minor/Patch Versions
Non-breaking changes:
```rust
.register_versioned("ProcessOrder", "1.1.0", process_order_v1_1)
.register_versioned("ProcessOrder", "1.1.1", process_order_v1_1_1)
```

## Cross-Crate Registry Composition

When composing registries from multiple crates, ensure unique names:

```rust
// payments_lib/src/lib.rs
pub fn create_activities() -> ActivityRegistry {
    ActivityRegistry::builder()
        .register("Payments::ChargeCard", charge_card)
        .register("Payments::RefundCard", refund_card)
        .build()
}

// inventory_lib/src/lib.rs
pub fn create_activities() -> ActivityRegistry {
    ActivityRegistry::builder()
        .register("Inventory::Reserve", reserve_inventory)
        .register("Inventory::Release", release_inventory)
        .build()
}

// main application
let activities = ActivityRegistry::builder()
    .merge(payments::create_activities())
    .merge(inventory::create_activities())
    .build();
```

Metrics will show:
- `activity.executions{activity_name="Payments::ChargeCard"}`
- `activity.executions{activity_name="Inventory::Reserve"}`

Clear attribution to the originating library.

## Activity Execution Time

Activities are automatically timed. Design activities with performance in mind:

### Fast Activities (< 1s)
```rust
async fn validate_email(email: String) -> Result<String, String> {
    // Quick validation logic
    if email.contains('@') {
        Ok("valid".to_string())
    } else {
        Err("invalid_email".to_string())
    }
}
```

Metrics: `activity.duration_ms{activity_name="ValidateEmail"}` ~ 1-10ms

### Slow Activities (> 1s)
```rust
async fn provision_infrastructure(spec: String) -> Result<String, String> {
    // External API call, may take seconds
    let client = CloudProvider::new();
    client.create_resources(spec).await
        .map_err(|e| format!("provisioning_failed: {}", e))
}
```

Metrics: `activity.duration_ms{activity_name="ProvisionInfrastructure"}` ~ 2-10s

**Tip**: Monitor duration histograms to find slow activities.

## Testing Observability in Libraries

### Test with Observability Enabled

```rust
#[tokio::test]
async fn test_workflow_observability() {
    let options = RuntimeOptions {
        observability: ObservabilityConfig {
            log_format: LogFormat::Json,
            log_level: "debug".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    
    let rt = Runtime::start_with_options(store, activities, orchestrations, options).await;
    
    // Run workflow
    // Verify logs contain expected fields
    // Verify no errors logged unexpectedly
}
```

### Verify Error Classification

Test that errors are properly classified:

```rust
#[tokio::test]
async fn test_activity_error_classification() {
    // Test app error
    let result = my_activity("invalid_input".to_string()).await;
    assert!(result.is_err()); // Should be app_error, not system_error
    
    // Metrics should show:
    // activity.executions{activity_name="MyActivity", outcome="app_error"} = 1
}
```

## Documentation for Library Users

Include observability information in your library docs:

### Activity Documentation

```rust
/// Validates and charges a payment method.
///
/// # Observability
///
/// - **Metric Name**: `Payments::ChargeCard`
/// - **Expected Duration**: 200-500ms (API call to payment gateway)
/// - **Error Outcomes**:
///   - `app_error`: Invalid card, insufficient funds, declined
///   - `system_error`: Payment gateway timeout, network issues
///
/// # Example
///
/// ```rust
/// let transaction_id = ctx.schedule_activity("Payments::ChargeCard", amount)
///     .await?;
/// ```
pub async fn charge_card(amount: String) -> Result<String, String> {
    // Implementation
}
```

### Orchestration Documentation

```rust
/// Processes an order through validation, payment, and fulfillment.
///
/// # Observability
///
/// - **Orchestration Name**: `Orders::ProcessOrder`
/// - **Versions**: 1.0.0 (current), 2.0.0 (beta)
/// - **Typical Duration**: 5-15 seconds
/// - **Activities Called**:
///   - `Orders::ValidateOrder` (50ms)
///   - `Payments::ChargeCard` (300ms)
///   - `Inventory::Reserve` (200ms)
///   - `Shipping::CreateLabel` (400ms)
///
/// # Logs
///
/// Key log messages to watch for:
/// - "Order validation complete" - Validation passed
/// - "Payment processed" - Payment successful
/// - "Order fulfilled" - Shipped
/// - "Compensation started" - Rollback initiated
///
/// # Example
///
/// ```rust
/// client.start_orchestration("order-123", "Orders::ProcessOrder", order_json).await?;
/// ```
pub async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
    // Implementation
}
```

## Best Practices

1. **Namespace activity names** by library: `LibName::ActivityName`
2. **Use semantic versioning** for orchestrations
3. **Log business events** not implementation details
4. **Document expected durations** for activities
5. **Test error paths** to ensure proper classification
6. **Avoid sensitive data** in logs and metrics
7. **Use descriptive error messages** that aid debugging
8. **Version orchestrations** when changing logic significantly

## Examples

See working examples:
- `examples/with_observability.rs` - Basic observability setup
- `tests/registry_composition_tests.rs` - Cross-crate registry patterns

## See Also

- [Observability Guide](observability-guide.md) - End user perspective
- [Provider Observability Guide](provider-observability.md) - Provider implementation
- [Cross-Crate Registry Pattern](cross-crate-registry-pattern.md) - Composing registries

