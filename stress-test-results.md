# Duroxide Stress Test Results

This file tracks all stress test runs, including performance metrics and commit changes.

---

## Commit: d73a4d2 - Timestamp: 2025-10-27 17:46:07 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        174        0          100.00     4.51            22.54           221.83         ms
In-Memory SQLite     2/2        247        0          100.00     6.45            32.23           155.13         ms
File SQLite          1/1        170        0          100.00     15.14           75.72           66.03          ms
File SQLite          2/2        294        0          100.00     27.44           137.20          36.44          ms
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
