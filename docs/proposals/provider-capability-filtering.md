# Provider Capability-Filtered Fetching (Replay Engine Capability Filtering)

**Date:** 2026-02-04  
**Status:** Draft proposal

## Problem

We need safe rolling upgrades without relying on a runtime node to fetch an execution and then decide it cannot process it.

The previous strategy (“fetch everything, then bounce if incompatible”) becomes operationally complex:

- Incompatible items still get fetched/locked and requeued.
- Poison/cancellation semantics are subtle when items keep bouncing.
- In mixed-version clusters, we want work to naturally route to nodes that can process it.

## Goals

- A runtime node should only receive **compatible** orchestration executions for its replay engine.
- Compatibility (phase 1) is determined only by:
   - Replay engine capability: supported pinned `duroxide_version` semver ranges.
- Keep the architecture open to extend the same mechanism to:
   - Orchestration handler capability (name + version routing)
   - Activity worker capability (only pull activities registered in a runtime)
- Preserve the provider contract: providers remain storage/queue abstractions; they do not implement orchestration logic.

## Non-goals (phase 1)

- Full history migration.
- Cross-provider feature parity (SQLite first).
- Perfect cluster-wide liveness detection (we will define a safe fallback for stuck items).

## Core idea

Extend the provider fetch APIs so the **dispatcher supplies a capability filter**, and the provider returns only items whose instance/execution metadata matches that filter.

This flips the model from:

- “runtime fetches, then decides if it can process”

to:

- “runtime declares what it can process; provider only hands it compatible work”.

## Definitions

### Pinned replay version (execution)

- Each execution is pinned to a Duroxide crate version derived from the execution’s first `OrchestrationStarted` event’s `Event.duroxide_version`.
- This value is immutable for the execution boundary.- **ContinueAsNew creates a new execution with its own pinned version.** The new execution starts
  with empty history and a fresh `OrchestrationStarted` event, so it is pinned to the `duroxide_version`
  of the runtime node that processes the `ContinueAsNew` work item — not the version of the original
  execution. This is correct because each execution's history is replay-independent (fresh start),
  so it should be pinned to the runtime that actually produces its history.
### Capability filter

A runtime node advertises (phase 1):

1. **Replay engine capability**
   - `supported_pinned_versions: Vec<SemverRange>`
   - **Default:** `>=0.0.0, <=CURRENT_BUILD_VERSION` — the runtime assumes it can replay any
     execution pinned at or below its own build semver. This is the safe default because replay
     engines are backward-compatible: a newer runtime can always replay history produced by an
     older version of itself.
   - Users can override this to narrow the range (e.g., `>=1.0.0, <2.0.0`) for clusters
     where strict version routing is desired.

Extension points (not implemented in phase 1, but the filter type should be designed to support them):

2. **Orchestration handler capability**
   - Conceptually: `supported_orchestrations: { name -> Vec<SemverRange or exact versions> }`

3. **Activity worker capability**
   - Conceptually: `supported_activities: { name -> bool }` (or ranges if activity versions are introduced)

## Provider API changes

### Fetch with filter

Add an optional filter to orchestration fetch:

- `fetch_orchestration_item(lock_timeout, poll_timeout, filter: Option<DispatcherCapabilityFilter>)`

If `filter` is `None`, behavior remains unchanged (legacy behavior / admin tooling).

If `Some(filter)`, the provider must only return an `OrchestrationItem` if:

- The current execution’s pinned `duroxide_version` is compatible with `filter.supported_pinned_versions`.

Future extension (not phase 1): also filter on orchestration handler name/version.

### Filtering semantics (queues + instance lock)

The provider must apply the filter *before* acquiring the instance lock / returning a batch.

In SQLite terms (conceptually):

- Select a candidate instance ID from `orchestrator_queue` whose messages are visible.
- Join `instances`/`executions` to get:
   - `current_execution_id`
   - `pinned_duroxide_version` for that execution
- Apply filter predicates.
- Only then acquire the instance lock and lock visible messages.

This avoids “compatibility bouncing” entirely.
## Runtime-side compatibility check (defense-in-depth)

Provider-level filtering is the primary mechanism, but the **runtime must also validate compatibility after fetching**, as a defense-in-depth layer. This handles:

- Providers that do not implement capability filtering (filter ignored or not supported).
- Legacy / admin tooling calls that use `filter=None`.
- Edge cases where provider-side filtering misses something (bugs, race conditions).

### Behavior

After `fetch_orchestration_item` returns an item, the orchestration dispatcher checks if the execution's pinned `duroxide_version` (available from the `OrchestrationStarted` event in the returned history) falls within the runtime's supported replay-engine version range.

If the pinned version is **not supported**:

1. The runtime **abandons** the item immediately via `abandon_orchestration_item()` with no delay.
2. This increments `attempt_count` naturally.
3. The existing max-attempts / poison-item logic handles termination — once `attempt_count` exceeds the configured threshold, the runtime terminates the orchestration with a configuration error.
4. The runtime logs a warning with the instance ID, pinned version, and the runtime's supported range.
5. An observability counter is incremented (e.g., `duroxide.orchestration.incompatible_version_abandoned`).

