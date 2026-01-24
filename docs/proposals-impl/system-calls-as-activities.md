# Proposal: System Calls as Real Activities

**Status:** Implemented

## Summary

Replace the bespoke "system call" mechanism (`ctx.new_guid()`, `ctx.utc_now()`) with regular activities using reserved names. System calls become normal activities that go through the worker queue, using the existing `ActivityScheduled` + `ActivityCompleted` event flow.

## Motivation

The current system call implementation has a **replay bug**: on replay, system calls return newly computed values instead of historical values, causing nondeterminism errors. The root cause is that system calls bypass the normal action/completion flow—they use `std::future::ready()` which resolves immediately with a fresh value.

The fix requires special-case re-polling logic in the replay engine, which is fragile and doesn't fit the existing architecture. A cleaner solution is to eliminate the special case entirely by treating system calls as activities.

## Design

### Reserved Namespace Constant

A single namespace prefix constant is used for all built-in system activities. This prefix is also used to validate user activity registrations:

```rust
/// Reserved prefix for built-in system activities.
/// User-registered activities cannot use names starting with this prefix.
pub(crate) const SYSCALL_ACTIVITY_PREFIX: &str = "__duroxide_syscall:";
```

### Reserved Activity Names

Activity names are built from the namespace constant:

```rust
pub(crate) const SYSCALL_ACTIVITY_NEW_GUID: &str = 
    concat!("__duroxide_syscall:", "new_guid");
pub(crate) const SYSCALL_ACTIVITY_UTC_NOW_MS: &str = 
    concat!("__duroxide_syscall:", "utc_now_ms");

// Or programmatically:
// format!("{}new_guid", SYSCALL_ACTIVITY_PREFIX)
```

### Built-in Activity Handlers

Simple functions that compute the value:

```rust
async fn syscall_new_guid(_ctx: ActivityContext, _input: String) -> Result<String, String> {
    Ok(generate_guid())
}

async fn syscall_utc_now_ms(_ctx: ActivityContext, _input: String) -> Result<String, String> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    Ok(ms.to_string())
}
```

### OrchestrationContext API Changes

**`new_guid()` — unchanged signature**
```rust
pub fn new_guid(&self) -> impl Future<Output = Result<String, String>> {
    self.schedule_activity(SYSCALL_ACTIVITY_NEW_GUID, "")
}
```

**`utc_now()` — renamed from `utcnow()` for consistency**
```rust
pub fn utc_now(&self) -> impl Future<Output = Result<std::time::SystemTime, String>> {
    let fut = self.schedule_activity(SYSCALL_ACTIVITY_UTC_NOW_MS, "");
    async move {
        let ms_str = fut.await?;
        let ms: u64 = ms_str.parse().map_err(|e| format!("parse error: {e}"))?;
        Ok(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms))
    }
}
```

### Runtime Startup

Built-in activities are auto-registered at runtime startup:

```rust
impl Runtime {
    pub async fn start_with_store(...) -> Self {
        // Inject built-in syscall activities
        let activities = user_activities
            .with_builtin(SYSCALL_ACTIVITY_NEW_GUID, syscall_new_guid)
            .with_builtin(SYSCALL_ACTIVITY_UTC_NOW_MS, syscall_utc_now_ms);
        
        // ... rest of startup
    }
}
```

### Reserved Name Validation

Use the existing registry error handling pattern (same as duplicate name detection). Do **not** panic.

`ActivityRegistry` collects errors and returns them from `build()`:

```rust
fn register_internal(&mut self, name: &str, version: &Version, handler: H) {
    // Check for reserved prefix (uses existing error collection pattern)
    if name.starts_with(SYSCALL_ACTIVITY_PREFIX) {
        self.errors.push(format!(
            "activity name '{name}' uses reserved prefix '{}'",
            SYSCALL_ACTIVITY_PREFIX
        ));
        return;
    }
    
    // Check for duplicates (existing logic)
    if self.check_duplicate(name, version, "activity") {
        return;
    }
    
    // ... normal registration
}
```

Errors are surfaced at `build()` time, same as duplicate registrations:
```rust
let registry = ActivityRegistry::builder()
    .register("__duroxide_syscall:evil", handler)  // <- collects error
    .build()?;  // <- returns Err with message
```

## Implementation Plan

### Phase 1: Add Built-in Activity Infrastructure

| File | Changes |
|------|---------|
| `src/lib.rs` | Add `SYSCALL_ACTIVITY_PREFIX` namespace constant |
| `src/lib.rs` | Add `SYSCALL_ACTIVITY_NEW_GUID`, `SYSCALL_ACTIVITY_UTC_NOW_MS` built from prefix |
| `src/runtime/registry.rs` | Add reserved prefix validation in `register_internal()` (use existing `errors` pattern) |
| `src/runtime/registry.rs` | Add `with_builtin()` method for injecting built-ins |
| `src/runtime/mod.rs` | Auto-register built-in activities at startup |

### Phase 2: Rewrite OrchestrationContext Methods

| File | Changes |
|------|---------|
| `src/lib.rs` | Rewrite `new_guid()` to call `schedule_activity()` |
| `src/lib.rs` | Rename `utcnow()` → `utc_now()`, rewrite to call `schedule_activity()` |
| `src/lib.rs` | Remove `schedule_system_call()` method |
| `src/lib.rs` | Remove `SYSCALL_OP_GUID`, `SYSCALL_OP_UTCNOW_MS` constants |

