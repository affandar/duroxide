# Runtime Design Invariants & Rationale

This document captures the design rules that govern the runtime, with rationale. Treat these as invariants to check before making changes.

## 1) History is the single source of truth
- All orchestration state derives from event history; no separate metadata store.
- `Event::OrchestrationStarted` holds required orchestration metadata: `{ name, version: String, parent_instance?, parent_id? }`.
- Removed Provider metadata APIs (`set/get_instance_orchestration`) and `ParentLinked` event.
- Rationale: avoids divergence between metadata and history; simplifies recovery and replay.

## 2) Version pinning and determinism
- The pinned version is the `version` recorded in `OrchestrationStarted`. It is definitive for that execution.
- Runtime resolves version at start per registry policy (e.g., latest), but the on-disk `version` is the durable pin.
- If history pins a version not present in the registry, replay must cancel the instance deterministically with an error.
- `version` is a `String` (no `Option`).
- Rationale: stable replay across code evolution; predictable behavior on code upgrades.

## 3) Multi-queue provider with peek-lock
- Distinct queues via `QueueKind`: `Orchestrator`, `Worker`, `Timer`.
- Dispatchers:
  - OrchestrationDispatcher consumes Orchestrator queue and drives replay/append/ack.
  - WorkDispatcher executes activities and enqueues completions back to Orchestrator.
  - TimerDispatcher schedules and later enqueues `TimerFired` back to Orchestrator.
- Rationale: separation of concerns, scalability, and durability boundaries.

## 4) Ack/abandon reliability rules
- Only ack items that change history after durable append succeeds.
- Duplicate/no-op items are acked immediately without append.
- If an instance is dehydrated (no inbox), rehydrate and abandon so the item redelivers.
- Invalid or stale `execution_id` → warn/ack; do not write.
- Rationale: at-least-once delivery with exactly-once effects via history dedupe.

## 5) Replay engine boundaries
- Pure replay engine computes `Decision`s from history and orchestration function.
- A separate applicator persists history and enqueues work.
- No side-effects occur inside replay; all effects go through provider queues.
- Rationale: deterministic, testable core; clear separation from I/O.

## 6) Start behavior
- If `StartOrchestration` arrives for an instance that already has history, warn and drop (no-op).
- `OrchestrationStarted` must be appended before a start is acked.
- Rationale: idempotent start; avoid duplicate begins.

## 7) External events (by name only)
- Removed `ExternalEvent` variant; keep `ExternalByName` routing.
- Timer bootstrap on startup removed (timers are persisted and rehydrated from queues).
- Rationale: simplify external signaling surface; avoid redundant scheduling.

## 8) Dehydration/rehydration of instances
- Idle orchestration inboxes are unregistered to free resources.
- When completions arrive for a dehydrated instance, trigger `ensure_instance_active` and abandon for redelivery.
- If rehydration finds no `OrchestrationStarted` in history, treat as corruption: log error and panic.
- Rationale: resource efficiency; robust reactivation on demand; clear fail-fast on corrupted history.

## 9) Execution IDs for completions
- All completion `WorkItem`s carry `execution_id`. The runtime keeps only the latest execution id in memory per instance.
- Orchestrator validates `execution_id` on every completion; mismatches are warned and ignored (acked), not written.
- Activity execution path:
  - `ActivityExecute` carries `execution_id` (captured at dispatch time).
  - Worker uses that `execution_id` when emitting `ActivityCompleted/Failed` (no lookups).
- Timer path:
  - `TimerSchedule` carries `execution_id` (captured at dispatch time).
  - Timer dispatcher uses that `execution_id` for `TimerFired` (no lookups).
- Rehydration best-effort uses placeholder `execution_id: 1`; orchestrator still validates on completion.
- Rationale: prevents cross-execution contamination, especially around ContinueAsNew.

## 10) Activity worker collapse and registries
- No separate ActivityWorker; WorkDispatcher directly executes activities.
- `ActivityRegistry` is a peer of `OrchestrationRegistry` (not owned by it).
- System activities are pre-registered: `SYSTEM_TRACE_ACTIVITY`, `SYSTEM_NOW_ACTIVITY`, `SYSTEM_NEW_GUID_ACTIVITY`.
- Rationale: simpler execution model; guaranteed availability of system primitives.

## 11) No in-memory scheduling shortcuts
- All work (activities, timers, sub-orchestrations) is scheduled via provider queues.
- In-memory pins (like version) must not cause direct side-effects; side-effects only occur after durable actions.
- Rationale: remove pre-persistence windows and race conditions; align with durable execution guarantees.

## 12) Strict dispatcher routing
- Each dispatcher must only see its expected `WorkItem`s. Any unexpected item indicates state/provider corruption.
- Unexpected items cause `error!` and `panic!` (fail fast) in all dispatchers.
- Rationale: prevent silent divergence; surface provider misrouting immediately.

## 13) Nondeterminism detection
- Detect and fail on:
  - Completion kind mismatches (e.g., `ActivityCompleted` for a timer id).
  - Unexpected completion ids (never scheduled).
  - Await mismatches and frontier nondeterminism during replay.
- Rationale: protect existing instances from code drift and logic changes.

## 14) Cancel and parental linkage
- `CancelRequested` is appended idempotently and terminates deterministically.
- Parent linkage flows through `OrchestrationStarted` and `SubOrch*` events only (no `ParentLinked`).
- Rationale: clear lineage via history.

## 15) Formatting and hygiene
- Repository-wide `rustfmt` with `max_width = 120`.
- Ignore/untrack `dtf-data/` in VCS.
- Rationale: consistent formatting; avoid committing test/data artifacts.

## 16) Tests and scenarios codifying invariants
- Nondeterminism tests consolidated under `tests/e2e_nondeterminism.rs`.
- Execution ID validation tests ensure old/future completions are ignored.
- Reliability tests around peek-lock, crash windows, and redelivery behavior.
- Rationale: automated checks for the invariants above.

---
Use this document as a checklist when modifying runtime behavior. Changes that violate these invariants should be discussed explicitly and the rationale updated here.
