## Reliability plan: work queue and history persistence

Goal: Ensure dequeued work is not lost if the runtime crashes or a history append fails, while keeping history deterministic and idempotent.

### Current behavior

- Queue (FS provider): JSONL file with destructive dequeue (remove-first, truncate+rewrite). No leases.
- Runtime: dequeues → dispatches → `append` history. On append failure, runtime panics.
- Idempotence: Now guarded at dispatch time in the runtime for activities/timers (skip dispatch if completion already in history).

Risks:
- Loss: dequeue removes item; crash before successful `append` loses work.
- Races/duplicates: retries can deliver same completion again; without guards it duplicates.
- Queue rewrite is not atomic nor leased.

### Phase 1: Immediate hardening (done/next)

- Runtime idempotent dispatch (done):
  - Before sending `CallActivity`, check history for `ActivityCompleted/Failed` with same id; if present, skip dispatch.
  - Before sending `CreateTimer`, check for `TimerFired` with same id; skip dispatch.
- Retry-on-failure (todo):
  - If a dequeued `WorkItem` processing fails (e.g., persistence error), re-enqueue the same item and back off. This restores at-least-once semantics despite destructive dequeue.
- Tests (todo):
  - Simulate append failure and verify re-enqueue + no duplicate history with idempotent dispatch.

### Phase 2: Leased durable queue (recommended)

Extend `HistoryStore`:

- `dequeue_work() -> Option<(WorkItem, LeaseId, Instant)>`
- `ack_work(LeaseId) -> Result<(), String>`
- Optional `extend_lease(LeaseId, Duration)`

FS provider:

- Track records: `{ id, item, invisible_until, delivery_count }` in JSONL.
- Dequeue: mark visible record invisible for a lease, rewrite file atomically (write temp file + rename), increment `delivery_count`.
- Ack: remove by id (atomic rewrite). Unacked items reappear after lease expiry.
- Dead-letter: if `delivery_count` exceeds threshold, move to `work-dlq.jsonl`.

Runtime:

- Reserve item (lease) → process → on success (after history `append`), ack. On failure, do not ack; item becomes visible again after lease.

### Phase 3: Idempotent history and terminal events

- Keep runtime dispatch guards for activities/timers.
- Optional provider-side dedupe: reject appends of identical completion for the same id/kind.
- Append terminal events (`OrchestrationCompleted/Failed`) only if not already present; runtime already does this check before appending.

### Phase 4: Observability and tooling

- Metrics: dequeue, redelivery, ack, dlq, append failures, processing latency.
- Admin helpers: list in-flight, extend lease, requeue, inspect DLQ.

### Rollout plan

1) Implement Phase 1 retry + tests.
2) Implement Phase 2 leased queue for FS provider (feature-gated), then in-memory.
3) Add metrics and basic admin utilities.

This gives us practical at-least-once delivery with idempotent processing now, and a robust leased queue shortly after, without changing the orchestrator model.


