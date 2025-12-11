# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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


