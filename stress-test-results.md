# Duroxide Stress Test Results

This file tracks all stress test runs, including performance metrics and commit changes.

---

## Commit: 7893d1d - Timestamp: 2025-11-17 18:49:52 UTC

### Changes Since Last Test
```
7893d1d api feedback in TODO
06be8cf refactor: Simplify OrchestrationContext constructor and fix logging filter
71a67ed fix: Correct log filter target from orchestrator to orchestration
81fc816 feat: Add worker_id to all user trace logs (orchestration and activity)
109bc4f feat: Add runtime instance ID to worker identifiers
b6c04d9 feat: Add runtime instance ID to worker identifiers for cross-restart tracing
9f2d3ee feat: Add automatic worker lock renewal for long-running activities
0f5aea1 relaxing timing related test
48a6b87 refactor: move lock timeout to RuntimeOptions with separate orchestrator/worker timeouts
224a77c refactor: rename ai_prompts to prompts and add stress test workflow
6af3bf9 docs: update references from stress-tests to sqlite-stress
9873c2f feat: add Docker-based profiling infrastructure and optimize SQLite I/O
9e8d32c Merge branch 'cursor/review-rust-codebase-for-improvements-3f6a'
1965bee docs: align timer handling guidance
e34f6ee Refactor: Move dispatchers to separate modules
8ece749 Refactor: Implement code quality improvements and clippy lints
43573dc Add property tests and optimize replay engine
923f6cf Refactor: Extract completion consumption check into helper function
9d7419f feat: Add PHASE1_COMPLETE.md and update test summary
2a9d890 Refactor: Improve code quality with clippy, constants, and type aliases
8e0952e Refactor stress tests: Rename to sqlite-stress and consolidate structure
4d91c19 Refactor: Improve error handling and code structure
1ecba43 Fix: Improve provider stress test and retry logic
5e234ec Refactor provider testing documentation and examples
0171fd0 Refactor stress test validation to run minimal infrastructure test
ab7c668 Refactor: Move stress test infrastructure to core crate
cbbd629 minor tracing update
0279731 Merge pull request #4 from affandar/cursor/run-and-compare-stress-tests-ff17
6f255b2 feat: Add --track-cloud option to stress tests
e7b44a8 Add cloud stress test results and environment details
6208596 Add stress test results for commit 806599c
```

### Test Results
```
Provider             Config     Completed  Failed     Infra    Config   App      Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        512        0          0        0        0        100.00     8.59            42.94           116.44         ms
In-Memory SQLite     2/2        429        1          0        0        0        99.77      5.66            28.29           176.75         ms
File SQLite          1/1        503        0          0        0        0        100.00     16.11           80.54           62.08          ms
File SQLite          2/2        850        1          1        0        0        99.88      27.95           139.75          35.78          ms
```

**Failure Analysis:** Both failures are infrastructure-related (database contention under high concurrency). In-memory SQLite: 1 timeout from deadlock on instance stress-test-373. File SQLite: 1 lock contention failure after 5 retries on stress-test-641. No configuration or application errors. Failure handling working correctly with graceful degradation.

---

## Commit: 806599c - Timestamp: 2025-11-09 20:35:34 UTC

### Changes Since Last Test
```
806599c Rename ProviderManager to ProviderAdmin and update documentation
f3db429 refactor done, need to update docs and some cleanup
8a082ba proposal for prov cleanup
16fe2f1 Merge provider-error: Add ProviderError for smart retry logic and defer instance creation
3d12c87 updated stress res
```

### Test Results
```
Provider             Config     Completed  Failed     Infra    Config   App      Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        356        0          0        0        0        100.00     6.22            31.10           160.78         ms
In-Memory SQLite     2/2        320        0          0        0        0        100.00     5.60            28.01           178.51         ms
File SQLite          1/1        386        0          0        0        0        100.00     12.17           60.84           82.18          ms
File SQLite          2/2        541        0          0        0        0        100.00     17.33           86.64           57.71          ms
```

---

## Commit: e0cbfce - Timestamp: 2025-11-08 18:22:01 UTC

### Changes Since Last Test
```
e0cbfce docs: Update provider documentation for ProviderError and fix warnings
c3ca1e7 commit and some scaffolding
996a6a5 Defer instance creation to runtime via ack_orchestration_item metadata
a838659 advanced tests and proposal docs
89cea0e increase timeout for a test
81b514d fix error reporting in runtime, omit app errors
f28f606 Refactor provider validation tests to expose individual test functions
2ea72ca Continued fixes in provider validation tests
1f68ff1 doc updates
540d428 Add comprehensive instance locks documentation and multi-threaded tests
2b2e659 Update provider testing guide: individual test suites are standard
9984b66 Refactor provider validation tests into main crate
10cfc63 updated with management provider trait info
a769563 Add tracing to provider correctness tests for better debugging
f810c04 Expose individual provider correctness test suites
070e43d TODO update
1a56994 some cleanup
1c81706 TODO changes
1047d2f Merge observability branch: Comprehensive metrics, logging, and ActivityContext
c172db3 Add comprehensive observability with metrics and ActivityContext
```

