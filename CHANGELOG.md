# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.9] - 2026-01-05

**Release:** <https://crates.io/crates/duroxide/0.1.9>

### Added

- **Management API for Instance Deletion and Pruning** - Comprehensive instance lifecycle management

  **Client API:**
  - `delete_instance(id, force)` - Delete single instance with cascading
  - `delete_instance_bulk(filter)` - Bulk delete with filters (IDs, timestamp, limit)
  - `prune_executions(id, options)` - Prune old executions from long-running instances
  - `prune_executions_bulk(filter, options)` - Bulk prune across multiple instances
  - `get_instance_tree(id)` - Inspect instance hierarchy before deletion

  **Provider API (ProviderAdmin trait):**
  - `delete_instance(id, force)` - Provider-level single deletion
  - `delete_instance_bulk(filter)` - Provider-level bulk deletion
  - `delete_instances_atomic(ids)` - Atomic batch deletion for cascading
  - `prune_executions(id, options)` - Provider-level pruning
  - `prune_executions_bulk(filter, options)` - Provider-level bulk pruning
  - `get_instance_tree(id)` - Provider-level tree traversal
  - `list_children(id)` - List direct child sub-orchestrations
  - `get_parent_id(id)` - Get parent instance ID

  **Safety Guarantees:**
  - Running instances protected (skip or error based on API)
  - Current execution never pruned
  - Sub-orchestrations cannot be deleted directly (must delete root)
  - Atomic cascading deletes (all-or-nothing)
  - Force delete available for stuck instances

- **102 new provider validation tests** - Deletion, bulk deletion, pruning, cascading deletes, filter combinations, safety tests

### Changed

- Provider implementation guide with deletion/pruning contracts
- Provider testing guide updates
- Continue-as-new docs with pruning section
- README instance management section
- Enhanced management-api-deletion proposal with force delete semantics

## [0.1.8] - 2026-01-02

**Release:** <https://crates.io/crates/duroxide/0.1.8>

### Added

- **Lock-stealing activity cancellation** - New mechanism for cancelling in-flight activities
  - Activities are cancelled by deleting their worker queue entries ("lock stealing")
  - Workers detect cancellation when lock renewal fails (entry missing)
  - More efficient than polling execution state on every renewal
  - Enables batch cancellation of multiple activities atomically

- **`ScheduledActivityIdentifier`** - New struct for identifying activities in worker queue
  - Fields: `instance` (String), `execution_id` (u64), `activity_id` (u64)
  - Used by `ack_orchestration_item` to specify activities to cancel
  - Exported from `duroxide::providers`

- **Provider validation tests for lock-stealing** - 5 new tests
  - `test_cancelled_activities_deleted_from_worker_queue` - Verify deletion during ack
  - `test_ack_work_item_fails_when_entry_deleted` - Verify permanent error on stolen lock
  - `test_renew_fails_when_entry_deleted` - Verify renewal fails on stolen lock
  - `test_cancelling_nonexistent_activities_is_idempotent` - Verify no error for missing entries
  - `test_batch_cancellation_deletes_multiple_activities` - Verify batch deletion

- **Worker queue activity identity columns** - Store activity identity for cancellation
  - New migration: `20240104000000_add_worker_activity_identity.sql`
  - SQLite provider stores `instance_id`, `execution_id`, `activity_id` on ActivityExecute items

### Changed

- **BREAKING:** `Provider::ack_orchestration_item` signature changed
  - Added 7th parameter: `cancelled_activities: Vec<ScheduledActivityIdentifier>`
  - Provider must delete matching worker queue entries atomically in same transaction

- **BREAKING:** `Provider::fetch_work_item` return type simplified
  - Changed from `(WorkItem, String, u32, ExecutionState)` to `(WorkItem, String, u32)`
  - Removed `ExecutionState` - cancellation detected via lock renewal failure instead

- **BREAKING:** `Provider::renew_work_item_lock` return type changed
  - Changed from `Result<ExecutionState, ProviderError>` to `Result<(), ProviderError>`
  - Failure indicates lock was stolen (activity cancelled) or expired

