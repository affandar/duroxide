# Run Comprehensive Stress Tests

## Objective
Run the full stress test suite and optionally update the tracked results file with performance metrics and commit history.

## Planning Phase

### 1. Understand the Request
Ask the user:
- **Duration**: How long to run? (default: 30 seconds, production: 60+ seconds)
- **Update results?**: Should we update the results file?
- **Which results file?**: 
  - `stress-test-results.md` (local development box)
  - `stress-test-results-cloud.md` (cloud/CI environment)

### 2. Determine Environment
- **Local box**: Running on developer machine
- **Cloud**: Running in CI/CD or cloud VM

## Execution Phase

### 1. Run Stress Tests

```bash
# Basic run (no tracking)
./run-stress-tests.sh

# With tracking (updates results file)
./run-stress-tests.sh --track

# Custom duration
./run-stress-tests.sh --track --duration 60
```

### 2. Review Results

The stress test outputs:
- **Performance metrics**: Throughput, latency, success rate
- **Error breakdown**: Infrastructure, configuration, application errors
- **Comparison table**: All provider configurations tested

**Example output:**
```
Provider             Config     Completed  Failed     Infra    Config   App      Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        228        0          0        0        0        100.00     4.58            22.88           218.56         ms
In-Memory SQLite     2/2        166        0          0        0        0        100.00     3.61            18.06           276.78         ms
File SQLite          1/1        399        0          0        0        0        100.00     12.56           62.82           79.59          ms
File SQLite          2/2        591        2          2        0        0        99.66      19.03           95.16           52.54          ms
```

### 3. Interpret Results

**Success criteria:**
- ✅ Success rate should be 100% (or very close)
- ✅ No configuration errors (indicates code issues)
- ✅ Minimal infrastructure errors (indicates provider issues)
- ✅ Throughput should be stable or improving over time

**Red flags:**
- ❌ Success rate < 95%
- ❌ Configuration errors (unregistered activities, nondeterminism)
- ❌ Many infrastructure errors (database locks, connection failures)
- ❌ Throughput regression compared to previous runs

### 4. Update Results File (If Requested)

**Ask the user which file to update:**

```
Which results file should I update?
1. stress-test-results.md (local development box)
2. stress-test-results-cloud.md (cloud environment)
3. Both
4. Neither (just run the tests)
```

**Then run with tracking:**
```bash
# This automatically appends to stress-test-results.md
./run-stress-tests.sh --track
```

**For cloud results:**
```bash
# Run the tests
./run-stress-tests.sh

# Manually append to cloud results file
# (or modify track-results.sh to target cloud file)
```

### 5. Analyze Changes

If updating results file, review:
- **Commit messages**: What changed since last test?
- **Performance trend**: Is throughput improving or regressing?
- **Error patterns**: Are new errors appearing?
- **Configuration impact**: Did concurrency changes affect results?

## Common Scenarios

### Scenario 1: After Performance Optimization

```bash
# User: "I optimized SQLite, let's run stress tests"

# 1. Run with tracking
./run-stress-tests.sh --track

# 2. Review results
# Look for throughput improvement
# Check if latency decreased
# Verify no new errors introduced

# 3. Compare with previous run in stress-test-results.md
# Should see improvement in metrics
```

### Scenario 2: Before Release

```bash
# User: "Let's do a comprehensive stress test before releasing"

# 1. Run longer duration
./run-stress-tests.sh --duration 120

# 2. Ask: Update results file?
# Usually yes for releases

# 3. Run with tracking
./run-stress-tests.sh --track --duration 120

# 4. Verify all metrics are stable
```

### Scenario 3: CI/Cloud Environment

```bash
# User: "Run stress tests in cloud and update cloud results"

# 1. Run tests
./run-stress-tests.sh --duration 60

# 2. Manually update stress-test-results-cloud.md
# Copy the comparison table
# Add commit hash and timestamp
# Note the environment
```

### Scenario 4: Quick Sanity Check

