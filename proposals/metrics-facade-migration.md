# Proposal: Migrate to `metrics` Facade Crate

**Status:** Draft  
**Created:** 2025-12-20  
**Author:** duroxide team

## Summary

Replace direct OpenTelemetry metrics instrumentation with the `metrics` facade crate, allowing users to choose their own metrics backend (OTel, Prometheus, StatsD, or none).

## Motivation

### Current State

duroxide directly depends on OpenTelemetry for metrics:

```toml
# Current Cargo.toml (observability feature)
opentelemetry = { version = "0.27", features = ["metrics"] }
opentelemetry-otlp = { version = "0.27", features = ["metrics"] }
opentelemetry_sdk = { version = "0.27", features = ["rt-tokio", "metrics"] }
```

This creates several issues:

1. **Opinionated backend** - Users must use OTel even if they prefer Prometheus direct scraping
2. **Heavy dependencies** - ~3 OTel crates add compile time and binary size
3. **Complex testing** - We maintain 11 parallel `AtomicU64` counters just for test observability
4. **450+ LOC** - `observability.rs` is one of our largest files

### Proposed State

Use the `metrics` facade (what axum, tower, sqlx, reqwest do):

```toml
# Proposed Cargo.toml
metrics = "0.24"
```

Users install their preferred backend:

```rust
// User's application - their choice!
metrics_exporter_opentelemetry::Recorder::builder("app").install_global()?;
// OR
metrics_exporter_prometheus::PrometheusBuilder::new().install()?;
// OR nothing - metrics become zero-cost no-ops
```

## Design

### Architecture Comparison

**Before (current):**

```
duroxide runtime
    │
    ├── MetricsProvider (owns OTel instruments)
    │       ├── 30+ Counter/Histogram/Gauge fields
    │       ├── 11 AtomicU64 for test snapshots
    │       └── ~450 LOC
    │
    └── ObservabilityHandle
            └── OTLP Exporter (hardcoded)
```

**After (proposed):**

```
duroxide runtime
    │
    └── metrics! macro calls
            │
            └── Global Recorder (user-installed, or no-op)
                    │
                    ├── OTel exporter (user's choice)
                    ├── Prometheus exporter (user's choice)
                    ├── DebuggingRecorder (tests)
                    └── None (zero-cost no-ops)
```

### Code Changes

#### Metrics Emission (simplified)

```rust
// Before: 20 lines per metric type
impl MetricsProvider {
    pub fn record_orchestration_completion(
        &self,
        orchestration_name: &str,
        version: &str,
        status: &str,
        duration_seconds: f64,
        turn_count: u64,
        history_events: u64,
    ) {
        self.orch_completions_total.add(
            1,
            &[
                KeyValue::new("orchestration_name", orchestration_name.to_string()),
                KeyValue::new("version", version.to_string()),
                KeyValue::new("status", status.to_string()),
            ],
        );
        self.orch_duration_seconds.record(duration_seconds, &[...]);
        self.orch_completions_atomic.fetch_add(1, Ordering::Relaxed);
    }
}

// After: 5 lines
pub fn record_orchestration_completion(name: &str, version: &str, status: &str, duration: f64) {
    counter!("duroxide_orch_completions_total", 
        "orchestration_name" => name.to_string(),
        "version" => version.to_string(),
        "status" => status.to_string()
    ).increment(1);
    
    histogram!("duroxide_orch_duration_seconds",
        "orchestration_name" => name.to_string()
    ).record(duration);
}
```

#### Test Assertions (simplified)

```rust
// Before: Custom MetricsSnapshot + atomic counters
let snapshot = rt.metrics_snapshot().expect("metrics available");
assert_eq!(snapshot.orch_completions, 1);
assert_eq!(snapshot.activity_success, 1);

// After: Use DebuggingRecorder
use metrics_util::debugging::{DebuggingRecorder, Snapshotter};

let recorder = DebuggingRecorder::new();
let snapshotter = recorder.snapshotter();
metrics::set_global_recorder(recorder).unwrap();

// ... run test ...

let snapshot = snapshotter.snapshot();
let counter = snapshot.into_hashmap()
    .get(&CompositeKey::new("duroxide_orch_completions_total", labels))
    .unwrap();
assert_eq!(counter.value(), 1);
```

#### Observable Gauges → Polled Gauges

```rust
// Before: OTel callback (value captured at scrape time)
meter.i64_observable_gauge("duroxide_queue_depth")
    .with_callback(move |observer| {
        observer.observe(atomic.load(Relaxed), &[]);
    })
    .build();

// After: Background task polls and sets
async fn queue_depth_reporter(store: Arc<dyn Provider>) {
    loop {
        if let Ok(depths) = store.get_queue_depths().await {
            gauge!("duroxide_orchestrator_queue_depth").set(depths.orch as f64);
            gauge!("duroxide_worker_queue_depth").set(depths.worker as f64);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
```

### API Changes

#### Removed

- `ObservabilityConfig.metrics_export_endpoint` - user configures their exporter
- `ObservabilityConfig.metrics_export_interval_ms` - user configures their exporter
- `ObservabilityConfig.metrics_enabled` - metrics are always available (no-op if no recorder)
- `runtime.metrics_snapshot()` - tests use `DebuggingRecorder` instead
- `MetricsSnapshot` struct - removed
- `observability` feature flag for metrics - could simplify or remove

