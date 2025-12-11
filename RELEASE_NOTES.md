# Release Notes - v0.1.1

## Summary

This release adds **long polling support** to the Provider interface and makes `continue_as_new()` awaitable. These changes enable more efficient providers that can block waiting for work, reducing CPU usage and latency.

---

## Breaking Changes

### 1. Provider Trait: New `poll_timeout` Parameter

The `fetch_orchestration_item` and `fetch_work_item` methods now require a `poll_timeout: Duration` parameter:

**Before (0.1.0):**
```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
) -> Result<Option<OrchestrationItem>, ProviderError>;

async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
) -> Result<Option<(WorkItem, String)>, ProviderError>;
```

**After (0.1.1):**
```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,  // NEW
) -> Result<Option<OrchestrationItem>, ProviderError>;

async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,  // NEW
) -> Result<Option<(WorkItem, String)>, ProviderError>;
```

**Migration for Provider Implementers:**
- Add `poll_timeout: Duration` parameter to both methods
- **Short-polling providers** (like SQLite): Ignore `poll_timeout` and return immediately
- **Long-polling providers** (like Redis, Azure Service Bus): MAY block up to `poll_timeout` waiting for work

### 2. RuntimeOptions: Renamed and New Fields

**Before (0.1.0):**
```rust
RuntimeOptions {
    dispatcher_idle_sleep: Duration,  // Default: 100ms
    // ...
}
```

**After (0.1.1):**
```rust
RuntimeOptions {
    dispatcher_min_poll_interval: Duration,    // Renamed from dispatcher_idle_sleep
    dispatcher_long_poll_timeout: Duration,    // NEW - Default: 5 minutes
    // ...
}
```

**Migration for Runtime Users:**
- Rename `dispatcher_idle_sleep` to `dispatcher_min_poll_interval`
- Optionally configure `dispatcher_long_poll_timeout` (default: 5 minutes)

### 3. `continue_as_new()` Now Returns Awaitable Future

**Before (0.1.0):**
```rust
ctx.continue_as_new(input);  // Fire-and-forget, returns ()
Ok(())
```

**After (0.1.1):**
```rust
return ctx.continue_as_new(input).await;  // Must await and return
```

**Migration:**
- Add `.await` after `continue_as_new()` calls
- Use `return` to ensure code after is unreachable
- The compiler will warn about unreachable code if you forget `return`

---

## New Features

### Long Polling Support

Providers can now implement efficient long polling. The dispatcher passes `poll_timeout` to providers, allowing them to block waiting for work instead of returning immediately and sleeping.

**Benefits:**
- Reduced CPU usage for idle systems
- Lower latency when work arrives
- Better integration with message queues that support blocking reads

**How it works:**
1. Dispatcher calls `provider.fetch_*(lock_timeout, poll_timeout)` 
2. Long-polling providers MAY block up to `poll_timeout`
3. Short-polling providers ignore `poll_timeout` and return immediately
4. Dispatcher enforces `min_poll_interval` to prevent hot loops

### New Provider Validation Tests

Added validation tests for polling behavior in `duroxide::provider_validations::long_polling`:

- `test_short_poll_returns_immediately` - Verifies short-polling providers return promptly
- `test_short_poll_work_item_returns_immediately` - Same for worker queue
- `test_fetch_respects_timeout_upper_bound` - Ensures providers don't block excessively

---

## Documentation Updates

- **`docs/provider-implementation-guide.md`** - Updated with `poll_timeout` parameter
- **`docs/continue-as-new.md`** - Documented awaitable API with usage examples
- **`docs/ORCHESTRATION-GUIDE.md`** - Updated all `continue_as_new` examples
- **`docs/long-polling-behavior-plan.md`** - Complete long polling design

---

## For Users

**If you only use the Client API:**
- No changes required unless you configure `RuntimeOptions` directly
- Rename `dispatcher_idle_sleep` to `dispatcher_min_poll_interval` if you use it

**If you use `continue_as_new()`:**
- Update calls to use `return ctx.continue_as_new(input).await`

---

## For Provider Developers

**Required changes:**
1. Add `poll_timeout: Duration` parameter to `fetch_orchestration_item` and `fetch_work_item`
2. Decide whether to implement long polling:
   - **No long polling:** Ignore `poll_timeout`, return immediately (like SQLite)
   - **Long polling:** Block up to `poll_timeout` waiting for work

**Example (short-polling, ignores timeout):**
```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,  // Ignored for short-polling
) -> Result<Option<OrchestrationItem>, ProviderError> {
    // Existing implementation unchanged
}
```

**Example (long-polling with Redis):**
```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<OrchestrationItem>, ProviderError> {
    // Use BLPOP with poll_timeout
    let result = self.redis.blpop(&self.queue_key, poll_timeout).await?;
    // ... process result
}
```

---

## Full Changelog

- Add `poll_timeout` parameter to Provider trait fetch methods
- Rename `dispatcher_idle_sleep` → `dispatcher_min_poll_interval`
- Add `dispatcher_long_poll_timeout` configuration (default: 5 minutes)
- Make `continue_as_new()` return awaitable future
- Add long polling validation tests
- Update dispatcher loop to support long polling providers
- Update documentation for all changes

