# Reliability: Transactional Rounds for Queue and History

Goal: Make each runtime “round” atomic at the provider boundary so that enqueue/dequeue and history append happen together. This enables at‑least‑once processing with bounded duplicates and simpler recovery.

## Problem Recap

Today, the runtime performs queue ops and history appends as separate calls. A crash between them can lose work (destructive dequeue) or create dangling items (enqueued without matching history). We also want reprocessing to be safe (idempotent) when items are retried.

## High‑level Design

- Introduce a transactional provider API that batches queue and history changes in an atomic commit per round.
- Adapt runtime paths to use this API for both scheduling (turn actions) and completion processing.
- Add minimal dedupe/constraints to reduce duplicates across retries.

We defer the concrete FS implementation details (temp files + rename, leases, etc.). This doc focuses on API shape and runtime flow.

## Provider API Extensions

New trait methods (async):

```rust
/// A token returned when an item is dequeued within a round.
pub struct DequeuedItem { pub item: WorkItem, pub token: String }

/// An atomic batch of queue and history changes. Changes are staged until commit().
pub struct Round<'a> { /* provider-internal */ }

#[async_trait]
pub trait HistoryStore {
    // Existing APIs remain.

    /// Run a transactional round. All changes inside either commit together or not at all.
    async fn run_round<R, F, Fut>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Round) -> Fut + Send,
        Fut: std::future::Future<Output = Result<R, String>> + Send,
        R: Send;
}

impl<'a> Round<'a> {
    /// Stage appending events to an instance’s history (MUST be idempotent by correlation id/kind).
    pub async fn append(&mut self, instance: &str, events: Vec<Event>) -> Result<(), String>;

    /// Stage enqueuing work items (MUST be idempotent by a stable key like correlation id + kind).
    pub async fn enqueue(&mut self, items: Vec<WorkItem>) -> Result<(), String>;

    /// Dequeue a single visible work item without removing it yet (remove on commit).
    pub async fn dequeue(&mut self) -> Result<Option<DequeuedItem>, String>;

    /// Acknowledge the dequeued item from this round. Effective only on commit.
    pub async fn ack(&mut self, token: &str) -> Result<(), String>;

    /// Commit/rollback managed by the provider around the closure; explicit calls optional.
}
```

Notes:
- dequeue() must NOT destructively remove the item immediately. Removal is bound to commit() together with append().
- If the process crashes before commit, the item must still be deliverable later (at‑least‑once).
- For single‑host initial implementation, no time‑based leases are strictly required; later we can extend with leases.

## Idempotence/Dedupe Contracts

History:
- Appending completion events must be idempotent by correlation id:
  - ActivityCompleted/Failed keyed by (id, kind)
  - TimerFired keyed by (id)
  - ExternalEvent keyed by (id)
  - SubOrchestrationCompleted/Failed keyed by (id)
- Provider MAY drop duplicates silently; runtime already tolerates duplicates by checking history before dispatch.

Queue:
- enqueue must be idempotent with a stable key per item (e.g., correlation id + kind).
- Completion items are unique by provider‑generated queue id, and only removed on commit of the paired append.

## Runtime Flow Changes

1) Turn execution (scheduling actions)

Current: host materializes actions by enqueuing work and appending schedule events in separate calls.

New:
```rust
store.run_round(|round| async move {
    // Record schedule/subscription events first
    round.append(instance, schedule_events).await?;
    // Enqueue the matching work items atomically (idempotent)
    round.enqueue(work_items).await?;
    Ok(())
}).await?;
```

Effects: Either both schedule events and work items are visible, or neither. No dangling state.

2) Worker completion processing (activities/timers/sub‑orch)

Current: dequeue → append completion → run orchestrator → schedule (append new schedule events + enqueue new work) → ack (spread across calls).

New (transactional):
```rust
store.run_round(|round| async move {
    if let Some(dq) = round.dequeue().await? {
        // a) Append the completion
        let completion_events = build_completion_events(&dq.item, …);
        round.append(&dq.item.instance, completion_events).await?;

        // b) Run the orchestrator turn(s) to the next await, producing schedule/subscription events and work items
        let (schedule_events, work_items) = drive_orchestrator_once(&dq.item.instance).await?;

        // c) Persist follow‑on scheduling atomically with the completion
        if !schedule_events.is_empty() { round.append(&dq.item.instance, schedule_events).await?; }
        if !work_items.is_empty() { round.enqueue(work_items).await?; }

        // d) Ack the completion only if everything above is durable
        round.ack(&dq.token).await?;
    }
    Ok(())
}).await?;
```

Effects: Either the completion and the follow‑on scheduling both land, or neither. If commit fails, the item is re‑delivered and dedupe prevents double‑append/duplicate enqueue.

3) External events (raised via runtime.raise_event)

