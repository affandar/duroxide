# Activity-Explicit Sessions — Progress Status

Last updated: 2026-02-16

Spec reference: [activity-explicit-sessions.md](activity-explicit-sessions.md)

## Overall Status

Substantial implementation is complete and validated in tests. Core session lifecycle, provider session-aware fetch semantics, runtime/provider side-channel cancellation behavior, and close-session lock stealing are in place.

An audit against the full spec (design + test matrix) on 2026-02-16 identified one critical routing bug, several missing spec features, and significant test coverage gaps. Details in the **Audit** section below.

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

- See **Audit** section below for the full gap analysis.

---

## Audit (2026-02-16)

Full review of implementation against the spec ([activity-explicit-sessions.md](activity-explicit-sessions.md)), covering design, code, and test matrix.

### Critical: Bugs That Will Break Multi-Worker Session Routing

**1. `session_id` column not populated during `ack_orchestration_item`**

When the orchestration dispatcher processes a turn and enqueues worker items via `ack_orchestration_item`, it inserts them into `worker_queue` WITHOUT the `session_id` column (`src/providers/sqlite.rs` ~line 1317):

```sql
INSERT INTO worker_queue (work_item, visible_at, instance_id, execution_id, activity_id)
VALUES (?, ?, ?, ?, ?)
```

Compare with `enqueue_for_worker` (used by provider validation tests), which correctly includes `session_id` (`src/providers/sqlite.rs` ~line 1621):

```sql
INSERT INTO worker_queue (work_item, visible_at, instance_id, execution_id, activity_id, session_id)
VALUES (?, ?, ?, ?, ?, ?)
```

The `fetch_session_work_item` query JOINs on `wq.session_id` to determine routing. When the column is NULL, all session-bound activities are treated as regular (non-session) items and dispatched to any worker. This completely defeats session routing in the real orchestration path.

**Why tests pass today:** scenario tests run single-worker, so routing doesn't matter. Provider validation tests use `enqueue_for_worker` directly (bypassing `ack_orchestration_item`), so they never hit this code path.

**Fix:** Extract `session_id` from `WorkItem::ActivityExecute` alongside the other identity fields and bind it in the `ack_orchestration_item` INSERT.

### High: Missing Spec Features

**2. ContinueAsNew does not carry open sessions**

Spec section "ContinueAsNew: Carrying Open Sessions":

> When an orchestration calls continue_as_new, the set of currently open sessions must be carried to the new execution. The runtime handles this automatically.

Not implemented. In `src/runtime/execution.rs`, `TurnResult::ContinueAsNew` creates a `ContinuedAsNew` event and `ContinueAsNew` work item but has no logic to persist the open session set or initialize it in the new execution. The new execution starts with an empty `open_sessions` set, so any `schedule_activity_on_session` calls will fail with "session not open."

This means the copilot pattern of `open_session -> turns -> continue_as_new -> more turns` is broken.

**3. Terminal orchestration does not clean up open sessions**

Spec section "Runtime Changes > Orchestration Dispatcher":

> On orchestration terminal (Completed, Failed, Cancelled): closes all open sessions for the instance.

The orchestration dispatcher (`src/runtime/dispatchers/orchestration.rs` ~line 688) cancels in-flight activities on terminal state but never queries or deletes session rows. The only session cleanup is for explicitly closed sessions (via `collect_closed_sessions_from_pending`). If an orchestration completes without calling `close_session`, session rows remain in the `sessions` table indefinitely.

**4. `max_sessions_per_worker` is defined but never enforced**

The config value exists in `RuntimeOptions` (default: 100), but no code in the worker dispatcher checks it. The spec says workers at session capacity should fall back to `fetch_work_item` for non-session work only. The worker always calls `fetch_session_work_item` regardless of how many sessions it owns.

**5. Graceful shutdown does not release session locks**

Spec section "Session Migration > Worker Shutdown (Graceful)":

> On graceful shutdown, the worker actively releases all session locks by calling `provider.release_session_lock()`.

`Runtime::shutdown` just sets `shutdown_flag = true` and waits. Session renewal tasks stop but don't call `release_session_lock`. Sessions become claimable only after `session_lock_duration` expires (default 10 minutes), unacceptable for rolling deployments.