- **BREAKING:** `Provider::ack_work_item` must fail when entry missing
  - Returns permanent error if work item entry was deleted (lock stolen)
  - Signals to worker that activity was cancelled

- Provider validation test count: 80 tests (up from 75)

### Removed

- **`ExecutionState` enum removed from Provider API** - No longer needed
  - Was used for state-polling cancellation approach
  - Lock-stealing provides more efficient cancellation mechanism
  - Provider validation tests for ExecutionState still exist (legacy support during migration)

### Migration Guide

**Provider implementers - Required changes:**

1. Update `ack_orchestration_item` signature:
```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
    cancelled_activities: Vec<ScheduledActivityIdentifier>,  // NEW
) -> Result<(), ProviderError>;
```

2. Update `fetch_work_item` return type:
```rust
async fn fetch_work_item(...) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
// Removed ExecutionState from tuple
```

3. Update `renew_work_item_lock` return type:
```rust
async fn renew_work_item_lock(...) -> Result<(), ProviderError>;
// Returns () instead of ExecutionState
```

4. Update `ack_work_item` to fail on missing entry:
```rust
// Return error if entry not found (lock was stolen)
if rows_affected == 0 {
    return Err(ProviderError::permanent("ack_work_item", "Entry not found (lock stolen)"));
}
```

5. Store activity identity on worker queue entries:
```sql
-- Add columns to worker_queue table
ALTER TABLE worker_queue ADD COLUMN instance_id TEXT;
ALTER TABLE worker_queue ADD COLUMN execution_id INTEGER;
ALTER TABLE worker_queue ADD COLUMN activity_id INTEGER;

-- Add index for efficient cancellation
CREATE INDEX idx_worker_queue_activity ON worker_queue(instance_id, execution_id, activity_id);
```

6. Implement batch deletion in `ack_orchestration_item`:
```rust
// Delete cancelled activities atomically within the ack transaction
for activity in cancelled_activities {
    DELETE FROM worker_queue 
    WHERE instance_id = activity.instance 
      AND execution_id = activity.execution_id 
      AND activity_id = activity.activity_id;
}
```

## [0.1.7] - 2025-12-28

**Release:** <https://crates.io/crates/duroxide/0.1.7>

### Added

- **Cooperative activity cancellation** - Activities can detect when their parent orchestration has been cancelled or completed
  - `ActivityContext` now provides cancellation awareness via `is_cancelled()` and `cancelled()` methods
  - Activities can cooperatively respond to cancellation by checking the cancellation token
  - Use `tokio::select!` with `ctx.cancelled()` for responsive cancellation in async activities
  - Configurable grace period before forced activity termination

- **ExecutionState enum** - Providers now report orchestration state with activity work items
  - `ExecutionState::Running` - Orchestration is active, activity should proceed
  - `ExecutionState::Terminal { status }` - Orchestration completed/failed/continued, activity result won't be observed
  - `ExecutionState::Missing` - Orchestration instance deleted, activity should abort

- **Provider validation tests for cancellation** - 13 new tests in `provider_validation::cancellation`
  - Verifies `ExecutionState` is correctly returned by `fetch_work_item` and `renew_work_item_lock`
  - Tests for Running, Terminal (Completed/Failed/ContinuedAsNew), and Missing states
  - Tests for state transitions during activity execution

- **Single-threaded runtime support** - Full compatibility with `tokio::runtime::Builder::new_current_thread()`
  - Essential for embedding in single-threaded environments (e.g., pgrx PostgreSQL extensions)
  - New scenario tests in `tests/scenarios/single_thread.rs`
  - Use `RuntimeOptions { orchestration_concurrency: 1, worker_concurrency: 1, .. }` for 1x1 mode

- **Configurable wait timeout for stress tests** - `StressTestConfig::wait_timeout_secs` field
  - Default: 60 seconds
  - Increase for high-latency remote database providers
  - Uses `#[serde(default)]` for backward compatibility with existing configs

