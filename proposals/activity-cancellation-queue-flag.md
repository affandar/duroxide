## Proposal: Activity cancellation via queue-row cancel flag (transactional, no `ExecutionState`)

### Status

Draft.

### Motivation

The current activity cancellation design cancels activities by **inferring** cancellation from the parent orchestration’s terminal status (`ExecutionState::{Terminal,Missing}` returned by the provider on `fetch_work_item` / `renew_work_item_lock`). This has two major shortcomings:

- **Cancellation scope is too narrow**: activities only cancel when the orchestration becomes terminal/missing, not when cancellation should occur while the orchestration continues (e.g., `ctx.select()` timeouts, select losers, proactive cancel paths).
- **Provider coupling**: providers must look up orchestration execution status during activity fetch/renew, which is an orchestration-level concern, not an activity-level signal.

This proposal replaces inferred cancellation with an **explicit cancel-requested flag stored on the worker queue row** for an activity execution. Workers observe cancellation via the existing renew thread and cooperate using the activity cancellation token.

### Goals

- **Remove `ExecutionState` completely** from the provider API and runtime cancellation logic.
- Make cancellation **explicit and activity-targeted** via a `cancel_requested` flag (plus optional reason) on the activity work item storage record.
- Ensure cancellation requests are **transactional** with orchestration turns (applied inside `ack_orchestration_item`).
- Support the following cancellation triggers:
  - orchestration cancellation (`CancelInstance`)
  - orchestration failure
  - continue-as-new (old execution should cancel its outstanding activities)
  - activity loses a `ctx.select()` / `select2()` race (e.g., timeout wins)
- Avoid head-of-line blocking and avoid introducing new “cancel message” work items in the worker queue.

Non-goals (for this proposal):

- Hard-kill of user code (we remain cooperative + best-effort abort after grace).
- Exactly-once cancellation delivery semantics (idempotent “set a flag” is sufficient).
- Changing durable future replay semantics (other than making select-loser cancellation requestable).

---

## High-level design

### Key idea: cancellation is metadata on the activity’s queue row

Each `WorkItem::ActivityExecute { instance, execution_id, id, ... }` is stored in the provider’s `worker_queue`. We extend that stored record with:

- the activity identity (`instance_id`, `execution_id`, `activity_id`), and
- cancellation metadata (`cancel_requested`, `cancel_reason`, optional timestamp).

Workers observe cancellation via the existing “activity manager” renew loop (lock renewal task). Instead of checking orchestration status, the renew loop checks the `cancel_requested` flag for the locked activity row.

### Key change: cancellation updates are applied transactionally inside `ack_orchestration_item`

When the runtime decides an activity should be cancelled, it includes an **activity cancel request** in the `ack_orchestration_item` payload, so the provider can update `worker_queue` rows in the same transaction that commits:

- history delta (events)
- enqueued work items (worker and orchestrator)
- instance locks/queue locks release

This provides a clean correctness story:

- If the orchestration turn commits, cancellation flags are durable.
- If the turn rolls back, neither the state transition nor cancellation flags are applied.

---

## API changes (core `Provider` trait)

### Remove `ExecutionState`

Remove the `ExecutionState` enum and the return values that include it.

### Introduce `CancelInfo`

Add a minimal cancellation metadata type surfaced by worker dequeue/renew:

- `cancel_requested: bool`
- `reason: Option<String>`

### Update worker APIs to surface `CancelInfo`

- `fetch_work_item(lock_timeout, poll_timeout) -> Option<(WorkItem, lock_token, attempt_count, CancelInfo)>`
- `renew_work_item_lock(lock_token, extend_for) -> CancelInfo`

Renew semantics:

- The provider **MUST extend the lock** for the locked row even if `cancel_requested=true`.
  - Rationale: the current worker should be able to run its grace period and then ack the item without another worker stealing it mid-cancel.

### Transactional cancel requests in `ack_orchestration_item`

Extend `ack_orchestration_item` to accept:

- `activity_cancels: Vec<ActivityCancelRequest>`

Where `ActivityCancelRequest` contains:

- `instance: String`
- `execution_id: u64`
- `activity_id: u64` (the scheduling event id)
- `reason: String`

Provider applies all cancel requests during the `ack_orchestration_item` transaction.

Performance note: providers should apply cancellations in a set-based way (see “Mass cancellation performance” below).

---

## Provider storage/schema changes

### Worker queue row changes

The worker queue must store the activity identity in structured columns so updates do not require scanning/deserializing JSON payloads.

Add columns to `worker_queue`:

- `instance_id TEXT NOT NULL` (only for `ActivityExecute` rows)
- `execution_id INTEGER/BIGINT NOT NULL`
- `activity_id INTEGER/BIGINT NOT NULL`
- `cancel_requested BOOLEAN/INTEGER NOT NULL DEFAULT 0`
- `cancel_reason TEXT NULL`
- `cancel_requested_at_ms BIGINT NULL` (optional; useful for observability)

