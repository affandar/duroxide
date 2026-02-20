# Proposal: Custom Status / Progress Reporting

## Problem Statement

Orchestrations today are opaque while running — clients can only see `Running` or a terminal state (`Completed`/`Failed`). There is no way for an orchestration to report intermediate progress. Users of the durable-copilot-sdk resorted to abusing `ActivityCompleted` events as a side channel for intermediate content, which is fragile and semantically wrong.

## Design

### Orchestration API

```rust
// Set the custom status (last write per turn wins)
ctx.set_custom_status("Processing item 3 of 10");

// Structured data via JSON
ctx.set_custom_status(serde_json::to_string(&MyProgress { step: 3, total: 10 }).unwrap());

// Clear it
ctx.set_custom_status("");
```

`set_custom_status()` is **not** a history event or an action. It's pure metadata — a field on `CtxInner` that gets plumbed into `ExecutionMetadata` at ack time. No impact on determinism, no replay implications.

- Imperative: call it whenever, as many times as you want within a turn
- Last write wins: if called twice in the same turn, only the last value is sent
- Persistent across turns: if you don't call it on a later turn, the provider keeps the previous value
- Not reset on CAN: carries forward as a provider-level column (not in history)

### Provider Changes

**`ExecutionMetadata`** — new field:

```rust
pub struct ExecutionMetadata {
    // ... existing fields ...
    /// User-defined status string for progress reporting.
    /// Updated on every ack when set. Provider preserves the last value
    /// across turns if not set (None = no update).
    pub custom_status: Option<String>,
}
```

**Schema migration** — new columns on `executions` table:

```sql
ALTER TABLE executions ADD COLUMN custom_status TEXT;
ALTER TABLE executions ADD COLUMN custom_status_version INTEGER NOT NULL DEFAULT 0;
```

**Provider contract:**
- `ack_orchestration_item()`: if `metadata.custom_status` is `Some(s)`, write `s` to the `custom_status` column and increment `custom_status_version`. If `None`, don't touch either column (preserve previous value).
- Reading: return `custom_status` and `custom_status_version` alongside status/output when building `OrchestrationStatus`.

### Client API

**`OrchestrationStatus`** — `custom_status` and `custom_status_version` on all non-`NotFound` variants:

```rust
pub enum OrchestrationStatus {
    /// Instance does not exist
    NotFound,
    /// Instance is currently executing
    Running { custom_status: Option<String>, custom_status_version: u64 },
    /// Instance completed successfully with output
    Completed { output: String, custom_status: Option<String>, custom_status_version: u64 },
    /// Instance failed with structured error details
    Failed { details: ErrorDetails, custom_status: Option<String>, custom_status_version: u64 },
}
```

Rationale for including on `Completed`/`Failed`:
- The last progress update set during execution is useful context even after termination
- `Completed`: "Processed 10/10 items" — tells you the final progress state alongside the output
- `Failed`: "Processing item 7/10" — tells you *where* it failed, complementing the error details
- Without it, the custom status is lost the moment you read the terminal status — you'd have to have been polling at exactly the right moment

**Breaking change:** `Running` changes from a unit variant to a struct variant. All existing `Running =>` matches need to become `Running { .. } =>`. This is a semver-minor bump, and the fix is mechanical.

**`get_orchestration_status()`** — reads `custom_status` from the provider alongside `status`/`output`:

```rust
match client.get_orchestration_status("order-123").await? {
    OrchestrationStatus::Running { custom_status } => {
        if let Some(status) = custom_status {
            println!("Progress: {status}");
        }
    }
    OrchestrationStatus::Completed { output, custom_status } => {
        println!("Done: {output}");
        if let Some(status) = custom_status {
            println!("Last progress: {status}");
        }
    }
    OrchestrationStatus::Failed { details, custom_status } => {
        eprintln!("Failed: {}", details.display_message());
        if let Some(status) = custom_status {
            eprintln!("Failed at: {status}");
        }
    }
    _ => {}
}
```

