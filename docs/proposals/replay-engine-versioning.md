# Replay Engine Versioning Proposal (Stable Event Envelope, Append-Only EventKind)

**Date:** 2026-02-03  
**Status:** Pending (re-evaluating approach)

> Note (2026-02-04): This proposal ended up getting too complex for the first iteration
> (especially around cluster behavior when no compatible node exists). We are exploring
> an alternative strategy where the provider filters work based on runtime capabilities
> (supported replay-engine semver ranges and supported orchestration handler versions),
> so incompatible executions are never fetched by a node in the first place.

## Problem

Duroxide needs a way to evolve the replay engine and history event schema over time while:

- Preserving the ability to continue running and replaying **older, long-running orchestrations**.
- Supporting **rolling upgrades** (mixed-version clusters).
- Avoiding provider-specific orchestration knowledge.
- Keeping the persisted `Event` envelope stable for providers and management tooling.

The specific change vectors we care about:

- Breaking changes to replay behavior / determinism rules.
- Changes to history event payloads (`EventKind`): add/remove/modify.

## Goals

1. **Immutable Event envelope**
   - The persisted `Event` struct’s *envelope* (top-level fields and meaning) must not change.

2. **Breaking-change-friendly engine evolution**
   - Changes that would break replay determinism must be isolatable via replay engine versioning.

3. **Rolling upgrade compatibility**
   - New binaries must safely process work for instances started by older binaries.

4. **Operational visibility**
   - Provide a management API to measure the version spread of **non-terminal** orchestrations.

## Non-goals (for now)

- No raw/unknown history decoding fallback.
- No “generic unknown EventKind” support.
- No history migration/rewrite tooling.

## Core Ideas

### 1) Stable `Event` envelope

The `Event` envelope is treated as a stable storage contract, e.g.:

- `event_id: u64`
- `source_event_id: Option<u64>`
- `instance_id: String`
- `execution_id: u64`
- `timestamp_ms: u64`
- `duroxide_version: String` (crate semver of the runtime that produced the event)

The `Event.kind` field is `#[serde(flatten)]` and uses `#[serde(tag = "type")]` for discrimination.

### 2) `EventKind` is **append-only**

We do **not** remove old variants from the persisted schema.

Instead:

- **Add**: introduce new variants freely.
- **Modify**: represent breaking “edits” as new variants (e.g., `ExternalSubscribed2`, `ExternalEvent2`).
- **Remove**: stop emitting the old variant in new executions, but keep it supported for replay.

This keeps provider deserialization (e.g., JSON → `Event`) stable and avoids needing generic decoding.

### 3) ReplayEngine compatibility gate (v1)

For the first iteration of this spec, we do **not** embed multiple replay engine generations.
Instead, each runtime node declares a single supported semantic version range for the replay
engine it can run. If an execution is pinned to a version outside that range, the node will
**bounce** the work item (abandon with backoff) so that an older/newer node can process it.

#### Selection key (pinned version)

For a given **execution**, the pinned version is derived from the execution’s first `OrchestrationStarted` event’s `Event.duroxide_version`.

This value is stable for the execution boundary.

#### Compatibility rule

Each runtime node is configured with a supported pinned-version range, e.g.:

- Supported pinned versions: `>= 0.1.4, < 0.2.0`

When the runtime fetches an orchestration item, it determines the pinned version from the
current execution's first `OrchestrationStarted` event. If the pinned version is **not** within
the node's supported range, the node treats the item as **temporarily unavailable to process**
and bounces it (abandon with backoff).

#### Behavior under rolling upgrades

- During rolling upgrades, work for some instances may land on nodes that cannot process it
   (either because the orchestration handler is not registered on that node, or because the
   execution history is pinned to an unsupported `duroxide_version`).
- In both cases, the correct behavior is to **bounce** the item with exponential backoff so it can
   be picked up by a node that can process it.
- If no node can process it, the item will eventually exceed `max_attempts` and be terminated by
   the runtime's poison-message handling.

### 4) Unified “bounce then poison” handling

This spec intentionally unifies the operational behavior for two classes of “cannot process on
this node” failures:

1. **Orchestration handler not registered** on this node.
2. **Execution history cannot be processed** by this node (e.g., pinned `duroxide_version` is
    outside the supported replay engine range).

Both conditions:

- Cause the orchestration work item to be **abandoned with exponential backoff** (bounce).
- If the item continues bouncing until `attempt_count > max_attempts`, the runtime terminates it
   via poison-message handling.

**Poison message wording:** the poison/cancellation message should use a more general phrasing,
such as:

> "work item could not be processed on this node (orchestration not registered here, or execution history is unsupported)"

## What counts as a breaking replay-engine change?

A change requires a new engine generation if it could alter determinism or interpretation of history, for example:

- Reordering or changing correlation semantics between scheduling/completion events.
- Changing how timers, selects, joins, or external events are matched.
- Changing the meaning of an existing `EventKind` variant.
- Introducing a new `EventKind` family that changes semantics (e.g. v2 subscription model).

Purely additive changes that do not affect replay decisions *may* not require an engine generation, but this proposal allows choosing to version anyway for operational clarity.

## Process: Adding a Breaking Change

1. **Decide the cutover crate version**
   - Choose a crate semver cutoff where the new engine becomes active (e.g. `0.1.4`).

2. **Decide the supported pinned-version range**
   - Choose the semver range that this runtime release can process.

3. **Add new `EventKind` variants (if needed)**
   - Do not modify existing variants in a breaking way.
   - Add new variants (e.g., `ExternalSubscribed2`/`ExternalEvent2`).

4. **Deploy (rolling upgrade)**
   - Nodes that cannot process a given execution (handler missing or pinned version unsupported)
     will bounce the item until it reaches a compatible node.

5. **Poison termination when no compatible nodes exist**
   - If no node in the cluster can process the execution, it will eventually be terminated by
     poison-message handling once `max_attempts` is exceeded.

## Operational Visibility: Version Spread of Running Instances

To decide when older engine generations are no longer needed, we need to measure how many **running** instances are pinned to older runtime versions.

### Management API

Provide a management helper:

- `ProviderAdmin::get_running_duroxide_version_spread() -> BTreeMap<String, u64>`

Semantics:

- Consider only instances whose **current execution** status is `Running`.
- For each instance, determine the pinned version from the current execution’s first `OrchestrationStarted` event’s `Event.duroxide_version`.
- Return a `version -> count` map.

### Usage

- Track rollout progress after deploying a new runtime version.
- Gate removal of older replay-engine generations from the codebase.

## Safety Invariants

- Pinned version derivation is deterministic and stable per execution.
- Unsupported pinned versions are treated as **temporarily unavailable to process on this node**
   (bounce), not as a permanent provider error.
- Eventually, if no compatible nodes exist, poison-message handling terminates the execution.
- `EventKind` is append-only; old history must remain decodable.

## Tradeoffs

### Pros

- Simple operational behavior under rolling upgrades (bounce-then-poison).
- Minimal provider complexity (providers can keep treating events as typed JSON).
- Management tooling remains straightforward (typed history reads keep working).
  

### Cons

- `EventKind` grows monotonically (schema accretion).
- Without embedding multiple replay engines, executions pinned outside the supported range will bounce
   until they find a compatible node (or poison).

## Future Enhancements (optional)

- Multiple embedded replay engine generations with semver-range routing.
- Cancel-only fallback engine for unsupported histories (to guarantee cancellation works even when
   replay is unsupported).
- Provider-level optimized version spread queries (avoid per-instance history reads).
- Persist pinned runtime version per execution for fast queries.
- Raw-history management reads for richer long-term inspection without schema accretion.

## Test Plan (v1)

This section defines the test plan for the first iteration of the spec:

- A node supports a single pinned-version semver range for replay compatibility.
- If a fetched execution is pinned to an unsupported `Event.duroxide_version`, the node bounces it
   with the same backoff behavior as an unregistered orchestration.
- If no compatible node exists, the item eventually exceeds `max_attempts` and is terminated by
   poison-message handling with a **generalized message**.

### Test placement strategy

