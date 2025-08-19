# External Event Semantics

This runtime delivers external events to orchestrations by name, correlated to the most recent matching subscription recorded in history.

Key points
- Subscriptions are explicit: a subscription is recorded when the orchestrator awaits `schedule_wait("Name")` (history: `ExternalSubscribed { id, name }`).
- Delivery requires a subscription: when an external event arrives (`ExternalRaised`), the runtime looks up the latest `ExternalSubscribed` for that name in the current execution and only then appends `ExternalEvent { id, name, data }`.
- Early events are dropped: if no subscription exists yet at the time of delivery, the event is dropped (with a warning).
- Execution-scoped: events are delivered to the currently active execution. After ContinueAsNew, the new execution must subscribe again.
- At-least-once: raising the same external event again after subscription is idempotent at the app layer if you handle duplicates; the runtime does not dedupe by payload.

Flow
1. Sender calls `Runtime::raise_event(instance, name, data)`.
2. Provider enqueues `ExternalRaised` in the work queue.
3. Poller forwards to the active instance as `ExternalByName`.
4. Instance loop (append_completion) resolves name → subscription id and appends `ExternalEvent` if found; otherwise logs a warning and drops the event.

Recommendations
- Subscribe early in your orchestrator before signaling external parties.
- If you need buffering before subscription, build a small durable mailbox (e.g., a table/queue) and have the orchestrator drain it after subscribing.
- Include correlation data in `data` and implement idempotency in your orchestrator for resilience.
