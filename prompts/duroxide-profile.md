# Profile Duroxide Performance

## Objective
Profile the Duroxide runtime to identify performance bottlenecks and optimization opportunities.

## Quick Start

### Run Profiling (Docker - Recommended)
```bash
# From project root
./profiling/scripts/profile-docker.sh 30 baseline
```

This will:
1. Build Docker image with Rust + Linux perf + flamegraph
2. Run stress tests for 30 seconds with profiling
3. Generate flamegraph SVG in `profiling/results/`
4. Open the flamegraph in your browser

### View Results
```bash
# Open latest flamegraph
open profiling/results/*.svg

# Or manually
ls -lth profiling/results/
open profiling/results/docker_baseline_TIMESTAMP.svg
```

## Reading Flamegraphs

### Structure
- **X-axis**: Alphabetical ordering (NOT time!)
- **Y-axis**: Stack depth (call hierarchy)
- **Width**: Proportion of CPU time spent
- **Color**: Red/orange = hot (lots of time)

### What to Look For

**Wide bars = bottlenecks:**
- `SqliteProvider::ack_orchestration_item` - Transaction overhead
- `serde_json::to_string/from_str` - Serialization cost
- `ReplayEngine::run_turn` - Replay overhead
- `vfs_write`, `pwrite64` - Disk I/O operations
- `Mutex::lock`, `futex_wait` - Lock contention

### Interactive Features
- **Click bar** → Zoom into that function
- **Click title** → Reset zoom
- **Ctrl+F** → Search for function names
- **Hover** → See function name and percentage

## Common Bottlenecks

### 1. SQLite I/O (Most Common)
**Symptoms:** Wide bars for `vfs_write`, `backing_file_write_iter`, `pwrite64`

**Fix:** Optimize SQLite pragmas in `src/providers/sqlite.rs`:
```rust
PRAGMA synchronous = WAL           // Only sync WAL
PRAGMA wal_autocheckpoint = 10000  // Batch writes
PRAGMA cache_size = -64000         // 64MB cache
```

### 2. Lock Contention
**Symptoms:** Wide bars for `Mutex::lock`, `futex_wait`, "Database locked" warnings

**Fix:**
- Increase connection pool size
- Implement transaction batching
- Use finer-grained locking
- Consider connection-level write locks

### 3. Serialization
**Symptoms:** Wide bars for `serde_json::to_string`, `serde_json::from_str`

**Fix:**
- Use binary formats (bincode) for internal storage
- Cache deserialized objects
- Use `&str` instead of `String` where possible

### 4. Replay Engine
**Symptoms:** Wide bars in `ReplayEngine::run_turn`, history iteration

**Fix:**
- Optimize history lookups (currently O(n) scans)
- Cache completion maps
- Consider incremental replay

## Comparison Workflow

### Before/After Optimization
```bash
# 1. Baseline
git checkout main
./profiling/scripts/profile-docker.sh 30 baseline

# 2. Make optimizations
git checkout -b optimize-something
# ... edit code ...

# 3. Rebuild Docker image (important!)
docker build --no-cache -t duroxide-profiler -f profiling/Dockerfile .

# 4. Re-profile
./profiling/scripts/profile-docker.sh 30 optimized

# 5. Compare
open profiling/results/docker_baseline_*.svg
open profiling/results/docker_optimized_*.svg
```

### Metrics to Track

From stress test output, compare:
- **Throughput** (orchestrations/sec) - Higher is better
- **Latency** (average ms) - Lower is better
- **Success rate** (%) - Should be 100%
- **Infrastructure failures** - Should be 0

## Platform Notes

### Docker (Recommended)
- **Works on:** macOS, Linux, Windows
- **Requires:** Docker Desktop
- **Pros:** Consistent, no system tools needed
- **Cons:** ~10% slower due to container overhead

### Native macOS
- **Requires:** Full Xcode (not Command Line Tools)
- **Uses:** `xctrace` for profiling
- **Faster** but more setup required

### Native Linux
- **Requires:** `perf` tools
- **Install:** `sudo apt-get install linux-perf`
- **Fastest** option

## Troubleshooting

### Docker not running
```bash
open -a Docker
# Wait for Docker to start
```

### Permission errors
The Docker command uses `--privileged` flag automatically.

### Flamegraph generation failed
If you see "Error: unable to run 'perf script'", check:
- Is the container still running? `docker ps`
- Check Docker logs: `docker logs <container_id>`
- Try re-running: `./profiling/scripts/profile-docker.sh 30 retry`

### No output file
```bash
# Check results directory
ls -lth profiling/results/

# Check Docker volumes
docker run --rm -v "$(pwd):/workspace" duroxide-profiler ls -l /workspace/profiling/results/
```

## Advanced Usage

### Custom Duration
```bash
./profiling/scripts/profile-docker.sh 60 long-run
```

### Profile Specific Example
```bash
# Profile a single example instead of stress tests
docker run --rm --privileged \
    -v "$(pwd):/workspace" \
    duroxide-profiler \
    bash -c "cargo flamegraph --example fan_out_fan_in"
```

### Higher Sampling Frequency
```bash
# More accurate but more overhead
docker run --rm --privileged \
    -v "$(pwd):/workspace" \
    duroxide-profiler \
    bash -c "cd sqlite-stress && cargo flamegraph --freq 4999 --bin sqlite-stress -- --duration 30"
```

## Historical Performance Data

Keep baseline flamegraphs when making optimizations:
```bash
# Tag important baselines
cp profiling/results/docker_baseline_*.svg profiling/results/before_optimization_X.svg

# Compare months later
./profiling/scripts/profile-docker.sh 30 current
# Compare with before_optimization_X.svg
```

## Integration with Development

### After Major Changes
Run profiling to ensure no regressions:
```bash
./profiling/scripts/profile-docker.sh 30 after-refactor
# Compare with last known good profile
```

### Before Releases
Profile with longer duration for accurate data:
```bash
./profiling/scripts/profile-docker.sh 120 release-candidate
```

## Cleanup

### Remove Old Profiles
```bash
cd profiling/results
ls -t *.svg | tail -n +6 | xargs rm  # Keep latest 5
```

### Clean Docker
```bash
# Remove old images
docker system prune -a

# Rebuild profiler image
docker build -t duroxide-profiler -f profiling/Dockerfile .
```

## Summary

**Quick workflow:**
1. `./profiling/scripts/profile-docker.sh 30 baseline`
2. Look for wide bars in flamegraph
3. Optimize the hot paths
4. Rebuild Docker: `docker build --no-cache -t duroxide-profiler -f profiling/Dockerfile .`
5. Re-profile: `./profiling/scripts/profile-docker.sh 30 optimized`
6. Compare results