Indexes:

- `UNIQUE(instance_id, execution_id, activity_id)` is ideal if you guarantee at most one outstanding row per scheduled activity id.
  - If uniqueness is not guaranteed (e.g., retries modeled as multiple rows), then use a non-unique index and define cancel semantics carefully (see below).
- At minimum, create `INDEX(instance_id, execution_id, activity_id)`.
- If frequently querying cancel status on renew, ensure the index supports that lookup efficiently.

### Enqueue path changes

Whenever a provider inserts an `ActivityExecute` into `worker_queue`, it must:

- populate `instance_id`, `execution_id`, `activity_id` from the work item
- initialize `cancel_requested=0`, `cancel_reason=NULL`

### Fetch path changes

`fetch_work_item` should return `CancelInfo` for the dequeued row:

- If `cancel_requested=true`, the runtime should skip starting the activity and ack the item (drop).

### Renew path changes

`renew_work_item_lock` should:

- locate the row by `lock_token`
- extend `locked_until`
- read and return `cancel_requested` + `cancel_reason`

---

## Runtime changes

### Worker dispatcher behavior

Worker behavior becomes purely “cancel flag driven”:

- On dequeue:
  - if `cancel_requested=true`, do not execute the activity.
  - ack the worker item with `completion=None` (drop), because orchestration has decided it will not consume the result.
- During execution:
  - the activity manager periodically renews the lock.
  - if renewal returns `cancel_requested=true`, it signals the activity cancellation token.
  - the worker waits up to `activity_cancellation_grace_period`, aborts the task if needed, and acks the worker item (drop completion).

### Orchestration turn: when to request cancellation

The runtime should request activity cancellation (by setting queue flags via the provider) in these cases:

1. **Orchestration cancellation requested** (`WorkItem::CancelInstance` → `OrchestrationCancelRequested`):
   - Cancel all outstanding activities for the current execution.
2. **Orchestration failure** (turn results in `OrchestrationFailed`):
   - Cancel outstanding activities for the current execution.
3. **ContinueAsNew**:
   - For the current execution (the one being continued), cancel outstanding activities for that execution.
4. **`ctx.select()` / `select2()` loser**:
   - If an activity future loses a select race, request cancellation for that activity id, with a reason like:
     - `"select_loser:timeout"` when a timer wins
     - `"select_loser:other"` otherwise
   - **Note:** This automatically covers the per-attempt timeout path in `schedule_activity_with_retry`, since it is implemented as `select2(activity, timer)`. When the timer wins, the activity becomes the select loser and will be cancelled via this mechanism.

### How to compute “outstanding activities”

For a given `instance` + `execution_id`:

- scheduled = all `EventKind::ActivityScheduled` event ids (these are the `activity_id`s)
- completed = all completion events with `source_event_id` referring to an activity schedule id:
  - `ActivityCompleted`, `ActivityFailed`
- outstanding = scheduled \ completed

The runtime should only request cancellation for outstanding ids. This reduces needless updates and avoids “cancel after completion” churn.

### Idempotency and repeated turns

The `activity_cancels` vector may be computed repeatedly across turns (e.g., if the orchestration is already cancelled and continues to receive items). The provider update is idempotent:

- setting `cancel_requested=1` repeatedly is safe
- `cancel_reason` should be set with a stable policy:
  - either “first reason wins” (`COALESCE(cancel_reason, new_reason)`), or
  - “last reason wins” (overwrite), but that can make debugging noisy

---

## Mass cancellation performance (critical)

Orchestration cancellation can require cancelling many outstanding activities. We must avoid N separate `UPDATE`s when N is large.

### Preferred approach: set-based update in one statement (batched)

Providers should apply cancellation updates in a set-based manner.

Examples:

- Postgres: `UPDATE ... FROM (VALUES ...)`
- SQLite: `WITH cancels(...) AS (VALUES ...) UPDATE ... WHERE EXISTS (...)`

Batching:

- If the cancel list is large, split into batches (e.g., 500–2000 items per statement) to avoid SQL parameter limits and huge statements.

Indexes:

- Ensure `(instance_id, execution_id, activity_id)` index exists so the update touches only relevant rows.

### Optional fast path: “cancel execution generation” (future)

If mass cancels become a scaling bottleneck, consider a future enhancement:

- store `execution_cancel_gen` in an execution metadata row
- store `cancel_gen_seen` on each worker_queue row
- renew checks `execution_cancel_gen > cancel_gen_seen`

This makes “cancel all outstanding work for execution” O(1), but complicates targeted cancels (select losers). This proposal does not include it.

---

## Semantics and edge cases

### If cancellation is requested before an activity starts

- Worker dequeues `ActivityExecute` and sees `cancel_requested=true`.
- Worker immediately acks the work item without executing.

### If cancellation is requested while an activity is running

