# Add Test Coverage to Duroxide

## Objective
Add comprehensive test coverage for existing or new features following duroxide testing patterns.

## Test Types and Organization

### 1. Unit Tests (`tests/unit_tests.rs`)
**Purpose**: Test individual functions/components in isolation

**Pattern:**
```rust
#[tokio::test]
async fn test_specific_behavior() {
    // Minimal setup
    let result = function_under_test(input);
    
    // Precise assertion
    assert_eq!(result, expected);
}
```

**When to use:**
- Testing pure logic functions
- Testing data structure behavior
- Testing determinism helpers
- Quick feedback, no runtime needed

### 2. E2E Tests (`tests/e2e_samples.rs`)
**Purpose**: Test real-world orchestration patterns with full runtime

**Pattern:**
```rust
#[tokio::test]
async fn sample_realistic_workflow() {
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    
    let activities = ActivityRegistry::builder()
        .register("DoWork", |ctx: ActivityContext, input: String| async move {
            Ok(format!("done: {}", input))
        })
        .build();
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("Workflow", |ctx: OrchestrationContext, input: String| async move {
            ctx.schedule_activity("DoWork", input).into_activity().await
        })
        .build();
    
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    let client = Client::new(store.clone());
    
    client.start_orchestration("test-instance", "Workflow", "input").await.unwrap();
    
    match client.wait_for_orchestration("test-instance", Duration::from_secs(5)).await.unwrap() {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "done: input"),
        OrchestrationStatus::Failed { details } => panic!("Failed: {}", details.display_message()),
        _ => panic!("Unexpected status"),
    }
    
    rt.shutdown(None).await;
}
```

**When to use:**
- Testing full workflows
- Testing runtime behavior
- Testing realistic scenarios
- Demonstrating usage patterns

### 3. Provider Tests (`tests/sqlite_tests.rs`, `tests/provider_correctness/`)
**Purpose**: Test provider implementation correctness

**Pattern:**
```rust
#[tokio::test]
async fn test_provider_specific_behavior() {
    let provider = SqliteProvider::new_in_memory().await.unwrap();
    
    // Test provider operations directly
    provider.enqueue_orchestrator_work(work_item, None).await.unwrap();
    let fetched = provider.fetch_orchestration_item().await.unwrap();
    
    assert_eq!(fetched.instance, "expected-instance");
}
```

**When to use:**
- Testing provider atomicity
- Testing queue semantics
- Testing lock expiration
- Testing multi-execution support

### 4. Determinism Tests (`tests/determinism_tests.rs`, `tests/nondeterminism_tests.rs`)
**Purpose**: Ensure replay correctness

**Pattern:**
```rust
#[tokio::test]
async fn test_deterministic_replay() {
    // Run orchestration twice with same input
    let result1 = run_orchestration(input.clone());
    let result2 = run_orchestration(input.clone());
    
    // Results and event history should be identical
    assert_eq!(result1.history, result2.history);
    assert_eq!(result1.output, result2.output);
}
```

**When to use:**
- Testing system call determinism
- Testing scheduler ordering
- Testing replay correctness

### 5. Observability Tests (`tests/observability_tests.rs`)
**Purpose**: Test metrics and logging behavior

**Pattern:**
```rust
#[tokio::test(flavor = "current_thread")]
async fn test_metrics_emitted() {
    let options = RuntimeOptions {
        observability: ObservabilityConfig {
            metrics_enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };
    
    let rt = runtime::Runtime::start_with_options(store, activities, orchestrations, options).await;
    
    // Trigger behavior
    // ...
    
    let snapshot = rt.metrics_snapshot().expect("metrics enabled");
    assert_eq!(snapshot.orch_completions, 1);
    
    rt.shutdown(None).await;
}
```

**When to use:**
- Testing metrics are recorded
- Testing log correlation fields
- Testing trace levels

