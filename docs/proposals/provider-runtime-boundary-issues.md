# Provider ↔ Runtime Boundary Issues

**Status:** Draft

This document captures places where the current Provider abstraction is not a “dumb store”.
In several areas, provider implementations are required to understand runtime semantics (or reconstruct them), which increases provider complexity and makes it harder to implement alternative backends.

## Goal

- Keep providers as close to **pure storage + queue primitives** as practical.
- Make runtime responsible for **all orchestration semantics**.
- Make the provider contract explicit about what *must* be understood.

## Current Boundary Leaks (Hot Path)

### 1) WorkItem queue semantics (instance extraction, delayed visibility)
In the hot path, the runtime already splits queue intent in `ack_orchestration_item()` via
separate parameters (`worker_items` vs `orchestrator_items`), so providers do **not** need to
infer “worker vs orchestrator” from `WorkItem` variants there.

However, providers still need to understand some `WorkItem` semantics to store/route correctly:
- **Instance extraction:** e.g., `SubOrchCompleted/SubOrchFailed` route by `parent_instance` rather than `instance`.
- **Delayed visibility:** e.g., `TimerFired` uses `visible_at = fire_at_ms`.

Additionally, the standalone provider APIs `enqueue_for_orchestrator()` / `enqueue_for_worker()`
encode queue intent at the call site, but may still require metadata extraction for indexing.

Why this is a leak:
- Providers are forced to understand message-specific fields/behavior beyond generic enqueue/dequeue.

Impact:
- Every provider must duplicate the same routing logic.

### 2) Activity identity & cancellation (lock-stealing) coupling
Providers must support activity cancellation by identity `(instance_id, execution_id, activity_id)`.
SQLite implements this by extracting identity fields from `WorkItem::ActivityExecute` and later deleting matching rows.

Why this is a leak:
- Provider must understand which message types carry the identity fields and what they mean.

### 3) Provider reconstructs orchestration metadata when missing
`fetch_orchestration_item()` requires `orchestration_name` and `version`.
When the `instances` row is missing, SQLite derives name/version by inspecting:
- persisted history (`EventKind::OrchestrationStarted`), or
- queued messages (`WorkItem::StartOrchestration` / `WorkItem::ContinueAsNew`).

Why this is a leak:
- Provider code is forced to understand event schemas / runtime invariants.
- This is orchestration knowledge in the provider.

### 4) History indexing requires EventKind matching
SQLite persists an `event_type` column and determines it via a `match` over `EventKind`.

Why this is a leak:
- The provider now needs to be updated whenever new history event variants are added.

Notes:
- This is not required for correctness (JSON already stores the event), but it is a cross-cutting maintenance burden.

### 5) Cancellation-request events as operational signals (potential)
Now that we have cancellation request events in history (`ActivityCancelRequested`, `SubOrchestrationCancelRequested`), it’s tempting to have providers derive cancellation operations from those events.

Why this is a leak:
- Providers would need to understand history semantics (which events imply which queue mutations).

Current stance (as of 2026-02-01):
- Cancellation request events are **observability breadcrumbs**.
- Operational queue cancellation is driven by an explicit `cancelled_activities` list passed to `ack_orchestration_item()`.

## Boundary Leaks (Cold Path / Management)

### 6) Management APIs require history shape understanding
Even if kept out of the hot path, management features (queries by status, event counts, filtering) often require DB schema or indexing that tracks event categories.

This is acceptable as long as:
- Management APIs are explicitly separate (they are today), and
- The runtime core provider interface remains simple.

## Options

### Option A — Keep providers dumb; keep cancellation list
**Summary:** Keep the provider runtime contract as “store history + enqueue/dequeue”, and keep `cancelled_activities` passed explicitly.

- Pros:
  - Provider does not need to interpret history.
  - Operational semantics stay runtime-owned.
- Cons:
  - Redundant representation (history breadcrumb + explicit cancel list).
  - Still requires providers to implement identity columns and lock-stealing semantics.

### Option B — Make cancellation fully history-driven
**Summary:** Remove `cancelled_activities` from `ack_orchestration_item()`. Provider derives cancellations by scanning `history_delta` for `ActivityCancelRequested`.

- Pros:
  - Single source of truth (history).
  - Fewer parameters in Provider API.
- Cons:
  - Provider is no longer a dumb store; it must understand history semantics.
  - Harder to evolve event schema without updating providers.
  - Mixed-version clusters become trickier (runtime emits new events; old provider may not know semantics).

### Option C — Introduce an explicit “queue-mutation” delta
**Summary:** Keep history as pure audit trail and add an explicit “queue mutations” structure emitted by runtime and applied by provider.

Example (conceptual):
- `QueueDelta { delete_worker_rows: Vec<ActivityIdentity>, enqueue_worker: Vec<WorkItem>, enqueue_orch: Vec<WorkItem>, ... }`

- Pros:
  - Providers apply mutations without interpreting history.
  - Clean separation: history = audit, delta = operational.
- Cons:
  - API expansion / complexity.
  - Needs careful idempotency and versioning.

### Option D — Reduce provider responsibilities with an envelope
**Summary:** Wrap `WorkItem` in a provider-facing envelope containing routing + identity metadata computed by runtime.

Example (conceptual):
- `EnqueuedWorkItem { queue: Worker|Orchestrator, instance_id, execution_id, activity_id, visible_at_ms, payload: WorkItem }`

- Pros:
  - Provider doesn’t need to pattern-match `WorkItem` to route or index.
  - Cleaner provider implementations.
- Cons:
  - Breaking-ish change to provider APIs and storage schema expectations.

### Option E — Make OrchestrationItem metadata optional
**Summary:** Allow providers to return an orchestration work batch without reconstructing name/version.

- Pros:
  - Removes “provider must inspect history/work items” requirement.
  - Improves purity of provider layer.
- Cons:
  - Requires runtime to resolve missing metadata earlier.
  - Touches a lot of call sites / types.

## Recommended Near-Term Direction

- Prefer **Option A** as the default stance: keep providers dumb and keep explicit cancellation lists.
- Continue evolving toward **Option C/D/E** when we’re ready to change the Provider API with a deliberate rolling-upgrade story.

## Related

- Provider contract: `src/providers/mod.rs`
- SQLite provider: `src/providers/sqlite.rs`
- History cancellation events: `docs/proposals-impl/history-cancellation-events.md`