### Flow

1. Orchestration calls `ctx.set_custom_status("step 3/10")`
2. Value stored in `CtxInner.custom_status`
3. Turn ends → dispatcher reads `custom_status` from context
4. Plumbed into `ExecutionMetadata { custom_status: Some("step 3/10"), .. }`
5. `ack_orchestration_item()` writes it to provider (`UPDATE executions SET custom_status = ?`)
6. Client calls `get_orchestration_status()` → provider reads `custom_status` column → returned on the enum variant

### What This Is NOT

- **Not a history event** — no `EventKind::CustomStatusSet`. It's metadata, not part of the replay-visible history. This avoids determinism concerns and history growth.
- **Not activity progress** — activities cannot set custom status. That's a separate feature (Part B: Activity → Orchestration progress reporting) with different execution model challenges.
- **Not a full streaming API** — `wait_for_status_change()` provides efficient change notification via long polling with version tracking. Full event streaming (e.g., `client.watch_execution()`) is a separate, larger feature.

### Change Notification API

**`client.wait_for_status_change()`** — blocks until the custom status changes or the orchestration reaches a terminal state:

```rust
/// Wait until the custom_status_version exceeds `last_seen_version`,
/// the orchestration reaches a terminal state (Completed/Failed),
/// or the timeout is hit.
///
/// Returns the current OrchestrationStatus with the new custom_status and version.
client.wait_for_status_change(
    instance: &str,
    last_seen_version: u64,    // pass 0 on first call to get current state immediately
    poll_interval: Duration,   // how often to check (e.g., 100ms for dashboards, 1s for background)
    timeout: Duration,         // max wait time (Duration::MAX for indefinite)
) -> Result<OrchestrationStatus, ClientError>
```

**Semantics:**
- If `custom_status_version > last_seen_version`, returns immediately with current state
- If the orchestration is already in a terminal state (`Completed`/`Failed`), returns immediately
- Otherwise, polls the provider at fixed `poll_interval` until version changes, terminal state, or timeout
- Timeout returns `ClientError::Timeout` (not an orchestration error)
- No backoff — this is for live dashboards and interactive UIs where consistent latency matters

**Version semantics:**
- Version starts at 0 (no custom status ever set)
- Incremented by 1 on every `set_custom_status()` call that's acked (once per turn, only if set)
- Monotonically increasing within an execution
- Reset to 0 on CAN (new execution)
- Handles the edge case where status is set to the same value twice — version increments, so the client is notified even though the string didn't change

**Usage pattern:**

```rust
let mut last_version = 0;

loop {
    match client.wait_for_status_change("order-123", last_version, Duration::from_millis(200), Duration::from_secs(30)).await? {
        OrchestrationStatus::Running { custom_status, custom_status_version } => {
            last_version = custom_status_version;
            if let Some(status) = custom_status {
                println!("Progress: {status}");
            }
        }
        OrchestrationStatus::Completed { output, .. } => {
            println!("Done: {output}");
            break;
        }
        OrchestrationStatus::Failed { details, custom_status, .. } => {
            eprintln!("Failed: {}", details.display_message());
            if let Some(status) = custom_status {
                eprintln!("Failed at: {status}");
            }
            break;
        }
        _ => break,
    }
}
```

**Implementation:**
- `wait_for_status_change()` lives in the `Client`
- The client internally calls `Provider::get_custom_status(instance, last_seen_version)` in a loop at `poll_interval`
- `get_custom_status()` returns `Some((custom_status, version))` if `version > last_seen_version`, `None` otherwise
- When `Some` is returned, the client does one final `get_orchestration_status()` to return the full `OrchestrationStatus`
- Also returns on terminal state or timeout

**Provider trait — new method:**

