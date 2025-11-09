# Duroxide Stress Test Results (Cloud)

<!-- This run executed in the cloud test environment; previous runs in `stress-test-results.md` were captured on the local dev box. -->

## Commit: 806599c - Timestamp: 2025-11-09 20:35:34 UTC

### Environment
- Cloud test environment

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
