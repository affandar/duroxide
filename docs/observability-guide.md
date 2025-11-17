# Observability Guide

This guide covers how to enable and use observability features in duroxide for production monitoring and debugging.

## Overview

Duroxide provides production-grade observability through:
- **Structured Logging**: Correlated logs with instance_id, execution_id, orchestration_name, and more
- **OpenTelemetry Metrics**: Counters, histograms, and gauges for performance monitoring
- **Log Analytics Integration**: Ready-to-use patterns for Elasticsearch, Loki, CloudWatch, and Azure Monitor

## Quick Start

### Basic Structured Logging (Text to stdout)

The simplest setup uses the Compact format:

```rust
use duroxide::runtime::{RuntimeOptions, ObservabilityConfig, LogFormat};

let options = RuntimeOptions {
    observability: ObservabilityConfig {
        log_format: LogFormat::Compact,
        log_level: "info".to_string(),
        ..Default::default()
    },
    ..Default::default()
};

let rt = Runtime::start_with_options(store, activities, orchestrations, options).await;
```

**Output Format**:
```
2025-11-01T17:22:04.494Z INFO duroxide::runtime Orchestration started instance_id=order-123 execution_id=1 orchestration_name=ProcessOrder worker_id=orch-0-cd54
2025-11-01T17:22:04.596Z INFO duroxide::runtime Activity started instance_id=order-123 execution_id=1 activity_name=ValidatePayment activity_id=3 worker_id=work-1-cd54
2025-11-01T17:22:04.698Z INFO duroxide::runtime Activity completed instance_id=order-123 execution_id=1 activity_name=ValidatePayment worker_id=work-1-cd54 outcome="success" duration_ms=102
2025-11-01T17:22:04.806Z INFO duroxide::orchestration Payment validated successfully instance_id=order-123 execution_id=1 orchestration_name=ProcessOrder worker_id=orch-0-cd54
2025-11-01T17:22:05.010Z INFO duroxide::runtime Orchestration completed instance_id=order-123 execution_id=1 worker_id=orch-0-cd54 history_events=9
```

Format: `timestamp level module message field1=value1 field2=value2 ...`

All correlation fields are included directly on each log line (flat structure, no span nesting).

## Log Formats

### Compact Format

Best for development and debugging with minimal noise:

```rust
ObservabilityConfig {
    log_format: LogFormat::Compact,
    log_level: "info".to_string(),
    ..Default::default()
}
```

Output: `timestamp level module [instance_id] [worker_id] message`

### Pretty Format

Full structured fields for detailed debugging:

```rust
ObservabilityConfig {
    log_format: LogFormat::Pretty,
    log_level: "debug".to_string(),
    ..Default::default()
}
```

Output includes all fields as `key=value` pairs:
```
2024-10-30T10:15:23.456Z INFO duroxide::runtime instance_id=order-123 execution_id=1 orchestration_name=ProcessOrder version=1.0.0 worker_id=orch-0-a3f9: Orchestration started
```

### JSON Format

For log aggregators and analytics platforms:

```rust
ObservabilityConfig {
    log_format: LogFormat::Json,
    log_level: "info".to_string(),
    ..Default::default()
}
```

Output: One JSON object per line with all structured fields

## Correlation Fields

Every log entry includes these fields for correlation:

- **instance_id** - Unique orchestration instance identifier
- **execution_id** - Execution number (1, 2, 3... for ContinueAsNew)
- **orchestration_name** - Name of the orchestration
- **orchestration_version** - Semantic version
- **activity_name** - Activity name (in activity context)
- **activity_id** - Event ID of the activity
- **worker_id** - Dispatcher worker ID (format: `work-{index}-{runtime_id}` or `orch-{index}-{runtime_id}`)
- **timestamp** - ISO8601 timestamp

### Example: Tracing an Orchestration

All logs for instance `order-123`:
```bash
# In Elasticsearch
instance_id:"order-123"

# In CloudWatch Insights
fields @timestamp, message | filter instance_id = "order-123" | sort @timestamp

# In Loki (LogQL)
{job="duroxide"} | json | instance_id="order-123"
```

## User Orchestration Logging

Use `ctx.trace_*()` methods in your orchestrations for replay-safe logging:

