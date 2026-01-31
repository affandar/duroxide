# Duroxide Code Coverage Skill

Guidance for measuring and improving code coverage in the Duroxide codebase.

## Quick Reference

### Running Coverage

```bash
# Main crate only (recommended for product coverage analysis)
cargo llvm-cov nextest -p duroxide --all-features --summary-only

# Full workspace (includes sqlite-stress binaries, skews results)
cargo llvm-cov nextest --workspace --all-features --summary-only

# Detailed report with file breakdown
cargo llvm-cov nextest -p duroxide --all-features

# HTML report (opens in browser)
cargo llvm-cov nextest -p duroxide --all-features --html --open
```

### Prerequisites

```bash
# Install cargo-llvm-cov if not present
cargo install cargo-llvm-cov

# Verify nextest is available (required by project)
cargo nt --version
```

## Coverage Targets

### Current Baseline (as of coverage improvement work)

| Metric | Target | Current |
|--------|--------|---------|
| Lines | >90% | 91.88% |
| Regions | >85% | 87.92% |
| Functions | >80% | 81.09% |

### Priority Files to Monitor

These files historically have lower coverage and should be watched:

| File | Risk Level | Notes |
|------|------------|-------|
| `src/providers/management.rs` | Low | Default trait impls; SQLite provides real coverage |
| `src/provider_validation/long_polling.rs` | Medium | Only testable with long-poll providers |
| `src/providers/mod.rs` | Low | Trait definitions and glue code |
| `src/providers/instrumented.rs` | Medium | Observability wrapper; error paths matter |
| `src/providers/sqlite.rs` | High | Reference implementation; edge cases important |
| `src/runtime/observability.rs` | Medium | Metrics correctness; ops confidence |

## Interpreting Coverage Reports

### What to Look For

1. **Uncovered Error Branches**
   - Provider error handling paths
   - Timeout and cancellation paths
   - Lock expiration scenarios

2. **Uncovered Edge Cases**
   - Multi-execution (ContinueAsNew) scenarios
   - Concurrent access patterns
   - Poison message handling

3. **Metrics/Observability Gaps**
   - All error classification types should be exercised
   - Queue depth tracking
   - Active orchestration gauges

### Coverage vs. Quality

Coverage is a **signal, not a goal**. Focus on:

1. **Risk-based coverage**: Critical paths > convenience methods
2. **Error path coverage**: Failures are harder to test but important
3. **Integration coverage**: Provider contracts > unit internals

## Adding Coverage Tests

### Where to Add Tests

| Coverage Gap | Test Location |
|--------------|---------------|
| Management API | `tests/coverage_improvement_tests.rs` or `tests/management_interface_test.rs` |
| Provider validation | `src/provider_validation/*.rs` + `tests/sqlite_provider_validations.rs` |
| Observability | `tests/coverage_improvement_tests.rs` or `tests/observability_tests.rs` |
| SQLite edge cases | `src/providers/sqlite.rs` (inline tests) or `tests/sqlite_provider_validations.rs` |

### Test Patterns

**Management API smoke test:**
```rust
#[tokio::test]
async fn test_management_method_coverage() {
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let mgmt = store.as_management_capability().unwrap();
    
    // Exercise the method
    let result = mgmt.list_instances().await;
    assert!(result.is_ok());
}
```

**Observability metrics test:**
```rust
#[tokio::test]
async fn test_metrics_outcome() {
    use duroxide::runtime::observability::{MetricsProvider, ObservabilityConfig};
    
    let config = ObservabilityConfig::default();
    let metrics = MetricsProvider::new(&config).unwrap();
    
    // Record metric
    metrics.record_orchestration_failure("Orch", "1.0", "app_error", "user_error");
    
    // Verify via snapshot
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.orch_application_errors, 1);
}
```

**Provider error path test:**
```rust
#[tokio::test]
async fn test_provider_error_path() {
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    
    // Trigger error condition
    let result = store.ack_orchestration_item(
        "invalid-token",  // Wrong token
        1,
        vec![],
        vec![],
        vec![],
        ExecutionMetadata::default(),
        vec![],
    ).await;
    
    assert!(result.is_err());
}
```

### Running Specific Tests

```bash
# Run tests matching pattern
cargo nt -E 'test(/management/)'
cargo nt -E 'test(/coverage/)'
cargo nt -E 'test(/observability/)'

# Run specific test file
cargo nt --test coverage_improvement_tests
cargo nt --test management_interface_test
```

## Common Coverage Gaps and Fixes

### Gap: ProviderError variants not exercised

**Fix:** Add tests in `tests/coverage_improvement_tests.rs`:
```rust
#[tokio::test]
async fn test_provider_error_variants() {
    let retryable = ProviderError::retryable("op", "msg");
    assert!(retryable.is_retryable());
    
    let permanent = ProviderError::permanent("op", "msg");
    assert!(!permanent.is_retryable());
}
```

### Gap: Metrics error classification paths

**Fix:** Exercise all error types:
```rust
metrics.record_orchestration_failure("Orch", "1.0", "app_error", "category");
metrics.record_orchestration_failure("Orch", "1.0", "infrastructure_error", "category");
metrics.record_orchestration_failure("Orch", "1.0", "config_error", "category");
```

### Gap: InstrumentedProvider wrapper

**Fix:** Wrap provider and run operations:
```rust
let base = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
let instrumented = Arc::new(InstrumentedProvider::new(base, Some(metrics)));
// Run operations through instrumented provider
```

### Gap: Long-poll provider tests

**Note:** SQLite is a short-poll provider. Long-poll tests require either:
1. A real long-poll provider (future work)
2. The `LongPollingSqliteProvider` wrapper in `tests/long_poll_tests.rs`

## CI/CD Integration

### GitHub Actions Example

```yaml
- name: Install coverage tools
  run: cargo install cargo-llvm-cov

- name: Run coverage
  run: cargo llvm-cov nextest -p duroxide --all-features --codecov --output-path codecov.json

- name: Upload to Codecov
  uses: codecov/codecov-action@v3
  with:
    files: codecov.json
```

### Coverage Thresholds

Consider failing CI if coverage drops below thresholds:

```bash
# Check coverage meets threshold (example script)
COVERAGE=$(cargo llvm-cov nextest -p duroxide --all-features --summary-only 2>&1 | grep "TOTAL" | awk '{print $10}' | tr -d '%')
if (( $(echo "$COVERAGE < 90" | bc -l) )); then
    echo "Coverage $COVERAGE% is below 90% threshold"
    exit 1
fi
```

## Troubleshooting

### "no such command: llvm-cov"

```bash
cargo install cargo-llvm-cov
```

### Coverage appears lower than expected

- Check if you're running workspace vs crate-only coverage
- Workspace includes `sqlite-stress` binaries which have 0% coverage
- Use `-p duroxide` for accurate product coverage

### Tests pass but coverage doesn't update

```bash
# Clean coverage data and rebuild
cargo llvm-cov clean --workspace
cargo llvm-cov nextest -p duroxide --all-features --summary-only
```

### Slow coverage runs

Coverage instrumentation adds overhead. Tips:
- Use `--summary-only` for quick checks
- Run full coverage less frequently (e.g., before PR merge)
- Use nextest's parallel execution (already configured)