#### Retained

- `ObservabilityConfig` - still controls logging (tracing)
- `ObservabilityHandle` - still manages logging lifecycle
- All metric names and labels - unchanged semantics

### Dependencies

```toml
# Remove
opentelemetry = { version = "0.27", features = ["metrics"], optional = true }
opentelemetry-otlp = { version = "0.27", features = ["metrics"], optional = true }
opentelemetry_sdk = { version = "0.27", features = ["rt-tokio", "metrics"], optional = true }

# Add
metrics = "0.24"

# Dev dependencies (for tests)
metrics-util = "0.19"  # DebuggingRecorder
```

## Migration Guide

### For Library Users

**Before:**
```rust
let config = ObservabilityConfig {
    metrics_enabled: true,
    metrics_export_endpoint: Some("http://localhost:4317".to_string()),
    ..Default::default()
};
let rt = Runtime::start_with_options(store, activities, orchestrations, 
    RuntimeOptions::default().with_observability(config)
).await;
```

**After:**
```rust
// Install your preferred metrics backend BEFORE starting runtime
metrics_exporter_opentelemetry::Recorder::builder("my-service")
    .with_exporter(opentelemetry_otlp::MetricExporter::builder().build()?)
    .install_global()?;

// Or for Prometheus:
// metrics_exporter_prometheus::PrometheusBuilder::new()
//     .with_http_listener(([0, 0, 0, 0], 9090))
//     .install()?;

let rt = Runtime::start_with_store(store, activities, orchestrations).await;
```

### For Test Code

**Before:**
```rust
let snapshot = rt.metrics_snapshot().expect("metrics available");
assert_eq!(snapshot.orch_completions, 1);
```

**After:**
```rust
use metrics_util::debugging::DebuggingRecorder;

// In test setup
let recorder = DebuggingRecorder::new();
let snapshotter = recorder.snapshotter();
metrics::set_global_recorder(recorder).unwrap();

// After test
let snapshot = snapshotter.snapshot();
// Use snapshot.into_hashmap() to inspect values
```

## Tradeoffs

### Gains

| Benefit | Impact |
|---------|--------|
| Reduced dependencies | -3 crates (~compile time, binary size) |
| Reduced LOC | ~400 lines removed from observability.rs |
| User backend choice | OTel, Prometheus, StatsD, or none |
| Better testing | DebuggingRecorder replaces 11 atomics |
| Idiomatic | Matches axum, tower, sqlx patterns |
| Zero-cost when unused | No recorder = no overhead |

### Losses

| Tradeoff | Mitigation |
|----------|------------|
| Observable gauges | Use polled gauges (5s interval) |
| `metrics-exporter-opentelemetry` limitations | Document unsupported features |
| Breaking API change | Major version bump, migration guide |

### Limitations of `metrics-exporter-opentelemetry`

The OTel bridge has gaps (from [docs](https://docs.rs/metrics-exporter-opentelemetry)):

- ❌ `Counter::absolute()` - not supported
- ❌ `Gauge::increment/decrement()` - not supported
- ❌ `Histogram::record_many()` - not supported
- ❌ `describe_*()` - no-op

duroxide doesn't use these features, so this is acceptable.

## Implementation Plan

### Phase 1: Add `metrics` facade (non-breaking)

1. Add `metrics` dependency
2. Create new `metrics_facade.rs` module with metric emission functions
3. Call both old OTel and new facade in parallel
4. Add `DebuggingRecorder` test utilities

### Phase 2: Migrate tests

1. Update `observability_tests.rs` to use `DebuggingRecorder`
2. Remove `metrics_snapshot()` usage from tests
3. Verify all tests pass

### Phase 3: Remove OTel metrics (breaking)

1. Remove `MetricsProvider` struct
2. Remove atomic counter fields
3. Remove OTel metrics dependencies (keep tracing deps if needed)
4. Update `ObservabilityConfig` to remove metrics fields
5. Update documentation and examples
6. Major version bump

### Phase 4: Documentation

1. Update `docs/observability-guide.md`
2. Update `docs/metrics-specification.md`
3. Add examples for Prometheus and OTel backends
4. Update `examples/with_observability.rs`

## Open Questions

1. **Keep `observability` feature flag?** The `metrics` crate is lightweight; could always include it.

2. **Tracing integration?** Keep direct `tracing` or also facade via `tracing-subscriber`?

3. **Queue depth polling interval?** 5 seconds? Configurable?

4. **Test helper utilities?** Provide `duroxide::testing::install_test_metrics()` helper?

## References

- [`metrics` crate](https://crates.io/crates/metrics)
- [`metrics-exporter-opentelemetry`](https://crates.io/crates/metrics-exporter-opentelemetry)
- [`metrics-exporter-prometheus`](https://crates.io/crates/metrics-exporter-prometheus)
- [axum metrics example](https://github.com/tokio-rs/axum/tree/main/examples/prometheus-metrics)
- Current implementation: `src/runtime/observability.rs`