```rust
async fn process_order(ctx: OrchestrationContext, order: String) -> Result<String, String> {
    ctx.trace_info("Processing order started");
    
    let validated = ctx.schedule_activity("ValidateOrder", order.clone())
        .into_activity().await?;
    
    ctx.trace_info(format!("Validation result: {}", validated));
    
    if validated == "invalid" {
        ctx.trace_error("Order validation failed");
        return Err("validation_failed".to_string());
    }
    
    ctx.trace_info("Order processing complete");
    Ok("processed".to_string())
}
```

**Output**:
```
2024-10-30T10:15:23.690Z INFO duroxide::orchestration [order-123] Processing order started
2024-10-30T10:15:23.750Z INFO duroxide::orchestration [order-123] Validation result: valid
2024-10-30T10:15:23.890Z INFO duroxide::orchestration [order-123] Order processing complete
```

**Key Points**:
- All `ctx.trace_*()` calls are deterministic (replay-safe)
- Context fields automatically included (instance_id, execution_id, etc.)
- Only logged on first execution (not during replay)
- Creates SystemCall events in history for determinism

## Metrics (with observability feature)

Enable OpenTelemetry metrics for production monitoring:

```rust
ObservabilityConfig {
    metrics_enabled: true,
    metrics_export_endpoint: Some("http://localhost:4317".to_string()),
    metrics_export_interval_ms: 10000,
    service_name: "my-duroxide-app".to_string(),
    ..Default::default()
}
```

### Available Metrics

#### Orchestration Metrics
- `duroxide.orchestration.completions` - Successful completions (labels: orchestration_name, version, status)
- `duroxide.orchestration.failures` - Failures by error type (labels: orchestration_name, error_type)
- `duroxide.orchestration.infrastructure_errors` - Infra errors (labels: orchestration_name, operation, error_type)
- `duroxide.orchestration.configuration_errors` - Config errors (labels: orchestration_name, error_kind)
- `duroxide.orchestration.history_size_events` - Event count at completion
- `duroxide.orchestration.history_size_bytes` - History size at completion
- `duroxide.orchestration.turns` - Turns to completion

#### Activity Metrics
- `duroxide.activity.executions` - Execution outcomes (labels: activity_name, outcome=success/app_error/system_error)
- `duroxide.activity.duration_ms` - Execution duration (labels: activity_name, outcome)
- `duroxide.activity.app_errors` - Application errors (labels: activity_name)
- `duroxide.activity.infrastructure_errors` - Infrastructure errors (labels: activity_name, operation)
- `duroxide.activity.configuration_errors` - Configuration errors (labels: activity_name, error_kind)

#### Provider Metrics
- `duroxide.provider.fetch_orchestration_item_duration_ms` - Fetch latency
- `duroxide.provider.ack_orchestration_item_duration_ms` - Ack latency
- `duroxide.provider.ack_worker_duration_ms` - Worker ack latency
- `duroxide.provider.ack_orchestration_retries` - Retry attempts
- `duroxide.provider.infrastructure_errors` - Provider errors (labels: operation, error_type)

#### Client Metrics
- `duroxide.client.orchestration_starts` - Orchestrations started
- `duroxide.client.external_events_raised` - Events raised
- `duroxide.client.cancellations` - Cancellations requested
- `duroxide.client.wait_duration_ms` - Wait operation duration

## Error Classification

Duroxide categorizes errors into three types for better observability:

### Infrastructure Errors
Provider failures, data corruption, network issues:
- Transaction failures
- Lock timeouts
- Serialization errors
- Database connectivity issues

**Metrics**: `*.infrastructure_errors` counters

### Configuration Errors
Deployment/registration issues:
- Unregistered orchestrations/activities
- Missing versions
- Nondeterminism detected

**Metrics**: `*.configuration_errors` counters

### Application Errors
Business logic failures:
- Activity returns `Err(...)`
- User-initiated cancellations
- Business validation failures

**Metrics**: Activity/orchestration failure counters with `app_error` outcome

## Log Analytics Integration

### 1. Elasticsearch / OpenSearch

**Setup**:
```yaml
# otel-collector-config.yaml
exporters:
  elasticsearch:
    endpoints: ["http://elasticsearch:9200"]
    logs_index: duroxide-logs
```

**Queries**:
```
# All logs for an orchestration
instance_id:"order-123"

# Failed activities
activity_name:* AND outcome:"app_error"

# Nondeterminism errors
error_type:"nondeterminism" AND level:"ERROR"
```

