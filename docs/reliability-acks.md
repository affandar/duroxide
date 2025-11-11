# Ack Semantics and Reliability

## Purpose
This document explains how the runtime maintains reliability guarantees using peek-lock acks and abandons across the orchestrator and worker queues. Timers now ride the orchestrator queue via delayed visibility, so there is no dedicated timer queue/dispatcher. The goal is to specify when an item is acknowledged, when it is abandoned for redelivery, and how these choices ensure at-least-once processing with durable history as the source of truth.

## Terminology
- **History**: Append-only event log per instance. All correctness is evaluated against this log.
- **Peek-lock dequeue**: A fetch returns `(WorkItem, token)`. The item is invisible until acknowledged (ack) or abandoned.
- **Ack**: Permanently removes a peek-locked item.
- **Abandon**: Makes the item visible again (immediately or after a visibility timeout) so it can be reprocessed.
- **Queues**: `orchestrator_queue` (drives orchestration turns, including `TimerFired`) and `worker_queue` (executes activities). Providers may still expose a `timer_queue` depth for backwards compatibility, but it remains zero.

## Core Rule
- Any operation that changes history is only acked after the corresponding events are durably appended to history.
- Items that do not change history (duplicates, invalid for the current execution, or intentionally dropped) may be acked without appending.

## StartOrchestration
Flow:
1. Orchestrator queue delivers `WorkItem::StartOrchestration { instance, orchestration, input }`.
2. Runtime calls `start_internal_rx`, which appends `Event::OrchestrationStarted { name, version, ... }` if the instance has no history.
3. After append succeeds (or if history already exists), the dispatcher acks the start item.

Outcome:
- Start is acked only after `OrchestrationStarted` exists on disk (or is a no-op acknowledged because the instance already has history).

## Completions (Activity, Timer, Sub-Orchestration)
Dispatch path:
1. Orchestrator queue delivers a completion with a peek-lock token (e.g., `ActivityCompleted`, `ActivityFailed`, `TimerFired`, `SubOrchCompleted`, `SubOrchFailed`).
2. The dispatcher validates `execution_id` and checks for an active inbox:
   - If the `execution_id` does not match the current execution, the item is ignored and acked immediately (no history change).
   - If the instance is dehydrated (no inbox), the dispatcher calls `ensure_instance_active` and abandons the token so the item is redelivered after rehydration.
3. If an inbox exists, the item (with `ack_token`) is forwarded to the instance router.
4. Inside the turn loop, `append_completion(...)` updates the in-memory history and returns whether it changed history.
5. Persistence and ack:
   - If history changed, the turn loop appends new events to the store. Only after append succeeds are the associated tokens acked.
   - If no change (duplicate/out-of-order), the token is acked immediately without append.

Outcome:
- At-least-once processing with exactly-once effects: duplicates are detected by history and acked without re-appending.

## Worker Queue (ActivityExecute)
Flow:
1. Worker queue delivers `WorkItem::ActivityExecute`.
2. Dispatcher executes the activity via the `ActivityRegistry`.
3. The dispatcher enqueues the corresponding completion (`ActivityCompleted` or `ActivityFailed`) to the Orchestrator queue.
4. The Worker token is acked after enqueueing the completion.

Rationale:
- Reliability transitions to the Orchestrator queue, where completion handling follows the history-append-then-ack rule.

## Timer Handling
Flow:
1. When a timer is scheduled during orchestration execution, `WorkItem::TimerFired` is created and enqueued to the Orchestrator queue with `visible_at = fire_at_ms`.
2. The Orchestrator queue respects the `visible_at` timestamp and only delivers the timer when it's ready to fire.
3. The timer is handled via the standard Orchestrator completion path.

Rationale:
- Timers are now handled directly via the Orchestrator queue with delayed visibility, eliminating the need for a separate timer queue and dispatcher.

## External Events (By Name)
Flow:
1. Orchestrator queue delivers `WorkItem::ExternalRaised { instance, name, data }`.
2. If the instance is dehydrated, the dispatcher rehydrates and abandons the token for redelivery.
3. If active, the item is forwarded to the instance router with `ack_token`.
4. In the turn loop, if a matching subscription exists, a history event is appended; otherwise, the message is dropped from history.
5. Ack rule:
   - If an event is appended, ack after persistence.
   - If dropped due to no subscription or no-op, ack immediately.

## Rehydration and Redelivery
- If a completion arrives for a dehydrated instance, the dispatcher calls `ensure_instance_active` and abandons the token, allowing redelivery once the inbox is registered.
- This ensures the item is processed by an active instance and only acked after the appropriate handling.

## Failure Scenarios and Outcomes
- Append fails after dequeue: Do not ack; the item is redelivered. Duplicate detection in history prevents re-appending on retry.
- Append succeeds, crash before ack: On restart, the item redelivers; duplicate detection causes no history change and it is acked immediately.
- Invalid `execution_id`: Ack immediately without append; the message is from an old or future execution and is intentionally ignored.

## Summary of Ack Points
- **Start**: Ack after `OrchestrationStarted` append (or immediately if history already exists).
- **Completions**: Ack after append if history changes; ack immediately if duplicate/no-op; abandon if instance inactive.
- **Worker**: Ack after enqueueing completion to Orchestrator queue.
- **Timer**: Ack after scheduling the future `TimerFired` enqueue.
- **External**: Ack after append if subscribed; ack immediately if no subscription.

## Guarantees
- **At-least-once delivery** for all work items.
- **Exactly-once effects** for history-changing operations via durable append and deduplication by history.
- **Liveness under failures** through peek-lock redelivery and rehydration.
