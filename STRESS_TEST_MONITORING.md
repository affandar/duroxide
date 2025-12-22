# Stress Test Monitoring Guide

This guide explains how to use the memory and CPU monitoring capabilities built into the Duroxide stress test suite.

## Overview

The stress test runner ([run-stress-tests.sh](run-stress-tests.sh)) includes built-in instrumentation to measure:
- **Peak RSS (Resident Set Size)**: Maximum memory footprint during the test
- **Average CPU Usage**: Mean CPU utilization as a percentage of one core
- **Sample Count**: Number of measurements taken

## Usage

### Basic Monitoring

The stress test runner has monitoring enabled by default:

```bash
# Run all tests with monitoring (default: 10s duration)
./run-stress-tests.sh

# Run all tests for 30 seconds
./run-stress-tests.sh 30

# Run only parallel orchestrations test
./run-stress-tests.sh --parallel-only

# Run only large payload test
./run-stress-tests.sh --large-payload

# Quick 5-second run of all tests
./run-stress-tests.sh 5
```

### Example Output

```
==========================================
Duroxide Stress Test Suite
==========================================
Test type: parallel
Monitoring: Enabled (RSS & CPU)

[... test output ...]

==========================================
Resource Usage Metrics
==========================================
Peak RSS:     13 MB (13792 KB)
Average CPU:  15.79% (of one core)
Samples:      53
==========================================
```

## How It Works

### Monitoring Implementation

The monitoring function:
1. Builds the test binary in release mode (to exclude build time from metrics)
2. Runs the test in the background
3. Samples RSS and CPU every 500ms using `ps`
4. Tracks the peak RSS and calculates average CPU
5. Displays metrics after test completion

### What Gets Measured

**Peak RSS (Resident Set Size)**:
- Actual physical memory used by the process
- Includes code, heap allocations, stack, and loaded libraries
- Does NOT include swapped memory or virtual address space
- Measured in MB and KB

**Average CPU**:
- Percentage of ONE CPU core utilized
- 100% = fully utilizing one core
- 200% = fully utilizing two cores
- Sampled every 500ms and averaged

## Use Cases

### 1. Comparing Iterator Optimization Impact

```bash
# Baseline (without optimization)
git stash push src/runtime/state_helpers.rs src/runtime/execution.rs src/runtime/dispatchers/orchestration.rs
./run-stress-tests.sh 15 --large-payload --monitor

# With optimization
git stash pop
./run-stress-tests.sh 15 --large-payload --monitor
```

Compare the Peak RSS and Average CPU between runs.

### 2. Profiling Different Test Types

```bash
# Small payloads (parallel orchestrations)
./run-stress-tests.sh 30 --monitor

# Large payloads
./run-stress-tests.sh 30 --large-payload --monitor
```

Expected results:
- Parallel: ~13-17 MB peak RSS
- Large payload: ~140-175 MB peak RSS

### 3. Testing Provider Implementations

```bash
# Test your custom provider with monitoring
./run-stress-tests.sh 30 --monitor
```

Use this to validate memory characteristics of custom provider implementations.

## Important Notes

### Compatibility

- **macOS**: Uses `ps -o %cpu=,rss=` (tested)
- **Linux**: Should work with same `ps` format
- **Windows**: Not supported (use WSL)

### Limitations

1. **Sampling Frequency**: 500ms intervals may miss short-lived spikes
2. **RSS vs Allocations**: RSS shows resident memory, not total allocations
3. **Process Scope**: Only measures the test process, not child processes
4. **Background Load**: System activity can affect measurements

### Track Mode Behavior

When using `--track` or `--track-cloud`, monitoring is automatically disabled:

```bash
# This will run with tracking but without monitoring
./run-stress-tests.sh 30 --track
```

Monitoring is always disabled in track mode since tracking focuses on result collection.

## Typical Results

### Parallel Orchestrations (Default)

```
Test: 30 seconds, 20 concurrent orchestrations, 5 tasks each
Peak RSS:     13-17 MB
Average CPU:  15-25% (of one core)
Throughput:   15-30 orch/sec
```

**Characteristics**:
- Low memory footprint
- Small event payloads
- Short histories (10-15 events)

### Large Payload

```
Test: 15 seconds, 5 concurrent orchestrations, 20 activities + 5 sub-orch
Peak RSS:     140-175 MB
Average CPU:  20-30% (of one core)
Throughput:   1-1.5 orch/sec
```

**Characteristics**:
- High memory footprint
- Large event payloads (10KB, 50KB, 100KB)
- Long histories (~80-100 events)

## Interpreting Results

### Peak RSS

**What affects it**:
- Event payload sizes
- History length
- Number of concurrent orchestrations
- SQLite cache size
- Provider implementation

**Expected ranges**:
- **< 20 MB**: Small payloads, low concurrency
- **20-100 MB**: Medium payloads or high concurrency
- **> 100 MB**: Large payloads with long histories

### Average CPU

**What affects it**:
- Concurrency settings (orch/worker dispatchers)
- Activity delay settings
- Database contention
- Serialization overhead

**Expected ranges**:
- **< 10%**: Very low activity (long delays)
- **10-30%**: Typical stress test
- **> 30%**: High concurrency, minimal delays

**Note**: CPU usage is **percentage of ONE core**. Values > 100% indicate multi-core utilization.

## Advanced Usage

### Custom Sampling Interval

Edit [run-stress-tests.sh](run-stress-tests.sh) and modify:

```bash
local interval=0.5  # Change to 0.1 for 100ms sampling
```

### Export Metrics

Capture metrics to a file:

```bash
./run-stress-tests.sh 30 --monitor 2>&1 | tee stress-test-metrics.log
```

Parse the output for automated regression testing:

```bash
grep "Peak RSS" stress-test-metrics.log
grep "Average CPU" stress-test-metrics.log
```

## Troubleshooting

### "bc: command not found"

The monitoring function uses `bc` for calculations. Install it:

```bash
# macOS
brew install bc

# Ubuntu/Debian
sudo apt-get install bc
```

### Inaccurate Measurements

If results seem inconsistent:
1. Close other applications to reduce background load
2. Run longer tests (60+ seconds) for more stable averages
3. Run multiple iterations and take the median
4. Disable OS background services if possible

### Permission Issues

If `ps` fails, ensure the script has permission to query process stats:

```bash
chmod +x run-stress-tests.sh
```

## Related Tools

For more detailed profiling:
- **heaptrack** (Linux): Heap allocation profiling
- **Instruments** (macOS): Memory and CPU profiling
- **Valgrind/Massif**: Memory profiling and leak detection
- **perf** (Linux): System-wide performance analysis

These tools provide deeper insights but require more setup.