### 6. Specialized Test Suites
- **`tests/recovery_tests.rs`** - Crash recovery, restart scenarios
- **`tests/cancellation_tests.rs`** - Orchestration cancellation
- **`tests/concurrency_tests.rs`** - Concurrent execution
- **`tests/errors_tests.rs`** - Error classification and handling
- **`tests/continue_as_new_tests.rs`** - Multi-execution scenarios

## Test Naming Conventions

### Good Test Names
```rust
test_activity_completes_successfully
test_orchestration_fails_with_unregistered_activity
test_continue_as_new_creates_new_execution
test_external_event_timeout_with_select2
test_provider_atomicity_on_ack_failure
```

### Poor Test Names
```rust
test_basic  // What basic behavior?
test_1      // Meaningless
test_bug    // Which bug?
test_works  // What works?
```

**Pattern**: `test_{component}_{scenario}_{expected_outcome}`

## Assertion Best Practices

### Descriptive Assertions
```rust
// ✅ Good: Custom message explains what's expected
assert!(
    matches!(status, OrchestrationStatus::Running),
    "expected Running after scheduling timer, got {:?}",
    status
);

// ❌ Bad: No context on failure
assert!(matches!(status, OrchestrationStatus::Running));
```

### Comprehensive Checks
```rust
// ✅ Good: Check all relevant fields
match result {
    OrchestrationStatus::Completed { output } => {
        assert_eq!(output, expected_output);
        let history = client.read_execution_history(instance, 1).await.unwrap();
        assert_eq!(history.len(), 4, "expected 4 events in history");
    }
    other => panic!("expected Completed, got {:?}", other),
}

// ❌ Bad: Only checking one thing
assert!(matches!(result, OrchestrationStatus::Completed { .. }));
```

## Test Coverage Goals

### For New Features
- **Happy path**: Feature works as intended
- **Error cases**: Feature fails gracefully
- **Edge cases**: Boundary conditions, empty inputs, etc.
- **Integration**: Feature works with existing features
- **Determinism**: Feature maintains replay correctness (if applicable)

### Coverage Matrix Example

For a new `ctx.schedule_batch_activities()` feature:

| Test | Scenario | File |
|------|----------|------|
| `test_batch_activities_all_succeed` | Happy path | `e2e_samples.rs` |
| `test_batch_activities_one_fails` | Partial failure | `e2e_samples.rs` |
| `test_batch_activities_empty_list` | Edge case | `unit_tests.rs` |
| `test_batch_activities_replay` | Determinism | `determinism_tests.rs` |
| `test_batch_activities_metrics` | Observability | `observability_tests.rs` |

## Common Test Patterns

### Pattern: Testing Activity Errors
```rust
#[tokio::test]
async fn test_activity_application_error() {
    let activities = ActivityRegistry::builder()
        .register("FailingActivity", |ctx: ActivityContext, _: String| async move {
            ctx.trace_error("Simulated failure");
            Err("application error".to_string())
        })
        .build();
    
    // ... orchestration that calls FailingActivity ...
    
    match client.wait_for_orchestration(instance, timeout).await.unwrap() {
        OrchestrationStatus::Failed { details } => {
            assert_eq!(details.category(), "application");
            assert!(details.display_message().contains("application error"));
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}
```

### Pattern: Testing Timeouts
```rust
#[tokio::test]
async fn test_timeout_with_select2() {
    let orchestration = |ctx: OrchestrationContext, _: String| async move {
        let timer = ctx.schedule_timer(100);
        let event = ctx.schedule_wait("NeverRaised");
        
        let (winner, _) = ctx.select2(timer, event).await;
        match winner {
            0 => Ok("timeout".to_string()),
            _ => Ok("event".to_string()),
        }
    };
    
    // Verify timer wins (event is never raised)
    // ...
}
```

### Pattern: Testing Replay
```rust
#[tokio::test]
async fn test_feature_replay_correctness() {
    // First execution - run to completion
    let history1 = run_and_capture_history();
    
    // Replay - should produce identical history
    let history2 = replay_from_empty(orchestration_logic);
    
    assert_eq!(history1, history2, "replay should be deterministic");
}
```

