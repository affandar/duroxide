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
3. Run individual test suite functions for each validation category

### Adding the Dependency

Add Duroxide with the `provider-test` feature:

```toml
[dependencies]
duroxide = { path = "../duroxide", features = ["provider-test"] }
```

> **Note:** The `provider-test` feature enables both stress tests and validation tests. Enable this single feature to get all provider testing infrastructure.

### Basic Example

Run validation tests by calling individual test suite functions:

```rust
use duroxide::providers::Provider;
use duroxide::provider_validations::{ProviderFactory, run_atomicity_tests, run_error_handling_tests, run_instance_locking_tests, run_lock_expiration_tests, run_multi_execution_tests, run_queue_semantics_tests, run_management_tests};
use std::sync::Arc;

struct MyProviderFactory;

#[async_trait::async_trait]
impl ProviderFactory for MyProviderFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        // Create a fresh provider instance for each test
        Arc::new(MyCustomProvider::new().await?)
    }
}

#[tokio::test]
async fn test_my_provider_atomicity() {
    let factory = MyProviderFactory;
    run_atomicity_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_error_handling() {
    let factory = MyProviderFactory;
    run_error_handling_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_instance_locking() {
    let factory = MyProviderFactory;
    run_instance_locking_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_lock_expiration() {
    let factory = MyProviderFactory;
    run_lock_expiration_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_multi_execution() {
    let factory = MyProviderFactory;
    run_multi_execution_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_queue_semantics() {
    let factory = MyProviderFactory;
    run_queue_semantics_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_management() {
    let factory = MyProviderFactory;
    run_management_tests(&factory).await;
}
```

> **Important:** Run each test suite separately. This provides better test isolation, clearer failure reporting, and allows parallel execution in CI/CD pipelines.

### What the Tests Validate

The validation test suite includes:

1. **Atomicity Tests**
   - All-or-nothing commit semantics
   - Rollback on failure
   - Single transaction guarantees

2. **Error Handling Tests**
   - Invalid lock token rejection
   - Duplicate event ID detection
   - Error propagation

3. **Instance Locking Tests**
   - Exclusive access to instances
   - Lock timeout behavior
   - Concurrent access prevention

4. **Lock Expiration Tests**
   - Automatic lock release
   - Delayed visibility for retries
   - Reacquisition after expiration

5. **Multi-Execution Tests**
   - Execution isolation
   - ContinueAsNew behavior
   - Execution ID tracking

6. **Queue Semantics Tests**
   - FIFO ordering
   - Atomic queue operations
   - Worker queue isolation

7. **Management Capability Tests**
   - Instance listing and filtering
   - Execution queries
   - System metrics
   - Queue depth reporting

### Running Individual Test Suites

Each validation category should be tested in its own test function. This provides:

- **Better isolation**: Failures are clearly attributed to specific categories
- **Clearer reporting**: Test output shows which category failed
- **Parallel execution**: CI/CD can run test suites in parallel
- **Focused debugging**: Fix one category at a time

```rust
use duroxide::provider_validations::{ProviderFactory, run_atomicity_tests, run_instance_locking_tests};

#[tokio::test]
async fn test_my_provider_atomicity() {
    let factory = MyProviderFactory;
    run_atomicity_tests(&factory).await;
}

#[tokio::test]
async fn test_my_provider_locking() {
    let factory = MyProviderFactory;
    run_instance_locking_tests(&factory).await;
}
```

**Available test suite functions:**
- `run_atomicity_tests()` - Transactional guarantees
- `run_error_handling_tests()` - Graceful failure modes
- `run_instance_locking_tests()` - Exclusive access
- `run_lock_expiration_tests()` - Peek-lock timeouts
- `run_multi_execution_tests()` - ContinueAsNew support
- `run_queue_semantics_tests()` - Queue behavior
- `run_management_tests()` - Management API tests

### Creating a Test Provider Factory

Your factory should create fresh, isolated provider instances for each test:

```rust
use duroxide::providers::Provider;
use duroxide::provider_validations::ProviderFactory;
use std::sync::Arc;
use tempfile::TempDir;

struct MyProviderFactory {
    // Keep temp directory alive
    _temp_dir: TempDir,
}

#[async_trait::async_trait]
impl ProviderFactory for MyProviderFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        // Create a new provider instance with unique path
        let db_path = self._temp_dir.path().join(format!("test_{}.db", 
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::File::create(&db_path).unwrap();
        Arc::new(MyProvider::new(&format!("sqlite:{}", db_path.display())).await?)
    }
}
```

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

- **Test Implementation**: `stress-tests/src/lib.rs`
- **Binary Entry Point**: `stress-tests/src/bin/parallel_orchestrations.rs`
- **Test Specification**: `docs/stress-test-spec.md`
- **Provider Guide**: `docs/provider-implementation-guide.md`
- **Built-in Providers**: `src/providers/sqlite.rs`

---

**With this guide, you can thoroughly test your custom Duroxide provider!** 🎉

