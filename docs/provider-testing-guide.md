# Provider Testing Guide

**For:** Developers testing custom Duroxide providers  
**Reference:** See `stress-tests/src/lib.rs` for test implementation details

---

## Quick Start

Duroxide provides two types of tests for custom providers:

### 1. Stress Tests (Performance & Throughput)
- Measures: throughput, latency, success rate
- Use `provider-test` feature
- See [Stress Tests](#stress-tests) section below

### 2. Validation Tests (Behavior Validation)
- Validates: atomicity, locking, error handling, queue semantics, management capabilities
- Use `provider-test` feature (same as stress tests)
- See [Provider Validation Tests](#provider-validation-tests) section below

**Recommended Testing Strategy:**
1. Run validation tests first to validate behavior
2. Run stress tests to measure performance
3. Both should pass with 100% success rate

---

## Adding the Dependency

Add Duroxide with the `provider-test` feature to your repository's `Cargo.toml`:

```toml
[dependencies]
duroxide = { path = "../duroxide", features = ["provider-test"] }
```

The `provider-test` feature enables access to the stress testing infrastructure.

---

## Stress Tests

### Basic Example

Here's a minimal example of running stress tests against a custom provider:

```rust
use duroxide::providers::Provider;
use duroxide_stress_tests::{run_stress_test, StressTestConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure the stress test
    let config = StressTestConfig {
        max_concurrent: 20,        // Max concurrent orchestration instances
        duration_secs: 10,         // Test duration in seconds
        tasks_per_instance: 5,     // Activities per orchestration
        activity_delay_ms: 10,      // Simulated work time per activity
        orch_concurrency: 2,       // Runtime orchestration dispatcher concurrency
        worker_concurrency: 2,      // Runtime activity worker concurrency
    };
    
    // Create your custom provider
    let provider = Arc::new(MyCustomProvider::new("connection_string").await?);
    
    // Run the stress test
    run_stress_test("MyCustomProvider", provider, config).await?;
    
    Ok(())
}
```

The test will:
1. Launch orchestrations continuously for the specified duration
2. Track completed and failed instances
3. Calculate throughput and latency metrics
4. Print a results table to the console
5. Return an error if success rate < 100%

---

## Test Configuration

The `StressTestConfig` struct controls test behavior:

```rust
pub struct StressTestConfig {
    pub max_concurrent: usize,      // Max concurrent instances at once
    pub duration_secs: u64,          // How long to run the test
    pub tasks_per_instance: usize,    // Activities per orchestration
    pub activity_delay_ms: u64,      // Simulated activity work time
    pub orch_concurrency: usize,      // Orchestration dispatcher threads
    pub worker_concurrency: usize,    // Activity worker threads
}
```

### Recommended Configurations

**Quick Validation (30 seconds):**
```rust
StressTestConfig {
    max_concurrent: 10,
    duration_secs: 30,
    tasks_per_instance: 3,
    activity_delay_ms: 50,
    orch_concurrency: 1,
    worker_concurrency: 1,
}
```

**Performance Baseline (10 seconds):**
```rust
StressTestConfig {
    max_concurrent: 20,
    duration_secs: 10,
    tasks_per_instance: 5,
    activity_delay_ms: 10,
    orch_concurrency: 1,
    worker_concurrency: 1,
}
```

**Concurrency Stress Test (10 seconds):**
```rust
StressTestConfig {
    max_concurrent: 20,
    duration_secs: 10,
    tasks_per_instance: 5,
    activity_delay_ms: 10,
    orch_concurrency: 2,
    worker_concurrency: 2,
}
```

---

## Custom Orchestrations and Activities

You can define custom orchestrations and activities to test provider-specific behavior:

```rust
use duroxide::{OrchestrationContext, Event};
use duroxide::providers::{Provider, ActivityRegistry};
use duroxide_stress_tests::{run_stress_test, StressTestConfig};
use std::sync::Arc;

// Custom activity that simulates work
async fn custom_activity(input: String) -> Result<String, String> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    Ok(format!("processed: {}", input))
}

// Custom orchestration that uses the activity
async fn custom_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, Box<dyn std::error::Error>> {
    let tasks = vec!["task1", "task2", "task3"];
    let mut results = Vec::new();
    
    for task in tasks {
        let result = ctx.call_activity("CustomActivity", task.to_string()).await?;
        results.push(result);
    }
    
    Ok(format!("completed: {}", results.join(", ")))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create custom activity registry
    let activities = Arc::new(
        ActivityRegistry::builder()
            .register("CustomActivity", custom_activity)
            .build()
    );
    
    // Create custom orchestration registry
    let orchestrations = OrchestrationRegistry::builder()
        .register("CustomOrchestration", custom_orchestration)
        .build();
    
    // Create provider and run custom test
    let provider = Arc::new(MyCustomProvider::new().await?);
    
    let config = StressTestConfig {
        max_concurrent: 10,
        duration_secs: 10,
        tasks_per_instance: 3,
        activity_delay_ms: 100,
        orch_concurrency: 1,
        worker_concurrency: 1,
    };
    
    // Note: You'll need to use the internal run_stress_test function with custom registries
    // Or implement your own test runner using the runtime directly
    
    Ok(())
}
```

---

## Test Scenarios

The current stress test implements the **Parallel Orchestrations** scenario:

### Parallel Orchestrations (Fan-Out/Fan-In)

Each orchestration:
1. Fans out to N activities in parallel
2. Waits for all activities to complete
3. Returns a success message

This pattern tests:
- Concurrent activity execution
- Message queue throughput
- Database write concurrency
- Instance-level locking correctness

### Creating Custom Scenarios

To add a new test scenario:

1. **Define the orchestration**:
```rust
async fn my_scenario(ctx: OrchestrationContext, input: String) -> Result<String, Box<dyn std::error::Error>> {
    // Your test scenario logic
    Ok("done".to_string())
}
```

2. **Register it in the test**:
```rust
let orchestrations = OrchestrationRegistry::builder()
    .register("MyScenario", my_scenario)
    .build();
```

3. **Create activities if needed**:
```rust
let activities = Arc::new(
    ActivityRegistry::builder()
        .register("MyActivity", my_activity)
        .build()
);
```

4. **Launch test instances**:
```rust
for i in 0..num_instances {
    let instance_id = format!("test-{}", i);
    client.start_orchestration("MyScenario", instance_id, input.clone()).await?;
}
```

---

## Interpreting Results

The stress test outputs a results table:

```
=== Comparison Table ===
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        179        0          100.00     4.63            23.13           216.21         ms
In-Memory SQLite     2/2        278        0          100.00     7.09            35.45           141.04         ms
File SQLite          1/1        167        0          100.00     14.75           73.76           67.78          ms
File SQLite          2/2        281        0          100.00     25.98           129.88          38.49          ms
```

### Key Metrics

**Completed**: Number of orchestrations that finished successfully  
**Failed**: Number of orchestrations that encountered errors  
**Success %**: `(completed / (completed + failed)) * 100` - MUST be 100%  
**Orch/sec**: Orchestrations completed per second (throughput)  
**Activity/sec**: Activities executed per second  
**Avg Latency**: Mean time from start to completion of an orchestration

### Expected Results

✅ **Success Rate = 100%**: All orchestrations complete without errors  
✅ **Consistent Throughput**: Metrics remain stable across runs  
✅ **Deterministic**: Same input produces same output  
✅ **Scalable**: Higher concurrency increases throughput

### Warning Signs

❌ **Success Rate < 100%**: Indicates correctness issues (locks, atomicity, etc.)  
❌ **Throughput = 0**: Provider not committing work  
❌ **Highly Variable Latency**: Lock contention or database issues  
❌ **Decreasing Throughput**: Resource exhaustion or contention

---

## Provider Validation Tests

Duroxide includes a comprehensive suite of validation tests that validate provider behavior. These tests verify critical correctness properties like atomicity, locking, error handling, queue semantics, and management capabilities.

### Quick Start

To run validation tests against your custom provider:

1. Add `duroxide` with `provider-test` feature to your project
2. Implement the `ProviderFactory` trait
3. Run individual test functions for each validation test

### Adding the Dependency

Add Duroxide with the `provider-test` feature:

```toml
[dependencies]
duroxide = { path = "../duroxide", features = ["provider-test"] }
```

> **Note:** The `provider-test` feature enables both stress tests and validation tests. Enable this single feature to get all provider testing infrastructure.

### Basic Example

Run validation tests by calling individual test functions:

```rust
use duroxide::providers::Provider;
use duroxide::provider_validations::{
    ProviderFactory,
    test_atomicity_failure_rollback,
    test_multi_operation_atomic_ack,
    test_exclusive_instance_lock,
    test_worker_queue_fifo_ordering,
    // ... import other tests as needed
};
use std::sync::Arc;
use std::time::Duration;

const TEST_LOCK_TIMEOUT_MS: u64 = 1000;

struct MyProviderFactory;

#[async_trait::async_trait]
impl ProviderFactory for MyProviderFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        // Create a fresh provider instance for each test
        Arc::new(MyCustomProvider::new().await?)
    }

    fn lock_timeout_ms(&self) -> u64 {
        // Return the lock timeout configured in your provider
        // This must match the timeout used when creating the provider
        TEST_LOCK_TIMEOUT_MS
    }
}

#[tokio::test]
async fn test_my_provider_atomicity_failure_rollback() {
    let factory = MyProviderFactory;
    test_atomicity_failure_rollback(&factory).await;
}

#[tokio::test]
async fn test_my_provider_exclusive_instance_lock() {
    let factory = MyProviderFactory;
    test_exclusive_instance_lock(&factory).await;
}

#[tokio::test]
async fn test_my_provider_worker_queue_fifo_ordering() {
    let factory = MyProviderFactory;
    test_worker_queue_fifo_ordering(&factory).await;
}
```

> **Important:** Run each test function individually. This provides better test isolation, clearer failure reporting, and allows parallel execution in CI/CD pipelines. When a test fails, you'll know exactly which behavior is broken.

### What the Tests Validate

The validation test suite includes **41 individual test functions** organized into 7 categories:

1. **Atomicity Tests (4 tests)**
   - `test_atomicity_failure_rollback` - All-or-nothing commit semantics, rollback on failure
   - `test_multi_operation_atomic_ack` - Complex ack succeeds atomically
   - `test_lock_released_only_on_successful_ack` - Lock only released on success
   - `test_concurrent_ack_prevention` - Only one ack succeeds with same token

2. **Error Handling Tests (5 tests)**
   - `test_invalid_lock_token_on_ack` - Invalid lock token rejection
   - `test_duplicate_event_id_rejection` - Duplicate event ID detection
   - `test_missing_instance_metadata` - Missing instances handled gracefully
   - `test_corrupted_serialization_data` - Corrupted data handled gracefully
   - `test_lock_expiration_during_ack` - Expired locks are rejected

3. **Instance Locking Tests (11 tests)**
   - `test_exclusive_instance_lock` - Exclusive access to instances
   - `test_lock_token_uniqueness` - Each fetch generates unique lock token
   - `test_invalid_lock_token_rejection` - Invalid tokens rejected for ack/abandon
   - `test_concurrent_instance_fetching` - Concurrent fetches don't duplicate instances
   - `test_completions_arriving_during_lock_blocked` - New messages blocked during lock
   - `test_cross_instance_lock_isolation` - Locks don't block other instances
   - `test_message_tagging_during_lock` - Only fetched messages deleted on ack
   - `test_ack_only_affects_locked_messages` - Ack only affects locked messages
   - `test_multi_threaded_lock_contention` - Locks prevent concurrent processing (multi-threaded)
   - `test_multi_threaded_no_duplicate_processing` - No duplicate processing (multi-threaded)
   - `test_multi_threaded_lock_expiration_recovery` - Lock expiration recovery (multi-threaded)

4. **Lock Expiration Tests (4 tests)**
   - `test_lock_expires_after_timeout` - Automatic lock release after timeout
   - `test_abandon_releases_lock_immediately` - Abandon releases lock immediately
   - `test_lock_renewal_on_ack` - Successful ack releases lock immediately
   - `test_concurrent_lock_attempts_respect_expiration` - Concurrent attempts respect expiration

5. **Multi-Execution Tests (5 tests)**
   - `test_execution_isolation` - Each execution has separate history
   - `test_latest_execution_detection` - read() returns latest execution
   - `test_execution_id_sequencing` - Execution IDs increment correctly
   - `test_continue_as_new_creates_new_execution` - ContinueAsNew creates new execution
   - `test_execution_history_persistence` - All executions' history persists independently

6. **Queue Semantics Tests (5 tests)**
   - `test_worker_queue_fifo_ordering` - Worker items dequeued in FIFO order
   - `test_worker_peek_lock_semantics` - Dequeue doesn't remove item until ack
   - `test_worker_ack_atomicity` - Ack_worker atomically removes item and enqueues completion
   - `test_timer_delayed_visibility` - TimerFired items only dequeued when visible
   - `test_lost_lock_token_handling` - Locked items become available after expiration

7. **Management Capability Tests (7 tests)**
   - `test_list_instances` - Instance listing returns all instance IDs
   - `test_list_instances_by_status` - Instance filtering by status works correctly
   - `test_list_executions` - Execution queries return all execution IDs
   - `test_get_instance_info` - Instance metadata retrieval
   - `test_get_execution_info` - Execution metadata retrieval
   - `test_get_system_metrics` - System metrics are accurate
   - `test_get_queue_depths` - Queue depth reporting is correct

### Running Individual Test Functions

Each validation test should be run individually. This provides:

- **Better isolation**: Failures are clearly attributed to specific behaviors
- **Clearer reporting**: Test output shows exactly which test failed
- **Parallel execution**: CI/CD can run tests in parallel
- **Focused debugging**: Fix one behavior at a time without running unrelated tests
- **Easier debugging**: When a test fails, you know exactly which behavior is broken

```rust
use duroxide::provider_validations::{
    ProviderFactory,
    test_atomicity_failure_rollback,
    test_exclusive_instance_lock,
    test_worker_queue_fifo_ordering,
};

#[tokio::test]
async fn test_my_provider_atomicity_failure_rollback() {
    let factory = MyProviderFactory;
    test_atomicity_failure_rollback(&factory).await;
}

#[tokio::test]
async fn test_my_provider_exclusive_instance_lock() {
    let factory = MyProviderFactory;
    test_exclusive_instance_lock(&factory).await;
}

#[tokio::test]
async fn test_my_provider_worker_queue_fifo_ordering() {
    let factory = MyProviderFactory;
    test_worker_queue_fifo_ordering(&factory).await;
}
```

**Available test functions:** See `duroxide::provider_validations` module documentation for the complete list of all 41 test functions, or refer to `tests/sqlite_provider_validations.rs` for a complete example using all tests.

### Creating a Test Provider Factory

Your factory should create fresh, isolated provider instances for each test. **Importantly, you must implement `lock_timeout_ms()` to return the lock timeout configured in your provider** - this ensures validation tests wait for the correct duration when testing lock expiration behavior.

```rust
use duroxide::providers::Provider;
use duroxide::provider_validations::ProviderFactory;
use std::sync::Arc;
use std::time::Duration;

const TEST_LOCK_TIMEOUT_MS: u64 = 1000;

struct MyProviderFactory {
    // Keep temp directory alive
    _temp_dir: TempDir,
}

#[async_trait::async_trait]
impl ProviderFactory for MyProviderFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        // Create a new provider instance with unique path
        // Configure the provider with the same lock timeout
        let db_path = self._temp_dir.path().join(format!("test_{}.db", 
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::File::create(&db_path).unwrap();
        
        let options = MyProviderOptions {
            lock_timeout: Duration::from_millis(TEST_LOCK_TIMEOUT_MS),
        };
        Arc::new(MyProvider::new(&format!("sqlite:{}", db_path.display()), Some(options)).await?)
    }

    fn lock_timeout_ms(&self) -> u64 {
        // CRITICAL: This must match the lock_timeout configured in create_provider()
        // Validation tests use this value to determine sleep durations when waiting
        // for lock expiration. If this doesn't match your provider's timeout,
        // tests will fail with timing issues.
        TEST_LOCK_TIMEOUT_MS
    }
}
```

**Important:** The `lock_timeout_ms()` value must match the lock timeout configured in your provider's options. If they don't match, validation tests that check lock expiration will fail because they'll wait for the wrong duration.

### Integration with CI/CD

Add validation tests to your CI pipeline:

```yaml
# .github/workflows/provider-tests.yml
name: Provider Validation Tests

on: [pull_request]

jobs:
  validation-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Setup Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      
      - name: Run validation tests
        run: |
          cargo test --features provider-test
```

The tests will run individually, providing granular failure reporting. You can also run specific tests:

```bash
# Run a specific test
cargo test --features provider-test test_my_provider_atomicity_failure_rollback

# Run all atomicity tests
cargo test --features provider-test test_my_provider_atomicity

# Run tests in parallel
cargo test --features provider-test -- --test-threads=4
```

---

## Comparing Providers

To compare multiple providers side-by-side:

```rust
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = StressTestConfig {
        max_concurrent: 20,
        duration_secs: 10,
        tasks_per_instance: 5,
        activity_delay_ms: 10,
        orch_concurrency: 2,
        worker_concurrency: 2,
    };
    
    // Test in-memory SQLite
    let in_mem_provider = Arc::new(SqliteProvider::new_in_memory().await?);
    run_stress_test("In-Memory SQLite", in_mem_provider, config).await?;
    
    // Test file-based SQLite
    let db_path = "/tmp/test.db";
    std::fs::File::create(&db_path)?;
    let file_provider = Arc::new(SqliteProvider::new(&format!("sqlite:{}", db_path), None).await?);
    run_stress_test("File SQLite", file_provider, config).await?;
    
    // Test your custom provider
    let custom_provider = Arc::new(MyCustomProvider::new().await?);
    run_stress_test("MyCustomProvider", custom_provider, config).await?;
    
    Ok(())
}
```

---

## Troubleshooting

### Test Fails with Success Rate < 100%

**Possible Causes:**
1. **Non-atomic commits**: Ensure `ack_orchestration_item` uses a single transaction
2. **Lock expiration**: Provider may be releasing locks prematurely
3. **Queue semantics**: Messages lost or duplicated
4. **Database errors**: Deadlocks, connection failures, or constraint violations

**Debug Steps:**
- Enable verbose logging: `RUST_LOG=debug cargo run --release`
- Check provider logs for errors
- Verify transaction boundaries
- Test with lower concurrency first

### Zero Throughput

**Possible Causes:**
1. **Work not committed**: Provider not writing to queues correctly
2. **Lock not released**: Messages stay locked indefinitely
3. **Queue not polling**: `fetch_orchestration_item` not returning work

**Debug Steps:**
- Manually inspect database/queue contents
- Verify `fetch_orchestration_item` returns items
- Check `ack_orchestration_item` completes successfully
- Test with a simple unit test first

### High Latency Variability

**Possible Causes:**
1. **Lock contention**: Too many workers competing for same locks
2. **Database bottleneck**: Insufficient connection pool or slow queries
3. **Missing indexes**: Full table scans on large tables

**Debug Steps:**
- Reduce concurrency to isolate contention
- Profile database queries
- Check for missing indexes
- Verify connection pool size

### Out of Memory

**Possible Causes:**
1. **History not cleaned**: Old executions accumulate in history table
2. **Connection leak**: Connections not properly pooled or closed
3. **Large payloads**: Work items contain excessive data

**Debug Steps:**
- Monitor memory usage during test
- Check history table size
- Verify connection pool limits
- Inspect work item sizes

---

## Integration with CI/CD

Add stress tests to your CI pipeline:

```yaml
# .github/workflows/stress-tests.yml
name: Stress Tests

on:
  pull_request:
    branches: [main]
  schedule:
    - cron: '0 0 * * 0'  # Weekly

jobs:
  stress-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Setup Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      
      - name: Run stress tests
        run: |
          cd stress-tests
          cargo run --release --bin parallel_orchestrations
```

---

## Performance Targets

Use these benchmarks to validate your provider:

### Minimum Requirements

- ✅ **Success Rate**: 100% under all configurations
- ✅ **Baseline Throughput**: ≥ 10 orch/sec (file-based provider, 1/1 config)
- ✅ **Latency**: Average < 200ms per orchestration
- ✅ **Scalability**: 2/2 config increases throughput by ≥ 30%

### High-Performance Targets

- **Throughput**: ≥ 50 orch/sec (file-based provider, 2/2 config)
- **Latency**: Average < 100ms per orchestration
- **Scalability**: Linear throughput increase with concurrency

---

## Advanced Usage

### Custom Test Runner

For complete control, build your own test runner:

```rust
use duroxide::{Runtime, RuntimeOptions, Client};
use duroxide::providers::Provider;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Arc::new(MyCustomProvider::new().await?);
    let activities = Arc::new(ActivityRegistry::builder().build());
    let orchestrations = OrchestrationRegistry::builder().build();
    
    let runtime = Runtime::start_with_options(
        provider.clone(),
        activities,
        orchestrations,
        RuntimeOptions {
            orchestration_concurrency: 2,
            worker_concurrency: 2,
            ..Default::default()
        },
    ).await;
    
    let client = Client::new(provider.clone()).await?;
    
    // Launch orchestrations
    for i in 0..100 {
        client.start_orchestration("TestOrch", format!("instance-{}", i), "input".to_string()).await?;
    }
    
    // Wait for completion
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    
    runtime.shutdown().await;
    
    Ok(())
}
```

### Result Tracking

To track results over time (like Duroxide's own tracking):

```bash
# Run with tracking enabled
./stress-tests/track-results.sh
```

This generates `stress-test-results.md` with:
- Commit hash and timestamp
- Changes since last test
- Performance metrics
- Historical trends

---

## Reference

- **Test Implementation**: `src/provider_validation/` (individual test modules)
- **Test API**: `src/provider_validations.rs` (test function exports)
- **Example Usage**: `tests/sqlite_provider_validations.rs` (complete example with all 41 tests)
- **Test Specification**: See individual test function documentation
- **Provider Guide**: `docs/provider-implementation-guide.md`
- **Built-in Providers**: `src/providers/sqlite.rs`

---

**With this guide, you can thoroughly test your custom Duroxide provider!** 🎉

