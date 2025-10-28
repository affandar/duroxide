# Duroxide Stress Test Results

This file tracks all stress test runs, including performance metrics and commit changes.

---

## Commit: 65fe385 - Timestamp: 2025-10-28 01:07:05 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        65         0          100.00     1.91            9.57            522.43         ms
In-Memory SQLite     2/2        102        0          100.00     3.06            15.30           326.76         ms
File SQLite          1/1        56         0          100.00     13.57           67.85           73.68          ms
File SQLite          2/2        84         0          100.00     22.57           112.84          44.31          ms
```

---

## Commit: 65fe385 - Timestamp: 2025-10-28 01:04:35 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        62         0          100.00     1.81            9.04            553.16         ms
In-Memory SQLite     2/2        94         0          100.00     2.80            14.01           356.99         ms
File SQLite          1/1        62         0          100.00     14.31           71.54           69.89          ms
File SQLite          2/2        101        0          100.00     25.75           128.75          38.83          ms
```

---

## Commit: 65fe385 - Timestamp: 2025-10-28 01:02:40 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        64         0          100.00     15.15           75.76           65.98          ms
In-Memory SQLite     2/2        91         0          100.00     2.71            13.56           368.70         ms
File SQLite          1/1        63         0          100.00     14.93           74.63           67.00          ms
File SQLite          2/2        87         0          100.00     24.77           123.84          40.37          ms
```

---

## Commit: 65fe385 - Timestamp: 2025-10-28 01:01:02 UTC

### Changes Since Last Test
```

```

### Test Results
```
 Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
 ------------------------------------------------------------------------------------------------------------------------
 In-Memory SQLite     1/1        63         0          100.00     1.85            9.23            541.63         ms
 In-Memory SQLite     2/2        96         0          100.00     2.87            14.35           348.51         ms
 File SQLite          1/1        63         0          100.00     14.91           74.53           67.08          ms
 File SQLite          2/2        84         0          100.00     23.24           116.19          43.02          ms
```

---

## Commit: 65fe385 - Timestamp: 2025-10-28 00:59:27 UTC

### Changes Since Last Test
```

```

### Test Results
```
00:48.317392Z  INFO duroxide_stress_tests: Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
00:48.317396Z  INFO duroxide_stress_tests: ------------------------------------------------------------------------------------------------------------------------
00:48.317398Z  INFO duroxide_stress_tests: In-Memory SQLite     1/1        65         0          100.00     1.92            9.60            520.68         ms
00:48.317404Z  INFO duroxide_stress_tests: In-Memory SQLite     2/2        102        0          100.00     3.04            15.20           328.96         ms
00:48.317407Z  INFO duroxide_stress_tests: File SQLite          1/1        65         0          100.00     15.01           75.03           66.63          ms
00:48.317411Z  INFO duroxide_stress_tests: File SQLite          2/2        89         0          100.00     23.29           116.44          42.93          ms
```

---

## Commit: 65fe385 - Timestamp: 2025-10-28 00:56:31 UTC

### Changes Since Last Test
```
65fe385 chore(tracking): strip timestamps and INFO prefixes from results table output
```

### Test Results
```
2025-10-28T00:57:50.561134Z  INFO duroxide_stress_tests: Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
2025-10-28T00:57:50.561159Z  INFO duroxide_stress_tests: ------------------------------------------------------------------------------------------------------------------------
2025-10-28T00:57:50.561166Z  INFO duroxide_stress_tests: In-Memory SQLite     1/1        64         0          100.00     2.01            10.03           498.50         ms
2025-10-28T00:57:50.561177Z  INFO duroxide_stress_tests: In-Memory SQLite     2/2        97         0          100.00     2.89            14.46           345.79         ms
2025-10-28T00:57:50.561184Z  INFO duroxide_stress_tests: File SQLite          1/1        65         0          100.00     15.01           75.06           66.60          ms
2025-10-28T00:57:50.561197Z  INFO duroxide_stress_tests: File SQLite          2/2        96         0          100.00     26.53           132.66          37.69          ms
```

---

## Commit: 0dedfc2 - Timestamp: 2025-10-28 00:53:29 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        66         0          100.00     15.63           78.13           63.98          ms
In-Memory SQLite     2/2        94         0          100.00     2.83            14.13           353.81         ms
File SQLite          1/1        62         0          100.00     14.34           71.69           69.74          ms
File SQLite          2/2        87         0          100.00     22.76           113.79          43.93          ms
```

---

## Commit: 0dedfc2 - Timestamp: 2025-10-28 00:49:56 UTC