This means that even if `filter=None` is supplied (or the provider ignores the filter), incompatible executions are **not silently stuck** — they get abandoned, attempt counts rise, and eventually the poison path terminates them with an explicit error message.

### Relationship to provider-level filtering

Provider-level filtering is still strongly preferred because:

- It avoids the fetch-lock-abandon cycle entirely (no wasted work).
- In mixed-version clusters, work routes directly to compatible nodes.
- The runtime-side check is a **fallback**, not the primary mechanism.

When both layers are active, the runtime-side check should almost never trigger (it only fires if the provider's filter has a bug or is unimplemented).
## Data requirements (how provider knows pinned version)

A provider must be able to answer “what is this execution’s pinned `duroxide_version`?” without parsing arbitrary event payloads.

Two acceptable approaches:

1. **Denormalize pinned version into execution metadata** (recommended)
   - Extend runtime-computed `ExecutionMetadata` to include `pinned_duroxide_version`.
   - Runtime sets this when stamping `OrchestrationStarted` for a new execution.
   - Provider stores it in an `executions.pinned_duroxide_version` column.
   - Provider never inspects event contents.

2. **Compute from history table** (not recommended for hot path)
   - Query the first `OrchestrationStarted` event row for the execution and read `duroxide_version`.
   - This increases read amplification and needs careful indexing.

## Version representation (phase 1)

For phase 1 we standardize on **numeric semver columns** using the canonical terms:

- `major` (integer)
- `minor` (integer)
- `patch` (integer)

The provider should store the execution’s pinned runtime version as three integers (e.g.,
`pinned_major`, `pinned_minor`, `pinned_patch`) so that compatibility checks do not require
string parsing or JSON reads in the hot path.

### Range comparisons using (major, minor, patch)

Supported replay-engine capability is expressed as one or more semver ranges. In phase 1,
the runtime should compile those ranges into numeric bounds, and the provider should evaluate
compatibility via numeric comparisons.

Implementation note: the easiest SQL predicate is a lexicographic compare over the tuple
`(major, minor, patch)` against `(min_major, min_minor, min_patch)` and `(max_major, max_minor, max_patch)`.
This can be implemented with explicit boolean logic (no string compares).

If later profiling shows this is still too costly, we can add an optional derived packed key
in addition to the columns, but the **contract** remains that the provider has the separate
`major/minor/patch` values available for filtering and debugging.

## Handling “orchestration not registered on this node”

Not phase 1.

Phase 1 only prevents a node from fetching executions whose pinned runtime version it cannot replay.
The same capability-filter mechanism should later be extended so a node also only fetches work for
orchestrations/versions that are registered on that node.

Separately, the same concept should extend to the worker dispatcher so it only pulls activity work
items for activities registered on that runtime node.

## Poison / cancellation / stuck items

Filtering introduces a new risk: if *no* node in the cluster declares compatibility for some execution,
then **no one will ever fetch it** via filtered queries, so runtime-side `attempt_count`-based poison
termination could be delayed.

The operator's remedy is to temporarily widen the `supported_replay_versions` range on one or more
runtime nodes (see below). Items with unknown event types will fail at deserialization and be
poisoned via the max-attempts path.

## RuntimeOptions changes

### `supported_replay_versions: Option<SemverRange>` (optional override)

```rust
pub struct RuntimeOptions {
    // ... existing fields ...

    /// Override the replay-engine version range used for capability filtering.
    ///
    /// By default, the runtime uses `>=0.0.0, <=CURRENT_BUILD_VERSION`, meaning it
    /// can replay any execution pinned at or below its own semver. This is correct for
    /// most deployments since replay engines are backward-compatible.
    ///
    /// Set this to change the range for advanced scenarios:
    /// - Narrowing: Restrict a node to only process a specific version band.
    /// - Widening to drain stuck items: Set a wide range like `>=0.0.0, <=99.0.0`
    ///   to fetch orchestrations pinned at any version. Items with unknown event types
    ///   will fail at deserialization and be poisoned via the max-attempts path.
    /// - Testing: Simulate version-routing behavior in tests.
    ///
    /// Default: `None` (uses `>=0.0.0, <=CURRENT_BUILD_VERSION`)
    pub supported_replay_versions: Option<SemverRange>,
}
```

## Rolling upgrades (desired behavior)

- Old nodes declare older supported ranges (up to their build version).
- New nodes declare newer/wider ranges (up to their newer build version).
- Provider naturally routes items to the correct nodes based on capabilities.
- No bouncing/abandon loops required for compatibility.
- If after a full upgrade some executions remain pinned to an unsupported old version,
  an operator can temporarily widen `supported_replay_versions` to clear them (see
  "Draining stuck orchestrations" in the versioning guide).

## Worked example: rolling upgrade with new event types

This walks through a concrete rolling upgrade where duroxide v2.0.0 introduces two new event
types (`RaiseEvent2`, `WaitEvent2`) that v1.x replay engines do not understand.

### Setup

- **Cluster:** 3 runtime nodes (Node A, Node B, Node C), all running duroxide v1.5.0.
- **Provider:** Shared SQLite (or Postgres) database.
- **Workload:** Mix of long-running orchestrations (some using ContinueAsNew) and new orchestrations
  being started continuously.
- **Change:** duroxide v2.0.0 adds `EventKind::RaiseEvent2` and `EventKind::WaitEvent2`. The v2.0.0
  replay engine can replay both v1.x and v2.x history. The v1.x replay engine will fail if it
  encounters `RaiseEvent2` or `WaitEvent2` in history (unknown event kind → deserialization error).

### Before upgrade (steady state)

| Node | Build version | Default supported range | Provider filter |
|------|---------------|------------------------|-----------------|
| A    | v1.5.0        | `>=0.0.0, <=1.5.0`    | `Some(>=0.0.0, <=1.5.0)` |
| B    | v1.5.0        | `>=0.0.0, <=1.5.0`    | `Some(>=0.0.0, <=1.5.0)` |
| C    | v1.5.0        | `>=0.0.0, <=1.5.0`    | `Some(>=0.0.0, <=1.5.0)` |

All executions have `pinned_duroxide_version` in the `1.x` range. All nodes can fetch and
process everything. Capability filtering is a no-op — everyone is compatible.

### Step 1: Upgrade Node A to v2.0.0

| Node | Build version | Default supported range | Provider filter |
|------|---------------|------------------------|-----------------|
| A    | **v2.0.0**    | `>=0.0.0, <=2.0.0`    | `Some(>=0.0.0, <=2.0.0)` |
| B    | v1.5.0        | `>=0.0.0, <=1.5.0`    | `Some(>=0.0.0, <=1.5.0)` |
| C    | v1.5.0        | `>=0.0.0, <=1.5.0`    | `Some(>=0.0.0, <=1.5.0)` |

**What happens:**

- **Existing v1.x executions:** All 3 nodes can process them (v1.x is within everyone's range).
  Node A's v2.0.0 replay engine is backward-compatible and replays v1.x history correctly.
- **New orchestrations started on Node A:** Their `OrchestrationStarted` event is stamped with
  `duroxide_version: "2.0.0"`, so `pinned_duroxide_version = (2, 0, 0)`. If the orchestration
  code uses the new `RaiseEvent2`/`WaitEvent2` APIs, history will contain v2.x-only events.
- **v2.0.0-pinned executions in the queue:** Only Node A's filter matches `(2, 0, 0)`.
  Nodes B and C's filter (`<=1.5.0`) excludes them. These executions are **only routed to Node A**.
- **ContinueAsNew on Node A:** If a v1.x execution does ContinueAsNew and Node A picks up the
  `ContinueAsNew` work item, the **new execution is pinned at v2.0.0** (fresh `OrchestrationStarted`
  stamped by Node A). From that point, the new execution can only be processed by v2.0.0+ nodes.
- **ContinueAsNew on Node B/C:** If Node B or C picks up the ContinueAsNew, the new execution
  is pinned at v1.5.0 and remains processable by all nodes.

**Risk:** Node A is the only node handling v2.0.0-pinned work. If Node A goes down, v2.0.0
executions queue up until Node A returns or another node is upgraded. This is expected and safe —
the work is not lost, just waiting.

### Step 2: Upgrade Node B to v2.0.0

| Node | Build version | Default supported range | Provider filter |
|------|---------------|------------------------|-----------------|
| A    | v2.0.0        | `>=0.0.0, <=2.0.0`    | `Some(>=0.0.0, <=2.0.0)` |
| B    | **v2.0.0**    | `>=0.0.0, <=2.0.0`    | `Some(>=0.0.0, <=2.0.0)` |
| C    | v1.5.0        | `>=0.0.0, <=1.5.0`    | `Some(>=0.0.0, <=1.5.0)` |

**What happens:**

- v2.0.0-pinned items now have **two nodes** (A and B) that can process them — better availability.
- v1.x items are still processable by all three nodes.
- Node C continues to only see v1.x work.

### Step 3: Upgrade Node C to v2.0.0

| Node | Build version | Default supported range | Provider filter |
|------|---------------|------------------------|-----------------|
| A    | v2.0.0        | `>=0.0.0, <=2.0.0`    | `Some(>=0.0.0, <=2.0.0)` |
| B    | v2.0.0        | `>=0.0.0, <=2.0.0`    | `Some(>=0.0.0, <=2.0.0)` |
| C    | **v2.0.0**    | `>=0.0.0, <=2.0.0`    | `Some(>=0.0.0, <=2.0.0)` |

**What happens:**

- All nodes are now v2.0.0. All work (v1.x and v2.x pinned) is processable by everyone.
- Capability filtering is again a no-op — everyone is compatible with everything.
- Any remaining v1.x-pinned executions continue to run fine (v2.0.0 replay engine is
  backward-compatible).
- Over time, as v1.x executions complete or do ContinueAsNew (now pinned at v2.0.0),
  the cluster naturally converges to all-v2.0.0-pinned work.

### Edge case: what if v1.x nodes can't replay v2.x history?

This is the key scenario that capability filtering prevents. Without filtering, different
providers will exhibit different failure modes depending on how they handle unknown event types
during history deserialization inside `fetch_orchestration_item`.

#### Failure modes WITHOUT these requirements

Without the contract requirements below, different providers exhibit different (and both
problematic) behaviors when encountering unknown event types:

- **Silent event dropping** (e.g., current SQLite `read_history_in_tx` uses `if let Ok(...)`): 
  History arrives with gaps → confusing nondeterminism errors → misleading poison messages.
- **Hard error with rollback**: `attempt_count` may not be incremented → infinite error loop,
  poison path never triggers.

Both are unacceptable. The provider contract must eliminate them.

#### Provider contract requirement 1: Filter before deserialize

Providers **MUST apply the capability filter before loading or deserializing history events**.
This is the primary defense. The correct implementation order inside `fetch_orchestration_item` is:

1. Find a candidate instance from the queue.
2. Check the execution's `pinned_major/minor/patch` against the filter. **Skip if incompatible.**
3. Only then acquire the instance lock.
4. Only then load and deserialize history.

When the filter is applied correctly, the provider never encounters unknown event types because
it never loads history from an incompatible execution. This is the happy path for all normal
operations and rolling upgrades.

#### Provider contract requirement 2: Deserialization errors must be surfaced, not swallowed

Even with filtering in place, a provider may still encounter undeserializable history events in
edge cases (e.g., `filter=None` in drain mode, corrupted data, bugs). When this happens:

- Providers **MUST NOT** silently drop events they cannot deserialize. Silent dropping leads to
  incomplete history, which causes confusing nondeterminism errors or worse — incorrect behavior.
- Providers **MUST** return `ProviderError::permanent(...)` when history deserialization fails.
  This is critical because the dispatcher handles permanent errors by logging and retrying, and
  — crucially — `attempt_count` must have been incremented before the error is returned so the
  existing max-attempts poison machinery can terminate the orchestration.

The required implementation pattern:

1. Acquire instance lock and increment `attempt_count` (commit this atomically).
2. Load history events.
3. If any event fails to deserialize → return `ProviderError::permanent("Failed to deserialize
   event at position N: {serde_error}")`. The attempt_count was already incremented in step 1.
4. On the next fetch cycle, the item is fetched again with a higher attempt_count.
5. Once `attempt_count > max_attempts`, the runtime poisons the orchestration with a clear
   error message.

This means deserialization failures are **not infinite loops** — they naturally flow into the
existing poison path, producing a clear terminal error.

> **TODO (doc):** Add both requirements to `docs/provider-implementation-guide.md`:
> - The `fetch_orchestration_item` section should specify that capability filter predicates
>   must be evaluated before history deserialization, with pseudocode showing correct ordering.
> - A new "History deserialization contract" subsection should specify that unknown events must
>   produce a permanent error (not be silently dropped), and that `attempt_count` must be
>   incremented before history loading so the poison path works.
> Also update `docs/provider-testing-guide.md` to reference the new provider validation tests.

> **TODO (code):** Fix the current SQLite provider's `read_history_in_tx()` to return an error
> instead of silently dropping undeserializable events. The current `if let Ok(event) = ...`
> pattern violates requirement 2.

### Worked example: versioned orchestration with new event types

The previous walkthrough assumed all orchestration code uses the same events. In practice,
new event types are introduced *alongside versioned orchestration handlers*. Here's how that
works end-to-end.

**Scenario:**

- duroxide v1.5.0 has `schedule_wait("event_name")` → produces `ExternalSubscribed` + `ExternalEvent` events.
- duroxide v2.0.0 adds `schedule_wait_v2("event_name", timeout)` → produces `ExternalSubscribedV2` + `ExternalEventV2` events with timeout support.
- The application has orchestration handler `"ProcessOrder"`:
  - **v1** (registered on v1.5.0 nodes): uses `ctx.schedule_wait("approval")`.
  - **v2** (registered on v2.0.0 nodes): uses `ctx.schedule_wait_v2("approval", Duration::from_secs(3600))`.

**Before upgrade — all nodes v1.5.0:**

```
Registered handlers:    ProcessOrder v1
History event types:    ExternalSubscribed, ExternalEvent (v1.x events)
All executions pinned:  v1.5.0
```

Everything runs on all nodes. No version conflicts.

**Step 1 — Upgrade Node A to v2.0.0, register both handler versions:**

```
Node A (v2.0.0):  ProcessOrder v1 + v2 registered
Node B (v1.5.0):  ProcessOrder v1 only
Node C (v1.5.0):  ProcessOrder v1 only
```

| What happens | Detail |
|--------------|--------|
| **Existing v1 executions** (pinned v1.5.0) | All nodes can fetch (v1.5.0 ∈ everyone's range). All have handler v1 registered. Replays fine. |
| **New orchestrations via `start_orchestration("ProcessOrder", ...)`** | Version resolution picks the latest registered version on the node that processes the start. On Node A → could resolve to v2. On B/C → resolves to v1. |
| **New orchestration started on Node A as v2** | `OrchestrationStarted` is stamped with `duroxide_version: "2.0.0"`, pinned at v2.0.0. Handler v2 runs, produces `ExternalSubscribedV2` events in history. |
| **v2 execution needs a turn later** | It's pinned at v2.0.0. Only Node A's filter matches. Nodes B/C never fetch it. Safe. |
| **v1 execution does ContinueAsNew on Node A** | New execution pinned at v2.0.0. If the registry resolves to handler v2, future turns use `schedule_wait_v2`. History now has v2 events. Only v2.0.0 nodes can replay. |
| **v1 execution does ContinueAsNew on Node B** | New execution pinned at v1.5.0. Handler v1 runs, produces v1 events. All nodes can replay. |

**Key insight:** The pinned version and the handler version work together naturally:

- Pinned version gates which *replay engine* can process the history.
- Handler version determines which *orchestration code* runs.
- A v2.0.0 node can replay v1.x history (backward-compatible replay engine) and run v1 handler code (both versions registered).
- A v1.5.0 node cannot replay v2.x history (doesn't understand new events) and doesn't have v2 handler (not registered).

**Step 2 — Upgrade Node B to v2.0.0:**

```
Node A (v2.0.0):  ProcessOrder v1 + v2
Node B (v2.0.0):  ProcessOrder v1 + v2
Node C (v1.5.0):  ProcessOrder v1 only
```

v2.0.0-pinned work now load-balances across A and B. Node C still handles only v1.x work.

**Step 3 — Upgrade Node C to v2.0.0:**

```
All nodes (v2.0.0): ProcessOrder v1 + v2
```

Full cluster upgraded. All work processes on all nodes.

**Step 4 — Deregister handler v1 (optional, after all v1 executions have completed):**

Once the operator confirms no running executions are using handler v1 (all have either
completed or done ContinueAsNew into v2), they can remove v1 from the registries:

```
All nodes (v2.0.0): ProcessOrder v2 only
```

Any straggler v1 execution that attempts a turn would hit the "unregistered orchestration"
backoff path (existing mechanism, orthogonal to capability filtering).

**What if an operator starts a new orchestration explicitly as v1 on a v2.0.0 node?**

```rust
client.start_orchestration_with_version("ProcessOrder", "v1", input).await?;
```

The `OrchestrationStarted` event is still stamped with `duroxide_version: "2.0.0"` (the
runtime build), not "1.5.0". This is correct — the *replay engine* version is v2.0.0
(that's what will replay this history). Handler v1 code runs, but it only uses `schedule_wait`
(v1 events), so any v2.0.0 node can replay it fine.

The pinned version reflects the **replay engine**, not the **handler version**. They are
independent dimensions.

### Post-upgrade cleanup (if needed)

If after a full upgrade to v2.0.0, some orchestrations are still pinned at, say, v0.8.0
(from a very old execution that's been idle), they should still work — v2.0.0's range
(`>=0.0.0, <=2.0.0`) includes v0.8.0.

If a future v3.0.0 intentionally **drops** replay compatibility for v0.x histories (breaking
change), those v0.8.0-pinned executions would become unfetchable. The operator would then
use the wide-range drain procedure (test #29): set `supported_replay_versions` to a wide
range like `[>=0.0.0, <=99.0.0]` on one node. The provider fetches old-version items and
attempts to deserialize their history. Unknown event types cause deserialization to fail at
the provider level with a permanent error — the items never reach the replay engine. Each
fetch cycle increments `attempt_count`, and the items remain in the queue permanently
erroring on every fetch. Compatible items are processed normally.

> **Beyond phase 1:** A dedicated `disable_version_filtering: true` RuntimeOptions flag
> could simplify this UX, but the wide-range approach already covers the same use case.
> Additionally, a built-in replay-engine compatibility check (separate from the configurable
> `supported_replay_versions` range) could catch semantic incompatibilities where history
> deserializes successfully but replay behavior differs across versions. Currently, drain
> relies entirely on serde deserialization failures for unknown `EventKind` variants.

## Observability

- Runtime should log capability declarations at startup. (implemented)
- **Beyond phase 1:**
   - Provider should expose counters for "items filtered out due to unsupported pinned version"
   - (future) "items filtered out due to unsupported orchestration handler/version"
   - (future) "worker items filtered out due to unknown activity handler"

## Test plan (phase 1)

### Where tests should live

- Provider filtering behavior is best validated in SQLite-focused integration tests and scenario tests:
  - `tests/scenarios/rolling_deployment.rs` (extend) or a new `tests/scenarios/capability_filtering.rs`
   - Provider validation suite can add a targeted validation for filtered fetch if we decide it’s part of the Provider trait contract.

Keep tests for unregistered orchestration handlers (today’s bounce behavior) in existing places; they are
orthogonal to phase 1 replay-engine filtering.

### Test categories

#### A. Provider validation tests (`src/provider_validation/capability_filtering.rs`)

These are generic tests runnable against any `Provider` implementation (SQLite, Postgres, etc.)
via the `ProviderFactory` trait, following the existing validation test pattern.

1. **`fetch_with_filter_none_returns_any_item`**
   - Seed an instance with pinned version `1.2.3`.
   - Fetch with `filter=None`.
   - Assert the item is returned (legacy behavior preserved).

2. **`fetch_with_compatible_filter_returns_item`**
   - Seed an instance with pinned version `1.2.3`.
   - Fetch with filter `supported_pinned_versions: [>=1.0.0, <2.0.0]`.
   - Assert the item is returned.

3. **`fetch_with_incompatible_filter_skips_item`**
   - Seed an instance with pinned version `1.2.3`.
   - Fetch with filter `supported_pinned_versions: [>=2.0.0, <3.0.0]`.
   - Assert `Ok(None)` is returned (item not fetched, not locked).

4. **`fetch_filter_skips_incompatible_selects_compatible`**
   - Seed two instances: `A` pinned at `1.0.0`, `B` pinned at `2.0.0`.
   - Fetch with filter `[>=2.0.0, <3.0.0]`.
   - Assert only `B` is returned.
   - Fetch again with filter `[>=1.0.0, <2.0.0]`.
   - Assert only `A` is returned.

5. **`fetch_filter_does_not_lock_skipped_instances`**
   - Seed instance `A` pinned at `1.0.0`.
   - Fetch with incompatible filter → returns `None`.
   - Fetch with compatible filter → returns `A` (proves it was not locked by the first fetch).

6. **`fetch_filter_null_pinned_version_always_compatible`**
   - Seed an instance whose execution has no pinned version columns (NULL — pre-migration data).
   - Fetch with any filter.
   - Assert the item is returned (NULL = always compatible, safe backfill default).

7. **`fetch_filter_boundary_versions`**
   - Seed instances with versions at range boundaries: `1.0.0`, `1.9.99`, `2.0.0`.
   - Fetch with filter `[>=1.0.0, <2.0.0]`.
   - Assert `1.0.0` and `1.9.99` are returned on successive fetches; `2.0.0` is never returned.

8. **`pinned_version_stored_via_ack_metadata`**
   - Start a new orchestration. On first ack, pass `ExecutionMetadata` with `pinned_duroxide_version`.
   - Fetch with a filter matching that version → item is available.
   - Confirms provider stores and reads back the version from metadata, not from event payloads.

9. **`pinned_version_immutable_across_ack_cycles`**
   - Seed an instance with pinned version `1.0.0`.
   - Fetch, process, ack (runtime acks with metadata that still says `1.0.0`).
   - Enqueue a new message for the same instance.
   - Fetch with filter `[>=1.0.0, <2.0.0]` → still returned.
   - Confirms pinned version persists across ack cycles within the same execution.

#### B. Provider validation: ContinueAsNew execution isolation

10. **`continue_as_new_execution_gets_own_pinned_version`**
    - Seed instance `A` with execution 1 pinned at `1.0.0`.
    - Ack execution 1 as `ContinuedAsNew`, creating execution 2 pinned at `2.0.0`.
    - Enqueue a message for the new execution.
    - Fetch with filter `[>=2.0.0, <3.0.0]` → returned (uses execution 2's pinned version).
    - Fetch with filter `[>=1.0.0, <2.0.0]` → `None` (execution 2 is not compatible with the old range).

11. **`continue_as_new_does_not_inherit_previous_pinned_version`**
    - Same as above but explicitly verifies the new execution's pinned version columns are set
      from the new `ExecutionMetadata`, not carried over from the previous execution.

#### C. Runtime-side compatibility check tests (`tests/scenarios/capability_filtering.rs`)

End-to-end scenario tests using real `Runtime` + `Client` + SQLite provider.

12. **`runtime_abandons_incompatible_execution`**
    - Directly insert (via provider API) an instance with a pinned version outside the runtime's
      supported range.
    - Start a runtime with `filter=None` (or provider that ignores filter).
    - Assert the runtime fetches the item but immediately abandons it (no replay attempted).
    - Assert `attempt_count` increments.

13. **`runtime_processes_compatible_execution_normally`**
    - Start an orchestration normally (pinned version = current crate version).
    - Runtime's supported range includes the current version.
    - Assert orchestration completes successfully (no abandons, no version-related warnings).

14. **`runtime_abandon_reaches_max_attempts_and_poisons`**
    - Insert an instance with incompatible pinned version.
    - Configure `RuntimeOptions { max_attempts: 3 }`.
    - Start a single runtime with `filter=None`.
    - Assert the item is abandoned 3 times, then on the 4th fetch the runtime terminates it
      with a `Configuration` error whose message includes the pinned version and supported range.
    - Assert final execution status is `Failed`.

15. **`runtime_abandon_uses_short_delay`**
    - Insert an incompatible instance.
    - Start runtime, observe the abandon call.
    - Assert the item is abandoned with a short delay (1 second) to prevent tight spin loops.
    - Assert the item becomes visible again after the delay, and subsequent fetches
      continue the abandon cycle toward max_attempts.

#### D. Rolling deployment scenario tests (`tests/scenarios/capability_filtering.rs`)

16. **`two_runtimes_different_version_ranges_route_correctly`**
    - Start two runtime instances sharing the same provider:
      - Runtime A supports `[>=1.0.0, <2.0.0]`
      - Runtime B supports `[>=2.0.0, <3.0.0]`
    - Start orchestration `X` (pinned at `1.x`) and orchestration `Y` (pinned at `2.x`).
    - Assert `X` completes on Runtime A and `Y` completes on Runtime B.
    - Assert neither runtime processes the other's orchestration.

17. **`overlapping_version_ranges_both_can_process`**
    - Two runtimes with overlapping ranges: `[>=1.0.0, <3.0.0]` and `[>=2.0.0, <4.0.0]`.
    - Start an orchestration pinned at `2.5.0`.
    - Assert either runtime can pick it up (no deadlock, no double-processing).

18. **`mixed_cluster_compatible_and_incompatible_items`**
    - Seed 5 instances: 3 compatible with Runtime A, 2 compatible with Runtime B.
    - Start both runtimes concurrently.
    - Assert all 5 complete, each on the correct runtime.

#### E. Metadata and migration tests

19. **`execution_metadata_includes_pinned_version_on_new_orchestration`**
    - Start a fresh orchestration via the normal runtime path.
    - After completion, inspect the execution's stored metadata.
    - Assert `pinned_major`, `pinned_minor`, `pinned_patch` columns match the current crate version.

20. **`existing_executions_without_pinned_version_remain_fetchable`**
    - Create instances using a provider *before* the migration (or set pinned columns to NULL).
    - Apply the migration.
    - Fetch with a filter → assert items are returned (NULL = always compatible).
    - Ensures backward compatibility with pre-capability-filtering data.

21. **`pinned_version_extracted_from_orchestration_started_event`**
    - Unit test for the runtime's `compute_execution_metadata()` function.
    - Provide a history containing `OrchestrationStarted` with `duroxide_version: "3.1.4"`.
    - Assert the computed `ExecutionMetadata` has `pinned_duroxide_version = (3, 1, 4)`.

#### F. Edge cases and error handling

22. **`filter_with_empty_supported_versions_returns_nothing`**
    - Fetch with `filter = Some(DispatcherCapabilityFilter { supported_duroxide_versions: vec![] })`.
    - Assert `Ok(None)` — empty range means "supports nothing".

23. **`concurrent_filtered_fetch_no_double_lock`**
    - Two concurrent fetch calls with the same filter, one compatible instance.
    - Assert exactly one caller gets the item; the other gets `None`.
    - Validates that filtering doesn't break instance-lock exclusivity.

24. **`sub_orchestration_gets_own_pinned_version`**
    - Parent orchestration pinned at `1.0.0` schedules a sub-orchestration.
    - Sub-orchestration starts as a new instance+execution with its own `OrchestrationStarted`.
    - Assert sub-orchestration's pinned version is set independently (from its own event, not inherited from parent).

25. **`activity_completion_after_continue_as_new_is_discarded`**
    - Execution 1 (pinned v1.0.0) schedules an activity.
    - Execution 1 does ContinueAsNew → Execution 2 starts (pinned v2.0.0).
    - Two sub-cases:
      a. **Activity cancelled by ContinueAsNew (common case):** The ContinueAsNew ack includes
         the activity in `cancelled_activities`, deleting its worker queue entry via lock stealing.
         The worker's next `ack_work_item` or `renew_work_item_lock` call returns a permanent error
         ("lock not found"). Assert: no `ActivityCompleted` message reaches the orchestrator queue.
      b. **Activity raced and completed before ContinueAsNew:** The `ActivityCompleted` work item
         reaches the orchestrator queue with `execution_id: 1`. When the runtime fetches and
         processes it, `is_completion_for_current_execution` returns `false` (current is 2).
         The completion is discarded with a "ignoring completion from previous execution" warning.
         Assert: the completion does NOT produce an event in execution 2's history.
    - In both sub-cases, the capability filter routes correctly: the orchestrator queue item
      is fetched by a v2-compatible runtime (based on execution 2's pinned version), and the
      stale completion is harmlessly discarded. No work is lost or misrouted.

#### F2. Additional provider contract validation tests

These tests validate specific implementation ordering and edge cases identified during code review.

43. **`fetch_filter_applied_before_history_deserialization`**
    - Seed an execution pinned at `99.0.0` with valid (but version-specific) history events.
    - Fetch with filter `[>=1.0.0, <=2.0.0]` (excludes `99.0.0`).
    - Assert `Ok(None)` — the provider must not attempt to load or deserialize history
      for this execution. If the provider incorrectly loads history before filtering,
      this test would still pass but test #39 (corrupted history variant) would catch it.

44. **`fetch_single_range_only_uses_first_range`**
    - Seed two instances: `A` pinned at `1.0.0`, `B` pinned at `3.0.0`.
    - Fetch with filter `{ supported_duroxide_versions: [>=1.0.0..<=1.5.0, >=3.0.0..<=3.5.0] }`.
    - Assert only `A` is returned (phase 1: only first range is used).
    - Document this as a known phase-1 limitation.

45. **`ack_stores_pinned_version_via_metadata_update`**
    - Create an execution without a pinned version (pre-migration simulation).
    - Ack with `ExecutionMetadata { pinned_duroxide_version: Some(1.2.3), .. }`.
    - Fetch with filter matching `1.2.3` → assert item is returned.
    - Confirms the UPDATE path for backfilling pinned version on existing executions.

46. **`provider_updates_pinned_version_when_told`**
    - Create an execution with pinned version `1.0.0`.
    - Ack again with `pinned_duroxide_version: Some(2.0.0)`.
    - Fetch with filter matching `2.0.0` → returned (provider updated unconditionally).
    - Fetch with filter matching only `1.0.0` → `None` (version was overwritten).
    - The provider is dumb storage — write-once semantics are enforced by a `debug_assert`
      in the runtime's orchestration dispatcher, not by the provider.

#### G. Version range and routing tests

26. **`default_supported_range_includes_current_and_older_versions`**
    - Start a runtime with default options (no `supported_replay_versions` override).
    - Insert instances pinned at `0.0.1`, `0.5.0`, and `CURRENT_BUILD_VERSION`.
    - Assert all three are processed successfully (default range is `>=0.0.0, <=CURRENT`).

27. **`default_supported_range_excludes_future_versions`**
    - Start a runtime with default options.
    - Insert an instance pinned at a version higher than the current build (e.g., `99.0.0`).
    - Assert the runtime abandons it (future versions are not supported).

28. **`custom_supported_replay_versions_narrows_range`**
    - Start a runtime with `supported_replay_versions: Some([>=1.0.0, <=1.9.999])`.
    - Insert instances pinned at `0.9.0`, `1.5.0`, and `2.0.0`.
    - Assert only `1.5.0` is processed; the others are abandoned.

29. **`wide_supported_range_drains_stuck_items_via_deserialization_error`**
    - Insert instances pinned at an unsupported future version (e.g., `99.0.0`) with
      history containing unknown event types.
    - Start a runtime with `supported_replay_versions: Some([>=0.0.0, <=99.0.0])`
      and `max_attempts: 3`.
    - The runtime fetches the items (wide range includes `99.0.0`), attempts replay,
      hits deserialization errors (unknown event types), and the poison path terminates
      them with clear error messages.
    - This is the recommended operational procedure for draining stuck orchestrations.

#### H. Observability tests

30. **`runtime_logs_warning_on_incompatible_abandon`**
    - Fetch an incompatible item, trigger runtime-side abandon.
    - Assert a warning log is emitted containing: instance ID, pinned version, supported range.

31. **`runtime_logs_capability_declaration_at_startup`**
    - Start a runtime with an explicit `supported_replay_versions` config.
    - Assert an info-level log at startup lists the supported version ranges.

#### I. Provider deserialization contract tests (`src/provider_validation/capability_filtering.rs`)

These are provider validation tests that enforce the two deserialization contract requirements.
All providers MUST pass these.

39. **`fetch_corrupted_history_filtered_vs_unfiltered`**
    - Seed an execution with deliberately undeserializable event data in its history.
    - Set the execution's pinned version to one that the filter excludes.
    - **Part A (filtered):** Fetch with the excluding filter.
      Assert `Ok(None)` — no error raised, no deserialization attempted.
      **Validates:** Requirement 1 — filter is applied before history loading.
    - **Part B (unfiltered):** Fetch with `filter=None`.
      Assert the fetch returns `Err(ProviderError)` where `e.is_retryable() == false` (permanent).
      **Validates:** Requirement 2 — deserialization errors produce permanent errors, not silent drops.

41. **`fetch_deserialization_error_increments_attempt_count`**
    - Seed an execution with deliberately undeserializable history events.
    - Fetch with `filter=None` → returns `Err(ProviderError::permanent(...))`.
    - Wait for lock to expire, fetch again with `filter=None` → same error.
    - On a third fetch, verify `attempt_count` has incremented on each cycle.
    - **Validates:** Requirement 2 — `attempt_count` is incremented before history loading,
      so the poison path can eventually terminate the orchestration.

42. **`fetch_deserialization_error_eventually_reaches_poison`**
    - End-to-end test: seed an execution with undeserializable history, start a runtime
      with `filter=None` and `max_attempts: 3`.
    - Assert that after enough fetch cycles, the orchestration is terminated with a
      poison/configuration error.
    - **Validates:** The full deserialization-error → attempt_count → poison pipeline works.

#### Future tests (not phase 1)

- Provider filters by orchestration handler name/version
- Worker queue filters by registered activity handlers
- Sweeper maintenance loop terminates permanently-stuck items
- Multi-range filter (disjoint ranges like `[>=1.0.0,<2.0.0] + [>=3.0.0,<4.0.0]`)

## Open questions

- Exact filter representation: string semver ranges vs structured min/max.
- How to represent handler capability efficiently (potentially a bounded list).
- Whether capability filtering applies only to the orchestrator queue, or also to worker completions.
- How to represent activity capability efficiently without hot-path overhead.
- Whether the provider should prioritize “most compatible” vs “first visible” (likely first visible for simplicity).