### Pattern: Testing Provider Atomicity
```rust
#[tokio::test]
async fn test_ack_atomicity() {
    let provider = SqliteProvider::new_in_memory().await.unwrap();
    
    // Setup
    provider.enqueue_orchestrator_work(start_item, None).await.unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    
    // Ack with multiple operations
    let result = provider.ack_orchestration_item(
        &item.lock_token,
        1,
        history_events,
        worker_items,
        orch_items,
        metadata,
    ).await;
    
    assert!(result.is_ok());
    
    // Verify ALL operations succeeded atomically
    let history = provider.read(instance).await;
    assert_eq!(history.len(), expected_count);
    
    let worker_item = provider.dequeue_worker_peek_lock().await;
    assert!(worker_item.is_some());
}
```

## Test Organization

### File Structure
```
tests/
├── unit_tests.rs              # Pure logic, minimal setup
├── e2e_samples.rs             # Full workflows, realistic scenarios
├── determinism_tests.rs       # Replay correctness
├── nondeterminism_tests.rs    # Nondeterminism detection
├── recovery_tests.rs          # Crash/restart scenarios
├── cancellation_tests.rs      # Cancellation flows
├── concurrency_tests.rs       # Concurrent execution
├── errors_tests.rs            # Error classification
├── observability_tests.rs     # Metrics and logging
├── sqlite_tests.rs            # SQLite provider specific
├── provider_correctness/      # Provider trait compliance
│   ├── atomicity.rs
│   ├── error_handling.rs
│   ├── instance_locking.rs
│   └── ...
└── common/                    # Test helpers
    └── mod.rs
```

### Test Helper Pattern
```rust
// tests/common/mod.rs
pub async fn create_test_runtime() -> Arc<Runtime> {
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder().build();
    let orchestrations = OrchestrationRegistry::builder().build();
    runtime::Runtime::start_with_store(store, Arc::new(activities), orchestrations).await
}
```

## Checklist for Adding Tests

- [ ] Test has clear, descriptive name
- [ ] Test focuses on one scenario
- [ ] Test includes setup, action, and assertion phases
- [ ] Assertions have helpful failure messages
- [ ] Test cleans up resources (calls `rt.shutdown()`)
- [ ] Test uses appropriate test flavor (`#[tokio::test]` vs `#[tokio::test(flavor = "current_thread")]`)
- [ ] Test is in the right file based on what it's testing
- [ ] Test passes: `cargo test test_name`
- [ ] Test passes in full suite: `cargo test --all`

## Anti-Patterns

**Don't:**
- Write tests that depend on timing/sleeps (flaky)
- Leave `#[ignore]` on tests without a good reason
- Test multiple unrelated things in one test
- Copy-paste test code instead of using helpers
- Skip cleanup (can affect other tests)
- Use `unwrap()` without context (use `.expect("context")` instead)
- Test implementation details instead of observable behavior

## Flaky Test Prevention

```rust
// ❌ Flaky: Depends on timing
tokio::time::sleep(Duration::from_millis(50)).await;
let status = client.get_orchestration_status(instance).await;
assert!(matches!(status, Running));  // Might not be running yet!

// ✅ Robust: Poll until condition met
for _ in 0..10 {
    tokio::time::sleep(Duration::from_millis(50)).await;
    let status = client.get_orchestration_status(instance).await;
    if matches!(status, Running) {
        break;
    }
}
assert!(matches!(status, Running), "expected Running after polling");
```

## Suggested Additional Tests

Based on the current codebase, consider adding tests for:
- [ ] Metrics flush on shutdown
- [ ] Log correlation across continue-as-new
- [ ] Provider queue depth accuracy
- [ ] Activity retry with different error types
- [ ] Sub-orchestration with all error categories
- [ ] External event matching edge cases
- [ ] Timer scheduling with very short/long delays
- [ ] Concurrent instance processing with same orchestration

