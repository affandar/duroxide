# Long Polling Behavior

## Overview

Duroxide supports both short-polling and long-polling providers through a unified interface. The key insight is that providers don't need to advertise their polling capability - they simply respect (or ignore) the `poll_timeout` parameter.

## Provider Interface

```rust
trait Provider {
    async fn fetch_orchestration_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Duration,
    ) -> Result<Option<OrchestrationItem>, ProviderError>;

    async fn fetch_work_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Duration,
    ) -> Result<Option<(WorkItem, String)>, ProviderError>;
}
```

### poll_timeout Semantics

- **Long-polling providers** (e.g., Azure Service Bus, Redis with BLPOP): MAY block up to `poll_timeout` waiting for work to arrive. Should return early if work becomes available.
  
- **Short-polling providers** (e.g., SQLite, PostgreSQL): Ignore `poll_timeout` and return immediately. The dispatcher handles the waiting.

## Dispatcher Behavior

The dispatcher uses two settings to control polling:

1. **`dispatcher_long_poll_timeout`**: Maximum time passed to provider's `poll_timeout` (default: 30 seconds)
2. **`dispatcher_min_poll_interval`**: Minimum cycle duration when no work found (default: 100ms)

```rust
loop {
    if shutdown.load(Ordering::Relaxed) { break; }
    
    let min_interval = rt.options.dispatcher_min_poll_interval;
    let long_poll_timeout = rt.options.dispatcher_long_poll_timeout;
    
    let start_time = Instant::now();
    let mut work_found = false;
    
    // Always pass the timeout - provider decides whether to use it
    match provider.fetch_orchestration_item(lock_timeout, long_poll_timeout).await {
        Ok(Some(item)) => {
            process(item);
            work_found = true;
        },
        Ok(None) => { /* No work available */ },
        Err(e) => {
            log_error(e);
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
    }
    
    // Enforce min_poll_interval to prevent hot loops
    if !work_found {
        let elapsed = start_time.elapsed();
        if elapsed < min_interval {
            let sleep_duration = min_interval - elapsed;
            if !shutdown.load(Ordering::Relaxed) {
                tokio::time::sleep(sleep_duration).await;
            }
        } else {
            // Waited long enough (long poll expired), yield and continue
            tokio::task::yield_now().await;
        }
    }
}
```

## Configuration

### RuntimeOptions

```rust
pub struct RuntimeOptions {
    /// Minimum duration of a polling cycle when no work is found.
    /// 
    /// If a provider returns 'None' faster than this duration,
    /// the dispatcher sleeps for the remainder. Prevents hot loops
    /// for short-polling providers.
    /// 
    /// Default: 100ms
    pub dispatcher_min_poll_interval: Duration,

    /// Maximum time to wait for work inside the provider.
    /// 
    /// Passed to provider's fetch methods. Long-polling providers
    /// may block up to this duration. Short-polling providers ignore it.
    /// 
    /// Default: 30 seconds
    pub dispatcher_long_poll_timeout: Duration,
}
```

## Provider Implementations

### Short-Polling (SQLite)

```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,  // Ignored
) -> Result<Option<OrchestrationItem>, ProviderError> {
    // Immediately check queue and return
    // poll_timeout is ignored - dispatcher handles waiting
}
```

### Long-Polling (Example: Redis)

```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<OrchestrationItem>, ProviderError> {
    // Use BLPOP with poll_timeout
    // Returns early if work arrives, or None after timeout
    let result = redis.blpop(&self.queue_key, poll_timeout).await?;
    // ... process result
}
```

## Benefits

- **Unified interface**: No capability flags needed
- **Robustness**: `min_poll_interval` prevents hot loops even if long-polling providers misbehave
- **Efficiency**: Long-polling providers can wait minutes without wasting CPU
- **Backward compatible**: Existing providers work unchanged (just ignore `poll_timeout`)

## Testing

Tests typically pass `Duration::ZERO` for `poll_timeout` to ensure immediate return behavior, which is correct for testing short-polling scenarios:

```rust
provider.fetch_orchestration_item(lock_timeout, Duration::ZERO).await
```

For long-polling validation tests, use non-zero timeouts:

```rust
provider.fetch_orchestration_item(lock_timeout, Duration::from_secs(1)).await
```
