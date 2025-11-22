# Toygres Feedback Fixes - 2025-11-22

## Issues Reported by Toygres (Real Production User)

Toygres is a PostgreSQL-as-a-Service orchestrator built on duroxide. When they enabled metrics with `--features observability` and tried to create Grafana dashboards, they found several bugs.

---

## Issue 1: Histogram Boundaries Wrong Type ✅ FIXED

**Commit**: `0077f60`

**Problem**:
```rust
// In src/runtime/observability.rs, lines 273 and 279
.with_boundaries(vec![10, 50, 100, 500, 1000, 5000, 10000])  // ❌ integers
.with_boundaries(vec![1, 2, 5, 10, 20, 50, 100, 200, 500])   // ❌ integers
```

**Error**:
```
error: Histogram boundaries need to be f64 but integers are passed
```

**Fix**:
```rust
.with_boundaries(vec![10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0])  // ✅ f64
.with_boundaries(vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0])  // ✅ f64
```

**Root Cause**: OpenTelemetry `with_boundaries()` expects `Vec<f64>`, not `Vec<i32>`.

---

## Issue 2: Missing Field Initialization ✅ FIXED

**Commit**: `5acd465` (in progress)

**Problem**:
```rust
// Real implementation (with --features observability)
pub struct MetricsProvider {
    active_orchestrations_atomic: Arc<AtomicI64>,  // ✅ Field defined
}

impl MetricsProvider {
    pub fn new(...) -> Result<Self, String> {
        Ok(Self {
            // ... 20 other fields ...
            // active_orchestrations_atomic: MISSING!  // ❌ Not initialized
        })
    }
}
```

**Error**: Compiler error when building with `--features observability`

**Fix**:
```rust
Ok(Self {
    // ... all fields ...
    active_orchestrations_atomic: active_orch_atomic_clone,  // ✅ Added
})
```

**Why We Missed It**: Tests run WITHOUT `--features observability` by default, so they tested the stub implementation which was correct.

---

## Issue 3: Active Orchestrations Gauge Not Exported ✅ FIXED

**Problem**:
```rust
// Only had atomic counter for tests, not actual OpenTelemetry gauge
active_orchestrations_atomic: Arc<AtomicI64>  // ← Not exported to Prometheus!
```

**Fix**:
```rust
// Added observable gauge that reads from atomic and exports to Prometheus
let active_orch_atomic_clone = Arc::new(AtomicI64::new(0));
let active_orch_for_callback = active_orch_atomic_clone.clone();

meter
    .i64_observable_gauge("duroxide_active_orchestrations")
    .with_description("Currently active orchestration instances")
    .with_callback(move |observer| {
        let count = active_orch_for_callback.load(Ordering::Relaxed);
        observer.observe(count, &[
            KeyValue::new("state", "all")
        ]);
    })
    .build();
```

**Result**: Gauge now exports to Prometheus and can be queried in Grafana!

---

## Issue 4: Orchestration Names Are "unknown" ❌ IN PROGRESS

**Problem**:
```json
// Activity metrics: ✅ Proper labels
{
  "activity_name": "toygres-activities::activity::test-connection",
  "outcome": "success"
}

// Orchestration metrics: ❌ Labels are "unknown"
{
  "orchestration_name": "unknown",
  "error_type": "app_error"
}
```

**Root Cause**:
```rust
// Line 178 in orchestration dispatcher
let orch_name = metadata.orchestration_name.as_deref().unwrap_or("unknown");
```

The `metadata` is computed from `history_delta` which extracts the orchestration name from `OrchestrationStarted` event. If that event isn't in the delta (e.g., during a failure after completion messages), it falls back to "unknown".

**Fix**:
```rust
// Use orchestration name from work item, not metadata
let orch_name = if workitem_reader.has_orchestration_name() {
    workitem_reader.orchestration_name.as_str()  // ✅ Always correct
} else {
    metadata.orchestration_name.as_deref().unwrap_or("unknown")
};
```

**Status**: ✅ Fixed in current working tree

---

## Issue 5: Testing Both Implementations

**Problem**:
```bash
# What we ran
cargo test  # Tests stub implementation only

# What toygres runs  
cargo build --features observability  # Uses real implementation (different code!)
```

**Fix**: Updated cargo aliases
```toml
# Before
t = "test --features test"

# After
t = "test --all-features"  # ✅ Tests both stub AND real OpenTelemetry
```

**Result**: Running `cargo t` now tests the code that users actually run!

---

## Summary of Fixes

| Issue | Status | Commit | Impact |
|-------|--------|--------|---------|
| Histogram boundaries type | ✅ Fixed | 0077f60 | Compilation error |
| Missing field initialization | ✅ Fixed | In progress | Compilation error |
| Observable gauge not exported | ✅ Fixed | In progress | Metrics don't appear |
| Orchestration names "unknown" | ✅ Fixed | In progress | Wrong labels in Prometheus |
| Testing wrong implementation | ✅ Fixed | In progress | Bugs not caught |

---

## Files Modified

```
M  .cargo/config.toml                      (test all-features)
M  src/runtime/observability.rs            (observable gauge + field init)
M  src/runtime/dispatchers/orchestration.rs (use workitem name not metadata)
M  docs/observability-guide.md             (turn count queries)
A  docs/METRICS-SYSTEM-DESIGN.md           (architecture explanation)
A  TEST-COMPARISON.md                      (testing strategy)
```

---

## Testing Strategy Going Forward

### Before (Broken)
```bash
cargo test  # Only tests stub, misses bugs in real code
```

### After (Fixed)
```bash
cargo t  # Alias for: cargo test --all-features
         # Tests: stub + OpenTelemetry + provider-test
         # Catches bugs in BOTH implementations!
```

---

## Lessons Learned

1. **Feature flags create multiple implementations** - Must test ALL of them
2. **Runtime config != Compile-time features** - `metrics_enabled: true` means nothing without feature flag
3. **Real users enable features** - We must test what they compile
4. **Metadata extraction can fail** - Use source-of-truth data from work items
5. **Observable gauges need callbacks** - Can't just increment/decrement, need to register observer

---

## Thank You Toygres! 🎉

These bugs would have been silent failures in production:
- Metrics with wrong labels → Dashboards show "unknown" → No useful data
- Missing gauge → Can't track active orchestrations → No capacity planning
- Wrong histogram types → Won't compile → Deployment blocked

Caught during development, not in production. This is why dogfooding matters! 🐶

---

**Status**: All fixes applied, ready for commit pending user approval.

