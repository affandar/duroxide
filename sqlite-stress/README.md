# Duroxide SQLite Stress Tests

Performance and correctness stress testing for Duroxide's SQLite provider implementations.

## Quick Start

Run stress tests from the workspace root:

```bash
# Run tests without tracking
./run-stress-tests.sh

# Run tests with result tracking (saves to stress-test-results.md)
./run-stress-tests.sh --track
```

Or run directly:

```bash
cd sqlite-stress
cargo run --release --bin sqlite-stress [DURATION_SECS]
```

## Result Tracking

When using `--track`, results are automatically saved to `stress-test-results.md` with:

- **Commit hash** for each test run
- **Commit messages** since last test
- **Performance metrics** for all configurations
- **Rolling averages** over time
- **Full test output** in collapsible sections

### Example Result Entry

```markdown
## Commit: abc1234
**Timestamp:** 2025-10-27 16:55:46 UTC

### Changes Since Last Test
```
abc1234 Fix deadlock handling
def5678 Add connection pooling
```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    
In-Memory SQLite     1/1        175        0          100.00     4.76            23.80           
File SQLite          2/2        289        0          100.00     26.71           133.53          
```

### Key Metrics
- In-Memory SQLite 1/1: 4.76 orch/sec
- File SQLite 2/2: 26.71 orch/sec
```

## Manual Tracking

You can also run the tracking script directly:

```bash
cd sqlite-stress
./track-results.sh
```

## What Gets Tested

### SQLite Providers
- **In-Memory SQLite**: Fastest execution, no I/O overhead
- **File-Based SQLite**: Real-world persistence with WAL mode

### Test Scenario
- **Parallel Orchestrations**: Fan-out/fan-in pattern with concurrent instance execution
- Multiple activities per orchestration
- Concurrent instance processing
- Sustained load over configurable duration

### Concurrency Configurations
- **1/1**: Sequential processing (baseline)
- **2/2**: Balanced concurrency (recommended)

## Configuring Tests

Edit `src/lib.rs` in the `run_test_suite` function to customize:

- `max_concurrent`: Maximum concurrent orchestrations (default: 20)
- `duration_secs`: Test duration (default: 30s, configurable via CLI)
- `tasks_per_instance`: Activities per orchestration (default: 5)
- `activity_delay_ms`: Simulated work time (default: 10ms)
- `concurrency_combos`: Which configurations to test (default: [(1,1), (2,2)])

## Understanding Results

### Success Metrics
- **Success Rate**: Should be 100% (any failures indicate issues)
- **Throughput**: Higher is better (orchestrations per second)
- **Activity Throughput**: Activities completed per second
- **Latency**: Lower is better (average time per orchestration)

### Expected Patterns
- File-based SQLite typically 3-10x faster than in-memory due to WAL optimization
- 2/2 configuration performs better than 1/1 for I/O-bound workloads
- Higher concurrency may reduce per-item latency but increase total throughput

### Failure Categories
- **Infrastructure**: Provider bugs, database lock issues, data corruption
- **Configuration**: Missing implementations, nondeterminism detection
- **Application**: Orchestration/activity errors (should be rare in stress tests)

## Continuous Integration

Add to CI/CD pipeline:

```yaml
- name: Run SQLite stress tests
  run: ./run-stress-tests.sh 10  # Quick 10 second test for CI
```

## Custom Providers

This package demonstrates how to use the `ProviderStressFactory` trait from the main duroxide crate.

See the main crate's documentation for details on testing custom provider implementations:
```bash
cargo doc --package duroxide --features provider-test --open
```

## Architecture

```
sqlite-stress/
├── src/
│   ├── lib.rs                    # SQLite factory implementations
│   └── bin/
│       └── sqlite-stress.rs      # CLI runner
├── Cargo.toml
├── README.md
└── track-results.sh              # Result tracking script
```

The factories (`InMemorySqliteFactory` and `FileSqliteFactory`) implement `ProviderStressFactory` from `duroxide::provider_stress_tests`, making them compatible with the generic stress test infrastructure.
