# Proposal: Queue Semantics for Persistent Events + Typed Event APIs

**Status:** Draft  
**Related:** [external-event-semantics.md](external-event-semantics.md) (Part 2 — original persistent events design)

## Observation

`raise_event_persistent()` and `schedule_wait_persistent()` are a queue. They have FIFO ordering,
named channels, consumed-once semantics, survive cancellation, and carry forward across
`continue_as_new`. The "persistent event" naming obscures this and makes the API harder to
discover and reason about.

This proposal makes two changes:
1. **Rename** the persistent event API to use queue terminology
2. **Add typed variants** for event raise/enqueue and positional wait APIs that are currently missing

No behavioral changes, no provider changes, no schema changes.

## API Changes

### Queue Rename (persistent events → queue)

| Current | Proposed | Notes |
|---|---|---|
| `client.raise_event_persistent(instance, name, data)` | `client.enqueue_event(instance, queue, data)` | Old method kept as `#[deprecated]` alias |
| `ctx.schedule_wait_persistent(name)` | `ctx.dequeue_event(queue)` | Old method kept as `#[deprecated]` alias |
| `ctx.schedule_wait_persistent_typed::<T>(name)` | `ctx.dequeue_event_typed::<T>(queue)` | Old method kept as `#[deprecated]` alias |

### New Typed Variants

| Method | Signature | Notes |
|---|---|---|
| `client.raise_event_typed::<T>(instance, name, data: &T)` | Serializes `T` to JSON before raising | Positional event, encode-only |
| `client.enqueue_event_typed::<T>(instance, queue, data: &T)` | Serializes `T` to JSON before enqueuing | Queue event, encode-only |

### Internal Renames (Rust types only — serde wire format unchanged)

| Current variant | Proposed variant | Serde stays |
|---|---|---|
| `Action::WaitExternalPersistent` | `Action::DequeueEvent` | n/a (not serialized) |
| `WorkItem::ExternalRaisedPersistent` | `WorkItem::QueueMessage` | `#[serde(alias = "ExternalRaisedPersistent")]` |
| `EventKind::ExternalSubscribedPersistent` | `EventKind::QueueSubscribed` | `#[serde(rename = "ExternalSubscribedPersistent")]` |
| `EventKind::ExternalEventPersistent` | `EventKind::QueueEventDelivered` | `#[serde(rename = "ExternalEventPersistent")]` |
| `EventKind::ExternalSubscribedPersistentCancelled` | `EventKind::QueueSubscriptionCancelled` | `#[serde(rename = "ExternalSubscribedPersistentCancelled")]` |
| `ScheduleKind::PersistentWait` | `ScheduleKind::QueueDequeue` | n/a (not serialized) |

### Internal Field Renames

| Current | Proposed |
|---|---|
| `persistent_subscriptions` | `queue_subscriptions` |
| `persistent_arrivals` | `queue_arrivals` |
| `persistent_cancelled_subscriptions` | `queue_cancelled_subscriptions` |
| `persistent_resolved_subscriptions` | `queue_resolved_subscriptions` |
| `bind_persistent_subscription()` | `bind_queue_subscription()` |
| `deliver_persistent_event()` | `deliver_queue_message()` |
| `get_persistent_event()` | `get_queue_message()` |
| `mark_persistent_subscription_cancelled()` | `mark_queue_subscription_cancelled()` |
| `get_cancelled_persistent_wait_ids()` | `get_cancelled_queue_ids()` |

## What Doesn't Change

- **Provider layer** — zero changes. Same `enqueue_for_orchestrator(WorkItem::QueueMessage { ... })`
- **FIFO matching logic** — same algorithm, renamed fields
- **Carry-forward pipeline** — same mechanism, renamed types
- **History storage** — same events, same serde wire format (via `rename`/`alias`)
- **Replay engine** — same logic, renamed Rust identifiers
- **`schedule_wait` / `raise_event` (positional)** — unchanged, still there for causal event patterns

## Backward Compatibility

- **Serde:** `#[serde(rename = "...")]` keeps the serialized format identical. Old and new runtimes
  produce byte-identical history events. Zero migration.