Keep the existing WorkItem::ExternalRaised path so routing/gating stays uniform. Handle it like any completion: dequeue within a round, append ExternalEvent if a matching subscription exists; otherwise drop with warning (no ack?). To avoid redelivery storms, ack even if dropped due to no subscription, but only after verifying the instance is currently active for the intended execution.

4) ContinueAsNew

Reset/create next execution and append OrchestrationContinuedAsNew/Started within a round when rolling over. If enqueueing any follow‑up work for the new execution, do it in the same round.

## Minimal Runtime Dedupe (kept)

Even with provider‑side idempotence, keep fast runtime guards:
- Before dispatching an activity/timer/sub‑orch for a correlation id, check history for an existing completion and skip dispatch.
- When processing a dequeued completion, if history already contains a completion for that id, treat as duplicate: ack and do not append again.

These reduce churn and simplify provider burden.

## Error Handling & Retries

- run_round closure returns Result; provider rolls back on error and returns the error.
- Call sites use targeted backoff and retry policies (especially for transient IO).
- Ensure retries are safe due to enqueue_if_absent and history dedupe.

## Test Plan

- Unit: provider mock that counts calls and enforces atomicity; verify no partial effects under injected failures.
- Integration (in‑memory provider):
  - Turn scheduling: inject commit failure; on retry, ensure no duplicates (exactly one set of schedule events and work items).
  - Completion processing: fail after dequeue but before append; ensure item is reprocessed and history has one completion.
  - External events early/late and drop‑with‑ack semantics preserved.
- E2E (FS provider later): same scenarios behind a feature flag.

## Migration Plan

1) Add the new provider API alongside existing methods; provide a default run_round implementation that locks a global mutex (in‑memory) and performs the sequence as best‑effort.
2) Update runtime paths to use run_round; keep old code path behind a fallback feature for quick bisect.
3) Implement proper in‑memory provider semantics.
4) Implement FS provider semantics (temp files + rename);
5) Add optional leases/timeouts in a follow‑up (Phase 2 in reliability doc).

## Open Questions

- Should dequeue expose a lease with timeout now or later? Initial single‑host can skip; multi‑host requires it.
- What’s the right behavior for ExternalRaised with no subscription present: ack immediately (drop) vs. requeue? Current design: warn and drop with ack to avoid tight redeliveries.
- Do we need per‑instance rounds vs global? For simplicity, scope rounds to single instance history + global queue ops.

---

## Fallback: Non‑transactional providers (peek‑lock mode)

If a provider can’t implement atomic rounds, we can still achieve at‑least‑once with bounded duplicates using a peek‑lock queue and idempotent operations:

Provider capabilities required:
- dequeue_peek_lock() -> (item, token, invisible_until)
- ack(token), abandon(token)
- append(instance, events): MUST be idempotent by correlation id/kind
- enqueue(items): MUST be idempotent by a stable key

Runtime flows:

1) Turn scheduling (actions -> schedule events + work items)
- Step A: append(schedule_events) [idempotent]
- Step B: enqueue(work_items) [idempotent]
- Rationale: if we crash after A but before B, retry will append no‑op and then enqueue. If we did B before A and crashed, workers might process items before schedule exists, causing replay mismatch.

2) Completion processing (activities/timers/sub‑orch)
- dequeue_peek_lock() -> (wi, token)
- If history already contains completion for wi.id: ack(token) and return (dedupe fast‑path)
- Else:
    - append(completion_events)
    - run orchestrator turn to produce schedule events + work items
    - append(schedule_events); enqueue(work_items)
    - On success: ack(token)
    - On any failure: abandon(token) so it’s retried later
- Crash cases:
    - After append success but before ack: item will redeliver; fast‑path detects completion and acks.
    - Before append: no history change; item reappears; safe to retry.

3) External events
- Keep ExternalRaised as queue items. On dequeue:
    - If subscription present (id known): append(ExternalEvent{id}) then ack
    - If no subscription: warn and ack (drop) to avoid tight retry loops; semantics unchanged.

4) ContinueAsNew
- Sequence without round:
    - append(OrchestrationContinuedAsNew)
    - reset_for_continue_as_new(instance, name, input) is idempotent (no‑op if new execution exists and has OrchestrationStarted)
    - Optionally enqueue initial work for the new execution via enqueue
- Crash after CAN but before reset: retry detects terminal event and calls reset idempotently.

Edge cases and guarantees:
- Enqueued‑without‑schedule is prevented by strict ordering (append schedule first, then enqueue).
- Duplicate completions are dropped by append_idempotent and fast‑path history checks.
- At‑least‑once is preserved because we only ack after successful append; abandon on failure.
- Idempotent enqueue prevents multiple identical work items after retries.

Tests to add for this mode:
- Crash window simulation: after schedule append success but before enqueue; ensure eventual enqueue happens once.
- Completion: append success then crash before ack; ensure single completion in history and eventual ack.
- External: early raise before subscription gets acked/dropped; post‑subscribe raise succeeds once.