```rust
/// Lightweight check for custom status changes.
/// Returns Some((custom_status, version)) if version > last_seen_version,
/// None if unchanged.
/// SQL: SELECT custom_status, custom_status_version
///      FROM executions WHERE instance_id = ?
///      AND custom_status_version > ?
async fn get_custom_status(
    &self,
    instance: &str,
    last_seen_version: u64,
) -> Result<Option<(Option<String>, u64)>, ProviderError>;
```

### Flow

1. Orchestration calls `ctx.set_custom_status("step 3/10")`
2. Value stored in `CtxInner.custom_status`
3. Turn ends → dispatcher reads `custom_status` from context
4. Plumbed into `ExecutionMetadata { custom_status: Some("step 3/10"), .. }`
5. `ack_orchestration_item()` writes to provider: `UPDATE executions SET custom_status = ?, custom_status_version = custom_status_version + 1`
6. Client calls `wait_for_status_change("order-123", 2, 200ms, 30s)` → polls `get_custom_status("order-123", 2)` every 200ms → returns `Some` when version > 2 → client calls `get_orchestration_status()` for full status

## Implementation Plan

1. **Schema migration** — add `custom_status TEXT` and `custom_status_version INTEGER NOT NULL DEFAULT 0` columns to `executions`
2. **`ExecutionMetadata`** — add `custom_status: Option<String>` field
3. **`Provider` trait** — add `get_custom_status(instance, last_seen_version)` method
4. **SQLite provider** — write `custom_status` and increment `custom_status_version` in `ack_orchestration_item()`, implement `get_custom_status()`, read both in status queries
5. **`OrchestrationStatus`** — change `Running` to struct variant, add `custom_status` and `custom_status_version` to all non-`NotFound` variants
6. **`CtxInner`** — add `custom_status: Option<String>` field
7. **`OrchestrationContext`** — add `set_custom_status()` method
8. **Orchestration dispatcher** — read `custom_status` from context after turn, include in metadata
9. **`compute_execution_metadata()`** — plumb `custom_status` from context into metadata
10. **`Client::get_orchestration_status()`** — read `custom_status` and `custom_status_version` from provider response
11. **`Client::wait_for_status_change()`** — client-side polling loop using `get_custom_status()`, final `get_orchestration_status()` on change
12. **Fix all `Running =>` match arms** across codebase
13. **Provider validation tests** — verify `custom_status` round-trips through ack/read

## Test Plan

| # | Test | Description |
|---|---|---|
| 1 | `custom_status_set_and_read` | Orchestration sets custom status, client reads it while running. |
| 2 | `custom_status_persists_across_turns` | Set status in turn 1, don't set in turn 2, verify it's still readable. |
| 3 | `custom_status_last_write_wins` | Set status twice in one turn, verify only the last value is persisted. |
| 4 | `custom_status_on_completed` | Set status, then complete. Verify `Completed { custom_status }` has the value. |
| 5 | `custom_status_on_failed` | Set status, then fail. Verify `Failed { custom_status }` has the value. |
| 6 | `custom_status_with_json` | Set structured JSON as custom status, read and deserialize on client. |
| 7 | `custom_status_none_by_default` | New orchestration has `custom_status: None`, `custom_status_version: 0`. |
| 8 | `wait_for_status_change_returns_on_update` | Client calls `wait_for_status_change` with version 0, orchestration sets status → client unblocks with new status and version 1. |
| 9 | `wait_for_status_change_returns_immediately_if_ahead` | Status already at version 3, client calls with `last_seen_version: 1` → returns immediately. |
| 10 | `wait_for_status_change_returns_on_terminal` | Client waiting, orchestration completes → client unblocks with `Completed` status. |
| 11 | `wait_for_status_change_timeout` | No status change within timeout → returns `ClientError::Timeout`. |
| 12 | `wait_for_status_change_same_value_increments_version` | Set status to "A", then "A" again on next turn → version increments from 1 to 2, client is notified despite same string. |
| 13 | `custom_status_version_resets_on_can` | Set status, CAN, verify version resets to 0 in new execution. |