The `SessionRenewalRegistry` has the infrastructure to track leases but `shutdown` never calls into it (it's a local variable inside `start_work_dispatcher`, unreachable from `shutdown`).

### Medium: Design Deviations From Spec

**6. `open_session_with_id` is not truly idempotent**

Spec:

> `open_session_with_id(id)` is idempotent: if a session with that ID already exists and is open, this is a no-op returning the same ID.

The implementation always emits an `Action::OpenSession` regardless of whether the session is already open (`src/lib.rs` ~line 2828). The replay engine handles this gracefully (no error, no double-count) but it produces duplicate `SessionOpened` events in history. If user code conditionally calls `open_session_with_id` (e.g., inside a branch), the resulting history diverges, causing nondeterminism on replay.

**Recommendation:** Check `open_sessions.contains(session_id)` in `open_session_with_id` and return early without emitting an action if already open.

**7. Session rows are not created at open time**

Spec section "Provider Contract Changes":

> `OpenSession`: insert session row with `worker_id = NULL`.

The implementation does not create a session row during `ack_orchestration_item`. Session rows are only created lazily when a worker claims the session during `fetch_session_work_item`. Functionally fine because the fetch path handles missing rows with an upsert, but it means there is no provider-level record of a session between `open_session` and the first activity fetch. This also makes terminal cleanup (finding #3) harder since there may be no rows to query.

**8. `session_lock_duration` derived inconsistently**

Two different values are used:
- During `fetch_work_item_inner` (session claim): `lock_timeout.as_millis() * 2` where `lock_timeout` is the *activity* lock timeout (`src/providers/sqlite.rs` ~line 199).
- During session renewal: `rt.options.session_lock_duration` (the configured value, default 10 minutes).

These can differ substantially. The fetch-time claim creates a short lock (e.g., 60s if activity lock is 30s), while the renewal task extends it to 10 minutes. The first renewal will extend it correctly, but the initial window is inconsistent with the configured duration.

Additionally, the spec default is `2 * worker_lock_timeout` (e.g., 60s). The code default is 10 minutes, a significant departure from the spec's recommendation to "keep this short for fast crash recovery."

### Low: Edge Cases and Missing Validation

**9. Empty session ID string not rejected** — Spec test 15.11. `open_session_with_id("")` is accepted and used without validation.

**10. No session ID length limit** — Spec test 15.12. Very long IDs could cause performance issues in primary key lookups.

**11. `renew_work_item_lock` does not piggyback session renewal** — Spec describes this but it's noted as deferred. Independent renewal loops work but are less efficient.

**12. No `supports_sessions` check in replay engine** — Spec test 8.6. The replay engine doesn't have access to the provider, so `open_session()` on a non-session provider will silently accept the action and fail later at fetch time instead of failing immediately.

### Test Coverage Gap Analysis

The spec defines 107 individual test cases across 18 sections. The implementation has 6 scenario tests and 13 provider validation tests. Approximate coverage: ~24 of 107 spec tests (~22%).

| Spec Section | Tests in Spec | Covered | Notes |
|---|---|---|---|
| 1. Session Lifecycle (Basic) | 9 | ~4 | Missing: close idempotency (1.5-1.6), reopen (1.7), concurrent (1.8) |
| 2. Session Routing (Happy Path) | 6 | ~3 | Missing: across turns (2.2), different workers (2.3), mixed (2.4) |
| 3. Multi-Worker Routing | 5 | ~2 | Missing: different workers (3.1), capacity (3.3, 3.5) |
| 4. Session Lock Renewal | 6 | ~2 | Missing: between turns (4.2), on close (4.3), on shutdown (4.4), idle timeout (4.5-4.6) |
| 5. Worker Crash Migration | 4 | 0 | Not covered |
| 6. Graceful Shutdown | 2 | 0 | Not covered (feature not implemented) |
| 7. Orchestration-Controlled Migration | 4 | ~1 | Only partial via close_session_lock_stealing |
| 8. History Validation | 6 | ~2 | Missing: close-without-open (8.1), max+reopen (8.4-8.5), no-session provider (8.6) |
| 9. Replay Determinism | 6 | 0 | Not covered |
| 10. ContinueAsNew | 6 | 0 | Not covered (feature not implemented) |
| 11. Terminal Cleanup | 4 | 0 | Not covered (feature not implemented) |
| 12. Mixed Session + Non-Session | 6 | ~1 | Only basic mixed (12.1) |
| 13. Multiple Sessions Interleaving | 4 | 0 | Not covered |
| 14. Provider Contract | 15 | ~8 | Missing: stolen lock renewal (14.8), noop release (14.10), ack creates/deletes row (14.11-14.12), ack stores session_id (14.13), piggyback renewal (14.14), default supports_sessions (14.15) |
| 15. Edge Cases / Race Conditions | 12 | ~1 | Only race (15.1). Missing all others. |
| 16. Serialization / Backward Compat | 5 | 0 | Not covered |
| 17. Observability | 5 | 0 | Not covered |
| 18. Single-Threaded Runtime | 2 | ~1 | 18.1 covered, 18.2 untested |

### Refactoring Opportunities

**A. Unify worker enqueue paths** — `ack_orchestration_item` has its own inline INSERT for worker queue rows (missing `session_id`) while `enqueue_for_worker` is a separate method with the correct schema. These should share a common helper to prevent divergence.

**B. `SessionRenewalRegistry` should be accessible for shutdown cleanup** — Currently a local variable inside `start_work_dispatcher`. For graceful shutdown to release all session locks, the registry (or a handle to it) needs to be reachable from `Runtime::shutdown`.

**C. Provider `ack_orchestration_item` could handle `OpenSession` actions** — Instead of relying purely on lazy session row creation, the ack path could process `OpenSession`/`CloseSession` actions from `pending_actions` to create/delete session rows atomically. This would make the system queryable and simplify terminal cleanup.

### Priority Actions

1. **Fix `session_id` column omission in `ack_orchestration_item`** — breaks multi-worker routing.
2. **Implement terminal session cleanup** — prevents session row leaks.
3. **Implement ContinueAsNew session carryover** — required for copilot use case.
4. **Implement graceful shutdown session release** — critical for rolling deployments.
5. **Enforce `max_sessions_per_worker`** — prevents worker overload.
6. **Make `open_session_with_id` truly idempotent** — prevents subtle replay bugs.
7. **Fix session lock duration inconsistency** — fetch uses `2*activity_lock_timeout`, renewal uses `session_lock_duration`.
8. **Add input validation** for empty/long session IDs.
9. **Expand test coverage** — particularly sections 5-7, 9-11, 13, 15 (zero coverage).
