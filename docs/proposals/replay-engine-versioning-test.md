# Replay Engine Extensibility — End-to-End Verification

## Problem

We claim the replay engine can evolve (new EventKind variants, new context methods) and that orchestration version upgrades via continue-as-new cleanly adopt those changes. There is no end-to-end test proving the full new-variant pipeline works: from context method → Action → EventKind → provider storage → replay engine delivery.

## Goal

Add a conditionally compiled "v2" extension to the replay engine that introduces pub/sub-style topic matching for external events, then write a scenario test that demonstrates:

1. **Replay engine extensibility**: New `EventKind` variants flow correctly through materialization, dedup, and delivery without breaking existing events.
2. **Clean event boundary via continue-as-new**: A v1→v2 orchestration upgrade produces independent execution histories — v2's history contains zero v1 events.
3. **Integration of the full new-variant pipeline**: `schedule_wait2()` → `Action::WaitExternal2` → `EventKind::ExternalSubscribed2` → provider storage → replay and back.
4. **Serde boundary isolation**: A build without the feature flag rejects v2 event types at deserialization (the same mechanism that makes capability filtering work).

All v2 code compiles to zero additional code when the feature flag is disabled.

### What this does NOT test

- **Binary-level isolation** between runtime versions — that is proven by the serde unit test (`v1_cannot_deserialize_v2_events`) and existing capability filtering tests in `tests/capability_filtering_tests.rs`.
- **Multiple runtime binaries** — both v1 and v2 handlers live in the same binary, using `register_versioned` (already tested extensively in `tests/versioning_tests.rs`).

## Feature Flag

```toml
[features]
replay-version-test = []   # Zero-cost: only gates test code for replay engine versioning verification
```

**Invariant**: With `replay-version-test` disabled, `cargo build` produces identical output to today. No new types, no new match arms, no new methods — everything behind `#[cfg(feature = "replay-version-test")]`.

## New Types (all `#[cfg(feature = "replay-version-test")]`)

### EventKind variants

```rust
/// V2 subscription: includes a topic filter for pub/sub matching.
#[cfg(feature = "replay-version-test")]
#[serde(rename = "ExternalSubscribed2")]
ExternalSubscribed2 { name: String, topic: String },

/// V2 event: includes the actual topic it was published on.
#[cfg(feature = "replay-version-test")]
#[serde(rename = "ExternalEvent2")]
ExternalEvent2 { name: String, topic: String, data: String },
```

### Action variant

```rust
#[cfg(feature = "replay-version-test")]
WaitExternal2 { scheduling_event_id: u64, name: String, topic: String },
```

### WorkItem variant

```rust
#[cfg(feature = "replay-version-test")]
ExternalRaised2 { instance: String, name: String, topic: String, data: String },
```

## New Context Methods (all `#[cfg(feature = "replay-version-test")]`)

### OrchestrationContext

```rust
/// Subscribe to an external event with topic-based pub/sub matching.
#[cfg(feature = "replay-version-test")]
pub fn schedule_wait2(&self, name: impl Into<String>, topic: impl Into<String>) -> DurableFuture<String>
```

Emits `Action::WaitExternal2 { name, topic }`, returns a `DurableFuture` — same pattern as `schedule_wait`.

### Client

```rust
/// Raise an external event with a topic for pub/sub matching.
#[cfg(feature = "replay-version-test")]
pub async fn raise_event2(
    &self,
    instance: impl Into<String>,
    event_name: impl Into<String>,
    topic: impl Into<String>,
    data: impl Into<String>,
) -> Result<(), ClientError>
```

Enqueues `WorkItem::ExternalRaised2`.

## Replay Engine Changes (all `#[cfg(feature = "replay-version-test")]`)

### ActionKind::External2

```rust
#[cfg(feature = "replay-version-test")]
External2 { name: String, topic: String },
```

### action_to_event arm

```rust
Action::WaitExternal2 { name, topic, .. } => EventKind::ExternalSubscribed2 { name, topic },
```

### action_matches_event_kind arm

```rust
(Action::WaitExternal2 { name, topic, .. }, EventKind::ExternalSubscribed2 { name: en, topic: et })
    => name == en && topic == et,
```

### update_action_event_id arm

```rust
Action::WaitExternal2 { name, topic, .. } => Action::WaitExternal2 {
    scheduling_event_id: event_id, name, topic,
},
```

### ExternalSubscribed2 history replay arm

Same as `ExternalSubscribed` but uses `ActionKind::External2 { name, topic }`:
- Calls `match_and_bind_schedule` with the new action kind
- Calls `bind_external_subscription2(event_id, name, topic)` on context inner

### ExternalEvent2 history replay arm

Topic-aware delivery:
- Calls `deliver_external_event2(name, topic, data)` on context inner
- Matching: exact match on both `name` AND `topic`

### ExternalRaised2 materialization (prep_completions)

Converts `WorkItem::ExternalRaised2` to `EventKind::ExternalEvent2`:
- Checks if `ExternalSubscribed2` with matching name AND topic exists in baseline history
- Same pattern as `ExternalRaised` → `ExternalEvent` materialization

### ExternalRaised2 dedup checks

In both `history_delta` and `baseline_history` dedup arms:
- Match by `name`, `topic`, AND `data` (all three fields)