- **Rolling upgrade:** Old nodes read events written by new nodes and vice versa.
- **Deprecated aliases:** `raise_event_persistent()` and `schedule_wait_persistent()` remain as
  `#[deprecated]` thin wrappers that call the new methods. Existing user code compiles with warnings.

## Example: Chat with Your Orchestration

```rust
// === Types ===
#[derive(Serialize, Deserialize)]
struct ChatMessage { seq: u32, text: String }

#[derive(Serialize, Deserialize)]
struct ChatStatus { state: String, last_response: Option<String>, msg_seq: u32 }

// === Orchestration ===
let chat = |ctx: OrchestrationContext, _input: String| async move {
    let mut seq = 0u32;
    loop {
        ctx.set_custom_status(serde_json::to_string(&ChatStatus {
            state: "waiting".into(), last_response: None, msg_seq: seq,
        }).unwrap());

        let msg: ChatMessage = ctx.dequeue_event_typed::<ChatMessage>("inbox").await;
        seq = msg.seq;

        let response = ctx.schedule_activity("GenerateResponse", &msg.text).await?;

        ctx.set_custom_status(serde_json::to_string(&ChatStatus {
            state: "replied".into(), last_response: Some(response.clone()), msg_seq: seq,
        }).unwrap());

        if msg.text.to_lowercase().contains("bye") {
            return Ok(format!("Chat ended after {seq} messages"));
        }
    }
};

// === Client: send messages, poll for responses ===
client.enqueue_event_typed("chat-42", "inbox", &ChatMessage { seq: 1, text: "Hello!".into() }).await?;
let (status, ver) = client.wait_for_custom_status("chat-42", 0, Duration::from_secs(30)).await?;
// status = {"state":"replied","last_response":"Hi there!","msg_seq":1}
```

## Test Plan

### Renamed API Tests (verify aliases + new names work identically)

| # | Test | Description |
|---|---|---|
| 1 | `queue_basic_delivery` | `enqueue_event` + `dequeue_event` → message delivered |
| 2 | `queue_typed_delivery` | `enqueue_event_typed::<T>` + `dequeue_event_typed::<T>` → round-trip typed |
| 3 | `queue_fifo_ordering` | 3 enqueues, 3 dequeues → correct FIFO order |
| 4 | `queue_survives_select_cancellation` | select2(dequeue, timer) → timer wins → enqueue → dequeue → resolves |
| 5 | `queue_survives_continue_as_new` | Enqueue in exec 1, CAN, dequeue in exec 2 → resolves |
| 6 | `deprecated_aliases_still_work` | `raise_event_persistent` + `schedule_wait_persistent` still compile and work |

### New Typed Variant Tests

| # | Test | Description |
|---|---|---|
| 7 | `raise_event_typed_round_trip` | `client.raise_event_typed::<T>` + `ctx.schedule_wait_typed::<T>` → typed round-trip |
| 8 | `enqueue_event_typed_round_trip` | `client.enqueue_event_typed::<T>` + `ctx.dequeue_event_typed::<T>` → typed round-trip |

### Scenario Test

| # | Test | Description |
|---|---|---|
| 9 | `copilot_chat_scenario` | Full chat-with-orchestration pattern: UI sends typed messages via `enqueue_event_typed`, orchestration dequeues, calls activity (simulated LLM), sets `custom_status` with response, UI polls `wait_for_custom_status`. Multiple rounds + graceful exit. |

## Implementation Order

1. **Internal renames** — `EventKind`, `Action`, `WorkItem`, `ScheduleKind`, `CtxInner` fields/methods
2. **Public API renames** — `dequeue_event`, `dequeue_event_typed`, `enqueue_event` on Client
3. **Deprecated aliases** — keep old names as `#[deprecated]` wrappers
4. **New typed variants** — `client.raise_event_typed`, `client.enqueue_event_typed`
5. **Update existing tests** — rename calls in persistent_event_tests.rs to use new API
6. **Add new tests** — typed round-trip tests
7. **Add scenario test** — copilot chat pattern under `tests/scenarios/`
8. **Update docs** — external-events.md, ORCHESTRATION-GUIDE.md, examples
