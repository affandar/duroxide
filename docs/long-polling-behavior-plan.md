# Long Polling Behavior Plan

## Current Behavior

### Short Polling (Current Implementation)

**Provider Interface:**
- `fetch_orchestration_item()` returns `Ok(None)` immediately if no work available
- `fetch_work_item()` returns `Ok(None)` immediately if no work available

**Dispatcher Behavior:**
```rust
loop {
    if shutdown.load(Ordering::Relaxed) { break; }
    
    if let Ok(Some(item)) = provider.fetch_orchestration_item(timeout).await {
        process(item);
    } else {
        tokio::time::sleep(dispatcher_idle_sleep).await;  // Default: 100ms
    }
}
```

**Characteristics:**
- High CPU usage when idle (constant polling)
- High database load (queries every 100ms)
- Low latency when work arrives (max 100ms delay)
- Simple shutdown handling (check before each fetch)

## Long Polling Behavior

### Proposed Provider Interface Change

**Option 1: Add timeout parameter to fetch methods (Recommended)**

```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Option<Duration>,  // NEW: None = immediate return, Some(duration) = long poll
) -> Result<Option<OrchestrationItem>, ProviderError>;

async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Option<Duration>,  // NEW: None = immediate return, Some(duration) = long poll
) -> Result<Option<(WorkItem, String)>, ProviderError>;
```

**Option 2: Provider capability detection**

```rust
trait Provider {
    fn supports_long_polling(&self) -> bool { false }
    
    async fn fetch_orchestration_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Option<Duration>,
    ) -> Result<Option<OrchestrationItem>, ProviderError>;
}
```

### Long Polling Implementation Pattern

**For providers that support long polling (e.g., PostgreSQL LISTEN/NOTIFY, Redis BLPOP):**

```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Option<Duration>,
) -> Result<Option<OrchestrationItem>, ProviderError> {
    let mut tx = self.pool.begin().await?;
    
    // Try immediate fetch first
    let row = sqlx::query("SELECT ... LIMIT 1")
        .fetch_optional(&mut *tx)
        .await?;
    
    if row.is_some() {
        // Work available immediately - process and return
        return process_and_return(row);
    }
    
    // No work available
    if let Some(poll_duration) = poll_timeout {
        // Long poll: wait for notification or timeout
        // PostgreSQL example:
        // LISTEN orchestrator_queue_changes;
        // SELECT pg_notify('orchestrator_queue_changes', '');
        // Wait up to poll_duration for notification
        
        // Redis example:
        // BLPOP orchestrator_queue poll_duration
        
        // SQLite: Use WAL mode + busy_timeout for pseudo-long-polling
        // Or: sleep in small increments checking shutdown
        
        // If notification received during wait:
        //   - Re-query for work
        //   - Return if found
        // If timeout expires:
        //   - Return Ok(None)
    }
    
    tx.rollback().await?;
    Ok(None)
}
```

**For providers that don't support long polling (e.g., SQLite):**

```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Option<Duration>,
) -> Result<Option<OrchestrationItem>, ProviderError> {
    // Ignore poll_timeout, return immediately
    // Dispatcher will handle the sleep
    self.fetch_orchestration_item_immediate(lock_timeout).await
}
```

## Dispatcher Behavior Changes

### Current Dispatcher Loop

```rust
loop {
    if shutdown.load(Ordering::Relaxed) { break; }
    
    if let Ok(Some(item)) = provider.fetch_orchestration_item(timeout).await {
        process(item);
    } else {
        tokio::time::sleep(dispatcher_idle_sleep).await;
    }
}
```

### Proposed Dispatcher Loop with Long Polling

```rust
loop {
    if shutdown.load(Ordering::Relaxed) { break; }
    
    // Determine poll timeout
    let poll_timeout = if provider.supports_long_polling() {
        Some(rt.options.dispatcher_idle_sleep)  // Use idle sleep as poll timeout
    } else {
        None  // Provider doesn't support it, will return immediately
    };
    
    // Race: fetch vs shutdown
    let fetch_fut = provider.fetch_orchestration_item(
        rt.options.orchestrator_lock_timeout,
        poll_timeout,
    );
    
    let shutdown_fut = async {
        while !shutdown.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    
    match tokio::select! {
        result = fetch_fut => result,
        _ = shutdown_fut => {
            // Shutdown requested during long poll
            break;
        }
    } {
        Ok(Some(item)) => {
            process(item);
        }
        Ok(None) => {
            // No work available
            // If provider supports long polling, it already waited
            // If not, sleep here
            if !provider.supports_long_polling() {
                tokio::time::sleep(rt.options.dispatcher_idle_sleep).await;
            }
        }
        Err(e) => {
            // Error - log and retry after short delay
            tracing::warn!("Fetch error: {:?}", e);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
```

