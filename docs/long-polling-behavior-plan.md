# Long Polling Behavior Plan

## Current Behavior

### Short Polling (Current Implementation)

**Provider Interface:**
- `fetch_orchestration_item()` returns `Ok(None)` immediately if no work available
- `fetch_work_item()` returns `Ok(None)` immediately if no work available

**Dispatcher Behavior:**
- Check for work -> if None, sleep `idle_sleep` (100ms) -> repeat.

## Long Polling Behavior

### Proposed Provider Interface Change

1. **Capability Check:** Add `supports_long_polling()` method.
2. **Timeout Parameter:** Add `poll_timeout` to fetch methods.

```rust
trait Provider {
    /// Whether this provider supports long polling (blocking wait for work).
    fn supports_long_polling(&self) -> bool { false }

    async fn fetch_orchestration_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Option<Duration>, // None = return immediately
    ) -> Result<Option<OrchestrationItem>, ProviderError>;

    async fn fetch_work_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Option<Duration>, // None = return immediately
    ) -> Result<Option<(WorkItem, String)>, ProviderError>;
}
```

### Dispatcher Behavior Changes

We introduce two settings to control polling behavior:

1. `dispatcher_long_poll_timeout`: Maximum time to wait *inside* the provider (e.g., 5 mins).
2. `dispatcher_min_poll_interval`: Minimum duration of a polling cycle (e.g., 1s). Prevents hot loops.

```rust
loop {
    if shutdown.load(Ordering::Relaxed) { break; }
    
    let min_interval = rt.options.dispatcher_min_poll_interval; // e.g. 1s
    let long_poll_timeout = rt.options.dispatcher_long_poll_timeout; // e.g. 5m
    
    let start_time = Instant::now();
    let mut work_found = false;
    
    if provider.supports_long_polling() {
        // --- LONG POLL MODE ---
        // Provider blocks up to long_poll_timeout.
        let fetch_result = provider.fetch_orchestration_item(
            lock_timeout, 
            Some(long_poll_timeout)
        ).await;
        
        match fetch_result {
            Ok(Some(item)) => {
                process(item);
                work_found = true;
            },
            Ok(None) => { /* Continue to wait logic */ },
            Err(e) => { /* log and sleep short delay */ }
        }
        
    } else {
        // --- SHORT POLL MODE ---
        // Explicitly pass None to force immediate return.
        let fetch_result = provider.fetch_orchestration_item(
            lock_timeout, 
            None
        ).await;
        
        match fetch_result {
            Ok(Some(item)) => {
                process(item);
                work_found = true;
            },
            Ok(None) => { /* Continue to wait logic */ },
            Err(e) => { /* log and sleep short delay */ }
        }
    }
    
    // Logic for "None" result: enforce min_poll_interval
    if !work_found {
        let elapsed = start_time.elapsed();
        if elapsed < min_interval {
            let sleep_duration = min_interval - elapsed;
            if !shutdown.load(Ordering::Relaxed) {
                tokio::time::sleep(sleep_duration).await;
            }
        } else {
            // Waited long enough (e.g. long poll timeout expired), loop immediately
            tokio::task::yield_now().await;
        }
    }
}
```

## Configuration Changes

### RuntimeOptions

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    // RENAME: dispatcher_idle_sleep -> dispatcher_min_poll_interval
    
    /// Minimum duration of a polling cycle when no work is found.
    /// 
    /// If a provider returns 'None' (no work) faster than this duration,
    /// the dispatcher will sleep for the remainder of the time.
    /// This prevents hot loops for providers that do not support long polling
    /// or return early.
    /// 
    /// Default: `1s`
    pub dispatcher_min_poll_interval: Duration,

    /// Maximum time to wait for work inside the provider (Long Polling).
    /// 
    /// Only used if the provider supports long polling.
    /// 
    /// Default: `30s` (can be set much higher, e.g., 5 mins)
    pub dispatcher_long_poll_timeout: Duration,
}
```

## Implementation Strategy

1.  **Provider Trait Update**:
    *   Add `supports_long_polling()` default `false`.
    *   Update `fetch_*` signatures with `poll_timeout: Option<Duration>`.
2.  **Provider Implementations**:
    *   `SqliteProvider`: Return `false` for support. Ignore `poll_timeout`.
    *   `InstrumentedProvider`: Pass through.
3.  **Runtime Options**:
    *   Rename `dispatcher_idle_sleep` to `dispatcher_min_poll_interval`.
    *   Add `dispatcher_long_poll_timeout`.
    *   Update defaults/builders.
4.  **Dispatcher Update**:
    *   Implement the branched logic based on capability check.
    *   Implement the start/elapsed/sleep logic.
5.  **Test Updates**:
    *   Update all test calls to `fetch_*` to pass `None`.
    *   This preserves existing test behavior (Short Poll mode).

## Benefits

-   **Robustness**: Prevents hot loops even if long-polling providers misbehave (return early).
-   **Efficiency**: Allows very long polls (minutes) while keeping short-polling checks reasonable (seconds).
-   **Explicit Contract**: Separates "waiting" (provider responsibility) from "throttling" (runtime responsibility).
