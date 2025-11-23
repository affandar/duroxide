# Duroxide Metrics Specification

**Version:** 1.0  
**Last Updated:** 2025-11-22  
**Status:** Current Implementation

This document provides a complete reference of all metrics emitted by duroxide, organized by functional area with detailed label specifications and bucket configurations.

---

## Summary Table

| Metric Name | Type | Category | Labels | Test Coverage | OTel Export | Notes |
|------------|------|----------|--------|---------------|-------------|-------|
| **Orchestration Lifecycle** |
| `duroxide_orchestration_starts_total` | Counter | Orchestration | `orchestration_name`, `version`, `initiated_by` | ✅ `test_labeled_metrics_recording` | ✅ Code Review | Recorded in `orchestration.rs:136` with all labels |
| `duroxide_orchestration_completions_total` | Counter | Orchestration | `orchestration_name`, `version`, `status`, `final_turn_count` | ✅ `metrics_capture_activity_and_orchestration_outcomes` | ✅ Code Review | Recorded in `orchestration.rs:200,283` with all labels |
| `duroxide_orchestration_failures_total` | Counter | Orchestration | `orchestration_name`, `version`, `error_type`, `error_category` | ✅ `test_error_classification_metrics` | ✅ Code Review | Recorded in `orchestration.rs:280` with all labels |
| `duroxide_orchestration_duration_seconds` | Histogram | Orchestration | `orchestration_name`, `version`, `status` | ✅ `test_activity_duration_tracking` | ✅ Code Review | Recorded in `observability.rs:514` with labels, buckets: 0.1-3600s |
| `duroxide_orchestration_history_size` | Histogram | Orchestration | `orchestration_name` | ✅ `test_labeled_metrics_recording` | ✅ Code Review | Recorded in `observability.rs:527` with label, buckets: 10-10000 events |
| `duroxide_orchestration_turns` | Histogram | Orchestration | `orchestration_name` | ✅ `test_labeled_metrics_recording` | ✅ Code Review | Recorded in `observability.rs:522` with label, buckets: 1-500 turns |
| `duroxide_orchestration_infrastructure_errors_total` | Counter | Orchestration | `orchestration_name`, `error_category` | ✅ `metrics_capture_activity_and_orchestration_outcomes` | ✅ Code Review | Recorded in `observability.rs:561` with labels |
| `duroxide_orchestration_configuration_errors_total` | Counter | Orchestration | `orchestration_name`, `error_category` | ✅ `test_error_classification_metrics` | ✅ Code Review | Recorded in `observability.rs:573` with labels |
| `duroxide_orchestration_continue_as_new_total` | Counter | Orchestration | `orchestration_name`, `execution_id` | ✅ `test_continue_as_new_metrics` | ✅ Code Review | Recorded in `orchestration.rs:308` with labels |
| `duroxide_active_orchestrations` | Gauge | Orchestration | `state` | ✅ `test_active_orchestrations_gauge` | ✅ Code Review | Observable gauge with callback, updated in `orchestration.rs:141,210,293` |
| `duroxide_orchestrator_queue_depth` | Gauge | Runtime | _(none)_ | ✅ `test_queue_depth_gauges_initialization` | ✅ Code Review | Observable gauge with callback, initialized via `initialize_gauges()` |
| `duroxide_worker_queue_depth` | Gauge | Runtime | _(none)_ | ✅ `test_queue_depth_gauges_tracking` | ✅ Code Review | Observable gauge with callback, initialized via `initialize_gauges()` |
| **Activity Execution** |
| `duroxide_activity_executions_total` | Counter | Activity | `activity_name`, `outcome`, `retry_attempt` | ✅ `metrics_capture_activity_and_orchestration_outcomes` | ✅ Code Review | Tracks all outcomes including app_error |
| `duroxide_activity_duration_seconds` | Histogram | Activity | `activity_name`, `outcome` | ✅ `test_activity_duration_tracking` | ✅ Code Review | Buckets: 0.01-300s |
| `duroxide_activity_infrastructure_errors_total` | Counter | Activity | `activity_name` | ✅ `metrics_capture_activity_and_orchestration_outcomes` | ✅ Code Review | Infra-specific errors |
| `duroxide_activity_configuration_errors_total` | Counter | Activity | `activity_name` | ✅ `test_error_classification_metrics` | ✅ Code Review | Config-specific errors |
| **Sub-Orchestration** |
| `duroxide_suborchestration_calls_total` | Counter | Sub-Orchestration | `parent_orchestration`, `child_orchestration`, `outcome` | ✅ `test_sub_orchestration_metrics` | ❌ Not Called | Method defined in `observability.rs:701` but never called from runtime |
| `duroxide_suborchestration_duration_seconds` | Histogram | Sub-Orchestration | `parent_orchestration`, `child_orchestration`, `outcome` | ✅ `test_sub_orchestration_metrics` | ❌ Not Called | Method defined in `observability.rs:718` but never called from runtime |
| **Provider (Storage)** |
| `duroxide_provider_operation_duration_seconds` | Histogram | Provider | `operation`, `status` | ✅ `test_provider_metrics_recorded` | ✅ Code Review | Recorded in `instrumented.rs` wrapper with labels |
| `duroxide_provider_errors_total` | Counter | Provider | `operation`, `error_type` | ✅ `test_provider_error_metrics` | ✅ Code Review | Recorded in `instrumented.rs` wrapper with labels |
| **Client Operations** |
| `duroxide_client_orchestration_starts_total` | Counter | Client | `orchestration_name` | ❌ Not instrumented | ❌ Not Wired | Instrument exists but never recorded |
| `duroxide_client_external_events_raised_total` | Counter | Client | `event_name` | ❌ Not instrumented | ❌ Not Wired | Instrument exists but never recorded |
| `duroxide_client_cancellations_total` | Counter | Client | _(none)_ | ❌ Not instrumented | ❌ Not Wired | Instrument exists but never recorded |
| `duroxide_client_wait_duration_seconds` | Histogram | Client | _(none)_ | ❌ Not instrumented | ❌ Not Wired | Instrument exists but never recorded |
| **Internal Dispatcher** |
| `duroxide.orchestration.dispatcher.items_fetched` | Counter | Internal | _(none)_ | ✅ Implicitly tested | ✅ Code Review | Recorded in dispatcher, no labels |
| `duroxide.orchestration.dispatcher.processing_duration_ms` | Histogram | Internal | _(none)_ | ✅ Implicitly tested | ✅ Code Review | Recorded in dispatcher, no labels |
| `duroxide.worker.dispatcher.items_fetched` | Counter | Internal | _(none)_ | ✅ Implicitly tested | ✅ Code Review | Recorded in dispatcher, no labels |
| `duroxide.worker.dispatcher.execution_duration_ms` | Histogram | Internal | _(none)_ | ✅ Implicitly tested | ✅ Code Review | Recorded in dispatcher, no labels |