- Renew loop observes `cancel_requested=true`.
- Worker signals cancellation token.
- After grace period, abort if needed and ack without completion.

### If cancellation is requested after the activity completed but before worker ack

- The completion is still in-process; policy is “cancel wins once observed”.
- Worker drops completion and acks without enqueueing orchestrator completion.

### If cancellation is requested after completion is already enqueued to orchestrator

- Cancellation flag update is benign; orchestration will observe the completion normally.
- This can happen if the cancel request list is computed using history that didn’t yet include the completion.
- Keeping updates idempotent makes this safe.

### Retry / multiple attempts

This proposal assumes **one worker_queue row per scheduled activity id** (and retries happen via re-delivery of the same row, not by creating a new row).

If the system creates multiple rows per activity id (e.g., explicit retry rows), then cancellation must update all rows for that `(instance, execution_id, activity_id)` or you must include an attempt identity. Prefer the former: “cancel the scheduled activity id cancels all its attempts”.

---

## Migration plan (providers)

### SQLite provider (in this repo)

- Add migration to extend `worker_queue` with identity + cancel columns.
- Update enqueue path to populate identity columns for `ActivityExecute`.
- Update fetch/renew to return `CancelInfo`.
- Update ack_orchestration_item implementation to apply `activity_cancels` set-based.
- Remove execution-status lookups for worker operations.

### Postgres provider (`duroxide-pg`)

- Add migration extending `worker_queue` similarly.
- Update stored procedures:
  - enqueue worker work to populate identity columns
  - fetch/renew to return cancel metadata and stop returning execution state
  - ack_orchestration_item procedure signature updated to accept cancellation batch payload and apply set-based updates
- Remove the “execution state support” procedure logic (previously derived from `executions.status`).

---

## Testing changes

### Remove/replace existing provider validation tests

Current validations asserting `ExecutionState` behavior become obsolete:

- `src/provider_validation/cancellation.rs` tests:
  - `fetch_work_item` returns `ExecutionState::{Running,Terminal,Missing}`
  - `renew_work_item_lock` returns `ExecutionState::{Running,Terminal,Missing}`

These should be removed or rewritten to validate the new cancel-flag semantics.

### New provider validation coverage (unit/validation tests)

Add a new provider validation module, e.g. `src/provider_validation/activity_cancel_flags.rs`, covering:

1. **Fetch surfaces cancel flag**
   - Enqueue an `ActivityExecute` row.
   - Mark `cancel_requested=true` transactionally (via the provider path used by ack).
   - `fetch_work_item` must surface `CancelInfo.cancel_requested=true`.

2. **Renew surfaces cancel flag**
   - Fetch an activity to obtain `lock_token`.
   - Mark `cancel_requested=true`.
   - `renew_work_item_lock` must return `cancel_requested=true`.

3. **Renew still extends lock when cancelled**
   - Fetch activity and lock it.
   - Mark cancelled.
   - Call renew and ensure subsequent renew calls still succeed within expected window (or inspect locked_until if provider exposes it via debug APIs).

4. **Transactional application in `ack_orchestration_item`**
   - Create an orchestration instance.
   - In a single `ack_orchestration_item`, enqueue an activity AND include `activity_cancels` for that activity id.
   - Fetch the activity and ensure `cancel_requested=true` immediately.

5. **Mass cancellation batching**
   - Enqueue many activities (e.g., 2000).
   - Apply cancellation in one ack in batches.
   - Assert performance indirectly by ensuring operation completes within a reasonable time limit in CI (avoid flakiness; use generous bounds).

### Runtime tests (integration)

Add/adjust runtime-level tests to validate behavior end-to-end:

1. **Select timeout cancels losing activity**
   - Orchestration schedules `select2(activity, timer)`, timer wins.
   - Assert the orchestration completes deterministically and does not hang.
   - Assert the activity handler observes cancellation token (activity should exit early) OR at minimum that the worker drops the activity.

2. **CancelInstance cancels outstanding activities**
   - Orchestration schedules a long-running activity.
   - Client cancels instance.
   - Verify the activity is cancelled by worker (token triggered) and does not keep running indefinitely.

3. **ContinueAsNew cancels previous execution activities**
   - Execution 1 schedules a long activity and then continues-as-new.
   - Verify execution 1’s activity gets cancelled (and doesn’t complete into execution 2 unexpectedly).

### Test plan additions (human/CI)

Update CI/test plan documentation (or the PR description) to include:

- **Correctness**:
  - cancellation requested before start: activity never begins
  - cancellation while running: renew detects and cancels within ≤ 1 renew interval + grace
  - no dependency on orchestration status for cancellation
- **Performance**:
  - mass cancel uses set-based updates; verify no O(N) round-trip loops
  - ensure indexes exist and are used (in Postgres, confirm via `EXPLAIN` in a provider test if feasible)
- **Regression**:
  - existing orchestration completion/failed behavior unchanged
  - poison message detection still works (attempt_count semantics unaffected)