### Changes Since Last Test
```
0dedfc2 revert(debug): remove dummy tracking mode and clean results file
```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        64         0          100.00     2.04            10.22           489.28         ms
In-Memory SQLite     2/2        95         0          100.00     2.88            14.42           346.84         ms
File SQLite          1/1        59         0          100.00     13.96           69.82           71.61          ms
File SQLite          2/2        93         0          100.00     24.35           121.77          41.05          ms
```

---

## Commit: 5d62b77 - Timestamp: 2025-10-28 00:44:41 UTC

### Changes Since Last Test
```
5d62b77 chore: Reduce stress test duration to 3s for faster debugging
```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        65         0          100.00     1.91            9.57            522.20         ms
In-Memory SQLite     2/2        93         0          100.00     2.88            14.38           347.66         ms
File SQLite          1/1        60         0          100.00     14.20           71.02           70.40          ms
File SQLite          2/2        96         0          100.00     26.57           132.84          37.64          ms
```

---

## Commit: 5c19e2a - Timestamp: 2025-10-28 00:25:39 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        61         0          100.00     1.87            9.34            535.13         ms
In-Memory SQLite     2/2        88         0          100.00     2.64            13.19           379.03         ms
File SQLite          1/1        64         0          100.00     14.80           74.01           67.55          ms
File SQLite          2/2        83         0          100.00     21.72           108.62          46.02          ms
```

---

## Commit: 5c19e2a - Timestamp: 2025-10-28 00:24:05 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        62         0          100.00     1.83            9.15            546.34         ms
In-Memory SQLite     2/2        100        0          100.00     2.98            14.91           335.32         ms
File SQLite          1/1        64         0          100.00     15.13           75.66           66.08          ms
File SQLite          2/2        96         0          100.00     24.51           122.54          40.80          ms
```

---

## Commit: 5c19e2a - Timestamp: 2025-10-28 00:16:32 UTC

### Changes Since Last Test
```
5c19e2a fix: Show console output when running stress tests with --track
```

### Test Results
```

```

---

## Commit: 4c88721 - Timestamp: 2025-10-28 00:02:32 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        182        0          100.00     4.56            22.78           219.51         ms
In-Memory SQLite     2/2        303        1          99.67      4.33            21.65           230.92         ms
File SQLite          1/1        167        0          100.00     15.01           75.04           66.62          ms
File SQLite          2/2        285        0          100.00     26.35           131.76          37.94          ms
```

---

## Commit: 4c88721 - Timestamp: 2025-10-27 23:58:27 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        177        0          100.00     4.51            22.57           221.57         ms
In-Memory SQLite     2/2        275        0          100.00     6.97            34.87           143.37         ms
File SQLite          1/1        166        0          100.00     14.79           73.95           67.61          ms
File SQLite          2/2        282        0          100.00     7.73            38.66           129.33         ms
```

---

## Commit: 4c88721 - Timestamp: 2025-10-27 23:56:41 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        179        0          100.00     4.50            22.52           222.04         ms
In-Memory SQLite     2/2        193        0          100.00     5.18            25.89           193.12         ms
File SQLite          1/1        168        0          100.00     15.10           75.52           66.21          ms
File SQLite          2/2        276        0          100.00     25.53           127.64          39.17          ms
```

---

## Commit: 4c88721 - Timestamp: 2025-10-27 23:45:09 UTC

### Changes Since Last Test
```

```

### Test Results
```

```

---

## Commit: 4c88721 - Timestamp: 2025-10-27 23:41:29 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        179        0          100.00     4.46            22.30           224.20         ms
In-Memory SQLite     2/2        284        0          100.00     7.11            35.57           140.56         ms
File SQLite          1/1        170        0          100.00     15.14           75.72           66.03          ms
File SQLite          2/2        274        0          100.00     25.57           127.84          39.11          ms
```

---

## Commit: 4c88721 - Timestamp: 2025-10-27 23:33:23 UTC

### Changes Since Last Test
```

```

### Test Results
```
Provider             Config     Completed  Failed     Success %  Orch/sec        Activity/sec    Avg Latency    
------------------------------------------------------------------------------------------------------------------------
In-Memory SQLite     1/1        173        0          100.00     4.38            21.88           228.53         ms
In-Memory SQLite     2/2        243        0          100.00     6.15            30.74           162.64         ms
File SQLite          1/1        171        0          100.00     15.37           76.87           65.05          ms
File SQLite          2/2        278        0          100.00     7.30            36.48           137.05         ms
```

---

## Commit: 4c88721 - Timestamp: 2025-10-27 23:17:38 UTC

### Changes Since Last Test
```
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

```

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