```bash
# User: "Just run a quick stress test to make sure nothing broke"

# 1. Run without tracking (faster)
./run-stress-tests.sh

# 2. Check for errors
# Success rate should be ~100%
# No unexpected failures

# 3. Don't update results file (not needed for quick checks)
```

## Results File Format

### stress-test-results.md Structure

```markdown
## Commit: abc1234 - Timestamp: 2025-11-11 10:00:00 UTC

### Changes Since Last Test
```
abc1234 Optimize SQLite I/O
def5678 Add new feature
```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
File SQLite          2/2        641        0          100.00     20.73           103.66          48.23          ms
```
```

### When to Update Results File

**Update when:**
- ✅ After performance optimizations
- ✅ After major refactoring
- ✅ Before releases
- ✅ After provider changes
- ✅ When establishing new baselines

**Don't update when:**
- ❌ Quick sanity checks
- ❌ During active development (too noisy)
- ❌ When results are anomalous (re-run first)
- ❌ In CI (unless specifically tracking cloud results)

## Troubleshooting

### Low Throughput

**Symptoms:** Throughput significantly lower than previous runs

**Check:**
1. Is the database file growing too large? (clear test data)
2. Are there many lock warnings? (SQLite contention)
3. Is the system under load? (close other apps)
4. Did concurrency settings change?

**Fix:**
```bash
# Clear test databases
rm -rf sqlite-stress/*.db*

# Re-run
./run-stress-tests.sh
```

### High Failure Rate

**Symptoms:** Success rate < 95%

**Check:**
1. Infrastructure errors → Provider issues (locks, connections)
2. Configuration errors → Code issues (unregistered activities, nondeterminism)
3. Application errors → Business logic issues

**Fix:**
- Infrastructure: Optimize provider settings, increase timeouts
- Configuration: Fix code, ensure determinism
- Application: Fix activity logic

### Inconsistent Results

**Symptoms:** Results vary significantly between runs

**Possible causes:**
1. System load (other processes)
2. Database state (old data)
3. Thermal throttling (laptop)
4. Network issues (if using remote DB)

**Fix:**
- Run multiple times and average
- Clear database between runs
- Use longer duration (60+ seconds)
- Run in consistent environment

## Validation Checklist

After running stress tests:

- [ ] Success rate is 100% (or very close)
- [ ] No configuration errors
- [ ] Infrastructure errors are minimal (< 1%)
- [ ] Throughput is stable or improving
- [ ] Latency is reasonable for workload
- [ ] Results file updated (if requested)
- [ ] Commit message includes performance impact

## Example Workflow

```bash
# User asks: "Run stress tests and update local results"

# 1. Confirm which results file
You: "I'll run the stress tests and update stress-test-results.md (local). This will take about 30 seconds."

# 2. Run with tracking
./run-stress-tests.sh --track

# 3. Review output
# Check for errors, verify metrics

# 4. Summarize results
You: "Stress tests complete:
- Throughput: 20.73 orch/sec (was 19.03, +8.9%)
- Success rate: 100% (was 99.66%)
- Zero infrastructure failures (was 2)
Results appended to stress-test-results.md"
```

## Integration with Other Prompts

### After Optimization
```
1. @duroxide-profile.md         - Identify bottleneck
2. @duroxide-refactor.md        - Implement optimization
3. @duroxide-stress-test.md     - Verify improvement ← YOU ARE HERE
4. @duroxide-merge-main.md      - Commit changes
```

### Before Release
```
1. @duroxide-add-test.md        - Ensure coverage
2. @duroxide-clean-warnings.md  - Clean up code
3. @duroxide-stress-test.md     - Run comprehensive tests ← YOU ARE HERE
4. @duroxide-update-docs.md     - Update documentation
5. @duroxide-merge-main.md      - Commit and tag release
```

## Summary

**Quick command:**
```bash
./run-stress-tests.sh --track
```

**Always ask first:**
- Which results file to update?
- What duration to use?
- Is this for baseline or comparison?

**Always report:**
- Success rate
- Throughput (orch/sec)
- Any failures or errors
- Comparison with previous run (if available)

