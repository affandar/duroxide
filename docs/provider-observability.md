# Provider Observability Guide

This guide is for developers implementing custom duroxide providers. It covers how to instrument providers for optimal observability.

## Overview

Providers should instrument their operations to emit:
- **Duration metrics**: For all queue and database operations
- **Error metrics**: Classified by operation and error type
- **Structured logs**: With instance_id context

## Provider Operation Metrics

### Required Metrics

All custom providers should track these operations:

#### 1. Fetch Operations
- **Metric**: `provider.fetch_orchestration_item_duration_ms`
- **Type**: Histogram
- **Description**: Time to fetch and lock an orchestration item
- **Labels**: None

#### 2. Ack Operations
- **Metric**: `provider.ack_orchestration_item_duration_ms`
- **Type**: Histogram
- **Description**: Time to commit orchestration changes atomically
- **Labels**: None

- **Metric**: `provider.ack_worker_duration_ms`
- **Type**: Histogram
- **Description**: Time to ack activity completion
- **Labels**: None

#### 3. Enqueue Operations
- **Metric**: `provider.enqueue_orchestrator_duration_ms`
- **Type**: Histogram
- **Description**: Time to enqueue work to orchestrator queue
- **Labels**: None

#### 4. Worker Operations
- **Metric**: `provider.dequeue_worker_duration_ms`
- **Type**: Histogram
- **Description**: Time to dequeue from worker queue
- **Labels**: None

### Error Metrics

Track infrastructure errors by operation and type:

- **Metric**: `provider.infrastructure_errors`
- **Type**: Counter
- **Labels**:
  - `operation`: fetch, ack_orch, ack_worker, enqueue
  - `error_type`: transaction_failed, lock_timeout, serialization_error, unknown

### Retry Metrics

Track retry attempts for ack operations:

- **Metric**: `provider.ack_orchestration_retries`
- **Type**: Counter
- **Description**: Number of retry attempts before success or final failure
- **Labels**: None

## Structured Logging

### Operation Spans

Use tracing spans for all provider operations:

```rust
#[tracing::instrument(skip(self), fields(instance = %instance))]
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    let start = std::time::Instant::now();
    
    // Fetch logic here
    
    let duration_ms = start.elapsed().as_millis() as u64;
    tracing::debug!(duration_ms, "Fetched orchestration item");
    
    // Record metric if available
    // metrics.provider_fetch_duration.record(duration_ms, &[]);
    
    Some(item)
}
```

### Error Logging

Log errors with structured context:

```rust
async fn ack_orchestration_item(...) -> Result<(), String> {
    match self.transaction().await {
        Ok(tx) => {
            // Process transaction
        }
        Err(e) => {
            tracing::error!(
                operation = "ack_orchestration",
                error_type = "transaction_failed",
                error = %e,
                "Failed to start transaction"
            );
            // Record error metric
            return Err(e.to_string());
        }
    }
}
```

## Example: SQLite Provider

The built-in SQLite provider demonstrates proper instrumentation:

```rust
// Fetch with timing
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    let start = std::time::Instant::now();
    
    // Fetch and lock instance
    let result = sqlx::query(...)
        .fetch_optional(&self.pool)
        .await;
    
    let duration_ms = start.elapsed().as_millis() as u64;
    
    match result {
        Ok(Some(row)) => {
            tracing::debug!(
                instance_id = %row.instance_id,
                duration_ms,
                "Fetched orchestration item"
            );
            // Build OrchestrationItem
            Some(item)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!(
                operation = "fetch",
                error_type = "query_failed",
                error = %e,
                "Fetch failed"
            );
            None
        }
    }
}
```

## Queue Depth Metrics

Providers supporting `ProviderAdmin` should implement queue depth queries:

```rust
async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
    let orchestrator_queue = sqlx::query_scalar("SELECT COUNT(*) FROM orchestrator_queue")
        .fetch_one(&self.pool)
        .await?;
    
    let worker_queue = sqlx::query_scalar("SELECT COUNT(*) FROM worker_queue")
        .fetch_one(&self.pool)
        .await?;
    
    let timer_queue = sqlx::query_scalar("SELECT COUNT(*) FROM timer_queue WHERE visible_at <= ?")
        .bind(now_millis())
        .fetch_one(&self.pool)
        .await?;
    
    Ok(QueueDepths {
        orchestrator_queue,
        worker_queue,
        timer_queue,
    })
}
```

The runtime can poll these periodically for gauge metrics.

## Testing Observability

### Unit Tests

Test that metrics are recorded correctly:

```rust
#[tokio::test]
async fn test_fetch_metrics() {
    let provider = MyProvider::new().await.unwrap();
    
    // Perform operation
    provider.fetch_orchestration_item().await;
    
    // Verify logs emitted (use tracing-test crate)
    // Verify metrics updated (check meter state)
}
```

### Integration Tests

Verify observability under load:

```rust
#[tokio::test]
async fn test_observability_under_load() {
    // Configure with observability
    let options = RuntimeOptions {
        observability: ObservabilityConfig {
            metrics_enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    
    // Run stress test
    // Verify no significant performance degradation
    // Verify all metrics populated
}
```

## Performance Guidelines

1. **Use async operations** - Don't block on metric recording
2. **Batch where possible** - Aggregate before recording
3. **Sample expensive metrics** - Not every operation needs metrics
4. **Use histograms wisely** - Pre-allocated buckets, no dynamic allocation
5. **Log at appropriate levels** - DEBUG for operations, ERROR for failures

## Common Pitfalls

### ❌ Don't: Block on metrics
```rust
// BAD: Blocking on metric export
metrics.record_and_flush(value); // Blocks!
```

### ✅ Do: Record asynchronously
```rust
// GOOD: Non-blocking metric recording
metrics.counter.add(1, &labels);
```

### ❌ Don't: Log sensitive data
```rust
// BAD: Logging sensitive information
tracing::info!(credit_card = %card_number, "Processing payment");
```

### ✅ Do: Sanitize or omit sensitive data
```rust
// GOOD: Log safe information only
tracing::info!(
    payment_id = %id,
    amount = %amount,
    "Processing payment"
);
```

## See Also

- [Observability Guide](observability-guide.md) - For end users
- [Provider Implementation Guide](provider-implementation-guide.md) - For building providers
- [src/providers/sqlite.rs](../src/providers/sqlite.rs) - Reference implementation

