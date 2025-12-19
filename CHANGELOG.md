# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.5] - 2025-12-18

**Release:** <https://crates.io/crates/duroxide/0.1.5>

### Added

- **Provider identity API** - Providers now expose `name()` and `version()` methods
  - `Provider::name()` returns provider name (e.g., "sqlite")
  - `Provider::version()` returns provider version
  - Default implementations return "unknown" and "0.0.0"
  - SQLite provider returns "sqlite" and the crate version

- **Runtime startup banner** - Version information logged on startup
  - Logs duroxide version and provider name/version
  - Example: `duroxide runtime (0.1.4) starting with provider sqlite (0.1.4)`

- **Worker queue visibility control** - Worker queue now uses `visible_at` for delayed visibility
  - Added `visible_at` column to worker_queue (matches orchestrator queue pattern)
  - `abandon_work_item` with delay now sets `visible_at` instead of keeping `locked_until`
  - Cleaner semantics: `visible_at` controls when item becomes visible, `locked_until` only for lock expiry
  - Migration file included for existing databases

- **New provider validation tests** - 2 additional queue semantics tests
  - `test_worker_item_immediate_visibility` - Verify newly enqueued items are immediately visible
  - `test_worker_delayed_visibility_skips_future_items` - Verify items with future visible_at are skipped

### Changed

- **Reduced default `dispatcher_long_poll_timeout`** from 5 minutes to 30 seconds
  - More responsive shutdown behavior
  - Better suited for typical workloads

## [0.1.3] - 2025-12-14

### Added

- **Provider validation tests** - 4 new tests for abandon and poison handling
  - `test_abandon_work_item_releases_lock` - Verify abandon_work_item releases lock immediately
  - `test_abandon_work_item_with_delay` - Verify abandon_work_item with delay defers refetch
  - `max_attempt_count_across_message_batch` - Verify MAX attempt_count returned for batched messages

### Changed

- Provider validation test count increased from 58 to 62

### Fixed

- `abandon_work_item` with delay now correctly keeps lock_token to prevent immediate refetch

## [0.1.2] - 2025-12-14

### Added

- **Poison message handling** - Automatic detection and failure of messages that exceed `max_attempts` (default: 10)
  - `RuntimeOptions::max_attempts` configuration option
  - `ErrorDetails::Poison` variant with detailed context
  - `PoisonMessageType` enum distinguishing orchestration vs activity poison
  - Dedicated metrics: `duroxide_orchestration_poison_total`, `duroxide_activity_poison_total`

- **Lock renewal for orchestrations** - Prevents lock expiration during long orchestration turns
  - `Provider::renew_orchestration_item_lock()` method
  - `RuntimeOptions::orchestrator_lock_renewal_buffer` configuration (default: 2s)
  - Automatic background renewal task in orchestration dispatcher

- **Work item abandon with retry** - Explicit lock release for failed activities
  - `Provider::abandon_work_item()` method with optional delay
  - Called automatically when `ack_work_item` fails

- **Attempt count management** - `ignore_attempt` parameter for abandon methods
  - `abandon_work_item(..., ignore_attempt: bool)` - decrement count on transient failures
  - `abandon_orchestration_item(..., ignore_attempt: bool)` - same for orchestrations
  - Prevents false poison detection from infrastructure errors

- **Provider validation tests** - 8 new poison message tests
  - `orchestration_attempt_count_starts_at_one`
  - `orchestration_attempt_count_increments_on_refetch`
  - `worker_attempt_count_starts_at_one`
  - `worker_attempt_count_increments_on_lock_expiry`
  - `attempt_count_is_per_message`
  - `abandon_work_item_ignore_attempt_decrements`
  - `abandon_orchestration_item_ignore_attempt_decrements`
  - `ignore_attempt_never_goes_negative`

### Changed

- **BREAKING:** SQLite provider is now optional - enable with `features = ["sqlite"]`
- **BREAKING:** `Provider::fetch_work_item` now returns `(WorkItem, String, u32)` tuple (added `attempt_count`)
- **BREAKING:** `Provider::fetch_orchestration_item` now returns `(OrchestrationItem, String, u32)` tuple (added `attempt_count`)
- **BREAKING:** `Provider::abandon_work_item` now requires `ignore_attempt: bool` parameter
- **BREAKING:** `Provider::abandon_orchestration_item` now requires `ignore_attempt: bool` parameter
- `OrchestrationItem` struct no longer contains `lock_token` (moved to return tuple)
- Provider validation test count increased from 50 to 58

### Migration Guide

**Cargo.toml (if using SQLite provider):**
```toml
# Before
duroxide = "0.1.1"

# After - SQLite now requires explicit feature
duroxide = { version = "0.1.2", features = ["sqlite"] }
```

**Provider implementers:**
```rust
// fetch_work_item now returns attempt_count
async fn fetch_work_item(...) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;

// fetch_orchestration_item now returns attempt_count
async fn fetch_orchestration_item(...) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError>;

// abandon methods now have ignore_attempt parameter
async fn abandon_work_item(&self, token: &str, delay: Option<Duration>, ignore_attempt: bool) -> Result<(), ProviderError>;
async fn abandon_orchestration_item(&self, token: &str, delay: Option<Duration>, ignore_attempt: bool) -> Result<(), ProviderError>;

// New method for orchestration lock renewal
async fn renew_orchestration_item_lock(&self, token: &str, extend_for: Duration) -> Result<(), ProviderError>;
```

**Runtime users:**
```rust
RuntimeOptions {
    max_attempts: 10,  // NEW - poison threshold
    orchestrator_lock_renewal_buffer: Duration::from_secs(2),  // NEW
    ..Default::default()
}
```

## [0.1.1] - 2025-12-10

### Added

- **Long polling support** - Providers can now block waiting for work, reducing CPU usage and latency
- `dispatcher_long_poll_timeout` configuration option (default: 5 minutes)
- `poll_timeout: Duration` parameter to `Provider::fetch_orchestration_item` and `Provider::fetch_work_item`
- Long polling validation tests in `duroxide::provider_validations::long_polling`

### Changed

- **BREAKING:** `Provider::fetch_orchestration_item` now requires `poll_timeout: Duration` parameter
- **BREAKING:** `Provider::fetch_work_item` now requires `poll_timeout: Duration` parameter
- **BREAKING:** `RuntimeOptions::dispatcher_idle_sleep` renamed to `dispatcher_min_poll_interval`
- **BREAKING:** `continue_as_new()` now returns an awaitable future (use `return ctx.continue_as_new(input).await`)

### Migration Guide

**Provider implementers:**
```rust
// Add poll_timeout parameter to both fetch methods
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,  // NEW - ignore for short-polling, block for long-polling
) -> Result<Option<OrchestrationItem>, ProviderError>;
```

**Runtime users:**
```rust
// Rename dispatcher_idle_sleep to dispatcher_min_poll_interval
RuntimeOptions {
    dispatcher_min_poll_interval: Duration::from_millis(100),
    dispatcher_long_poll_timeout: Duration::from_secs(300),  // NEW
    ..Default::default()
}
```

**Orchestration authors using continue_as_new:**
```rust
// Before: ctx.continue_as_new(input);
// After:
return ctx.continue_as_new(input).await;
```

## [0.1.0] - 2025-12-01

### Added

- Initial release
- Deterministic orchestration execution with replay
- Activity scheduling with automatic retries
- Timer support (create_timer)
- Sub-orchestration support
- External event handling
- Continue-as-new for long-running workflows
- SQLite provider implementation
- OpenTelemetry metrics and structured logging
- Provider validation test suite
- Comprehensive documentation


