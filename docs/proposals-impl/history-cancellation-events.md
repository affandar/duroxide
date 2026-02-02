# Proposal: Persist Activity/Sub-Orchestration Cancellation in History

**Status:** Implemented

**Created:** 2026-02-01

## Motivation

Today, cancellation decisions for:

- **Activities** (select losers, terminal cleanup, continue-as-new cleanup) and
- **Sub-orchestrations** (dropping a sub-orchestration future)

are implemented via **queue-side effects**:

- activities: provider deletes worker-queue rows ("lock stealing")
- sub-orchestrations: runtime enqueues `CancelInstance` to the child

These are effective, but **not visible in the orchestration history**. This has downsides:

- **Observability:** hard to explain why work stopped or why a completion was ignored.
- **Recovery / derived-state reconstruction:** cancellation decisions are not derivable from history.
- **Debuggability:** users/operators can’t answer “was this cancelled or never scheduled?” from history alone.

This proposal adds explicit history events recording cancellation **decisions** (not guarantees) for activities and sub-orchestrations.

## Goals

- Record cancellation *decisions* durably in history for:
  - activity cancellation
  - sub-orchestration cancellation requests
- Preserve replay determinism.
- Keep providers as pure storage/queues (no new provider responsibilities).
- Maintain current best-effort cooperative cancellation model.

## Non-goals

- Exactly-once cancellation delivery.
- Hard-killing user code.
- Changing the existing lock-stealing mechanism.

## Proposed History Events

Add **new event kinds** (names are intentionally “requested”, not “completed”):

- `ActivityCancelRequested { reason: String }`
  - Stored as an `EventKind` variant.
  - `Event.source_event_id = Some(<ActivityScheduled.event_id>)`
  - Indicates the runtime decided an activity should be cancelled.

- `SubOrchestrationCancelRequested { reason: String }`
  - Stored as an `EventKind` variant.
  - `Event.source_event_id = Some(<SubOrchestrationScheduled.event_id>)`
  - Indicates the runtime decided to cancel a scheduled child.

### Reason strings

Use a small, stable set of reason strings (avoid free-form):

- `select_loser`
- `orchestration_terminal_completed`
- `orchestration_terminal_failed`
- `orchestration_terminal_cancelled`
- `orchestration_terminal_continued_as_new`
- `dropped_future`

(Exact strings can be bikeshed; the key is stability for telemetry/logging.)

## Semantics

### Activity cancellation

When does the runtime emit `ActivityCancelRequested`?

- **Select losers / dropped futures:** when an activity `DurableFuture` is dropped after losing a `ctx.select*()` race (or otherwise dropped without completion).
- **Terminal cleanup:** when an orchestration reaches a terminal outcome and the runtime cancels in-flight activities.

**Emission policy:** Emit this event for *all* cases where the runtime is currently cancelling activities via lock stealing.

What does it mean?

- It records a deterministic *decision*: “this activity is no longer needed”.
- It does **not** guarantee the activity will not complete (races exist).
- The system remains **at-least-once** for activity execution.

Interaction with completions:

- It is valid (and expected in rare races) for history to contain:
  - `ActivityCancelRequested(source_event_id=X)` and later
  - `ActivityCompleted(source_event_id=X)` / `ActivityFailed(source_event_id=X)`

This should be treated as “cancel was attempted but lost the race”.

### Sub-orchestration cancellation

When does the runtime emit `SubOrchestrationCancelRequested`?

- When a sub-orchestration `DurableFuture` is dropped before completion (e.g., select loser).

What does it mean?

- It records that the parent runtime attempted to cancel the child by enqueueing `WorkItem::CancelInstance` to the child.
- The child’s own history already records `OrchestrationCancelRequested` when processed.
- Parent history gains an explicit causal breadcrumb.

As with activities, it does **not** guarantee the child will terminate before completing.

## Determinism and Replay

These events are emitted based on deterministic orchestration control flow:

- `ctx.select*` resolution is deterministic (history-driven)
- `DurableFuture::drop` cancellation is deterministic given deterministic control flow

Replay engine considerations:

- The replay engine must treat these as **correlated** events (via `source_event_id`) but they are not “completions”.
- Nondeterminism checks for completions should remain schedule-kind-based (activity/timer/sub-orch), and must not reject completions that arrive after a cancel request.

