# Proposal: Custom Status as History Events

**Status:** Draft  
**Date:** 2026-02-20  
**Supersedes:** [custom-status-progress.md](custom-status-progress.md) (original design — metadata-only, no history)

## Problem Statement

The current custom status implementation is pure metadata — a side-channel on `CtxInner` that bypasses the event log entirely. This means:

1. **No determinism guarantees** — `set_custom_status()` / `reset_custom_status()` are invisible to the replay engine. If orchestration code changes the order or values of status writes between deployments, there is no way to detect the divergence.
2. **No history audit trail** — status changes don't appear in the event log. Debugging "what status was set at what point" requires external logging.
3. **CAN semantics are fragile** — custom status survives CAN only because the provider column is instance-scoped, not because it's part of the execution model. There's no principled carry-forward mechanism.
4. **Inconsistent with the rest of the model** — every other orchestration side effect (activities, timers, external events, sub-orchestrations, detached orchestrations) flows through the action/event pipeline. Custom status is the sole exception.

## Design

### Approach: Write = History Event, Read = Local State

- **Set/Clear** emits an action → persisted as a history event → determinism-checked on replay
- **Get** reads accumulated local state derived from the event sequence — sync, no await, no history event

This avoids the complexity of a "self-completing schedule" pattern (the earlier `get_custom_status().await` proposal) while preserving full determinism on writes.

### Action

One new `Action` variant (folded set + clear into a single variant):

```rust
/// Update the custom status. Some(value) = set, None = clear.
Action::UpdateCustomStatus { status: Option<String> }
```

- `ctx.set_custom_status("Processing 3/10")` → `Action::UpdateCustomStatus { status: Some("Processing 3/10") }`
- `ctx.reset_custom_status()` → `Action::UpdateCustomStatus { status: None }`
- Neither called → no action emitted (ternary: no-change / clear / set)

### Event

One new `EventKind` variant:

```rust
#[serde(rename = "CustomStatusUpdated")]
CustomStatusUpdated { status: Option<String> },
```

One-way event — no completion, no `source_event_id`. Same pattern as `OrchestrationChained`.

### Orchestration Context API

```rust
impl OrchestrationContext {
    /// Set a user-defined custom status for progress reporting.
    ///
    /// This is a deterministic history event. Each call emits an `UpdateCustomStatus`
    /// action that is persisted and replayed. The replay engine verifies that the same
    /// sequence of set/clear calls occurs on replay.
    ///
    /// The status is readable via `get_custom_status()` and externally via the
    /// management API (`get_orchestration_status()`).
    pub fn set_custom_status(&self, status: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap();
        let _ = inner.emit_action(Action::UpdateCustomStatus {
            status: Some(status.into()),
        });
    }

    /// Clear the custom status back to `None`.
    ///
    /// Emits a deterministic history event, same as `set_custom_status`.
    pub fn reset_custom_status(&self) {
        let mut inner = self.inner.lock().unwrap();
        let _ = inner.emit_action(Action::UpdateCustomStatus { status: None });
    }

    /// Read the current custom status.
    ///
    /// Returns the accumulated value from the sequence of `set_custom_status` /
    /// `reset_custom_status` calls in this execution. This is a local state read —
    /// sync, no history event, no await. Determinism is implicit: the value is
    /// fully derived from the deterministic write sequence in history.
    ///
    /// Returns `None` if custom status has never been set or was cleared.
    pub fn get_custom_status(&self) -> Option<String> {
        self.inner.lock().unwrap().accumulated_custom_status.clone()
    }
}
```

### Replay Engine Changes

**New accumulated state:**

```rust
// In CtxInner
accumulated_custom_status: Option<String>,
```

Initialized from `OrchestrationStarted.initial_custom_status` (see CAN section below), defaults to `None` for fresh instances.

**Processing `CustomStatusUpdated` in `apply_history_event`:**

