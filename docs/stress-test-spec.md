# Duroxide Stress Test Specification

## Overview

This document specifies the stress testing framework for Duroxide orchestration runtime. The stress tests validate both correctness and performance under various configurations and workloads.

## Test Dimensions

### 1. Test Scenarios

Stress tests are organized by scenario type, where each scenario represents a different orchestration pattern:

#### Current Scenarios

- **Parallel Orchestrations**: Fan-out/fan-in orchestration pattern with concurrent instance execution
  - Each orchestration fans out to N activities and waits for all to complete
  - Measures throughput, latency, and correctness

#### Planned Scenarios

- **Timer-Based Orchestrations**: Orchestrations with significant timer usage
- **Long-Running Orchestrations**: Continuously running orchestrations with periodic work
- **Sub-Orchestration Patterns**: Parent orchestrations spawning child orchestrations
- **Mixed Workloads**: Combination of different orchestration types running concurrently

### 2. Storage Providers

Tests run against multiple provider implementations:

- **In-Memory SQLite**: `SqliteProvider::new_in_memory()`
  - Fastest execution, no I/O overhead
  - Tests pure runtime performance
  
- **File-Based SQLite**: `SqliteProvider::new("sqlite:<path>", None)`
  - Real-world persistence with WAL mode
  - Tests I/O overhead and concurrency behavior
  
- **Custom Providers**: Extensible via `provider-test` feature
  - Users can test their own provider implementations
  - Enables benchmarking different storage backends

### 3. Concurrency Configurations

Each test runs with different runtime concurrency settings:

| Configuration | Orchestration Workers | Activity Workers | Description |
|---------------|----------------------|------------------|-------------|
| 1/1 | 1 | 1 | Sequential processing, minimal contention |
| 2/2 | 2 | 2 | Balanced concurrency, default recommended |
| Higher configs | N/M | N/M | Test scalability limits |

## Test Output Format

### Console Output

Each test produces a summary table in the console:

```
=== Duroxide Stress Test Results ===

Test Scenario: Parallel Orchestrations
Duration: 10s
Max Concurrent Instances: 20
Tasks per Instance: 5
Activity Delay: 10ms

| Provider          | Config | Completed | Failed | Success % | Orch/sec | Activity/sec | Avg Latency |
|-------------------|--------|-----------|--------|-----------|----------|--------------|-------------|
| In-Memory SQLite | 1/1    | 179       | 0      | 100.0%    | 4.43     | 22.13        | 232ms       |
| In-Memory SQLite | 2/2    | 257       | 0      | 100.0%    | 6.66     | 33.28        | 150ms       |
| File SQLite      | 1/1    | 171       | 0      | 100.0%    | 15.24    | 76.21        | 66ms        |
| File SQLite      | 2/2    | 267       | 0      | 100.0%    | 7.75     | 38.77        | 129ms       |
```

### Success Metrics

Each test validates:

1. **Success Rate**: `completed / (completed + failed) * 100`
   - Target: 100% success rate
   - Any failures indicate correctness issues

2. **Determinism**: Verify orchestrations produce identical results on repeated runs
   - Test with same input data multiple times
   - Assert output consistency

3. **Ordering**: Validate message processing order matches expectations
   - For tests with ordering requirements

4. **Completion**: All launched orchestrations should complete within timeout
   - No hanging or incomplete instances

5. **Memory**: Monitor for memory leaks over long runs
   - Track memory usage trends

6. **Database Consistency**: For file-based providers, verify WAL integrity
   - Ensure no corruption or lost writes

## Running Stress Tests

### Standard Provider Tests

Run all built-in provider tests:

```bash
cd stress-tests
cargo run --release --bin parallel_orchestrations
```

### Custom Provider Tests

To enable custom provider testing:

1. Add to `Cargo.toml`:

```toml
[dependencies]
duroxide = { path = "..", features = ["provider-test"] }
```

2. Implement the provider test adapter:

```rust
use duroxide::providers::Provider;
use duroxide_stress_tests::StressTestConfig;

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
    
    // Create your custom provider
    let provider = Arc::new(MyCustomProvider::new().await?);
    
    // Run stress test with custom provider
    duroxide_stress_tests::run_with_provider(config, provider).await?;
    
    Ok(())
}
```

3. Run your custom test:

```bash
cargo run --release --bin my_provider_stress_test
```

## Implementation Plan

### Phase 1: Refactor Current Tests

- [ ] Extract common test infrastructure
- [ ] Create `StressTestConfig` struct
- [ ] Implement `run_stress_test` generic function
- [ ] Add provider abstraction layer

### Phase 2: Add Provider Test Feature

- [ ] Add `provider-test` feature to `duroxide` crate
- [ ] Export provider testing utilities
- [ ] Create provider adapter trait
- [ ] Document custom provider integration

### Phase 3: Enhanced Reporting

- [ ] Add structured JSON output option
- [ ] Implement latency percentiles (P50, P95, P99)
- [ ] Add memory usage tracking
- [ ] Create comparison tables across configurations

### Phase 4: Additional Scenarios

- [ ] Timer-based orchestration stress test
- [ ] Long-running orchestration test
- [ ] Sub-orchestration stress test
- [ ] Mixed workload test

## Key Measurements

### Performance Metrics

- **Throughput**: Orchestrations completed per second
- **Activity Throughput**: Activities executed per second
- **Average Latency**: Mean time per orchestration completion
- **Tail Latency**: P95, P99 percentiles (planned)
- **Memory Usage**: Peak and average memory consumption (planned)

### Correctness Metrics

- **Success Rate**: Percentage of successful completions
- **Determinism**: Consistency across multiple runs
- **Data Integrity**: No corruption or lost data
- **Isolation**: Instance-level isolation maintained

## Success Criteria

### Minimum Requirements

- ✅ 100% success rate across all configurations
- ✅ Deterministic replay produces identical results
- ✅ No data corruption or loss
- ✅ Instance isolation maintained
- ✅ No memory leaks (pending implementation)

### Performance Targets

For parallel orchestrations with file-based SQLite:
- **Baseline (1/1)**: ≥ 10 orch/sec
- **Optimized (2/2)**: ≥ 15 orch/sec
- **Latency**: Average < 200ms per orchestration

## Future Enhancements

### Additional Dimensions

- **Load Testing**: Scale to 100+ concurrent instances
- **Duration Tests**: Run for extended periods (1+ hours)
- **Failure Injection**: Test behavior under failures
- **Network Latency**: Simulate network delays
- **Database Load**: Test with shared database under load

### Comparison Benchmarks

- Track performance trends over time
- Compare Duroxide performance to other frameworks
- Document regression detection criteria

### Continuous Integration

- Run stress tests as part of CI/CD pipeline
- Fail builds on performance regressions
- Store historical performance data

## Notes

- File-based SQLite uses WAL mode for better concurrency
- Database files are created in `/tmp` with unique names
- Each test run creates fresh database instances
- Tests run in release mode for accurate performance metrics
- Logging is set to INFO level to reduce noise

## Related Documentation

- **[Provider Testing Guide](provider-testing-guide.md)** - Complete instructions for testing custom providers
- **[Provider Implementation Guide](provider-implementation-guide.md)** - How to implement a custom provider