### Simplified Dispatcher Loop (Better Approach)

**Key insight:** Use `tokio::select!` to race fetch against shutdown, but let provider handle the long poll internally.

```rust
loop {
    if shutdown.load(Ordering::Relaxed) { break; }
    
    // Create a cancellation token for the fetch
    let shutdown_flag = shutdown.clone();
    let poll_timeout = rt.options.dispatcher_idle_sleep;
    
    // Fetch with long polling (provider checks shutdown internally)
    let fetch_result = provider.fetch_orchestration_item(
        rt.options.orchestrator_lock_timeout,
        Some(poll_timeout),  // Provider decides if it supports this
    ).await;
    
    match fetch_result {
        Ok(Some(item)) => {
            process(item);
        }
        Ok(None) => {
            // Provider already waited poll_timeout, or doesn't support long polling
            // No additional sleep needed
        }
        Err(e) => {
            tracing::warn!("Fetch error: {:?}", e);
            // Short sleep on error to avoid tight loop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
```

## Provider Implementation Requirements

### 1. Shutdown Handling During Long Poll

**Critical:** Providers MUST check for shutdown signals during long polls.

**Pattern:**
```rust
async fn fetch_with_long_poll(
    &self,
    poll_timeout: Duration,
    shutdown: Arc<AtomicBool>,
) -> Result<Option<Item>, ProviderError> {
    let start = Instant::now();
    let check_interval = Duration::from_millis(100);  // Check shutdown every 100ms
    
    while start.elapsed() < poll_timeout {
        // Check shutdown first
        if shutdown.load(Ordering::Relaxed) {
            return Ok(None);  // Graceful exit
        }
        
        // Try immediate fetch
        if let Some(item) = self.try_fetch_immediate().await? {
            return Ok(Some(item));
        }
        
        // Wait a bit before next check
        tokio::time::sleep(check_interval.min(poll_timeout - start.elapsed())).await;
    }
    
    Ok(None)
}
```

**For database-native long polling (PostgreSQL LISTEN/NOTIFY):**
```rust
async fn fetch_with_listen_notify(
    &self,
    poll_timeout: Duration,
    shutdown: Arc<AtomicBool>,
) -> Result<Option<Item>, ProviderError> {
    // Start listening
    sqlx::query("LISTEN orchestrator_queue_changes").execute(&mut *tx).await?;
    
    // Race: notification vs shutdown vs timeout
    let notification = async {
        // Wait for notification
        // This is database-specific
    };
    
    let shutdown_check = async {
        while !shutdown.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    
    let timeout = tokio::time::sleep(poll_timeout);
    
    tokio::select! {
        _ = notification => {
            // Notification received - fetch work
            self.try_fetch_immediate().await
        }
        _ = shutdown_check => {
            Ok(None)  // Shutdown requested
        }
        _ = timeout => {
            Ok(None)  // Timeout expired
        }
    }
}
```

### 2. Backward Compatibility

**Providers that don't support long polling:**
- Ignore `poll_timeout` parameter
- Return `Ok(None)` immediately
- Dispatcher will handle sleep (current behavior)

**Providers that support long polling:**
- Use `poll_timeout` to determine wait duration
- Return `Ok(None)` after timeout or when work arrives
- Must handle shutdown gracefully

## Configuration Changes