Prefer extending existing test files where the behavior is already exercised:

- Add bounce→poison assertions to **existing** tests in `tests/unregistered_backoff_tests.rs`.
- Add multi-node “bounces until compatible node” scenarios to **existing** scenario suite
   (`tests/scenarios/rolling_deployment.rs`) or a small new scenario file under `tests/scenarios/`.
- Add small pure logic tests (range parsing/matching, pinned-version extraction) to an **existing**
   unit-test oriented file (e.g., `tests/registry_tests.rs` or `tests/runtime_options_test.rs`) depending
   on where the implementation lands.

Avoid introducing new test files unless the existing ones become overly cluttered.

### Unit / logic tests

**File:** `tests/registry_tests.rs` (or another focused unit-test file if the implementation lands elsewhere)

1. **Semver range match**
    - Given a supported range and a pinned version string, match succeeds/fails deterministically.
    - Include boundary coverage: inclusive lower bound, exclusive upper bound (depending on chosen syntax).

2. **Pinned version extraction**
    - From a history vector containing `OrchestrationStarted`, extract pinned runtime version from
       `Event.duroxide_version`.
    - Missing/empty history cases are handled deterministically (either treated as processable or
       treated as unsupported and therefore bounced — whichever the implementation chooses).

3. **Generalized reason formatting**
    - Ensure the string used for the “cannot be processed on this node” path is stable and includes:
       - whether it was handler-unregistered vs history-unsupported (if you choose to include details)
       - attempt_count/max_attempts
       - the pinned version (for history-unsupported)

### Integration tests: bounce → poison for unsupported pinned version

**File:** `tests/unregistered_backoff_tests.rs` (extend)

Add a new test alongside `unknown_orchestration_fails_with_poison`:

1. **unsupported_pinned_runtime_version_bounces_then_poison**
    - Setup a runtime with a supported pinned-version range that does **not** include a chosen
       pinned version (e.g., supported `>= 99.0.0`, pinned `0.1.0`, or vice versa).
    - Seed the store with an orchestration instance whose first `OrchestrationStarted` has
       `Event.duroxide_version = <pinned>` (construct the `Event` manually so the pinned version can
       differ from `env!("CARGO_PKG_VERSION")`).
    - Enqueue a work item for that instance so the orchestrator dispatcher repeatedly fetches it.
    - Assertions:
       - The instance ends in `OrchestrationStatus::Failed { details: ErrorDetails::Poison { .. } }`.
       - `details.display_message()` (or equivalent) contains the **generalized message**:
          “orchestration not registered on this node OR execution history could not be processed by this node”.
       - History contains a terminal failure event for poison.

2. **supported_pinned_runtime_version_processes_normally**
    - Same setup, but with a pinned version inside the supported range.
    - Assert the orchestration completes successfully (sanity check that the gate doesn’t block).

### Scenario tests: multi-node bounce routing

**Preferred file:** `tests/scenarios/rolling_deployment.rs` (extend)

Add a scenario similar in spirit to the existing rolling deployment tests:

1. **e2e_replay_engine_version_bounce_two_nodes**
    - Two runtimes share the same provider.
    - Node A supports pinned-version range `R1`, Node B supports range `R2`.
    - Seed an instance pinned to a version in `R2` only.
    - Assert that:
       - Node A repeatedly abandons (bounce) and does not commit progress.
       - Node B eventually processes the item (i.e., the instance reaches a terminal state), proving
          that bouncing is sufficient for “land on a compatible node”.

If this makes `rolling_deployment.rs` too busy, create a new scenario file:

- `tests/scenarios/replay_engine_versioning.rs`

### Poison-path regression tests

**File:** `tests/poison_message_tests.rs` (extend if needed)

Add a regression that specifically validates the poison output wording for the new generalized
reason and that it is emitted for both:

- unknown/unregistered orchestration handler
- unsupported pinned runtime version

### How to run

- Targeted: `cargo nt -E 'test(/unregistered_backoff/)'`
- Scenarios: `cargo nt -E 'test(/rolling_deployment/)'`
- Full validation before landing: `cargo nt`
