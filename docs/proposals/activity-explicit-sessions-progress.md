# Activity-Explicit Sessions — Progress Status

Last updated: 2026-02-15

Spec reference: [activity-explicit-sessions.md](activity-explicit-sessions.md)

## Overall Status

Substantial implementation is complete and validated in tests. Core session lifecycle, provider session-aware fetch semantics, runtime/provider side-channel cancellation behavior, and close-session lock stealing are in place.

## Completed

### Runtime + API Surface

- Session lifecycle APIs are available in orchestration context:
  - `open_session()`
  - `open_session_with_id()`
  - `close_session()`
  - `schedule_activity_on_session()`
- Session events exist and replay:
  - `SessionOpened`
  - `SessionClosed`
- Replay engine maintains open-session state and enforces session scheduling validation.

### Provider Contract + SQLite Provider

- Session-aware worker fetch semantics implemented:
  - `fetch_work_item()` returns non-session items only.
  - `fetch_session_work_item(worker_id)` returns non-session plus claimable/owned session items.
  - FIFO order preserved.
  - Conditional session claim race-safe via upsert + `rows_affected` check.
- Session lock renew/release methods implemented in provider contract and SQLite.
- Close-session lock stealing implemented as explicit out-of-band runtime signal to provider (no provider history introspection):
  - Runtime passes `cancelled_sessions` in `ExecutionMetadata`.
  - SQLite `ack_orchestration_item` atomically:
    - deletes `worker_queue` rows for cancelled sessions
    - deletes `sessions` rows for cancelled sessions (causes future session renewals to fail)
- Existing activity lock stealing continues to work; close-session cancellation races with completion are benign.

### Worker Session Renewal Model

- Renewal is shared per `(instance_id, session_id, worker_id)` lease (no one-task-per-activity duplication).
- Session lock loss triggers cancellation signal to in-flight session activities (defensive fail-safe path).
- `session_idle_timeout` lifecycle implemented in worker lease registry:
  - idle timer starts after last activity for the lease finishes (complete/error/cancel).
  - `Some(duration)`: release after timeout.
  - `None` (default): lease retained indefinitely.

### Defaults and Documentation

- `session_lock_duration` default updated to 10 minutes.
- Provider implementation guide updated with explicit pseudocode for session-aware fetch/claim path.
- Added TODO notes for optimization: piggyback session renewals on activity lock-renewal ticks.

## Tests Added / Updated

Session/provider validation additions include:

- `plain_fetch_only_non_session`
- `session_fetch_mixed_behavior`
- `session_claim_race_single_winner`
- `close_session_lock_stealing_signal`

Also added/updated session scenario coverage in `tests/scenarios/sessions.rs` and session validation wiring in `tests/sqlite_provider_validations.rs`.

Latest verification at time of this status:

- `cargo nt` passes (full suite): 754/754.
- Session-focused suites pass.

## Proposal Mapping (Current)

Primary spec: [activity-explicit-sessions.md](activity-explicit-sessions.md)

### Implemented and aligned

- Provider contract split between non-session and session-aware fetch.
- Session claim/ownership/expiry semantics.
- Close-session as provider-level lock stealing signal.
- Session renewal failure path causing cancellation signal for in-flight activities.
- `session_idle_timeout` behavior started after last activity completion/cancel.

### Still open / partial vs spec

- Orchestration terminal cleanup to close all open sessions for the instance (explicit provider-side cleanup signal path) should be reviewed for full parity against spec section "Runtime Changes → Orchestration Dispatcher".
- Graceful shutdown immediate release of all owned sessions should be verified/finished end-to-end against spec expectations.
- Optional optimization to piggyback session renewal on activity lock renewal remains TODO (intentionally deferred).

## Next Steps

1. **Terminal session cleanup parity**
   - Confirm and, if needed, complete automatic close/lock-steal of all open sessions when orchestration reaches terminal states.
   - Spec pointer: [activity-explicit-sessions.md](activity-explicit-sessions.md) — Runtime Changes / Orchestration Dispatcher.

2. **Graceful shutdown parity**
   - Ensure worker shutdown path explicitly releases owned sessions immediately (without waiting for expiration).
   - Spec pointer: [activity-explicit-sessions.md](activity-explicit-sessions.md) — Session Migration / Graceful Shutdown.

3. **Additional proposal test matrix pass**
   - Expand and verify tests for close/reopen, terminal cleanup, and continue-as-new edge paths listed in proposal tables.
   - Spec pointer: [activity-explicit-sessions.md](activity-explicit-sessions.md) — sections 7, 10, 11, 14, 15.

4. **Renewal optimization (deferred)**
   - Implement piggyback renewal strategy to reduce periodic session renewal task overhead.
   - Spec pointer: [activity-explicit-sessions.md](activity-explicit-sessions.md) — provider/runtime renewal behavior notes.
