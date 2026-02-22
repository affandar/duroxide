# Proposal: External Event Semantics Fix + Persistent Events

**Issue:** [#57 — Orphaned ExternalSubscribed from race/select consumes future events with same name](https://github.com/microsoft/duroxide/issues/57)

## Problem Statement

When a `schedule_wait("name")` is used inside a `select2` and the other branch wins (e.g. a timer), the losing `schedule_wait` creates an `ExternalSubscribed` history event that is never completed. This orphaned subscription still consumes subsequent `ExternalEvent` entries with the same name via positional index matching, preventing later `schedule_wait("name")` calls from receiving the event.

Additionally, there is no API for "fire-and-forget" signals where timing shouldn't matter — events that should stick around until a subscription appears, survive cancellation, and carry through `continue_as_new` boundaries.

## Design Principles

### Positional events are causal

A `raise_event("X")` is a response to the state of the orchestration *at the moment it's raised*. The Nth `raise_event("X")` is paired with the Nth `schedule_wait("X")`. If the subscription that existed at that moment was cancelled (select loser), the event should be **dropped** — it was answering a question nobody's asking anymore. A future `schedule_wait("X")` is a *new* question and needs a *new* answer.

### Persistent events are non-causal

For "notify me whenever" patterns where timing shouldn't matter, a separate API pair provides mailbox semantics — no positional pairing, no causality assumption, events stick around.

---

## Part 1: Bug Fix — Positional Events (`schedule_wait` / `raise_event`)

### Current Behavior (Broken)

1. `schedule_wait("X")` emits `ExternalSubscribed { name: "X" }` → assigned subscription index 0
2. Timer wins `select2` → `DurableFuture` dropped, `mark_token_cancelled` fires, but subscription index 0 is permanently occupied
3. Later `schedule_wait("X")` → assigned subscription index 1
4. `raise_event("X")` arrives → materialized as `ExternalEvent` at arrival index 0
5. Subscription 1 looks for `arrivals[1]` — nothing there. Orchestration hangs forever.

### Fixed Behavior

Two coordinated changes:

**1. Gate materialization in `prep_completions()` (causality)**

- When checking whether to materialize an `ExternalRaised("X")`, count the number of **active** (non-cancelled) `ExternalSubscribed` entries and existing `ExternalEvent` entries for "X" in baseline history + delta
- Only materialize if `arrival_count < active_subscription_count` — i.e., there's an active subscription with no matching arrival yet
- If the only unmatched subscription is cancelled, the `ExternalRaised` is **dropped** (acked and gone)
- This preserves causality: events destined for a cancelled subscription slot are never persisted

**2. Compress indices in `get_external_event()` (delivery)**

- Active subscriptions use an *effective* index that skips cancelled subscriptions:
  ```
  effective_index = raw_index - count_of_cancelled_subscriptions_with_lower_index
  ```
- This aligns active subscriptions with the arrival array, which only contains events that passed the materialization gate
- Example: subscriptions `[cancelled(0), active(1)]` → subscription 1's effective index = `1 - 1 = 0` → matches `arrivals[0]`

**Supporting infrastructure:**

- New history breadcrumb: `ExternalSubscribedCancelled { name }` — emitted when a `schedule_wait` future is dropped (select loser), in `collect_cancelled_from_context()`, same pattern as `ActivityCancelRequested`
- Enables both `prep_completions()` and `get_external_event()` to know which subscriptions are dead
- Ensures deterministic replay

**Example walkthrough:**

| Step | Subscriptions | Cancelled | Arrivals | Result |
|------|--------------|-----------|----------|--------|
| `schedule_wait("X")` | [sub0] | — | — | |
| Timer wins select | [sub0] | {sub0} | — | breadcrumb emitted |
| Stale `raise_event("X")` | [sub0] | {sub0} | — | **dropped** (0 active, 0 arrivals) |
| `schedule_wait("X")` | [sub0, sub1] | {sub0} | — | 1 active |
| Fresh `raise_event("X")` | [sub0, sub1] | {sub0} | [data] | materialized (1 active > 0 arrivals) |
| sub1 polls | | | | effective = 1−1 = 0 → `arrivals[0]` ✓ |

Net effect: events that were destined for a cancelled subscription slot are dropped. Active subscriptions receive only events that arrived after them. Causality is preserved.

### New History Event

```
ExternalSubscribedCancelled { name: String }
```

Emitted as a breadcrumb during `collect_cancelled_from_context()` when a `ScheduleKind::ExternalWait` token is in the cancelled set. Ensures deterministic replay — on replay the engine knows which subscriptions were dropped and can skip them during matching.

### Backward Compatibility

- Old histories without `ExternalSubscribedCancelled` breadcrumbs work unchanged — the old matching behavior is preserved for existing instances (orphaned subscriptions were never cancelled before)
- No provider API changes, no schema migration

---

## Part 2: New API — Persistent Events (`schedule_wait_persistent` / `raise_event_persistent`)

### Semantics

A durable mailbox. No positional indexing.

| | Positional (existing) | Persistent (new) |
|---|---|---|
| Matching | Nth raise → Nth wait | Any unresolved wait ← any unmatched raise |
| Cancelled wait | Slot consumed, event dropped | Slot freed, event available for next wait |
| Event before subscription | Dropped | Buffered until a subscription appears |
| Survives CAN | No (history resets) | Yes (carried forward) |
| Multiple same-name | Ordered positional pairs | Queue (FIFO) |

### Orchestration API

```rust
// Subscribe to next available persistent event with this name
ctx.schedule_wait_persistent("name") -> DurableFuture<String>

// Typed variant
ctx.schedule_wait_persistent_typed::<T>("name") -> impl Future<Output = T>
```

### Client API

```rust
// Send a persistent event — sticks around until consumed
client.raise_event_persistent(instance, name, data) -> Result<(), ClientError>
```

### New Types

**Actions & Events:**
- `Action::WaitExternalPersistent { scheduling_event_id, name }` → `EventKind::ExternalSubscribedPersistent { name }`
- `EventKind::ExternalEventPersistent { name, data }`

**Work Items:**
- `WorkItem::ExternalRaisedPersistent { instance, name, data }`

### Matching Logic

- Persistent subscriptions and arrivals use a **separate matching lane** (not the positional index)
- FIFO: first unresolved persistent subscription gets first unmatched persistent arrival
- Cancelled persistent subscriptions are skipped (don't consume arrivals)

### Always Materialize into History

- In `prep_completions()`: `ExternalRaisedPersistent` is **always** materialized into history as `ExternalEventPersistent { name, data }`, unconditionally — no subscription check needed
- The event is persisted in the current execution's history and available for matching whenever a `schedule_wait_persistent` appears (same turn or future turn)
- The matching logic in `get_external_event_persistent()` scans the current execution's history for unmatched `ExternalEventPersistent` entries and pairs them FIFO with active persistent subscriptions
- **Limit: 20 persistent events per execution.** If more than 20 `ExternalEventPersistent` entries exist in the current execution's history, the 21st+ `ExternalRaisedPersistent` is dropped in `prep_completions()` with a `WARN` log: `"Dropping persistent event '{name}' — exceeded limit of 20 per execution"`

### CAN Carry-Forward

- Previous execution history is **never read** (can be pruned immediately) — so persistent events cannot survive CAN via history alone
- During CAN processing, the replay engine scans the current execution's history for **unmatched** `ExternalEventPersistent` entries (persistent events that were materialized but never consumed by a subscription)
- These are packed into the `OrchestrationContinuedAsNew` event (or a sidecar field on the `StartOrchestration` work item for the new execution) as `pending_persistent_events: Vec<(String, String)>` (name, data pairs)
- On the new execution's first turn, these are injected as synthetic `ExternalEventPersistent` events in the history delta, available for immediate matching
- The 20-event limit applies across the carry-forward: if more than 20 unmatched persistent events exist at CAN time, the oldest beyond 20 are dropped with a `WARN` log

---

## Test Plan

### Part 1 — Positional fix

| # | Test | Description |
|---|---|---|
| 1 | `external_event_wait_in_select_should_not_steal_future_event` | (Existing, currently failing) Timer wins select, second `schedule_wait` same name, raise event → orchestration completes. |
| 2 | `cancelled_subscription_drops_intervening_event` | Timer wins select over `schedule_wait("X")`. Event raised for "X" arrives (matched to cancelled subscription → **dropped**). Then `schedule_wait("X")` again + fresh `raise_event("X")` → second wait resolves with the *second* event's data only. First event is gone. Causality preserved. |
| 3 | `positional_matching_preserved_without_cancellation` | Two sequential `schedule_wait("X")` calls (no select). Two `raise_event("X")` calls. First raise → first wait, second raise → second wait. Regression guard for existing behavior. |
| 4 | `cancelled_subscription_breadcrumb_replays_correctly` | Same as test 1 but verify history contains `ExternalSubscribedCancelled` and replay produces the same result. |

### Part 2 — Persistent events

| # | Test | Description |
|---|---|---|
| 5 | `persistent_event_basic_delivery` | `schedule_wait_persistent("X")`, then `raise_event_persistent("X", data)` → resolves with data. |
| 6 | `persistent_event_arrives_before_subscription` | `raise_event_persistent("X", data)` first, then `schedule_wait_persistent("X")` → resolves immediately. |
| 7 | `persistent_event_survives_select_cancellation` | `select2(schedule_wait_persistent("X"), timer)` → timer wins. `raise_event_persistent("X", data)` arrives between cancellation and next wait. Then `schedule_wait_persistent("X")` again → resolves with data. Proves persistent events are non-causal: the event sticks around despite the cancelled subscription. |
| 7b | `positional_event_dropped_after_select_cancellation` | Same pattern but positional: `select2(schedule_wait("X"), timer)` → timer wins. `raise_event("X", data)` arrives between cancellation and next wait → **dropped** (causality). Then `schedule_wait("X")` again + second `raise_event("X", data2)` → resolves with data2 only. Proves positional events enforce causality. |
| 8 | `persistent_event_fifo_ordering` | Two `raise_event_persistent("X")` with different data, two `schedule_wait_persistent("X")` → first wait gets first data, second gets second. |
| 9 | `persistent_event_survives_continue_as_new` | `raise_event_persistent("X")` during execution 1, CAN, `schedule_wait_persistent("X")` in execution 2 → resolves with the carried-forward event. |
| 10 | `persistent_and_positional_are_independent` | `schedule_wait("X")` + `schedule_wait_persistent("X")` in same orchestration. `raise_event("X")` resolves the positional one. `raise_event_persistent("X")` resolves the persistent one. No cross-lane contamination. |

---

## Implementation Order

1. **Part 1 first** — bug fix for positional events. Smaller scope, fixes the reported issue.
   - Add `ExternalSubscribedCancelled` event kind
   - Emit breadcrumb in `collect_cancelled_from_context()` for `ScheduleKind::ExternalWait`
   - Update `prep_completions()` to drop events destined for cancelled subscriptions
   - Make test 1 pass, add tests 2-4

2. **Part 2 second** — persistent events. Larger scope, new feature.
   - Add new types (`Action`, `EventKind`, `WorkItem` variants)
   - Add `schedule_wait_persistent()` to `OrchestrationContext`
   - Add `raise_event_persistent()` to `Client`
   - Implement separate matching lane in `CtxInner`
   - Implement CAN carry-forward in provider/dispatcher
   - Add tests 5-10