### ExternalRaised2 execution-id check

```rust
WorkItem::ExternalRaised2 { .. } => true, // No execution ID, like ExternalRaised
```

### ExternalRaised2 nondeterminism check

```rust
WorkItem::ExternalRaised2 { .. } => {}, // No schedule to validate, like ExternalRaised
```

### Context inner additions

```rust
#[cfg(feature = "replay-version-test")]
fn bind_external_subscription2(&mut self, schedule_id: u64, name: &str, topic: &str)

#[cfg(feature = "replay-version-test")]
fn deliver_external_event2(&mut self, name: String, topic: String, data: String)
```

Uses a separate `external2_subscriptions: HashMap<u64, (String, String, usize)>` keyed by schedule_id → (name, topic, subscription_index), and `external2_arrivals: HashMap<(String, String), Vec<String>>` keyed by (name, topic) → payloads.

## Provider Changes (all `#[cfg(feature = "replay-version-test")]`)

### SQLite provider

- `WorkItem::ExternalRaised2` instance extraction in `enqueue_orchestrator_work_with_delay` (routes to orchestrator queue, same as `ExternalRaised`)
- `EventKind::ExternalSubscribed2` → `"ExternalSubscribed2"` and `EventKind::ExternalEvent2` → `"ExternalEvent2"` in event_type string mapping
- Serde handles serialization/deserialization automatically

## Test Scenario

### File: `tests/scenarios/replay_versioning.rs`

Wired into `tests/scenarios.rs`. Compiled only under `#[cfg(feature = "replay-version-test")]`.

### Test: `e2e_eternal_orch_v1_to_v2_event_upgrade`

Both handler versions live in the same binary, registered via `register_versioned`:

```rust
worker.register_versioned("Upgrader", "1.0.0", |ctx, input| async move {
    for _ in 0..3 {
        ctx.schedule_wait("approval").await;  // old-style, name-only
    }
    ctx.continue_as_new_versioned("2.0.0", state).await
});

worker.register_versioned("Upgrader", "2.0.0", |ctx, input| async move {
    for _ in 0..2 {
        ctx.schedule_wait2("approval", "orders.us-east").await;  // new-style, with topic
    }
    Ok(final_output)
});
```

The test driver raises events via `client.raise_event()` for v1 and `client.raise_event2()` for v2.

**Assertions**:
1. Orchestration completes successfully with correct output
2. V1 execution history contains `ExternalSubscribed` / `ExternalEvent` events
3. V2 execution history contains `ExternalSubscribed2` / `ExternalEvent2` events
4. Both execution histories are independently valid

### Test: `v1_cannot_deserialize_v2_events` (unit test)

Always compiled (no feature flag). Tests raw JSON deserialization:

```rust
#[test]
fn v1_cannot_deserialize_v2_events() {
    let json = r#"{"type": "ExternalSubscribed2", "name": "x", "topic": "y"}"#;
    // Without replay-version-test feature: deserialization fails
    // With replay-version-test feature: deserialization succeeds
    #[cfg(not(feature = "replay-version-test"))]
    assert!(serde_json::from_str::<EventKind>(json).is_err());
    #[cfg(feature = "replay-version-test")]
    assert!(serde_json::from_str::<EventKind>(json).is_ok());
}
```

This validates the serde boundary: a build without `replay-version-test` cannot parse v2 events. This is the same mechanism that makes capability filtering's drain procedure work — unknown event types fail deserialization.

## Version Boundary Mechanism

The `#[serde(tag = "type")]` strategy on `EventKind` means:

1. Old code (no `replay-version-test`) encounters `{"type": "ExternalSubscribed2", ...}` → **serde deserialization error**
2. Provider's `read_history_in_tx` fails → returns `ProviderError::permanent`
3. Capability filtering prevents old runtimes from fetching items with v2 pinned versions
4. Continue-as-new creates a new execution with a new pinned duroxide version → only v2-capable runtimes pick it up

This is the complete chain: orchestration versioning + capability filtering + serde boundaries = safe replay engine evolution.

## Implementation Order

1. Add `replay-version-test` feature flag to `Cargo.toml`
2. Add `EventKind` variants (`ExternalSubscribed2`, `ExternalEvent2`)
3. Add `Action::WaitExternal2` variant
4. Add `WorkItem::ExternalRaised2` variant
5. Add context inner fields + methods (`bind_external_subscription2`, `deliver_external_event2`, `get_external_event2`)
6. Add `OrchestrationContext::schedule_wait2` method
7. Add `Client::raise_event2` method
8. Add replay engine changes: `ActionKind::External2`, `action_to_event`, `action_matches_event_kind`, `update_action_event_id`, history match arms, materialization, dedup, execution-id check
9. Add SQLite provider: instance extraction arm, event_type string mapping
10. Write scenario test (`e2e_eternal_orch_v1_to_v2_event_upgrade`)
11. Write negative test (`v1_cannot_deserialize_v2_events`)

## Zero-Cost Guarantee

With `replay-version-test` disabled:
- No new enum variants compiled into `EventKind`, `Action`, `WorkItem`
- No new methods on `OrchestrationContext` or `Client`
- No new match arms in replay engine or provider
- No new test code compiled
- Binary output identical to pre-change

This feature exists purely to verify the versioning mechanism works. It is not a production feature.