- Update `ctx.accumulated_custom_status` to the event's `status` value
- No result delivery, no `must_poll` — this is a passive state update
- No pending schedule to match — the event is self-contained

**Processing `UpdateCustomStatus` in `convert_emitted_actions`:**

1. Pop the `UpdateCustomStatus` action from the emitted actions queue
2. Assign the next `event_id`
3. Write `CustomStatusUpdated { status }` to `history_delta`
4. Update `ctx.accumulated_custom_status`
5. Push to `pending_actions` as a one-way action (no completion routing)
6. Does NOT set `must_poll` — no future to wake

**Determinism check in `action_matches_event_kind`:**

```rust
(Action::UpdateCustomStatus { .. }, EventKind::CustomStatusUpdated { .. }) => true,
```

Kind-only matching — does not compare the `status` value. Rationale:
- The status string is often a progress message (e.g., "Processing item 3/10") that may format differently across code versions
- Value-level divergence is caught indirectly: if a different value causes different branching, the subsequent actions will mismatch
- This matches how `ActivityScheduled` checks name but not input content

### ContinueAsNew Carry-Forward

Add `initial_custom_status: Option<String>` to `OrchestrationStarted`:

```rust
EventKind::OrchestrationStarted {
    name: String,
    version: String,
    input: String,
    parent_instance: Option<String>,
    parent_id: Option<u64>,
    carry_forward_events: Option<Vec<(String, String)>>,
    /// Custom status carried forward from the previous execution during CAN.
    /// `None` for fresh instances or if no custom status was set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    initial_custom_status: Option<String>,
},
```

**CAN flow:**

1. CAN is detected → the dispatcher scans `history_delta` + `baseline_history` for the last `CustomStatusUpdated` event to compute current value
2. If `Some(s)`, packs it into the new execution's `OrchestrationStarted { initial_custom_status: Some(s), .. }`
3. Replay engine processes `OrchestrationStarted`, initializes `accumulated_custom_status` from `initial_custom_status`
4. `get_custom_status()` returns the carried-forward value immediately in the new execution

**Why not a synthetic event?** Adding `initial_custom_status` to `OrchestrationStarted` avoids the "phantom event with no matching action" problem. A synthetic `CustomStatusUpdated` at position 1 would need special-casing in determinism checks (no emitted action to match against). Embedding it in `OrchestrationStarted` keeps the replay engine logic clean.

**Backward compatibility:** `#[serde(skip_serializing_if = "Option::is_none")]` + `#[serde(default)]` means old history without this field deserializes as `None`. New history written by upgraded runtimes includes the field. This field alone is NOT a breaking change (additive, optional, defaulted).

### Size Enforcement

256 KB cap enforced at ack time, consistent with current behavior.

**Enforcement point:** The orchestration dispatcher scans `history_delta` for `CustomStatusUpdated` events after the turn completes. If the last one's `status` payload exceeds 256 KB, the orchestration is failed with a configuration error.