### RuntimeOptions Extension

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    /// Long polling timeout for dispatchers.
    /// 
    /// When providers support long polling, dispatchers will wait up to this duration
    /// for work to arrive before returning None.
    /// 
    /// - `None`: Disable long polling (providers return immediately)
    /// - `Some(duration)`: Enable long polling with this timeout
    /// 
    /// Default: `Some(dispatcher_idle_sleep)` - uses same value as idle sleep
    /// 
    /// **Note:** Not all providers support long polling. Providers that don't support
    /// it will ignore this setting and return immediately.
    pub dispatcher_long_poll_timeout: Option<Duration>,
}
```

**Default behavior:**
```rust
impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            dispatcher_idle_sleep: Duration::from_millis(100),
            dispatcher_long_poll_timeout: Some(Duration::from_millis(100)),  // Same as idle sleep
            // ... other fields ...
        }
    }
}
```

## Benefits of Long Polling

### Performance Improvements

1. **Reduced CPU Usage**
   - Current: Constant polling every 100ms = 10 queries/second per dispatcher
   - Long polling: 1 query per poll_timeout (e.g., 1 query per 100ms) but waits instead of spinning
   - **Savings:** ~90% reduction in idle CPU usage

2. **Reduced Database Load**
   - Current: 10 queries/second per dispatcher when idle
   - Long polling: 1 query per poll_timeout when idle
   - **Savings:** ~90% reduction in idle database queries

3. **Lower Latency for Work Arrival**
   - Current: Up to `dispatcher_idle_sleep` delay (100ms default)
   - Long polling: Immediate when work arrives (if provider supports notifications)
   - **Improvement:** Near-zero latency for work arrival

### Resource Efficiency

- **Database connections:** Fewer active queries = lower connection pool pressure
- **Network:** Fewer round trips to database
- **Scaling:** Better behavior with many dispatchers (linear scaling instead of multiplicative)

## Implementation Plan

### Phase 1: Interface Changes

1. **Add `poll_timeout` parameter to Provider trait**
   - Update `fetch_orchestration_item()` signature
   - Update `fetch_work_item()` signature
   - Add `supports_long_polling()` method (default: false)

2. **Update all existing providers**
   - SQLite: Ignore `poll_timeout`, return immediately
   - Other providers: Add stub implementation

### Phase 2: Dispatcher Updates

1. **Update orchestration dispatcher**
   - Pass `poll_timeout` from RuntimeOptions
   - Remove `dispatcher_idle_sleep` when provider supports long polling
   - Add shutdown handling during long polls

2. **Update worker dispatcher**
   - Same changes as orchestration dispatcher

### Phase 3: Provider-Specific Long Polling

1. **PostgreSQL provider** (if exists)
   - Implement LISTEN/NOTIFY for long polling
   - Handle shutdown gracefully

2. **Redis provider** (if exists)
   - Use BLPOP for long polling
   - Handle shutdown gracefully

3. **SQLite provider**
   - Option: Use WAL mode + busy_timeout for pseudo-long-polling
   - Or: Keep current behavior (immediate return)

### Phase 4: Testing

1. **Unit tests**
   - Test shutdown during long poll
   - Test immediate return when work available
   - Test timeout behavior

2. **Integration tests**
   - Test with multiple dispatchers
   - Test with high concurrency
   - Test shutdown behavior

3. **Performance tests**
   - Measure CPU usage reduction
   - Measure database query reduction
   - Measure latency improvements

## Edge Cases and Considerations

### 1. Shutdown During Long Poll

**Problem:** Dispatcher is blocked in long poll when shutdown requested.

**Solution:**
- Provider must check shutdown flag periodically during long poll
- Use `tokio::select!` in provider to race poll vs shutdown
- Return `Ok(None)` immediately when shutdown detected

### 2. Multiple Dispatchers

**Current:** Each dispatcher polls independently.

**Long Polling:** Each dispatcher still polls independently, but waits longer.

**Consideration:** With many dispatchers, still get some benefit (fewer queries per dispatcher).

### 3. Work Arrival During Long Poll

**Best case:** Provider supports notifications (PostgreSQL LISTEN/NOTIFY, Redis BLPOP)
- Work arrives → notification → immediate return
- Zero latency

**Fallback:** Provider polls internally (SQLite with WAL)
- Work arrives → next poll check → return
- Low latency (poll interval)

### 4. Provider Capability Detection

**Option A:** Runtime queries `supports_long_polling()` and adjusts behavior
- Simple, explicit
- Requires runtime to know about provider capabilities

**Option B:** Provider always accepts `poll_timeout`, ignores if not supported
- Simpler dispatcher code
- Provider decides internally

**Recommendation:** Option B (provider decides internally)

### 5. Configuration Tuning

**Guidelines:**
- `dispatcher_long_poll_timeout` should match `dispatcher_idle_sleep` for consistency
- Longer timeouts = less CPU, higher latency when work arrives
- Shorter timeouts = more CPU, lower latency
- Default: 100ms (same as current idle sleep)

## Migration Path

### Backward Compatibility

1. **Existing providers:** Continue to work (ignore `poll_timeout`)
2. **Existing dispatchers:** Continue to work (pass `None` for `poll_timeout`)
3. **New providers:** Can opt into long polling support

### Gradual Rollout

1. **Phase 1:** Add interface, keep behavior same (all providers return immediately)
2. **Phase 2:** Update dispatchers to pass `poll_timeout`
3. **Phase 3:** Implement long polling in providers that support it
4. **Phase 4:** Enable by default for supported providers

## Test Compatibility Issues

### Tests That Would Break

Several tests verify that `fetch_*` methods return `None` immediately when work is not available "yet":

1. **`test_abandon_orchestration_item_with_delay`** (`tests/provider_atomic_tests.rs:392-432`)
   - Abandons item with 500ms delay
   - Immediately checks `fetch_orchestration_item()` returns `None`
   - **Problem:** If long polling waits >500ms, it might find the item and break the test

2. **`test_timer_delayed_visibility`** (`src/provider_validation/queue_semantics.rs:260-289`)
   - Creates timer with 5 second delay
   - Immediately checks `fetch_orchestration_item()` returns `None`
   - **Problem:** If long polling waits >5 seconds, it might find the timer and break the test

3. **`test_lost_lock_token_handling`** (`src/provider_validation/queue_semantics.rs:293+`)
   - Fetches item (locks it)
   - Immediately checks second fetch returns `None` (item is locked)
   - **Problem:** If long polling waits, it might wait for lock expiration and break the test

### Solution: Test-Specific Immediate Return

**Option 1: Pass `None` for `poll_timeout` in tests (Recommended)**

Tests should explicitly pass `None` for `poll_timeout` to get immediate return behavior:

```rust
// In tests
let item = provider
    .fetch_orchestration_item(lock_timeout, None)  // Explicitly disable long polling
    .await
    .unwrap();