### Changed

- **BREAKING:** `Provider::fetch_work_item` now returns 4-tuple: `(WorkItem, String, u32, ExecutionState)`
  - Added `ExecutionState` as fourth element to report parent orchestration state
  - Required for activity cancellation support

- **BREAKING:** `Provider::renew_work_item_lock` now returns `ExecutionState` instead of `()`
  - Allows runtime to detect orchestration state changes during long-running activities
  - Triggers cancellation token when orchestration becomes terminal

- Provider validation test count increased from 62 to 75

- Documentation updates:
  - Added "Runtime Polling Configuration" section to provider-implementation-guide
  - Default polling interval (10ms) is aggressive; configure for remote/cloud providers
  - Updated provider-testing-guide with new test count and wait_timeout_secs examples

### Fixed

- **test_worker_lock_renewal_extends_timeout** - Fixed timing sensitivity (GitHub #34)
  - Test now creates proper orchestration with Running status before testing renewal
  - Uses 0.6x pre-renewal wait + 0.4x post-renewal wait for reliable timing

- **test_multi_threaded_lock_expiration_recovery** - Fixed race condition (GitHub #32)
  - Uses `tokio::sync::Barrier` to synchronize thread start times
  - Eliminates false failures from connection pool cold-start latency

### Migration Guide

**Provider implementers:**
```rust
// fetch_work_item now returns ExecutionState
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError>;

// renew_work_item_lock now returns ExecutionState
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_for: Duration,
) -> Result<ExecutionState, ProviderError>;
```

**Determining ExecutionState:**
```rust
// Query the execution status for the work item's instance/execution_id
let state = match (instance_exists, execution_status) {
    (false, _) => ExecutionState::Missing,
    (true, None) => ExecutionState::Missing,
    (true, Some(status)) if status == "Running" => ExecutionState::Running,
    (true, Some(status)) => ExecutionState::Terminal { status },
};
```

**Activity authors (using cancellation):**
```rust
activities.register("LongTask", |ctx: ActivityContext, input: String| async move {
    for item in items {
        // Check cancellation periodically
        if ctx.is_cancelled() {
            return Err("Cancelled".into());
        }
        process(item).await;
    }
    Ok("done".into())
});

// Or use select! for responsive cancellation
activities.register("AsyncTask", |ctx: ActivityContext, input: String| async move {
    tokio::select! {
        result = do_work(input) => result,
        _ = ctx.cancelled() => Err("Cancelled".into()),
    }
});
```

## [0.1.6] - 2025-12-21

**Release:** <https://crates.io/crates/duroxide/0.1.6>

### Added

- **Large payload stress test** - New memory-intensive stress test scenario
  - Tests large event payloads (10KB, 50KB, 100KB) and longer histories (~80-100 events)
  - New binary: `large-payload-stress` for running the test standalone
  - Uses the same `ProviderStressFactory` trait as parallel orchestrations test
  - Configurable payload sizes and activity/sub-orchestration counts
  - See `docs/provider-testing-guide.md` for usage

- **Stress test monitoring** - Resource usage tracking in `run-stress-tests.sh`
  - Peak RSS (Resident Set Size) measurement
  - Average CPU usage tracking
  - Sampling every 500ms during test execution
  - New documentation: `STRESS_TEST_MONITORING.md`
  - Supports `--parallel-only` and `--large-payload` flags

### Changed

- **Memory optimization** - Reduced allocations in history processing
  - Added `HistoryManager::full_history_len()` - get count without allocation
  - Added `HistoryManager::is_full_history_empty()` - check emptiness without allocation
  - Added `HistoryManager::full_history_iter()` - iterate without allocation
  - Updated runtime to use efficient methods in hot paths
  - Improved child cancellation to use iterator instead of collecting full history

- **Orchestration naming** - Renamed "FanoutWorkflow" to "FanoutOrchestration" for consistency

### Fixed

- Child sub-orchestration cancellation now uses iterator-based approach for better memory efficiency

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