**Why at ack time, not at emit time?** Matching the current design — the orchestration code runs freely, and limit violations are caught before committing. This avoids panics inside synchronous `set_custom_status()` calls and keeps the error path clean (fail the orchestration, don't crash the runtime).

### Provider Changes

**Provider trait — unchanged API:**

```rust
/// Lightweight check for custom status changes.
async fn get_custom_status(
    &self,
    instance: &str,
    last_seen_version: u64,
) -> Result<Option<(Option<String>, u64)>, ProviderError>;
```

Same signature, same semantics. The provider is free to implement this via a denormalized column (fast) or a history scan (correct but slower). This is an internal implementation choice.

**`ExecutionMetadata` — `custom_status` field removed:**

```rust
pub struct ExecutionMetadata {
    pub history_delta: Vec<Event>,
    pub pending_timers: Vec<PendingTimer>,
    pub pending_activities: Vec<PendingActivity>,
    pub pending_sub_orchestrations: Vec<PendingSubOrchestration>,
    // custom_status: REMOVED — now derived from history_delta
}
```

**`CustomStatusUpdate` enum — kept for provider internals:**

The enum stays as an internal type used by provider implementations to represent the denormalized update. It is no longer part of `ExecutionMetadata` or `CtxInner`.

**SQLite provider `ack_orchestration_item()` — derive from history delta:**

At ack time, the SQLite provider scans `metadata.history_delta` for `CustomStatusUpdated` events:
- If none found → no-op (preserves existing denormalized value)
- If found → applies the **last** one:
  - `Some(s)` → `UPDATE instances SET custom_status = ?, custom_status_version = custom_status_version + 1`
  - `None` → `UPDATE instances SET custom_status = NULL, custom_status_version = custom_status_version + 1`

The denormalized `custom_status` and `custom_status_version` columns on the `instances` table remain for fast management API reads. Source of truth is the event log; the columns are a cache.

### `CtxInner` Changes

```rust
// REMOVED
custom_status_update: Option<crate::providers::CustomStatusUpdate>,

// ADDED
accumulated_custom_status: Option<String>,
```

**`ctx.custom_status_update()` — removed.** The dispatcher no longer reads custom status from the context. It reads it from `history_delta` events.

### Schema Migration

No new migration needed. The `custom_status` and `custom_status_version` columns already exist on the `instances` table (migration `20240108`). The change is purely in how they are populated (from `ExecutionMetadata` → from `history_delta` scanning).

The new `initial_custom_status` field on `OrchestrationStarted` is additive (`serde(default)`) and requires no schema change.

## Breaking Changes

| Change | Impact | Mitigation |
|--------|--------|------------|
| New `EventKind::CustomStatusUpdated` variant | Old runtimes cannot deserialize new history | Major version bump |
| New `Action::UpdateCustomStatus` variant | Old runtimes cannot process new actions | Major version bump |
| `ExecutionMetadata.custom_status` removed | Provider implementations that read this field must be updated | Provider trait version bump |
| `ctx.custom_status_update()` removed | Internal API only — no external impact | N/A |
| `initial_custom_status` on `OrchestrationStarted` | Additive + defaulted — backward compatible for deserialization, but old runtimes won't populate it on CAN | Major version bump (CAN correctness) |
| In-flight orchestrations | Orchestrations that called `set_custom_status` before upgrade will hit nondeterminism on replay (old history has no `CustomStatusUpdated` events, new code emits `UpdateCustomStatus` actions) | Drain in-flight orchestrations before upgrade |

**Verdict:** Major version bump. Not compatible with rolling upgrades from pre-change versions.

## Test Plan

### Unit Tests — Replay Engine

| # | Test | Description | Coverage |
|---|------|-------------|----------|
| 1 | `custom_status_action_emission` | (a) `set_custom_status("foo")` emits `UpdateCustomStatus { Some("foo") }`, (b) `reset_custom_status()` emits `UpdateCustomStatus { None }`, (c) no call → no action, (d) 3 calls → 3 separate actions (no last-write-wins collapsing). Verify via `drain_emitted_actions()`. | Set, clear, no-op, multiple |
| 2 | `custom_status_get_accumulated` | (a) Fresh context → `get_custom_status()` returns `None`. (b) Set "A", set "B" → returns `Some("B")`. (c) Set "A", reset → returns `None`. | Get before/after set, overwrite, clear |
| 3 | `custom_status_replay_determinism` | (a) On replay, `UpdateCustomStatus` matches `CustomStatusUpdated` by kind (value ignored). (b) On replay, `CallActivity` where history has `CustomStatusUpdated` → nondeterminism error. (c) History with 3 `CustomStatusUpdated` events (set → clear → set) → accumulated state correct after replay. | Kind-match, mismatch, accumulation |

### Integration Tests — E2E

| # | Test | Description | Coverage |
|---|------|-------------|----------|
| 4 | `custom_status_e2e_lifecycle` | Orchestration sets status to "step 1", awaits activity (turn boundary), sets "step 2", completes. Verify: (a) `get_custom_status()` returns correct value in each turn, (b) client reads via `get_orchestration_status()` after completion, (c) `Completed` variant has last status, (d) `custom_status_version` increments correctly (2 after 2 updates), (e) default is `None`/version 0 before first set. | Set, read, persist across turns, version, on-completed, default |
| 5 | `custom_status_clear_and_multi_write` | Orchestration sets status 3 times, then resets in same turn. Verify: (a) provider column is NULL (last event wins for denormalization), (b) `get_custom_status()` returns `None`. | Multi-write, clear, last-event-wins |
| 6 | `custom_status_on_failed` | Orchestration sets status, then returns `Err`. Verify `Failed { custom_status, .. }` has the last value. | Terminal-state preservation |

### ContinueAsNew Tests

| # | Test | Description | Coverage |
|---|------|-------------|----------|
| 7 | `custom_status_can_carry_forward` | (a) Set "X", CAN → new execution `get_custom_status()` returns `Some("X")`. (b) New execution overrides to "Y" → returns `Some("Y")`, provider shows "Y". (c) Never set, CAN → `None`. (d) Set "X", reset, CAN → `None`. | Carry, override, no-status CAN, clear-then-CAN |
| 8 | `custom_status_can_chain` | Set "A" → CAN → set "B" → CAN → set "C" → CAN. Final execution: `get_custom_status()` returns `Some("C")`. | Multi-hop CAN |

### Size Enforcement Tests

| # | Test | Description | Coverage |
|---|------|-------------|----------|
| 9 | `custom_status_size_enforcement` | (a) 100 KB status → succeeds. (b) 300 KB status → fails with configuration error. (c) 300 KB then 100 B → succeeds (only last event checked). | Within limit, over limit, recovery |

### Provider Validation Tests

| # | Test | Description | Coverage |
|---|------|-------------|----------|
| 10 | `provider_custom_status_from_history_delta` | (a) Ack with `CustomStatusUpdated { Some("foo") }` → `get_custom_status()` returns `Some("foo")`, version incremented. (b) Ack with `CustomStatusUpdated { None }` → returns `(None, version)`, version incremented. (c) Ack with no `CustomStatusUpdated` → previous value unchanged. (d) Ack with 3 `CustomStatusUpdated` events → column reflects last one. | Set, clear, no-op, multi-event |

### Scenario Tests

| # | Test | Description | Coverage |
|---|------|-------------|----------|
| 11 | `custom_status_instance_actor_pattern` | Long-running orchestration with CAN loop, setting custom status on each iteration. Verify status is readable externally and carries forward correctly across CAN boundaries. (Add to `tests/scenarios/`) | Production pattern regression |

## Documentation Updates

### docs/ORCHESTRATION-GUIDE.md

| Section | Change |
|---------|--------|
| **Custom Status** (~L570) | Rewrite to document history-event semantics: each `set_custom_status` / `reset_custom_status` emits a deterministic history event. Add `get_custom_status()` to the API. Remove "not a history event" language. Add note about determinism implications (adding/removing/reordering `set_custom_status` calls between deployments will cause nondeterminism violations for in-flight orchestrations). |
| **Determinism Rules** | Add `set_custom_status` / `reset_custom_status` to the list of deterministic operations that emit history events. |
| **API Reference table** | Add `get_custom_status()` → `Option<String>`. Update `set_custom_status` description. |

### docs/provider-implementation-guide.md

| Section | Change |
|---------|--------|
| **Simple Methods table** (~L348) | `get_custom_status()` — update description: "Lightweight status polling (reads denormalized column populated from history events)". |
| **ack_orchestration_item pseudocode** (~L1022) | Replace step 2a: instead of reading from `ExecutionMetadata.custom_status`, scan `history_delta` for `CustomStatusUpdated` events and apply the last one. Update pseudocode and SQL. |
| **get_custom_status() section** (~L1570) | Update to note that the denormalized column is populated from history events at ack time, not from `ExecutionMetadata`. |
| **Implementation Checklist** (~L1939) | Update checklist items: remove `metadata.custom_status` references, add "scan `history_delta` for `CustomStatusUpdated` events". |
| **ExecutionMetadata** | Remove `custom_status` from the field listing. |

### docs/provider-testing-guide.md

| Section | Change |
|---------|--------|
| **Provider Validation Tests** (~L379) | Add a new subsection documenting the 4 new provider validation tests (tests 27-30 above). Update test count. |

### docs/replay-engine.md

| Section | Change |
|---------|--------|
| **Event variants** | Add `CustomStatusUpdated { status: Option<String> }` to the event catalog. |
| **Action variants** | Add `UpdateCustomStatus { status: Option<String> }` to the action catalog. |
| **One-way actions** | Document `UpdateCustomStatus` alongside `ContinueAsNew` and `StartOrchestrationDetached` as one-way actions (no completion event). |

### docs/continue-as-new.md

| Section | Change |
|---------|--------|
| **What carries forward** | Add custom status to the list of state that carries forward across CAN (alongside persistent events). Document how it's embedded in `OrchestrationStarted.initial_custom_status`. |

### docs/migration-guide.md

| Section | Change |
|---------|--------|
| **Version X → Y** | Add migration notes: `set_custom_status` / `reset_custom_status` are now history events. In-flight orchestrations that used these methods before upgrading will hit nondeterminism violations. Drain before upgrade. Provider implementations must update `ack_orchestration_item()` to derive custom status from `history_delta` instead of `ExecutionMetadata`. |

### docs/proposals/custom-status-progress.md

| Section | Change |
|---------|--------|
| **Header** | Add "Superseded by [custom-status-history-events.md](custom-status-history-events.md)" note. |

### .github/copilot-instructions.md

| Section | Change |
|---------|--------|
| **Critical Patterns** | Add `ctx.set_custom_status()` / `ctx.get_custom_status()` to the "Orchestrations vs Activities" section as a deterministic operation. |

## Implementation Plan

Ordered by dependency:

1. **`Action::UpdateCustomStatus` + `EventKind::CustomStatusUpdated`** — Add the new variants to `Action` and `EventKind` enums in `src/lib.rs`.
2. **`CtxInner` changes** — Remove `custom_status_update`, add `accumulated_custom_status`. Initialize from `OrchestrationStarted.initial_custom_status`.
3. **`OrchestrationContext` API** — Rewrite `set_custom_status()` / `reset_custom_status()` to emit actions. Add `get_custom_status()`.
4. **`OrchestrationStarted` extension** — Add `initial_custom_status: Option<String>` field.
5. **Replay engine** — Handle `CustomStatusUpdated` in `apply_history_event` (accumulate state) and `convert_emitted_actions` (write event, no completion). Add `action_matches_event_kind` case.
6. **Orchestration dispatcher** — Remove `ctx.custom_status_update()` reads. Remove custom status from `ExecutionMetadata` construction. Add size enforcement scan of `history_delta`.
7. **CAN handler** — Compute current custom status from history, populate `initial_custom_status` on new `OrchestrationStarted`.
8. **`ExecutionMetadata`** — Remove `custom_status` field.
9. **SQLite provider** — Update `ack_orchestration_item()` to derive custom status from `history_delta` events instead of `ExecutionMetadata`.
10. **Provider validation tests** — Add test 10.
11. **Unit tests** — Add replay engine tests 1-3.
12. **Integration tests** — Add E2E tests 4-6.
13. **CAN tests** — Add tests 7-8.
14. **Size tests** — Add test 9.
15. **Scenario test** — Add test 11.
16. **Documentation updates** — All docs listed above.
17. **Fix all callers** — Update any code that reads `ctx.custom_status_update()` or `ExecutionMetadata.custom_status`.