### Phase 3: Remove SystemCall from Enums

| File | Changes |
|------|---------|
| `src/lib.rs` (`Action`) | Remove `SystemCall` variant |
| `src/lib.rs` (`EventKind`) | Remove `SystemCall` variant |
| `src/lib.rs` (`CompletionResult`) | Remove `SystemCallValue` variant |
| `src/lib.rs` | Remove `EVENT_TYPE_SYSTEM_CALL` constant |

### Phase 4: Clean Up Replay Engine

| File | Changes |
|------|---------|
| `src/runtime/replay_engine.rs` | Remove `EventKind::SystemCall` match arm in history loop |
| `src/runtime/replay_engine.rs` | Remove `action_matches_event_kind` for SystemCall |
| `src/runtime/replay_engine.rs` | Remove `action_to_event` for SystemCall |
| `src/runtime/replay_engine.rs` | Remove `update_action_event_id` for SystemCall |
| `src/runtime/replay_engine.rs` | Remove the system-call-specific re-poll loop (`delivered_system_call` flag and associated logic) |

### Phase 5: Clean Up Provider

| File | Changes |
|------|---------|
| `src/providers/sqlite.rs` | Remove `EventKind::SystemCall` case in event type mapping |

### Phase 6: Update Tests

| File | Changes |
|------|---------|
| `tests/replay_engine/replay_with_completions.rs` | Remove `system_call_in_history` test |
| `tests/replay_engine/edge_cases.rs` | Remove `system_call_fresh_execution` test |
| `tests/replay_engine/helpers.rs` | Remove `system_call()` helper function |
| `tests/replay_engine/helpers.rs` | Remove `SystemCallHandler` struct |
| `tests/system_calls_test.rs` | Update `utcnow()` → `utc_now()` calls; tests should now pass |
| All other test files | Search/replace `utcnow` → `utc_now` |

### Phase 7: Update Documentation and Examples

| File | Changes |
|------|---------|
| `docs/ORCHESTRATION-GUIDE.md` | Update API examples |
| `examples/*.rs` | Update any `utcnow()` calls |
| `README.md` | Update if system calls are mentioned |

## Metrics

No system-call-specific metrics exist today. After this change:
- `new_guid()` and `utc_now()` calls will be counted as normal activity executions
- No special metrics changes are required
- If activity-level metrics exist (e.g., "activities completed by name"), syscall activities will appear under their reserved names

## Files to Delete

None — we're modifying existing files, not deleting whole files.

## Backward Compatibility

**Not supported.** Any persisted histories containing `SystemCall` events will fail to replay. This is acceptable for pre-1.0.

## Tradeoffs

| Aspect | Impact |
|--------|--------|
| **Latency** | Each `new_guid()` / `utc_now()` now incurs a worker queue round-trip (~1-5ms per call). |
| **Throughput** | More DB operations per orchestration turn if system calls are frequent. |
| **Simplicity** | Much simpler replay engine; no special-case code paths. |
| **Correctness** | Determinism is guaranteed by the activity model; no more replay bugs. |

## Future Optimization (Out of Scope)

If latency becomes a problem, a future optimization could introduce "local activities" that complete in-process without hitting the worker queue. This would use the same `ActivityScheduled` + `ActivityCompleted` events but skip the queue. This proposal does not include that optimization.

## Additional Tests

### Reserved Name Protection Tests

| Test | Description |
|------|-------------|
| `reserved_prefix_rejected` | User attempts to register `__duroxide_syscall:evil` → `build()` returns error (not panic) |
| `builtins_exist_with_empty_registry` | Runtime starts with empty user registry; `new_guid()` and `utc_now()` still work |
| `user_cannot_override_builtins` | User registers activity with same name as builtin → error at build time |

### Replay Determinism Tests

| Test | Description |
|------|-------------|
| `utc_now_as_activity_input_replays_correctly` | `utc_now()` result used as activity input, external event forces replay → no mismatch |
| `new_guid_as_activity_input_replays_correctly` | `new_guid()` result used as activity input, external event forces replay → no mismatch |

### Ordering / Interleaving Tests

| Test | Description |
|------|-------------|
| `activity_then_syscall_ordering` | Schedule activity A, call `new_guid()`, schedule activity B → history order stable across replay |
| `syscall_then_activity_ordering` | Call `new_guid()`, schedule activity → no nondeterminism on replay |
| `multiple_syscalls_same_type` | Call `new_guid()` twice → each returns distinct value; replay returns same two values in order |

### Single-Thread Runtime Test

| Test | Description |
|------|-------------|
| `syscalls_work_in_single_thread_mode` | `#[tokio::test(flavor="current_thread")]` with 1x1 runtime; orchestration calls `utc_now()` + `new_guid()` several times → completes |

### Cancellation Test

| Test | Description |
|------|-------------|
| `cancellation_with_pending_syscall` | Orchestration calls `utc_now()` then waits; request cancellation → cancels cleanly without stuck activities |

## Success Criteria

1. All existing tests pass (after updating `utcnow` → `utc_now`)
2. `test_utc_now_as_activity_input_replays_correctly` passes
3. `test_new_guid_as_activity_input_replays_correctly` passes
4. No `SystemCall` variant in `Action`, `EventKind`, or `CompletionResult`
5. No special-case system call handling in replay engine
6. Reserved name validation uses error collection, not panic
7. All additional tests listed above pass