assert!(item.is_none());
```

**Option 2: Provider detects test mode**

Providers could detect test mode and always return immediately, but this is less explicit.

**Option 3: Short poll timeout for tests**

Tests could use a very short poll timeout (e.g., 1ms) to get near-immediate return while still testing the long polling path.

**Recommendation:** Option 1 - Tests should explicitly pass `None` for `poll_timeout` to ensure immediate return behavior. This makes test intent clear and doesn't require special test mode detection.

### Test Updates Required

**Files to update:**
1. `tests/provider_atomic_tests.rs` - `test_abandon_orchestration_item_with_delay`
2. `src/provider_validation/queue_semantics.rs` - `test_timer_delayed_visibility`, `test_lost_lock_token_handling`
3. Any other tests that verify immediate `None` return

**Pattern:**
```rust
// Before:
let item = provider.fetch_orchestration_item(lock_timeout).await.unwrap();

// After:
let item = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap();
//                                                                    ^^^^ Explicitly disable long polling
```

### Provider Behavior During Long Poll

**Critical:** Providers must respect `visible_at` timestamps during long polling.

If a provider supports long polling, it should:
1. Check for work with `visible_at <= now()` immediately
2. If none found and `poll_timeout` is `Some(duration)`:
   - Wait for new work to arrive OR timeout
   - **BUT:** Only return work that has `visible_at <= now()` at the time of return
   - **DO NOT** return work that becomes visible during the wait if it wasn't visible when the poll started

**Example:**
```rust
async fn fetch_with_long_poll(
    &self,
    poll_timeout: Option<Duration>,
) -> Result<Option<Item>, ProviderError> {
    let now = Self::now_millis();
    
    // Try immediate fetch
    if let Some(item) = self.try_fetch_immediate(now).await? {
        return Ok(Some(item));
    }
    
    // No work available now
    if let Some(poll_duration) = poll_timeout {
        let start = Instant::now();
        
        // Wait for new work, but check visibility on each iteration
        while start.elapsed() < poll_duration {
            // Check for shutdown
            if shutdown.load(Ordering::Relaxed) {
                return Ok(None);
            }
            
            // Check for new work (only return if visible_at <= now)
            let current_now = Self::now_millis();
            if let Some(item) = self.try_fetch_immediate(current_now).await? {
                return Ok(Some(item));
            }
            
            // Sleep a bit before next check
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    
    Ok(None)
}
```

**Key point:** Long polling should only return work that is **currently visible**, not work that becomes visible during the wait period. This ensures tests that check "not available yet" continue to work correctly.

## Summary

Long polling provides significant performance benefits:
- **~90% reduction** in idle CPU usage
- **~90% reduction** in idle database queries  
- **Near-zero latency** for work arrival (with notification support)
- **Better scalability** with many dispatchers

Key requirements:
- Providers must handle shutdown gracefully during long polls
- Backward compatible (providers can ignore `poll_timeout`)
- Configuration via `RuntimeOptions.dispatcher_long_poll_timeout`
- Provider capability detection via `supports_long_polling()`
- **Tests must pass `None` for `poll_timeout` to get immediate return behavior**
- **Providers must respect `visible_at` timestamps - only return work visible at poll start time**

