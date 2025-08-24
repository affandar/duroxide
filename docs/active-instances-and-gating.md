# Active instances and gating design

This document explains how the runtime uses an in‑memory `active_instances` set and simple gate policies to guarantee single‑run per instance, correct ordering across ContinueAsNew, and safe delivery of worker completions and external events.

## Goals

- Exactly one active orchestration loop per instance at any time.
- Hold new starts until the previous execution fully releases (including ContinueAsNew rollover).
- Deliver completions/external events only while the orchestrator loop is ready to consume them.
- Keep behavior provider‑agnostic (applies to in‑memory and filesystem providers).

## Lifecycle: when an instance is active vs. inactive

- Becomes active
  - In `Runtime::run_instance_to_completion`, the instance id is inserted into `active_instances` at entry.
  - The runtime registers a router inbox for the instance and rehydrates pending timers/activities from history.
- Becomes inactive
  - A scoped `ActiveGuard` removes the id from `active_instances` in its `Drop` when the run task returns (completion, failure, ContinueAsNew short‑circuit, or panic).

## Gate points and policies

Two background workers enforce gates using `active_instances`:

1) Start request worker (local queue)
- If an instance is already active, duplicate starts are ignored by an in-memory `active_instances` guard.
- If inactive, it marks the instance as pending and spawns `run_instance_to_completion`.

2) Provider poller (work queues)
- The poller dequeues `WorkItem`s and chooses a `GatePolicy`:
  - `StartOrchestration` → RequireInactive (hold while active)
  - `ActivityCompleted`/`ActivityFailed`/`TimerFired`/`ExternalRaised`/`SubOrch*` → RequireActive (hold until active)
- If the gate condition is not met, the item is re‑enqueued to the provider queue and retried later (with a small sleep for fairness). When the gate is satisfied, the poller forwards to the in‑process router.

## ContinueAsNew interaction

- When an orchestrator calls `ctx.continue_as_new(new_input)`, the runtime:
  - Appends `OrchestrationContinuedAsNew` to the current execution (terminal for this execution).
  - Asks the provider to create a new execution and appends a fresh `OrchestrationStarted { name, version, input: new_input, parent_instance?, parent_id? }`.
  - Enqueues a `StartOrchestration` for the same instance (both locally and in the provider queue).
- The existing run still holds the active flag until it returns; both the local start worker and the poller will hold the next start (RequireInactive) until the flag is released.
- External/worker completions arriving between executions are held by the poller (RequireActive) until the new run is active.

## Router inbox and delivery

- While active, the router has a registered inbox for the instance; forwarded messages become `OrchestratorMsg` and are appended to history by the loop.
- If a message is forwarded but no inbox exists (e.g., the run just finished), the router logs a warning and drops it. The poller gate prevents most of these cases.

## External event semantics with gating

- `Runtime::raise_event` enqueues `ExternalRaised` to the provider queue.
- The poller applies RequireActive, so it forwards the event only when the instance is active.
- Inside the orchestrator loop, `append_completion` resolves `ExternalByName` to the latest `ExternalSubscribed { name }` in the current execution:
  - If found, it appends `ExternalEvent { id, name, data }`.
  - If not found, it logs a warning and drops the event (no buffering before subscription).
- After ContinueAsNew, the new execution must subscribe again; early events to the new execution are dropped at the same point.

## Rehydration on activation

Immediately after activation, the runtime scans history to:
- Re‑enqueue scheduled activities that haven’t completed.
- Re‑arm timers that were created but didn’t fire yet (sleeping the remaining delay).

## Correctness properties

- Single active orchestrator per instance: enforced by `active_instances` and start gating.
- Progress is monotonic per turn: the loop persists event deltas and advances `turn_index` only when new events are appended.
- Safe delivery: worker completions and external events are only routed while the instance is active.
- Cross‑execution isolation: events are effectively delivered to the latest execution; unmatched externals are not carried across executions.

## Failure and edge cases

- Run task panic/abort: the `ActiveGuard` still removes the active flag; subsequent starts proceed normally.
- Router inbox missing: warnings and drops are possible if a message arrives just after deactivation; the poller’s gates minimize this.
- Queue churn/backoff: re‑enqueue loops use a small fixed sleep; provider implementations may introduce leases/visibility timeouts in the future for stronger guarantees.

## Tests exercising this design

- `tests/e2e_continue_as_new.rs`
  - `continue_as_new_multiexec_fs`: rollover across multiple executions.
  - `continue_as_new_event_routes_to_latest_fs`: externals hit the latest execution only.
  - `continue_as_new_event_drop_then_process_fs`: early event to new execution is dropped; post‑subscribe event delivered.
  - `event_drop_then_retry_after_subscribe_fs`: early event while active but pre‑subscribe is dropped; retry after subscribe succeeds.
- `tests/e2e_errors.rs`
  - `event_after_completion_is_ignored_fs`: events after completion are ignored.
- `tests/e2e_races.rs`: ordering around external/timer races.

## Code pointers

- Activation/deactivation and run loop: `src/runtime/mod.rs::Runtime::run_instance_to_completion`
- Start worker and gating: `src/runtime/mod.rs` (background start queue worker)
- Provider poller and `GatePolicy`: `src/runtime/mod.rs` (work queue poller loop)
- Router and inbox registration: `InstanceRouter` in `src/runtime/mod.rs`
- External delivery/drop: `append_completion` (ExternalByName → ExternalEvent or warn+drop)
- Rehydration: `rehydrate_pending` in `src/runtime/mod.rs`

## Future improvements

- Provider queue leases/visibility timeouts for stronger delivery guarantees and to avoid busy re‑enqueue.
- Optional provider‑side buffering of unmatched external events (mailbox), delivered upon first subscription.
- Backoff strategies and jitter for re‑enqueue loops.
