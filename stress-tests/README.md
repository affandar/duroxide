# Duroxide Stress Tests

Performance and stress testing suite for Duroxide orchestration runtime.

## Running Tests

### Parallel Orchestrations Test

Tests concurrent execution of multiple orchestration instances with fan-out/fan-in patterns:

```bash
cd stress-tests
cargo run --release --bin parallel_orchestrations
```

**Configuration** (edit in `src/parallel_orchestrations.rs`):
- `max_concurrent`: Maximum number of concurrent orchestrations (default: 20)
- `duration_secs`: How long to run the test (default: 30s)
- `tasks_per_instance`: Number of activities each orchestration fans out to (default: 5)
- `activity_delay_ms`: Simulated work time per activity (default: 10ms)

**How it works**:
- Continuously launches orchestrations for the configured duration
- Maintains up to `max_concurrent` orchestrations running at once
- Each orchestration completes independently and frees a slot for a new one
- After duration elapses, waits for all active orchestrations to complete

**What it measures**:
- Total orchestrations launched/completed/failed
- Orchestrations per second (throughput)
- Average time per orchestration
- Activity throughput

### Environment Variables

Control logging level:
```bash
RUST_LOG=info cargo run --release --bin parallel_orchestrations
RUST_LOG=debug cargo run --release --bin parallel_orchestrations
```

## Interpreting Results

**Baseline (single-threaded dispatcher)**:
- Orchestrations execute sequentially in the dispatcher loop
- Throughput limited by single-threaded processing
- Expected: ~sequential completion based on activity count

**After multi-threaded dispatcher**:
- Multiple orchestrations can execute concurrently
- Throughput scales with concurrency setting
- Expected: significant reduction in total time

## Adding New Tests

Create a new binary in `src/` and add it to `Cargo.toml`:

```toml
[[bin]]
name = "my_test"
path = "src/my_test.rs"
```