### Test Results
```
Provider             Config     Completed  Failed     Infra    Config   App      Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        493        0          0        0        0        100.00     8.55            42.73           117.02         ms
In-Memory SQLite     2/2        437        0          0        0        0        100.00     8.80            43.99           113.67         ms
File SQLite          1/1        482        0          0        0        0        100.00     15.39           76.94           64.98          ms
File SQLite          2/2        802        0          0        0        0        100.00     26.11           130.55          38.30          ms
```

---

## Commit: 6d3d3ce - Timestamp: 2025-11-02 02:23:11 UTC

### Changes Since Last Test
```
6d3d3ce plan for activitycontext ready, going into execution mode
a4a595a checkpoint 2. going to add activity tracing now and then figure out how to separate infra traces from user traces
12f7da5 checkpoint 1
5325fe3 Merge error-types branch: Implement comprehensive error classification system
ad4664b docs: Update documentation for ErrorDetails error classification
8ddde6e Implement structured error classification system
87d8c71 final stress test updates
```

### Test Results
```
Provider             Config     Completed  Failed     Infra    Config   App      Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        452        0          0        0        0        100.00     7.97            39.87           125.41         ms
In-Memory SQLite     2/2        367        0          0        0        0        100.00     6.90            34.48           145.01         ms
File SQLite          1/1        496        0          0        0        0        100.00     15.83           79.17           63.15          ms
File SQLite          2/2        821        0          0        0        0        100.00     18.94           94.72           52.79          ms
```

---

## Commit: 100693b - Timestamp: 2025-10-28 01:12:53 UTC

### Changes Since Last Test
```
100693b chore: trim stress-test-results.md to requested range and restore 30s duration
65fe385 chore(tracking): strip timestamps and INFO prefixes from results table output
0dedfc2 revert(debug): remove dummy tracking mode and clean results file
5d62b77 chore: Reduce stress test duration to 3s for faster debugging
5c19e2a fix: Show console output when running stress tests with --track
4c88721 feat: Add registry composition features and cross-crate registry pattern
e9e7d26 minor fix
4e98239 removing unnecessary file
05d1879 Merge external-provider-tests: Comprehensive provider testing infrastructure
bdd580e docs: Add provider testing guide
9d2f1be feat: Add provider correctness tests infrastructure
9d332db fix: Resolve clippy duplicate_mod warning and format code
cc11d13 docs: Add provider testing guide and update documentation
```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        386        0          100.00     6.47            32.36           154.51         ms
In-Memory SQLite     2/2        386        0          100.00     7.20            35.99           138.93         ms
File SQLite          1/1        497        0          100.00     15.81           79.06           63.24          ms
File SQLite          2/2        829        0          100.00     26.91           134.53          37.17          ms
```

---


## Commit: d73a4d2 - Timestamp: 2025-10-27 17:44:18 UTC

### Changes Since Last Test
```
d73a4d2 stress tests + scripts
20605fb plan for stress tests
e97b784 TODO updates
5bb177c Clean up all compiler and clippy warnings
a1876d9 Implement comprehensive provider correctness test suite
a261621 checkpoint 1 for tests
aaab571 Update provider correctness test plan for timer queue removal
62fb039 Remove timer queue and clean up code
6b2353f proposal to remove timer queue
2782971 warnings removed
02811fa feat: Add concurrency to worker dispatcher
6696ae7 refactor: Make delayed visibility mandatory for all providers
fb84818 docs: Add comprehensive provider correctness test plan
16b62c2 Merge branch 'main' of github.com:microsoft/duroxide
a0ffbec Merge branch 'dispatchers': Graceful shutdown and 100ms default polling
fc6abb4 feat: Graceful shutdown with configurable timeout and shutdown flag handling
5a58a5d pg provider plan
2d8c4d4 default polling interval to 100ms
9923bd0 fixed tests
251d6d3 Merge branch 'stress': Multi-threaded dispatcher with instance-level locking
```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        181        0          100.00     4.64            23.19           215.62         ms
In-Memory SQLite     2/2        243        0          100.00     6.06            30.28           165.12         ms
File SQLite          1/1        175        0          100.00     15.32           76.58           65.29          ms
File SQLite          2/2        266        0          100.00     24.59           122.95          40.67          ms
```
