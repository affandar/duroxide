# RFC: Deterministic System Calls with Auto-Completion (GUID, Time, etc.)

Status: Draft

Author: duroxide team

Date: 2025-09-05

## Summary

Introduce deterministic, host-only “system calls” (e.g., GUID generation, current time, random) that resolve synchronously within an orchestration turn and are persisted as a single history event, avoiding worker round-trips and eliminating completion routing for these operations. On replay, system call results are auto-completed from the original single history entry.

Key properties:
- No worker queue usage, no orchestrator completions.
- Deterministic: during replay we never recompute; we read the prior value from history.
- Single event per call captures both the intent and result.
- Select/join semantics remain predictable (system futures appear already-completed during replay).

## Motivation

Some common operations are inherently host-local and cheap but need deterministic replay in the orchestration:
- New GUID/UUID (e.g., correlation/idempotency keys)
- Current time (utcnow in ms)
- Random values (rare, opt-in)
- Trace/logging (diagnostics)

Today, modeling these as activities or pseudo-activities (e.g., "__system_trace") introduces nondeterminism and unnecessary queue churn. We need a first-class, deterministic model.

## Goals

- Provide synchronous, deterministic primitives for common system operations.
- No worker round-trips and no extra orchestrator messages.
- One history event per call that auto-completes on replay.
- Preserve deterministic semantics with `select`/`join`/`race`.

## Non-Goals

- Pluggable external providers for system calls (initially host-only).
- Guaranteed monotonic time or distributed time semantics (we store what was observed once).
- Durable audit for traces (we keep trace as host logging only; optional future TraceSink).

## Design Overview

Add new API on `OrchestrationContext`:
- `ctx.new_guid() -> DurableFuture<String>`
- `ctx.utcnow_ms() -> DurableFuture<u64>`
- (Optional) `ctx.random_u64() -> DurableFuture<u64>`

Add new action and event types:
- `Action::SystemCall { id: u64, op: String, value: String }`
- `Event::SystemCall { id: u64, op: String, value: String, execution_id: u64 }`

System call futures:
- Future kind: `Kind::System { id, op }`.
- On first execution, compute value synchronously (e.g., uuid v4, now_ms), resolve immediately, and record `Action::SystemCall`.
- On replay, adopt from `Event::SystemCall` in baseline history matching `(id, op)`; resolve immediately without recomputation.

No queue traffic:
- `Action::SystemCall` is converted to `Event::SystemCall` during `ack_orchestration_item` as part of the turn’s history delta.
- No `WorkItem`, no worker or orchestrator queue operations are generated.

## Execution Semantics

Turn lifecycle (atomic path):
1. Runtime builds working history: baseline history + applied completion map.
2. Orchestration code executes and calls `ctx.new_guid()` (or similar):
   - Replay path: find `Event::SystemCall { id, op }` in the current execution history and return `value`.
   - First-execution path: compute value; the future resolves; record `Action::SystemCall { id, op, value }`.
3. `apply_decisions` maps `Action::SystemCall` to `Event::SystemCall` in `history_delta` (no queues).
4. Provider `ack_orchestration_item` appends `history_delta` atomically.

Determinism:
- On replay, if a system call is requested but no matching event exists, raise deterministic error (missing prior result).
- If duplicates are encountered (same `id, op`), treat subsequent calls as adopted (no-op append) or raise error based on strictness configuration.

## Completion Map Interaction

System calls do not produce `OrchestratorMsg`. They are not added to the completion map and never require consumption.
- In `select`/`join`, system futures appear as ready once scheduled:
  - First execution: they resolve immediately after scheduling (value already computed in-turn).
  - Replay: they resolve from history immediately.

## API Sketch

```rust
impl OrchestrationContext {
    pub fn new_guid(&self) -> DurableFuture { self.schedule_system_call("guid") }
    pub fn utcnow_ms(&self) -> DurableFuture { self.schedule_system_call("utcnow_ms") }

    fn schedule_system_call(&self, op: &str) -> DurableFuture {
        let mut inner = self.inner.lock().unwrap();
        // Adopt if exists in baseline history for current execution
        if let Some((id, value)) = adopt_system_call(&inner.history, op) {
            return DurableFuture(Kind::System { id, op: op.to_string(), adopted: Some(value), ctx: self.clone() })
        }
        // Allocate id; compute value; stage action
        let id = inner.next_id();
        let value = match op { "guid" => generate_uuid(), "utcnow_ms" => now_ms_str(), _ => unimplemented!() };
        inner.record_action(Action::SystemCall { id, op: op.to_string(), value: value.clone() });
        DurableFuture(Kind::System { id, op: op.to_string(), adopted: Some(value), ctx: self.clone() })
    }
}
```

Resolver semantics:
- Future `poll_once` for `Kind::System` returns immediately from `adopted`/staged value.

## Trace/Logging Policy

- `ctx.trace_info/trace_warn/trace_error` remain host-side logging only.
- Use `ctx.is_replaying()` to suppress re-emission during replay.
- Optionally include correlation fields: instance, execution_id, turn_index, correlation_id.
- (Future) Optional `TraceSink` interface to persist traces outside orchestration history.

## Data Schema

`Event::SystemCall` fields:
- `id: u64` — correlation id within turn/execution.
- `op: String` — e.g., "guid", "utcnow_ms".
- `value: String` — serialized result (stringified number or UUID string).
- `execution_id: u64` — to scope lookups to the current execution.

No new provider tables or queues are required. Providers only need to append events via `ack_orchestration_item`.

## Error Handling

- If replay is missing a required `Event::SystemCall` for a requested `(id, op)`, return deterministic error: `missing system call result for id/op`.
- If system call computation fails (should not in basic ops), fail the turn with deterministic error.

## Backwards Compatibility & Migration

- Deprecate using activities for GUID/time/trace.
- Remove any internal uses of "__system_trace" pseudo-activity and replace with host logging.
- Existing workflows that relied on pseudo-activities may still run; a compatibility layer can ignore such completions.

## Testing Strategy

Unit tests:
- First-run: system call appends `Event::SystemCall`; future resolves with computed value.
- Replay: system call returns value from history; no new events appended.
- Negative: missing event on replay triggers deterministic error.

Integration tests:
- Combine system calls with `select/join` and ensure no nondeterminism.
- Ensure no worker/orchestrator queue messages are produced by system calls.

## Implementation Steps

1) Types & API
   - Add `Action::SystemCall` and `Event::SystemCall` to core enums.
   - Add `ctx.new_guid()`, `ctx.utcnow_ms()`; add internal `schedule_system_call()`.
   - Implement `Kind::System` future with adopt/compute logic.

2) Turn & Ack Wiring
   - In `run_single_execution_atomic/apply_decisions`, map `Action::SystemCall` → `Event::SystemCall` in `history_delta`.
   - Ensure no `WorkItem` generated for system calls.

3) Determinism & Replay
   - Implement adopt-from-history by `(execution_id, id, op)`.
   - Add strictness checks for missing/duplicate entries.

4) Trace Policy
   - Update `ctx.trace_*` to log only when `!is_replaying()`.
   - Remove any pseudo-activity usage for trace.

5) Docs & Samples
   - Document system call usage (Quick Start, examples) and discourage activity-based GUID/time.

## Open Questions

- Do we need typed values beyond strings for `Event::SystemCall.value`? (We can keep string encoding for simplicity.)
- Do we want a global opt-in for strict duplicate checks vs. adopt semantics?
- Should we support monotonic time semantics (e.g., guard against time going backwards across turns)?


