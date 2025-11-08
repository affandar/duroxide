# Duroxide Stress Test Results

This file tracks all stress test runs, including performance metrics and commit changes.

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
16b62c2 Merge branch 'main' of github.com:affandar/duroxide
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
