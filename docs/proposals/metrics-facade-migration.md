# Proposal: Migrate to `metrics` Facade Crate

**Status:** Draft  
**Created:** 2025-12-20  
**Author:** duroxide team

## Summary

Replace direct OpenTelemetry metrics instrumentation with the `metrics` facade crate, allowing users to choose their own metrics backend (OTel, Prometheus, StatsD, or none).

This migration makes OpenTelemetry a *consumer choice* for metrics export rather than a duroxide dependency.

Follow-up (separate, optional) work: remove OpenTelemetry entirely from duroxide (including tracing/log export) and rely on standard Rust observability libraries (`tracing`, `tracing-subscriber`, and the `metrics` facade). When users want OTLP/collector export, they can add it in their application via `tracing-opentelemetry` / `opentelemetry-*` without duroxide carrying those dependencies.

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

// After: small, allocation-free label recording
// Note: Metric names/labels should match `docs/metrics-specification.md`.
pub fn record_orchestration_completion(
    orchestration_name: &str,
    version: &str,
    status: &str,
    final_turn_count: &str,
    duration_seconds: f64,
) {
    counter!(
        "duroxide_orchestration_completions_total",
        "orchestration_name" => orchestration_name,
        "version" => version,
        "status" => status,
        "final_turn_count" => final_turn_count,
    )
    .increment(1);

    histogram!(
        "duroxide_orchestration_duration_seconds",
        "orchestration_name" => orchestration_name,
        "version" => version,
        "status" => status,
    )
    .record(duration_seconds);
}
```

#### Test Assertions (simplified)

```rust
// Before: Custom MetricsSnapshot + atomic counters
let snapshot = rt.metrics_snapshot().expect("metrics available");
assert_eq!(snapshot.orch_completions, 1);
assert_eq!(snapshot.activity_success, 1);

// After: Install DebuggingRecorder once per test *process*, then assert on deltas.
// `metrics::set_global_recorder(...)` can only be called once.
use std::sync::OnceLock;
use metrics_util::debugging::{DebuggingRecorder, Snapshotter};

static SNAPSHOTTER: OnceLock<Snapshotter> = OnceLock::new();

fn snapshotter() -> &'static Snapshotter {
    SNAPSHOTTER.get_or_init(|| {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::set_global_recorder(recorder)
            .expect("global metrics recorder already installed");
        snapshotter
    })
}

let before = snapshotter().snapshot();

// ... run test ...

let after = snapshotter().snapshot();
// Assert on (after - before) rather than absolute values.
```

#### Gauges: Transition-set vs Polled

OpenTelemetry's observable gauges (callbacks at scrape-time) don't have a direct equivalent in the `metrics` facade.
For duroxide, split gauges into two categories:

1) **Stateful gauges** (set when state changes)
2) **Polled gauges** (best-effort background polling)

```rust
// Before: OTel callback (value captured at scrape time)
meter.i64_observable_gauge("duroxide_queue_depth")
    .with_callback(move |observer| {
        observer.observe(atomic.load(Relaxed), &[]);
    })
    .build();

// After (stateful gauge): update on transitions (no background task)
fn set_active_orchestrations(active: i64) {
    gauge!("duroxide_active_orchestrations").set(active as f64);
}