## Additional Nondeterminism Detection Enabled by These Events

Recording cancellations in history creates a new class of deterministic “breadcrumbs” that can be used to detect more replay drift.

Today, schedule events (e.g., `ActivityScheduled`, `SubOrchestrationScheduled`) and completion events are already used for nondeterminism detection. However, *cancellation decisions* are largely queue-side effects and are not compared against history.

With cancellation events in history, the runtime can detect additional replay mismatches, for example:

- **Select winner/loser drift:** if code changes cause a different branch to win a `select`/`select2`, the set of cancelled losers can change. During replay, the runtime can require that the same `*CancelRequested` events are produced for the same `source_event_id` values.

- **Dropped-future lifetime drift:** if a scheduled future is dropped in the original execution but is now awaited (or kept alive longer) after a code change, the original history will contain `*CancelRequested` but replay would not. That mismatch can be surfaced as nondeterminism.

- **Accidental additional cancellations:** if new code cancels an activity/sub-orchestration that was not cancelled historically, replay would attempt to append a new `*CancelRequested` event that does not exist in history. This is detectable.

Implementation note (conceptual): treat cancellation decisions as another expected, correlated event stream. On each turn, compare “cancellations we would emit now” against cancellation-request events already present in history for this turn. If they differ, raise a nondeterminism configuration error.

## Compatibility Notes

This proposal adds new `EventKind` variants. It is a schema evolution and requires all readers of history to understand the new variants.

## Implementation Sketch

### Runtime changes

- At the point we compute cancellation targets for activities (select losers and terminal cleanup), append `ActivityCancelRequested` events to `history_delta`.
- At the point we enqueue `CancelInstance` for dropped sub-orchestration futures, append `SubOrchestrationCancelRequested` to the *parent* `history_delta`.

The cancellation mechanisms themselves do not change:

- activities: still lock-stealing (delete worker queue entries)
- sub-orchestrations: still enqueue `CancelInstance` for the child

### Event shape

- Use `Event.source_event_id` to correlate cancellations to their schedule event id.
- `execution_id` is the current execution.
- Ordering: cancellation request events should be appended in the same turn where the cancellation decision becomes known.

## Impact Evaluation

### Pros

- Clear observability: history answers “was this cancelled?”
- Enables history-based reasoning for recovery/derived state tools.
- Makes cancellation behavior explicit and testable.

### Cons

- Increased history volume (especially for large fan-out + select patterns).
- Introduces new edge cases in “cancel requested but completion arrived anyway” narratives.

## Test Plan

Add tests in two layers:

1) **Replay-engine unit tests**
- Scheduling an activity, dropping it (select loser) produces `ActivityCancelRequested` in history delta.
- Scheduling a sub-orch, dropping it produces `SubOrchestrationCancelRequested` in history delta.
- Replay tolerates completion after a cancel request (no nondeterminism error).
- Replay fails with nondeterminism when cancellation decisions differ from history (e.g., select winner changes).

2) **End-to-end integration tests**
- A select-loser activity produces a cancel-request event in persisted history.
- Sub-orchestration drop results in:
  - parent history contains `SubOrchestrationCancelRequested`
  - child eventually reaches cancelled/failed status (best-effort; allow timing variability)

## Open Questions

This proposal chooses:

- **Requested-only.** No ack/confirmed variants.
- **Strings for reasons.**
- **Emit for all current lock-stealing cancellations** (select losers + terminal cleanup + continue-as-new cleanup).

## Implementation Notes

- **Reason strings:** The implementation uses a small stable set and currently records dropped-future cancellations as `dropped_future` (it does not distinguish `select_loser` vs explicit drop).
- **Sub-orchestration cancellation scope:** The implementation records `SubOrchestrationCancelRequested` for dropped sub-orchestration futures, and also when the parent orchestration is cancelled and proactively enqueues `CancelInstance` for still-in-flight children.
- **Replay nondeterminism detection:** The replay engine enforces that the set of `*CancelRequested(reason=dropped_future)` events in persisted history matches the cancellation decisions produced during replay **when those cancel-request events are already present in history** (i.e., when replaying a previously-committed turn). Mismatches raise a nondeterminism configuration error.
