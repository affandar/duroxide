---
name: duroxide-code-coverage
description: Measure and improve code coverage in the Duroxide durable execution runtime. Use when asked about coverage, testing coverage, running llvm-cov, or improving test coverage percentages.
license: Apache-2.0
metadata:
  author: duroxide
  version: "1.0"
---

# Duroxide Code Coverage

Guidance for measuring and improving code coverage in the Duroxide codebase.

## Quick Commands

```bash
# Summary (recommended for quick checks)
cargo llvm-cov nextest -p duroxide --all-features --summary-only

# Detailed per-file report
cargo llvm-cov nextest -p duroxide --all-features

# HTML report (opens in browser)
cargo llvm-cov nextest -p duroxide --all-features --html --open

# Install llvm-cov if needed
cargo install cargo-llvm-cov
```

## Coverage Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Lines | >90% | Primary metric |
| Regions | >85% | Branch coverage |
| Functions | >80% | Often lower due to unused helpers |

## Priority Files

When coverage drops, focus on these files:

1. **src/providers/error.rs** - Target 100%, error classification
2. **src/runtime/observability.rs** - Target 90%, metrics correctness
3. **src/providers/sqlite.rs** - Target 85%, reference implementation
4. **src/providers/instrumented.rs** - Target 75%, observability wrapper
5. **src/providers/management.rs** - Note: default trait impls inflate gaps

## Adding Tests

| Gap Type | Location |
|----------|----------|
| Management API | `tests/coverage_improvement_tests.rs` |
| Provider validation | `tests/sqlite_provider_validations.rs` |
| Observability | `tests/observability_tests.rs` |
| SQLite edge cases | `src/providers/sqlite.rs` (inline tests) |

## Test Patterns

### Management API
```rust
#[tokio::test]
async fn test_management() {
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let mgmt = store.as_management_capability().unwrap();
    assert!(mgmt.list_instances().await.is_ok());
}
```

### Metrics
```rust
#[tokio::test]
async fn test_metrics() {
    use duroxide::runtime::observability::{MetricsProvider, ObservabilityConfig};
    let metrics = MetricsProvider::new(&ObservabilityConfig::default()).unwrap();
    metrics.record_orchestration_failure("Orch", "1.0", "app_error", "cat");
    assert_eq!(metrics.snapshot().orch_application_errors, 1);
}
```

### Provider Errors
```rust
#[tokio::test]
async fn test_error() {
    let err = ProviderError::retryable("op", "msg");
    assert!(err.is_retryable());
}
```

## Troubleshooting

- **"no such command: llvm-cov"** → `cargo install cargo-llvm-cov`
- **Coverage seems low** → Use `-p duroxide` (not `--workspace`)
- **Stale data** → `cargo llvm-cov clean --workspace`