// After (polled gauges): background task polls provider and sets queue depths
// - Default poll interval: 30s
// - Lenient: errors are ignored (optionally logged at debug)
// - Disabled when running on a single-threaded Tokio runtime to avoid background churn
async fn queue_depth_reporter(store: Arc<dyn Provider>, poll_interval: Duration) {
    loop {
        match store.get_queue_depths().await {
            Ok(depths) => {
                gauge!("duroxide_orchestrator_queue_depth").set(depths.orch as f64);
                gauge!("duroxide_worker_queue_depth").set(depths.worker as f64);
            }
            Err(_err) => {
                // Best-effort: queue depth is auxiliary and should not impact runtime correctness.
            }
        }
        tokio::time::sleep(poll_interval).await;
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

### OpenTelemetry Dependency

With the facade approach, duroxide does **not** need OpenTelemetry *for metrics*.
OpenTelemetry becomes one possible **consumer-side exporter** (via `metrics-exporter-opentelemetry`) rather than a duroxide dependency.

If duroxide also removes OpenTelemetry for tracing/logging, the runtime can still provide structured logs and spans via `tracing` + `tracing-subscriber`. Users that want OTLP/collector export can opt into it in their application (e.g., with `tracing-opentelemetry` + `opentelemetry-otlp`).

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
use std::sync::OnceLock;
use metrics_util::debugging::{DebuggingRecorder, Snapshotter};

static SNAPSHOTTER: OnceLock<Snapshotter> = OnceLock::new();

fn snapshotter() -> &'static Snapshotter {
    SNAPSHOTTER.get_or_init(|| {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::set_global_recorder(recorder)
            .expect("global metrics recorder already installed");
        snapshotter
    })
}

let before = snapshotter().snapshot();

// ... run test ...

let after = snapshotter().snapshot();
// Assert on (after - before) deltas.
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
| Callback-based observable gauges | Use transition-set gauges for in-process state; use best-effort polling for provider-derived gauges (30s default) |
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

Optional (in this phase): Add a small internal helper to start pollers for provider-derived gauges:
- Default poll interval: 30 seconds
- Best-effort error handling
- Disabled when running on a single-threaded Tokio runtime

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

### Phase 5 (Optional): Remove OpenTelemetry entirely from duroxide

Goal: duroxide ships *zero* OpenTelemetry dependencies. The library remains fully observable via standard Rust crates:
- Tracing/logging: `tracing`, `tracing-subscriber`
- Metrics: `metrics` facade (exporters installed by the user if desired)

Planned steps:

1. **Cargo cleanup**
    - Remove `opentelemetry`, `opentelemetry_sdk`, and `opentelemetry-otlp` dependencies.
    - Remove (or repurpose) the `observability` feature flag; if it remains, it should gate only `tracing-subscriber` formatting choices (not OTLP export).

2. **Public API cleanup (breaking)**
    - Remove `ObservabilityConfig` fields that exist purely for OTLP/collector export (e.g., `*_export_endpoint`, export interval knobs).
    - Keep only configuration relevant to log formatting and filtering (e.g., `log_format`, `log_level`, and a stable set of correlation fields).
    - Ensure `RuntimeOptions` still has a straightforward “enable structured logging” story with no exporter dependencies.

3. **Runtime implementation changes**
    - Replace OTLP/OTel initialization with plain `tracing-subscriber` setup.
    - Preserve existing correlation fields and log format options.
    - Do not attempt to install global subscriber/formatter in a way that breaks embedding; provide a documented approach and make initialization idempotent.

4. **Docs/examples update**
    - Remove claims like “OpenTelemetry metrics/logging built-in”.
    - Document two recommended production patterns:
      1) stdout JSON logs + external collection (Loki/Elastic/CloudWatch)
      2) (Optional, consumer-side) OTLP export via `tracing-opentelemetry` in the application.

5. **Tests update**
    - Ensure tests validate structured logging configuration does not require OTLP/OTel.
    - Ensure metrics tests rely on the `metrics` facade test recorder pattern (install-once + delta assertions).

6. **Versioning**
    - Treat this as a breaking change (major version bump) since it removes public config fields and changes the built-in exporter story.

## Open Questions

1. **Always include `metrics`?** Yes.
    - Pros: avoids feature-matrix complexity; keeps metric callsites unconditional; metrics become a stable part of the public behavior.
    - Cons: adds a small dependency and a tiny no-op fast-path cost even if the user never installs a recorder.

2. **Tracing/logging integration?** Keep direct `tracing` + `tracing-subscriber` (no facade needed).

3. **Queue depth polling interval?** Default to 30 seconds.

4. **Single-thread behavior?** Disable pollers when running on a single-threaded Tokio runtime (rather than keying off 1x1 concurrency settings).

5. **Test helper utilities?** Yes: provide `duroxide::testing::install_test_metrics()` (install-once + snapshot + delta helpers).

## References

- [`metrics` crate](https://crates.io/crates/metrics)
- [`metrics-exporter-opentelemetry`](https://crates.io/crates/metrics-exporter-opentelemetry)
- [`metrics-exporter-prometheus`](https://crates.io/crates/metrics-exporter-prometheus)
- [axum metrics example](https://github.com/tokio-rs/axum/tree/main/examples/prometheus-metrics)
- Current implementation: `src/runtime/observability.rs`