### 2. Grafana Loki

**Setup**:
```yaml
# promtail-config.yaml
scrape_configs:
  - job_name: duroxide
    static_configs:
      - targets:
          - localhost
        labels:
          job: duroxide-worker
```

**LogQL**:
```logql
# Instance timeline
{job="duroxide"} | json | instance_id="order-123"

# Error rate
rate({job="duroxide", level="error"}[5m]) by (orchestration_name)
```

### 3. AWS CloudWatch Logs

**Setup**: Use CloudWatch Logs agent or OTLP exporter

**CloudWatch Insights Queries**:
```sql
# Orchestration trace
fields @timestamp, instance_id, orchestration_name, message
| filter instance_id = "order-123"
| sort @timestamp asc

# Activity success rate
fields activity_name, outcome
| filter activity_name != ""
| stats count() by activity_name, outcome
```

### 4. Azure Monitor / Application Insights

**Setup**: Use Azure Monitor OTLP exporter

**KQL Queries**:
```kql
// Full orchestration timeline
traces
| where customDimensions.instance_id == "order-123"
| project timestamp, severityLevel, message
| order by timestamp asc
```

## Debugging Workflows

### Trace a Single Orchestration

1. Note the instance_id when starting: `order-123`
2. Query logs by instance_id
3. Filter by execution_id if using ContinueAsNew
4. Trace activity execution within the orchestration

### Find Slow Activities

Query activity duration metrics:
```promql
# Prometheus query
histogram_quantile(0.99, 
  rate(duroxide_activity_duration_ms_bucket[5m])
) by (activity_name)
```

### Identify Nondeterminism Issues

Search logs for configuration errors:
```
error_type:"nondeterminism" AND level:"ERROR"
```

These indicate scheduling order mismatches that need code fixes.

## Performance Impact

### With Metrics Disabled (default)
- Zero overhead from metrics collection
- Minimal logging overhead (only structured field extraction)
- Recommended for most use cases

### With Metrics Enabled
- ~2-5% overhead for metrics collection
- Lock-free atomic operations
- Background export (no blocking)
- Recommended for production monitoring

### Log Format Impact
- **Compact**: Lowest overhead
- **Pretty**: Slight overhead for formatting
- **JSON**: Minimal additional overhead

## Environment Variables

Override configuration via environment variables:

```bash
# Logging level
RUST_LOG=info,duroxide=debug

# OpenTelemetry (if using OTLP)
OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4317
OTEL_SERVICE_NAME=duroxide-worker
OTEL_SERVICE_VERSION=1.0.0
```

## Best Practices

1. **Use Compact format in development** for readable logs
2. **Use JSON format in production** for log aggregation
3. **Include context in user traces**: Use descriptive messages
4. **Monitor infrastructure_errors** closely - indicates system issues
5. **Track configuration_errors** - may require deployment fixes
6. **Use instance_id** as primary correlation field
7. **Query by orchestration_name** to find patterns across instances
8. **Enable metrics in production** for performance insights

## Troubleshooting

### Logs Not Showing

Check the log level:
```rust
ObservabilityConfig {
    log_level: "debug".to_string(),  // Lower for more logs
    ..Default::default()
}
```

### Missing Context Fields

Ensure logs are within an orchestration or activity span. Framework logs automatically include context.

### Metrics Not Exporting

Verify:
1. `observability` feature flag is enabled
2. OTLP endpoint is accessible
3. Export interval allows time for collection

## Example: Full Production Configuration

```rust
use duroxide::runtime::{RuntimeOptions, ObservabilityConfig, LogFormat};

let options = RuntimeOptions {
    observability: ObservabilityConfig {
        // Metrics
        metrics_enabled: true,
        metrics_export_endpoint: Some("http://otel-collector:4317".to_string()),
        metrics_export_interval_ms: 10000,
        
        // Logging
        log_format: LogFormat::Json,
        log_level: "info".to_string(),
        
        // Service identification
        service_name: "payment-processor".to_string(),
        service_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    },
    orchestration_concurrency: 4,
    worker_concurrency: 8,
    ..Default::default()
};
```

## See Also

- [Provider Observability Guide](provider-observability.md) - For custom provider implementors
- [Library Observability Guide](library-observability.md) - For library developers
- [examples/with_observability.rs](../examples/with_observability.rs) - Working example

