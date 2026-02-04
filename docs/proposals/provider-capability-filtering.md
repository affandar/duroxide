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
- This value is immutable for the execution boundary.

### Capability filter

A runtime node advertises (phase 1):

1. **Replay engine capability**
   - `supported_pinned_versions: Vec<SemverRange>`
   - Example: `[">=0.1.0,<0.2.0"]`

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

Filtering introduces a new risk: if *no* node in the cluster declares compatibility for some execution, then **no one will ever fetch it**, so runtime-side `attempt_count`-based poison termination will never trigger.

We need an explicit strategy for this.

### Option A (recommended): A “sweeper” runtime path

Introduce a maintenance mode that periodically fetches without capability filtering:

- `fetch_orchestration_item(..., filter=None)`

It does not attempt normal execution. It only:

- Terminates items that have been unprocessable for too long (time-based TTL), OR
- Converts them into a terminal failure with an explicit message, OR
- Enqueues a `CancelInstance` and then applies cancellation-only logic.

This keeps normal dispatching simple while ensuring liveness/cleanup.

### Option B: Provider-level deadletter policy

Provider keeps track of “blocked by capability” duration/counters per instance and eventually:

- Moves the item to a deadletter store, OR
- Marks the execution Failed with a Poison/Configuration error.

This pushes more policy into the provider and is harder to keep consistent across providers.

## Rolling upgrades (desired behavior)

- Old nodes declare older `supported_pinned_versions` and a limited set of handler versions.
- New nodes declare newer ranges/handlers.
- Provider naturally routes items to the correct nodes based on capabilities.
- No bouncing/abandon loops required for compatibility.

## Observability

- Provider should expose counters for:
   - “items filtered out due to unsupported pinned version”
   - (future) “items filtered out due to unsupported orchestration handler/version”
   - (future) “worker items filtered out due to unknown activity handler”
- Runtime should log capability declarations at startup.

## Test plan (phase 1)

### Where tests should live

- Provider filtering behavior is best validated in SQLite-focused integration tests and scenario tests:
  - `tests/scenarios/rolling_deployment.rs` (extend) or a new `tests/scenarios/capability_filtering.rs`
   - Provider validation suite can add a targeted validation for filtered fetch if we decide it’s part of the Provider trait contract.

Keep tests for unregistered orchestration handlers (today’s bounce behavior) in existing places; they are
orthogonal to phase 1 replay-engine filtering.

### Tests

1. **Provider filters by pinned version**
   - Seed two instances with different execution pinned versions.
   - Start two runtimes with different supported pinned-version ranges.
   - Assert each runtime only processes the compatible instance.

2. **No compatible nodes → sweeper cleanup**
   - With no compatible runtimes running, assert that the sweeper path eventually terminates or cancels stuck instances, and that the terminal error message is explicit.

Future tests (when orchestration/activity capability filtering is implemented):

- Provider filters by orchestration handler version
- Worker queue filters by registered activity handlers

## Open questions

- Exact filter representation: string semver ranges vs structured min/max.
- How to represent handler capability efficiently (potentially a bounded list).
- Whether capability filtering applies only to the orchestrator queue, or also to worker completions.
- How to represent activity capability efficiently without hot-path overhead.
- Whether the provider should prioritize “most compatible” vs “first visible” (likely first visible for simplicity).