**Test Location:** All tests are in `tests/observability_tests.rs`

**OTel Export Status:**
- **✅ Code Review** - Metric is properly wired to OTel API with correct labels/buckets (verified by code audit)
- **⚠️ Defined** - Method exists but not called from runtime (will always be 0)
- **❌ Not Called** - Method defined but never invoked (metric won't appear in exports)
- **❌ Not Wired** - Instrument created but never used

Tests validate atomic counters (that metrics are recorded) but do not validate full OpenTelemetry export format. OTel export status is determined by code audit to verify metrics are properly wired to the OpenTelemetry API.

**Known Issues:**
- **Sub-orchestration metrics require complex implementation** - `duroxide_suborchestration_calls_total` and `duroxide_suborchestration_duration_seconds` are defined but not wired up. Implementation is complex because: (1) Parent orchestration name is in parent execution context, (2) Child orchestration name must be extracted from `SubOrchestrationScheduled` event by looking up `parent_id` in history, (3) Duration calculation requires event timestamps or turn-based approximation, (4) Metrics must be recorded when `SubOrchCompleted`/`SubOrchFailed` work items are processed in `replay_engine.rs`. Sub-orchestrations currently appear as regular orchestrations in metrics (counted in `duroxide_orchestration_starts_total`, etc.) but parent-child relationship is not tracked.
- **Client metrics not instrumented** - All 4 client metrics have instruments created but are never recorded from `Client` methods.

**OTel Export Validation:**
- Tests validate that metrics are recorded (via atomic counters)
- Code review confirms most metrics use OTel API with proper labels
- Automated validation of OTel export format (labels, buckets) is not implemented
- For full validation, manually export to Prometheus and verify metric structure

---

## 1. Orchestration Lifecycle Metrics

### 1.1 `duroxide_orchestration_starts_total` (Counter)

**Description:** Total number of orchestration instances started

**Labels:**
- `orchestration_name` (string) - Fully qualified orchestration name (e.g., "ProcessOrder", "toygres::CreateInstance")
- `version` (string) - Orchestration version (e.g., "1.0.0")
- `initiated_by` (string) - How the orchestration was started:
  - `"client"` - Started via Client API
  - `"suborchestration"` - Started as a sub-orchestration
  - `"continueAsNew"` - Started via continue-as-new

**Purpose:** Track which orchestrations are being used, identify version distribution

**Example Queries:**
```promql
# Orchestration start rate by type
rate(duroxide_orchestration_starts_total[5m]) by (orchestration_name)

# Version distribution
duroxide_orchestration_starts_total by (orchestration_name, version)
```

---

### 1.2 `duroxide_orchestration_completions_total` (Counter)

**Description:** Orchestrations that completed (successfully or failed)

**Labels:**
- `orchestration_name` (string) - Orchestration name
- `version` (string) - Orchestration version
- `status` (string) - Completion status:
  - `"success"` - Completed successfully
  - `"failed"` - Failed with error
  - `"cancelled"` - Cancelled by client
- `final_turn_count` (string) - Bucketed turn count:
  - `"1-5"` - 1 to 5 turns
  - `"6-10"` - 6 to 10 turns
  - `"11-50"` - 11 to 50 turns
  - `"50+"` - More than 50 turns

**Purpose:** Success rate tracking, identify orchestrations requiring many turns

**Example Queries:**
```promql
# Success rate
rate(duroxide_orchestration_completions_total{status="success"}[5m]) 
/ 
rate(duroxide_orchestration_completions_total[5m])

# Orchestrations requiring many turns (optimization candidates)
duroxide_orchestration_completions_total{final_turn_count="50+"}
```

---

### 1.3 `duroxide_orchestration_failures_total` (Counter)

**Description:** Orchestration failures with detailed error classification

**Labels:**
- `orchestration_name` (string) - Orchestration name
- `version` (string) - Orchestration version
- `error_type` (string) - Error classification:
  - `"app_error"` - Application/business logic error
  - `"infrastructure_error"` - Provider/storage failure
  - `"config_error"` - Configuration issue (unregistered, nondeterminism)
- `error_category` (string) - High-level error category (e.g., "database", "network", "logic", "validation")

**Purpose:** Root cause analysis, distinguish infrastructure vs application errors

**Example Queries:**
```promql
# Infrastructure failures (actionable, not user errors)
rate(duroxide_orchestration_failures_total{error_type="infrastructure_error"}[5m])

# Nondeterminism bugs (critical - requires code fix)
duroxide_orchestration_failures_total{error_type="config_error"}
```

---

### 1.4 `duroxide_orchestration_duration_seconds` (Histogram)

**Description:** End-to-end orchestration execution time from start to completion

**Labels:**
- `orchestration_name` (string) - Orchestration name
- `version` (string) - Orchestration version
- `status` (string) - Completion status (success, failed, cancelled)

**Buckets:** `[0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0]` seconds

**Purpose:** Identify slow orchestrations, track p50/p95/p99 latency over time

**Example Queries:**
```promql
# p95 orchestration duration
histogram_quantile(0.95, 
  rate(duroxide_orchestration_duration_seconds_bucket[5m])
) by (orchestration_name)

# Orchestrations taking >5 minutes
rate(duroxide_orchestration_duration_seconds_bucket{le="300"}[5m])
```

---

### 1.5 `duroxide_orchestration_history_size` (Histogram)

**Description:** Number of history events at orchestration completion

**Labels:**
- `orchestration_name` (string) - Orchestration name

**Buckets:** `[10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0]` events

**Purpose:** Identify orchestrations with unbounded history growth (memory leak detection)

**Example Queries:**
```promql
# Orchestrations with large history (>1000 events)
sum(rate(duroxide_orchestration_history_size_bucket{le="+Inf"}[5m]))
-
sum(rate(duroxide_orchestration_history_size_bucket{le="1000"}[5m]))
```

---

### 1.6 `duroxide_orchestration_turns` (Histogram)

**Description:** Number of execution turns to orchestration completion

**Labels:**
- `orchestration_name` (string) - Orchestration name

**Buckets:** `[1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]` turns

**Purpose:** Detect orchestrations requiring many turns (optimization targets)

**Example Queries:**
```promql
# p95 turn count
histogram_quantile(0.95,
  rate(duroxide_orchestration_turns_bucket[5m])
) by (orchestration_name)

# Orchestrations that took >100 turns
sum(rate(duroxide_orchestration_turns_bucket{le="+Inf"}[5m]))
-
sum(rate(duroxide_orchestration_turns_bucket{le="100"}[5m]))
```

---

### 1.7 `duroxide_orchestration_infrastructure_errors_total` (Counter)

**Description:** Infrastructure-level orchestration errors (subset of failures)

**Labels:**
- `orchestration_name` (string) - Orchestration name
- `error_category` (string) - Error category

**Purpose:** Track infrastructure issues separately for alerting

---

### 1.8 `duroxide_orchestration_configuration_errors_total` (Counter)

**Description:** Configuration-level orchestration errors (unregistered orchestrations, nondeterminism)

**Labels:**
- `orchestration_name` (string) - Orchestration name
- `error_category` (string) - Error category

**Purpose:** Track deployment/configuration issues separately

---

### 1.9 `duroxide_orchestration_continue_as_new_total` (Counter)

**Description:** Continue-as-new operations performed

**Labels:**
- `orchestration_name` (string) - Orchestration name
- `execution_id` (string) - Execution number (1, 2, 3, ...)

**Purpose:** Verify continue-as-new is working, identify long-running actors

**Example Queries:**
```promql
# Continue-as-new operations per second
rate(duroxide_orchestration_continue_as_new_total[5m])

# Orchestrations using continue-as-new
count(rate(duroxide_orchestration_continue_as_new_total[5m]) > 0) by (orchestration_name)
```

---

### 1.10 `duroxide_active_orchestrations` (Gauge)

**Description:** Current number of orchestration instances that are actively running (not completed/failed)

**Labels:**
- `state` (string) - Current state (currently always "all", future: "executing", "waiting_for_activity", etc.)

**Purpose:** Track concurrent orchestrations, detect leaks, capacity planning

**Note:** This is a **GAUGE** (can increase/decrease), unlike counters. Continue-as-new does NOT change this count (orchestration stays active).

**Example Queries:**
```promql
# Total active orchestrations right now
duroxide_active_orchestrations

# Detect orchestration leaks (if this keeps growing over time)
increase(duroxide_active_orchestrations[1h]) > 100
```

---

### 1.11 `duroxide_orchestrator_queue_depth` (Gauge)

**Description:** Current number of unlocked items in the orchestrator queue (items waiting to be processed)

**Labels:** _(none)_

**Purpose:** Monitor orchestrator queue backlog, capacity planning, performance troubleshooting

**Note:** This is a **GAUGE** initialized from the provider on startup to reflect actual queue state.

**Example Queries:**
```promql
# Current orchestrator queue backlog
duroxide_orchestrator_queue_depth

# Alert if queue is growing too large
duroxide_orchestrator_queue_depth > 1000

# Queue depth trend over time
rate(duroxide_orchestrator_queue_depth[5m])
```

**Use Cases:**
- **Capacity Planning**: Scale orchestration dispatchers when queue grows
- **Performance Monitoring**: Identify bottlenecks in orchestration processing
- **Alerting**: Trigger alerts when backlog exceeds thresholds

---

### 1.12 `duroxide_worker_queue_depth` (Gauge)

**Description:** Current number of unlocked items in the worker queue (activities waiting to be executed)

**Labels:** _(none)_

**Purpose:** Monitor worker queue backlog, capacity planning, activity execution throughput

**Note:** This is a **GAUGE** initialized from the provider on startup to reflect actual queue state.

**Example Queries:**
```promql
# Current worker queue backlog
duroxide_worker_queue_depth

# Alert if worker queue is backed up
duroxide_worker_queue_depth > 500

# Compare orchestrator vs worker queue depths
duroxide_orchestrator_queue_depth / duroxide_worker_queue_depth
```

**Use Cases:**
- **Worker Scaling**: Scale activity workers when queue grows
- **Bottleneck Detection**: Identify if workers are the bottleneck
- **Load Balancing**: Understand distribution of work

---

## 2. Activity Execution Metrics

### 2.1 `duroxide_activity_executions_total` (Counter)

**Description:** Activity execution attempts (including retries)

**Labels:**
- `activity_name` (string) - Fully qualified activity name (e.g., "ValidateOrder", "toygres::DeployPostgres")
- `outcome` (string) - Execution outcome:
  - `"success"` - Executed successfully
  - `"app_error"` - Application/business logic error
  - `"infra_error"` - Infrastructure failure
  - `"config_error"` - Configuration issue (unregistered activity)
- `retry_attempt` (string) - Retry attempt number:
  - `"0"` - First attempt
  - `"1"` - First retry
  - `"2"` - Second retry
  - `"3+"` - Third or later retry

**Purpose:** Identify flaky activities, track retry rates, track all error types including application errors

**Note:** Application-level activity errors are tracked via `outcome="app_error"` in this metric. There is no separate `activity_errors_total` counter.

**Example Queries:**
```promql
# Activity failure rate (all types)
rate(duroxide_activity_executions_total{outcome!="success"}[5m]) 
by (activity_name)

# Activity app error rate
rate(duroxide_activity_executions_total{outcome="app_error"}[5m])
by (activity_name)

# Activities requiring retries
rate(duroxide_activity_executions_total{retry_attempt!="0"}[5m])
```

---

### 2.2 `duroxide_activity_duration_seconds` (Histogram)

**Description:** Activity execution time (wall clock)

**Labels:**
- `activity_name` (string) - Activity name
- `outcome` (string) - Execution outcome (success, app_error, infra_error, config_error)

**Buckets:** `[0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0]` seconds

**Purpose:** Identify slow activities, set appropriate timeouts

**Example Queries:**
```promql
# p99 activity duration by outcome
histogram_quantile(0.99, 
  rate(duroxide_activity_duration_seconds_bucket[5m])
) by (activity_name, outcome)

# Top 5 slowest activities
topk(5,
  histogram_quantile(0.95,
    rate(duroxide_activity_duration_seconds_bucket[5m])
  ) by (activity_name)
)
```

---

### 2.3 `duroxide_activity_infrastructure_errors_total` (Counter)

**Description:** Infrastructure-level activity errors (subset of errors)

**Labels:**
- `activity_name` (string) - Activity name

**Purpose:** Track infrastructure issues separately for alerting

---

### 2.4 `duroxide_activity_configuration_errors_total` (Counter)

**Description:** Configuration-level activity errors (unregistered activities)

**Labels:**
- `activity_name` (string) - Activity name

**Purpose:** Track deployment/configuration issues separately

---

## 3. Sub-Orchestration Metrics

### 3.1 `duroxide_suborchestration_calls_total` (Counter)

**Description:** Sub-orchestration invocations

**Labels:**
- `parent_orchestration` (string) - Parent orchestration name
- `child_orchestration` (string) - Child orchestration name
- `outcome` (string) - Execution outcome:
  - `"success"` - Completed successfully
  - `"failed"` - Failed with error

**Purpose:** Understand orchestration composition, trace call graphs

**Example Queries:**
```promql
# Sub-orchestration call rate
rate(duroxide_suborchestration_calls_total[5m]) by (parent_orchestration, child_orchestration)

# Sub-orchestration success rate
rate(duroxide_suborchestration_calls_total{outcome="success"}[5m])
/
rate(duroxide_suborchestration_calls_total[5m])
```

---

### 3.2 `duroxide_suborchestration_duration_seconds` (Histogram)

**Description:** Sub-orchestration execution time

**Labels:**
- `parent_orchestration` (string) - Parent orchestration name
- `child_orchestration` (string) - Child orchestration name
- `outcome` (string) - Execution outcome (success, failed)

**Buckets:** `[0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0]` seconds

**Purpose:** Track sub-orchestration performance

**Example Queries:**
```promql
# p95 sub-orchestration duration
histogram_quantile(0.95,
  rate(duroxide_suborchestration_duration_seconds_bucket[5m])
) by (parent_orchestration, child_orchestration)
```

---

## 4. Provider (Storage) Metrics

### 4.1 `duroxide_provider_operation_duration_seconds` (Histogram)

**Description:** Database/storage operation latency

**Labels:**
- `operation` (string) - Provider operation type:
  - `"fetch"` - Fetch orchestration item
  - `"ack"` - Acknowledge/commit orchestration
  - `"save_event"` - Save event to history
  - `"create_instance"` - Create new orchestration instance
  - `"query"` - Query operation
- `status` (string) - Operation status:
  - `"success"` - Operation succeeded
  - `"error"` - Operation failed

**Buckets:** `[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0]` seconds

**Purpose:** Database performance monitoring, identify slow queries

**Example Queries:**
```promql
# p95 database operation latency
histogram_quantile(0.95,
  rate(duroxide_provider_operation_duration_seconds_bucket[5m])
) by (operation)

# Slow database operations (>100ms)
histogram_quantile(0.99,
  rate(duroxide_provider_operation_duration_seconds_bucket{le="0.1"}[5m])
)
```

---

### 4.2 `duroxide_provider_errors_total` (Counter)

**Description:** Provider/storage layer errors

**Labels:**
- `operation` (string) - Provider operation (fetch, ack, save_event, etc.)
- `error_type` (string) - Error classification:
  - `"timeout"` - Operation timeout
  - `"connection"` - Connection failure
  - `"deadlock"` - Database deadlock
  - `"corruption"` - Data corruption
  - `"constraint_violation"` - Database constraint violation

**Purpose:** Database health monitoring, alerting on infrastructure issues

**Example Queries:**
```promql
# Provider error rate by operation
rate(duroxide_provider_errors_total[5m]) by (operation)

# Database connection errors
rate(duroxide_provider_errors_total{error_type="connection"}[5m])
```

---

## 5. Client Operations Metrics

⚠️ **Note:** Client metrics are currently **defined but not instrumented**. The metric instruments exist in the code but are not being recorded during client operations. These will be fully implemented in a future release.

### 5.1 `duroxide_client_orchestration_starts_total` (Counter)

**Description:** Orchestrations started via Client API

**Labels:**
- `orchestration_name` (string) - Orchestration name

**Status:** ⚠️ Defined but not yet instrumented

---

### 5.2 `duroxide_client_external_events_raised_total` (Counter)

**Description:** External events raised via Client API

**Labels:**
- `event_name` (string) - Event name

**Status:** ⚠️ Defined but not yet instrumented

---

### 5.3 `duroxide_client_cancellations_total` (Counter)

**Description:** Orchestration cancellations via Client API

**Labels:** _(none)_

**Status:** ⚠️ Defined but not yet instrumented

---

### 5.4 `duroxide_client_wait_duration_seconds` (Histogram)

**Description:** Client wait operation duration (wait_for_orchestration)

**Labels:** _(none)_

**Buckets:** `[0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0]` seconds

**Status:** ⚠️ Defined but not yet instrumented

---

## 6. Internal Dispatcher Metrics

⚠️ **Note:** These metrics use dot notation (not underscore) and milliseconds (not seconds), making them **not Prometheus-compliant**. They are intended for internal diagnostics and are not typically exported to monitoring systems.

### 6.1 `duroxide.orchestration.dispatcher.items_fetched` (Counter)

**Description:** Items fetched by orchestration dispatcher

**Labels:** _(none)_

**Note:** Internal metric, uses milliseconds

---

### 6.2 `duroxide.orchestration.dispatcher.processing_duration_ms` (Histogram)

**Description:** Time to process an orchestration item

**Labels:** _(none)_

**Note:** Internal metric, uses milliseconds

---

### 6.3 `duroxide.worker.dispatcher.items_fetched` (Counter)

**Description:** Activities fetched by worker dispatcher

**Labels:** _(none)_

**Note:** Internal metric, uses milliseconds

---

### 6.4 `duroxide.worker.dispatcher.execution_duration_ms` (Histogram)

**Description:** Activity execution duration

**Labels:** _(none)_

**Note:** Internal metric, uses milliseconds

---

## Common Queries

### Success Rates

```promql
# Orchestration success rate by name
rate(duroxide_orchestration_completions_total{status="success"}[5m])
/
rate(duroxide_orchestration_completions_total[5m])
by (orchestration_name)

# Activity success rate
rate(duroxide_activity_executions_total{outcome="success"}[5m])
/
rate(duroxide_activity_executions_total[5m])
by (activity_name)
```

### Latency (p95, p99)

```promql
# p95 orchestration latency
histogram_quantile(0.95,
  rate(duroxide_orchestration_duration_seconds_bucket[5m])
) by (orchestration_name)

# p99 activity latency
histogram_quantile(0.99,
  rate(duroxide_activity_duration_seconds_bucket[5m])
) by (activity_name)
```

### Error Rates

```promql
# Infrastructure errors (actionable)
rate(duroxide_orchestration_failures_total{error_type="infrastructure_error"}[5m])
by (orchestration_name)

# Configuration errors (deployment issues)
rate(duroxide_orchestration_failures_total{error_type="config_error"}[5m])
by (orchestration_name)
```

### Capacity Planning

```promql
# Total active orchestrations
duroxide_active_orchestrations

# Queue depths (for scaling decisions)
duroxide_orchestrator_queue_depth
duroxide_worker_queue_depth

# Alert if queues are backing up
(duroxide_orchestrator_queue_depth > 1000) or (duroxide_worker_queue_depth > 500)

# Orchestrations requiring many turns (optimization targets)
sum(duroxide_orchestration_completions_total{final_turn_count="50+"})
by (orchestration_name)
```

---

## Label Cardinality Guidelines

**Low Cardinality (Safe):**
- `orchestration_name` - 10-100 unique values
- `activity_name` - 50-500 unique values
- `status`, `outcome`, `error_type` - Fixed set (5-10 values)
- `retry_attempt` - Fixed set (0, 1, 2, 3+)

**High Cardinality (Avoid):**
- ❌ `instance_id` - Unique per orchestration (thousands+)
- ❌ `user_id` - Unique per user
- ❌ `timestamp` - Unique per event

**Best Practice:** Keep total time series under 10,000 for optimal Prometheus performance.

---

## Configuration

Metrics are enabled via `ObservabilityConfig`:

```rust
use duroxide::runtime::{RuntimeOptions, ObservabilityConfig};

let options = RuntimeOptions {
    observability: ObservabilityConfig {
        metrics_enabled: true,
        metrics_export_endpoint: Some("http://localhost:4317".to_string()),
        metrics_export_interval_ms: 10000,
        service_name: "my-duroxide-app".to_string(),
        ..Default::default()
    },
    ..Default::default()
};
```

---

## Test Coverage

All implemented metrics are validated in `tests/observability_tests.rs`. The test suite validates that metrics are recorded correctly but does **not** test full OpenTelemetry export with labels and histograms.

### What Is Tested

**✅ Validated (In-Memory Atomic Counters):**
- Metrics are recorded when events occur
- Error classification works correctly (app/config/infra)
- Gauges increment/decrement properly
- All metric code paths are exercised

**❌ Not Validated (Would Require Full OTel Export):**
- Label correctness (e.g., `orchestration_name="MyOrch"`)
- Histogram bucket distributions
- Multi-dimensional aggregations
- Prometheus naming conventions
- Actual metric values in exported format

### Test Details

| Test Function | Metrics Covered |
|--------------|-----------------|
| `activity_tracing_emits_all_levels` | Activity logging (not metrics) |
| `orchestration_tracing_emits_all_levels` | Orchestration logging (not metrics) |
| `metrics_capture_activity_and_orchestration_outcomes` | All orchestration/activity counters, error classification |
| `test_fetch_orchestration_item_fault_injection` | Infrastructure error handling |
| `test_labeled_metrics_recording` | Orchestration starts, completions, duration, history, turns |
| `test_continue_as_new_metrics` | Continue-as-new counter |
| `test_activity_duration_tracking` | Activity duration histograms |
| `test_error_classification_metrics` | Error type classification |
| `test_active_orchestrations_gauge` | Active orchestrations gauge lifecycle |
| `test_active_orchestrations_gauge_comprehensive` | Active gauge with multiple orchestrations |
| `test_separate_error_counters_exported` | Separate error counters for infra/config |
| `test_sub_orchestration_metrics` | Sub-orchestration calls and duration |
| `test_versioned_orchestration_metrics` | Version label tracking |
| `test_provider_metrics_recorded` | Provider operation duration |
| `test_provider_error_metrics` | Provider error classification |
| `test_queue_depth_gauges_initialization` | Queue depth gauges initialized from DB |
| `test_queue_depth_gauges_tracking` | Queue depth gauges track changes |
| `test_all_gauges_initialized_together` | All gauges (active + queues) work after restart |

### Running Tests

```bash
# Run all metrics tests
cargo test --features observability --test observability_tests

# Run with output
cargo test --features observability --test observability_tests -- --nocapture

# Run specific test
cargo test --features observability test_sub_orchestration_metrics -- --nocapture
```

### Test Architecture

**Approach:** Tests use `ManualReader` (in-memory) by setting `metrics_export_endpoint: None`. This captures metrics internally without requiring external infrastructure.

**Validation Method:** Tests read atomic counters via `runtime.metrics_snapshot()` to verify metrics were recorded. These atomic counters are maintained alongside the OpenTelemetry metrics.

**Limitation:** This validates that metric recording code paths work but doesn't test the full OTel pipeline (labels, histogram buckets, export format).

### Future: Full OTel Integration Tests

To validate labels and histogram buckets, a full OTel integration test would need to:

1. Export to a real OTel collector or use a test exporter
2. Call `meter_provider.force_flush()` to flush metrics
3. Read back exported data and verify:
   - Label values are correct
   - Histogram buckets match specification
   - Metric names follow Prometheus conventions

This is currently **not implemented** due to complexity and external dependencies.

---

## See Also

- [Observability Guide](observability-guide.md) - User-facing observability documentation
- [Duroxide Telemetry Spec](duroxide-telemetry-spec.md) - Desired state telemetry requirements
- [Active Orchestrations Metric Spec](duroxide-active-orchestrations-metric-spec.md) - Detailed gauge specification
- [Provider Observability](provider-observability.md) - Provider implementation guide

---

**Document Status:** This specification reflects the **current implementation** as of 2025-11-23. Client metrics are defined but not yet instrumented. All implemented metrics have test coverage in `tests/observability_tests.rs`.

