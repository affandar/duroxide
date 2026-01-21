# Durable Futures Internals: Token-Based Scheduling, Replay, and Completion Delivery

This document explains how Duroxide creates the illusion of *durable code*—code that survives process crashes, restarts, and machine failures—by replaying deterministic orchestration logic against a persisted event history.

- `ctx.schedule_*()` methods return ordinary Rust `impl Future` values that can be `.await`ed directly.
- Internally, each scheduled operation is represented by a **token** that is later **bound** to a persisted **schedule event ID**.
- The replay engine walks history in order, binds tokens, delivers completions, and polls the orchestration future when it might make progress.

This file is intentionally “guts-level”. If you want the user-facing programming model, start with `docs/ORCHESTRATION-GUIDE.md`.

## Table of Contents

1. [Core Concept: History-Based Execution](#core-concept-history-based-execution)
2. [High-Level Architecture](#high-level-architecture)
3. [What "Durable Futures" Mean](#what-durable-futures-mean)
4. [Events, Actions, and IDs](#events-actions-and-ids)
5. [Token Emission, Binding, and Result Storage](#token-emission-binding-and-result-storage)
6. [Replay Engine Walkthrough (Turn Execution)](#replay-engine-walkthrough-turn-execution)
7. [Operation Walkthroughs](#operation-walkthroughs)
8. [Aggregate Futures: select/join](#aggregate-futures-selectjoin)
9. [Rust Async Differences (Why This Works)](#rust-async-differences-why-this-works)
10. [Nondeterminism Detection (Common Failure Modes)](#nondeterminism-detection-common-failure-modes)

---

## Core Concept: History-Based Execution

### What are we actually doing?

The goal of Duroxide is to make a piece of code **durable**: it continues running through process resets, machine restarts, and crashes. That code is an *orchestration function*.

We are not snapshotting memory. Instead, we provide the *illusion* of durability using:

1. **Futures**: familiar Rust async, but with durable semantics
2. **Replay**: re-running the orchestration from the beginning each turn
3. **History**: a persisted log of events, allowing replay to “fast-forward”

This model is used widely (AWS SWF, Azure Durable Functions, Temporal/Cadence, Durable Task Framework, etc.).

### The key insight

Orchestrations must be **deterministic**. Given the same event history, the orchestration must:

- emit the same scheduling actions in the same order
- observe the same completion data
- make the same decisions

Determinism is what makes replay equivalent to resuming.

### The execution model

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Orchestration Lifecycle                          │
│                                                                      │
│   ┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐  │
│   │  Turn 1  │────▶│  Turn 2  │────▶│  Turn 3  │────▶│  Turn N  │  │
│   │ (fresh)  │     │ (replay) │     │ (replay) │     │(complete)│  │
│   └──────────┘     └──────────┘     └──────────┘     └──────────┘  │
│        │                │                │                │         │
│        ▼                ▼                ▼                ▼         │
│   [history]        [history]        [history]        [history]      │
│   grows            grows            grows            final          │
└─────────────────────────────────────────────────────────────────────┘
```

Each turn:

1. Load persisted history for the instance.
2. Create a fresh orchestration future by calling the orchestration function.
3. Walk history in order; bind schedule actions; deliver completions.
4. Poll the orchestration when completions may have unblocked it.
5. Persist any newly emitted schedule actions as new events; dispatch them.
6. Drop the future. Only history persists.

---

## High-Level Architecture

The user-level API is intentionally plain:

- `schedule_activity(...) -> impl Future<Output = Result<String, String>>`
- `schedule_timer(...) -> impl Future<Output = ()>`
- `schedule_wait(...) -> impl Future<Output = String>`
- `schedule_sub_orchestration(...) -> impl Future<Output = Result<String, String>>`

Those futures are “durable” only because the runtime replays the orchestration and feeds them completion data from history.

Internally, the runtime uses a token-and-binding mechanism:

```
┌─────────────────────────────────────────────────────────────────────┐
│                    OrchestrationContext                              │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │ CtxInner (Arc<Mutex<...>>)                                     │  │
│  │                                                               │  │
│  │ next_token: u64                                               │  │
│  │ emitted_actions: Vec<(token, Action)>                         │  │
│  │ token_bindings: HashMap<token, schedule_id>                   │  │
│  │ completion_results: HashMap<token, CompletionResult>          │  │
│  │                                                               │  │
│  │ external_subscriptions: HashMap<schedule_id, (name, idx)>      │  │
│  │ external_arrivals: HashMap<name, Vec<data>>                   │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                              ▲
                              │ bind_token(...) / deliver_result(...)
                              │ (replay engine)
                              │
┌─────────────────────────────────────────────────────────────────────┐
│                        ReplayEngine                                  │
│  Walks history in order, matches schedule events to emitted actions, │
│  delivers completion data, and polls orchestration when needed.      │
└─────────────────────────────────────────────────────────────────────┘
```

---

## What "Durable Futures" Mean

A **durable future** is a Rust `Future` whose result survives process crashes. When you write:

```rust
let result = ctx.schedule_activity("Greet", "Alice").await?;
```

That `.await` might complete immediately (if we're replaying and the result is already in history) or it might block until a worker executes the activity and the result is persisted. Either way, *the code looks identical*—that's the illusion of durability.

### The Core Problem

Normal Rust futures don't survive crashes. If your process dies mid-await, the future is gone. Duroxide solves this by:

1. **Recording** what the orchestration scheduled (as events in persistent history)
2. **Replaying** the orchestration from the beginning after a crash
3. **Feeding** previously-recorded results back to the futures during replay

The challenge: how do we connect a future (created fresh each replay) to its persisted result?

### The Solution: Tokens

Each `schedule_*` call allocates an in-memory **token**—a simple incrementing integer. This token is the bridge between:

- The **future** (which polls for results keyed by token)
- The **replay engine** (which binds tokens to persisted event IDs and delivers results)

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Token Lifecycle                                  │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  1. SCHEDULE: Orchestration calls schedule_activity("Greet", "Alice")│
│     ┌──────────────────────────────────────────────────────────┐    │
│     │ token = 1                                                 │    │
│     │ emitted_actions.push((1, CallActivity{name, input}))      │    │
│     │ return poll_fn that checks completion_results[token=1]    │    │
│     └──────────────────────────────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│  2. BIND: Replay engine matches action to history event             │
│     ┌──────────────────────────────────────────────────────────┐    │
│     │ History: ActivityScheduled { event_id: 10, ... }          │    │
│     │ token_bindings[1] = 10                                    │    │
│     └──────────────────────────────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│  3. DELIVER: Replay engine sees completion, delivers to token       │
│     ┌──────────────────────────────────────────────────────────┐    │
│     │ History: ActivityCompleted { source_event_id: 10, ... }   │    │
│     │ Find token where token_bindings[token] == 10 → token=1    │    │
│     │ completion_results[1] = ActivityOk("Hello, Alice!")       │    │
│     └──────────────────────────────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│  4. RESOLVE: Future polls and finds its result                      │
│     ┌──────────────────────────────────────────────────────────┐    │
│     │ poll_fn checks completion_results[1] → Some(ActivityOk)   │    │
│     │ Returns Poll::Ready(Ok("Hello, Alice!"))                  │    │
│     └──────────────────────────────────────────────────────────┘    │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

### Example: Fresh Execution vs Replay

Consider this orchestration:

```rust
async fn greet(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    let greeting = ctx.schedule_activity("Greet", &name).await?;
    Ok(greeting)
}
```

**Turn 1 (Fresh Execution)** — No history yet:

```
┌─────────────────────────────────────────────────────────────────────┐
│ History: [OrchestrationStarted]                                      │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│ 1. Orchestration runs, calls schedule_activity("Greet", "Alice")    │
│    → emits (token=1, CallActivity{...})                             │
│    → returns future                                                  │
│                                                                      │
│ 2. Future awaited → polls → no result for token=1 → Pending          │
│                                                                      │
│ 3. Replay engine sees no matching history event                      │
│    → allocates event_id=2                                            │
│    → binds token=1 → event_id=2                                      │
│    → persists ActivityScheduled{event_id=2}                          │
│    → dispatches CallActivity to worker queue                         │
│                                                                      │
│ 4. Turn ends (orchestration suspended)                               │
│                                                                      │
├─────────────────────────────────────────────────────────────────────┤
│ History after: [OrchestrationStarted, ActivityScheduled{id=2}]       │
└─────────────────────────────────────────────────────────────────────┘
```

**Turn 2 (After Activity Completes)** — Worker executed, completion in history:

```
┌─────────────────────────────────────────────────────────────────────┐
│ History: [OrchestrationStarted,                                      │
│           ActivityScheduled{id=2},                                   │
│           ActivityCompleted{id=3, source=2, result="Hello!"}]        │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│ 1. Orchestration replays from beginning                              │
│    → calls schedule_activity("Greet", "Alice")                      │
│    → emits (token=1, CallActivity{...})                             │
│                                                                      │
│ 2. Replay engine walks history:                                      │
│    → sees ActivityScheduled{id=2}                                    │
│    → matches emitted action, binds token=1 → event_id=2              │
│    → sees ActivityCompleted{source=2, result="Hello!"}               │
│    → delivers: completion_results[1] = ActivityOk("Hello!")          │
│                                                                      │
│ 3. Future awaited → polls → finds result → Ready(Ok("Hello!"))       │
│                                                                      │
│ 4. Orchestration returns Ok("Hello!"), completes                     │
│                                                                      │
├─────────────────────────────────────────────────────────────────────┤
│ History after: [..., OrchestrationCompleted{output="Hello!"}]        │
└─────────────────────────────────────────────────────────────────────┘
```

### Why Tokens Instead of Event IDs?

The orchestration can't use event IDs directly because:

1. **Fresh execution**: The event ID doesn't exist yet when `schedule_activity` is called—it's allocated *after* the turn when the engine persists the schedule event.

2. **Separation of concerns**: The orchestration code shouldn't know about history mechanics. It just calls `schedule_activity` and awaits the result.

Tokens are the indirection layer that makes this work. They're allocated synchronously, bound to event IDs by the replay engine, and used to route completions back to the right futures.

---

## Events, Actions, and IDs

### Events

Durability comes from persisting `Event` values. Each event has an `event_id` that is monotonically increasing per instance.

Two broad classes:

1. **Schedule events** (begin an operation):
   - `ActivityScheduled { name, input }`
   - `TimerCreated { fire_at_ms }`
   - `ExternalSubscribed { name }`
   - `SubOrchestrationScheduled { name, instance, input, ... }`
   - `SystemCall { op, value }` (special: schedule+result together)

2. **Completion events** (finish an operation):
   - `ActivityCompleted { source_event_id, result }`
   - `ActivityFailed { source_event_id, details }`
   - `TimerFired { source_event_id, ... }`
   - `SubOrchestrationCompleted { source_event_id, result }`
   - `SubOrchestrationFailed { source_event_id, details }`

External events are special:

- `ExternalEvent { name, data }`

They are correlated by name + subscription index rather than `source_event_id`.

### Actions

`Action` values represent what the orchestration *wants to do*.

- `CallActivity { scheduling_event_id, name, input }`
- `CreateTimer { scheduling_event_id, fire_at_ms }`
- `WaitExternal { scheduling_event_id, name }`
- `StartSubOrchestration { scheduling_event_id, name, instance, input, version }`
- `SystemCall { scheduling_event_id, op, value }`
- `ContinueAsNew { input, version }`

The replay engine converts (most) actions into schedule events for persistence.

### Schedule IDs

The durable schedule ID is simply the schedule event's `event_id`.

Example linkage:

```
[10] ActivityScheduled { event_id: 10, name: "Greet", input: "Alice" }
[11] ActivityCompleted { event_id: 11, source_event_id: 10, result: "Hello" }
```

Here the schedule ID is `10`.

---

## Token Emission, Binding, and Result Storage

This section is the key replacement for the old claim system.

### 1) Emission

Each `schedule_*()` call emits an action and returns a future.

Example:

```
ctx.schedule_activity("Greet", "Alice")
  emits: (token=1, Action::CallActivity { scheduling_event_id: 0, ... })
  returns: Future that polls for completion_results[token=1]
```

Actions are emitted in a Vec in order; schedule order matters.

### 2) Binding

When the replay engine sees a schedule event in history, it pops the next emitted action and validates it matches.

If it matches, it binds token → schedule ID:

```
token_bindings[token] = schedule_event.event_id
```

Binding also happens for *new* schedules beyond history: when the engine allocates a fresh `event_id` for a newly emitted action, it binds the token to that new `event_id`.

### 3) Result storage (keyed by token)

Completion events refer to `source_event_id` (the schedule ID). The context stores results keyed by **token**, not schedule ID.

So delivering a completion performs:

1. Find the token that is bound to that schedule ID.
2. Store `completion_results[token] = CompletionResult::...`.

If a completion arrives with no known binding, the current behavior is to log a warning and drop the result.

This explains why schedule binding must occur before completion delivery for a given operation.

### 4) must_poll

The replay engine does not busy-poll. It polls the orchestration only when progress might be possible.

It sets `must_poll = true` when:

- starting execution
- delivering a completion
- delivering an external event
- delivering a system call value

---

## Replay Engine Walkthrough (Turn Execution)

This section describes the core loop in `src/runtime/replay_engine.rs`.

### The overall shape

At a high level, a turn:

1. Builds a `working_history` (baseline persisted history + any new completion events preprocessed for this turn).
2. Creates an `OrchestrationContext`.
3. Creates a pinned orchestration future by invoking the handler.
4. Walks `working_history` in order, repeatedly:
   - polling the orchestration when `must_poll`
   - binding schedule events
   - delivering completion events
5. After history is processed, converts any remaining emitted actions into new schedule events and pending actions.
6. Returns `TurnResult`.

### Safety: catch_unwind

The engine wraps orchestration polling in `catch_unwind`. A panic is treated as a nondeterminism/configuration failure.

This is a pragmatic choice: orchestrations are *replayed*; panics are often caused by “this path should never happen” assumptions that break under replay.

### Replay boundary and ctx.is_replaying()

The engine tracks where “persisted history” ends.

- While iterating events before the replay boundary, `ctx.is_replaying()` is true.
- Once iteration reaches the replay boundary, it flips to false.

This allows orchestrations to suppress side effects during replay.

### Schedule matching and binding

When the engine encounters a schedule event, it:

1. pops the next emitted `(token, action)`
2. checks `action_matches_event_kind(action, event.kind)`
3. calls `ctx.bind_token(token, event.event_id())`

Matching is strict for most operations:

- Activities match by `(name, input)`.
- External subscriptions match by name.
- Sub-orchestrations match by `(name, input)`.
- System calls match by op.

Timers are intentionally matched *by position* (timestamp is allowed to differ), because `fire_at_ms` is computed from “now”.

### Completion delivery

Completion events deliver to a schedule ID (`source_event_id`). The engine validates a few things before delivering:

- The schedule ID must be in the “open schedules” set.
- Some kinds check “expected kind” before accepting completions.

Then it calls `ctx.deliver_result(source_id, CompletionResult::...)` and sets `must_poll = true`.

### External events

External events do not target a specific schedule ID.

The engine appends arrivals by name, then re-polls the orchestration. `schedule_wait()` futures resolve by reading the correct arrival at a deterministic subscription index.

### New schedules beyond history

After history is processed, any remaining emitted actions represent new schedules requested in this turn.

For each `(token, action)`:

1. Allocate a fresh `event_id`.
2. Bind token → event_id.
3. Update the action’s `scheduling_event_id` to the new `event_id`.
4. Convert the action to a schedule event (`history_delta`).
5. Add the updated action to `pending_actions`.

Sub-orchestrations also update/normalize instance IDs:

- If an instance ID wasn’t explicitly provided, a placeholder like `sub::pending_*` is emitted.
- The engine replaces it with a deterministic final ID `sub::{event_id}`.

### Cancellation precedence

The engine checks for `OrchestrationCancelRequested` and returns `TurnResult::Cancelled` before returning a normal completion.

---

## Operation Walkthroughs

This section shows end-to-end flows in the token model.

### Activity

Orchestration code:

```rust
let x = ctx.schedule_activity("Greet", "Alice").await?;
```

Turn 1 (fresh):

1. Orchestration polls and emits `(token=1, CallActivity{name:"Greet", input:"Alice"})`.
2. No completion exists, so the future is pending.
3. After history is processed, the engine allocates `event_id=10`:
   - binds token 1 → 10
   - persists `ActivityScheduled{event_id:10, ...}`
   - enqueues `CallActivity{scheduling_event_id:10, ...}`

Turn 2 (replay, after completion arrives):

1. The engine replays `ActivityScheduled{event_id:10,...}`:
   - pops emitted action, validates it matches, binds token 1 → 10
2. The engine sees `ActivityCompleted{source_event_id:10, result:"Hello"}`:
   - delivers completion → stores `completion_results[token=1] = ActivityOk("Hello")`
   - sets `must_poll=true`
3. Polling sees `token=1` has `ActivityOk` and resolves the `.await`.

### Timer

Orchestration code:

```rust
ctx.schedule_timer(Duration::from_secs(30)).await;
```

Timers compute `fire_at_ms` from `SystemTime::now()` at schedule time.

The replay engine matches timer schedule events by position (not exact `fire_at_ms`) to avoid false nondeterminism.

### External wait

Orchestration code:

```rust
let payload = ctx.schedule_wait("MyEvent").await;
```

Flow:

1. Scheduling emits `(token, Action::WaitExternal { name: "MyEvent" })`.
2. The engine persists `ExternalSubscribed { event_id: S, name: "MyEvent" }` and binds token → S.
3. The engine binds a deterministic subscription index for `(S, "MyEvent")`.
4. Each `ExternalEvent { name:"MyEvent", data }` appends to `external_arrivals["MyEvent"]`.
5. The waiting future resolves when arrival list contains element at its subscription index.

Important current limitation (also documented in the code): external events arriving before subscription binding are currently unsupported.

### Sub-orchestration

Orchestration code:

```rust
let child = ctx.schedule_sub_orchestration("Child", "input").await?;
```

Flow:

1. Scheduling emits `StartSubOrchestration` with a placeholder instance ID if none was provided.
2. When the engine allocates a real `event_id`, it rewrites that placeholder to a deterministic `sub::{event_id}` instance ID.
3. Completion events deliver `SubOrchOk`/`SubOrchErr` to the bound token.

### System calls

System calls are executed synchronously (no worker dispatch). In history they appear as `EventKind::SystemCall { op, value }`.

When replay encounters a system call event, it:

1. matches/binds it like a schedule event
2. immediately delivers `CompletionResult::SystemCallValue(value)`
3. re-polls the orchestration

---

## Aggregate Futures: select/join

Duroxide leans on standard `futures` combinators, but orchestration determinism still matters.

### join

`ctx.join(vec![...]).await` uses `futures::future::join_all`.

Properties:

- Deterministic given deterministic schedule order.
- Result ordering matches the input vector order (not completion arrival order).

### select

`ctx.select2(a, b).await` uses biased selection so poll order is stable.

Important:

- Do not use `tokio::select!` or `tokio::join!` inside orchestrations.
- Use the context helpers so replay behavior stays deterministic.

### What about loser cancellation?

Each future is keyed by its own token, so:

- each future is keyed by its own token
- completions don’t have to be consumed in global FIFO order
- “loser” completions can arrive later without blocking progress

They will still appear in history, but they won’t change execution if the orchestration is no longer awaiting them.

---

## Rust Async Differences (Why This Works)

Orchestrations are not normal async tasks.

### No wakers

Orchestration polling uses a no-op waker (`poll_once`). Futures do not wake the executor. Progress is driven by:

- new history arriving
- the replay engine delivering completions
- the engine re-polling when it may have unblocked the future

### Futures are recreated every turn

Orchestrations are re-run from the beginning. Local variables “work” because they are deterministically reconstructed.

### Side effects happen multiple times

Anything you do in orchestration code will run on every replay unless guarded. Prefer:

- replay-safe tracing helpers
- `if !ctx.is_replaying()` guards
- moving side effects into activities

---

## Nondeterminism Detection (Common Failure Modes)

The replay engine treats mismatches as configuration errors.

### Schedule mismatch

If the orchestration emits a different schedule than history expects (wrong kind, wrong name/input/op), replay fails.

Common causes:

- changing orchestration logic without versioning
- renaming an activity
- changing activity input encoding

### History schedule but no emitted action

If history contains a schedule event but the orchestration did not emit a matching action (at that point in replay), replay fails.

### Completion without open schedule

If a completion references a schedule ID that is not considered “open”, replay fails.

### External event ordering changes

External waits are correlated by subscription index. Changing the number/order of `schedule_wait("X")` calls can rebind arrivals to different awaits.

---

## Summary

Duroxide uses a token-based mechanism for durable futures:

- schedule_* emits `(token, Action)` and returns a `poll_fn` future
- replay binds token → schedule event ID by matching actions to history
- completions deliver by schedule ID but are stored by token
- the engine polls only when it may have progressed

This preserves Duroxide’s core durable execution model while keeping the user-facing API simple and idiomatic.
