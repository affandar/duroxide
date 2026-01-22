# Coverage + Prioritized Test Plan (nextest suite)

This document captures:

1. The current coverage snapshot for the test suite run by `cargo nextest` (aka `cargo nt`)
2. A prioritized test plan to improve coverage in the weakest areas

> Notes
> - Coverage was measured with `cargo-llvm-cov` + nextest.
> - Coverage is a *signal*, not a goal; the plan below focuses on risk/importance first, then easy wins.

## How coverage was measured

Commands used:

- Full workspace (includes `sqlite-stress` bins, which skew the “worst file” list with 0% binaries):
  - `cargo llvm-cov nextest --workspace --all-features --summary-only`

- Main crate only (recommended for “where are we weak in the product?”):
  - `cargo llvm-cov nextest -p duroxide --all-features --summary-only`

## Coverage snapshot (crate: duroxide)

From `cargo llvm-cov nextest -p duroxide --all-features --summary-only`:

- **Lines:** 90.68%
- **Regions:** 86.67%
- **Functions:** 78.56%

Interpretation:

- Line/region coverage is solid overall.
- Function coverage is the lagging indicator, which usually means “lots of small helpers / variants never executed,” often in:
  - provider error/management surfaces
  - instrumentation wrappers
  - edge-case control flow

### Lowest-covered files (most relevant)

These are the weakest files by coverage in the `duroxide` crate run (excluding `sqlite-stress` binaries):

| File | Lines | Functions | Why this matters |
|---|---:|---:|---|
| `src/providers/management.rs` | 28.00% | 18.18% | Management API surface; easy to regress and often untested |
| `src/provider_validation/long_polling.rs` | 52.60% | 60.00% | Long-poll semantics are subtle; correctness depends on timing/ordering |
| `src/providers/mod.rs` | 53.85% | 60.00% | Provider trait wiring/glue; low risk but can hide “forgot to call X” |
| `src/providers/instrumented.rs` | 65.62% | 57.89% | Observability wrapper; failures here are painful to debug |
| `src/providers/error.rs` | 72.73% | 71.43% | Retryable vs non-retryable classification correctness |
| `src/providers/sqlite.rs` | 74.09% | 49.79% | Large surface; remaining gaps usually error/edge branches |
| `src/runtime/observability.rs` | 74.74% | 65.52% | Metrics/tracing correctness; important for ops confidence |

### Replay-engine coverage status

`src/runtime/replay_engine.rs` is **not** a weak spot:

- Lines: 91.72%
- Functions: 93.48%

So the new replay-engine test suite is pulling its weight; the biggest opportunities are around provider/management/observability surfaces.

## Prioritized test plan

### Priority 0 (highest): Management provider surface

**Target:** `src/providers/management.rs` (28% lines / 18% functions)

**Goal:** Execute every public management method across *both* sqlite and in-memory providers (where supported), including error cases.

Proposed tests (add to existing management test suite, likely `tests/management_interface_test.rs`):

1. **Per-method smoke coverage**
   - For each management API method:
     - create instance
     - run to completion (or fail)
     - call management method and assert response shape
   - Ensure methods are exercised for statuses: Running / Completed / Failed / Cancelled / ContinuedAsNew.

2. **Error classification / validation**
   - Call management APIs with:
     - unknown instance id
     - invalid execution id
     - deleted instance
   - Assert stable error behavior (and/or `ErrorDetails` classification when applicable).

3. **Tree + multi-execution correctness**
   - Start parent orchestration with child orchestrations; verify:
     - `get_instance_tree()` correctness
     - latest execution detection
     - per-execution history visibility

Why P0:

- Management endpoints are user-facing diagnostics; if these regress, users lose visibility even when the runtime is otherwise healthy.

### Priority 1: Provider long-poll semantics

**Target:** `src/provider_validation/long_polling.rs` (52.60% lines)

**Goal:** Cover edge timing paths and ensure invariants hold for long-poll behavior.

Proposed tests:

1. **“Returns early on work” variants**
   - Work arrives:
     - immediately
     - after a short delay
     - near timeout boundary
   - Assert: returns before timeout and returns expected work.

2. **“Waits for timeout” variants**
   - No work present.
   - Assert: does not return early (allow small jitter), returns empty.

3. **Cancellation/terminal interaction**
   - Orchestration becomes terminal while a long poll is waiting.
   - Assert: long poll returns in a bounded time and does not return stale work.

Why P1:

- Long polling is a major correctness/perf surface, and it’s easy to introduce subtle issues during refactors.

### Priority 2: Instrumentation wrapper correctness

**Target:** `src/providers/instrumented.rs` (65.62% lines)

**Goal:** Ensure wrapper does not alter semantics, and that it records observability for success/error paths.

Proposed tests:

1. **Semantic equivalence checks**
   - Wrap a provider with the instrumented provider.
   - Run the same small workflow on both and compare:
     - final orchestration status
     - history contents
     - work-queue behavior (basic invariants)

2. **Error-path instrumentation**
   - Force provider errors using existing fault-injection patterns (if present) or by triggering known error branches.
   - Assert:
     - errors propagate unchanged
     - expected spans/metrics are emitted (or at least that the code path executes without panics).

Why P2:

- Instrumentation bugs are operationally expensive; also tends to be low-effort to cover with targeted tests.

### Priority 3: ProviderError classification + conversions

**Target:** `src/providers/error.rs` (72.73% lines)

**Goal:** Exercise each error variant and ensure retryability flags/classification are correct.

Proposed tests:

- Unit tests for:
  - constructing each `ProviderError` variant
  - mapping/conversion paths used by runtime
  - retryable vs non-retryable behavior (as exposed)

Why P3:

- Misclassification can cause infinite retries or dropped work.

### Priority 4: SQLite provider edge/error branches

**Target:** `src/providers/sqlite.rs` (74.09% lines / 49.79% functions)

**Goal:** Cover the “rare” branches: corruption, deleted rows mid-operation, lock-token mismatch, idempotent cancellations, etc.

Proposed tests (prefer to extend existing suites):

1. **Corruption/invalid serialization cases**
   - Attempt to fetch/ack with corrupted payloads (if test hooks exist).

2. **Concurrent-ish edge cases** (simulated)
   - Delete an item between fetch and ack.
   - Ack with wrong token.
   - Renew after expiration.
   - Assert behavior matches provider contract.

3. **Batch cancellation coverage**
   - Ensure cancellation of many activity IDs in one call is covered, including:
     - duplicates
     - non-existent IDs

Why P4:

- SQLite provider is the reference implementation; edge correctness matters, but many cases are already validated (coverage is decent).

### Priority 5: Runtime observability edge cases

**Target:** `src/runtime/observability.rs` (74.74% lines)

**Goal:** Cover:

- error classification metrics
- poison-message metrics
- gauge init edge cases
- paths that run only under unusual outcomes

Proposed tests:

1. **Outcome matrix**
   - Run orchestrations that:
     - complete
     - fail
     - continue-as-new
     - cancel
   - Assert expected counters and duration metrics change.

2. **Fault injection**
   - Trigger provider error paths and assert infrastructure/configuration/application classification counters are updated.

Why P5:

- Useful for ops and confidence; lower priority than correctness surfaces.

## Implementation notes (how to execute the plan)

- Prefer adding tests to existing integration suites rather than creating new harnesses.
- Use nextest filters for fast iteration:
  - `cargo nt -E 'test(/management_interface/)'`
  - `cargo nt -E 'test(/long_poll/)'`
  - `cargo nt -E 'test(/observability/)'`
- When adding time-sensitive tests, allow jitter and use generous bounds to avoid flakiness.

## Suggested “first batch” (1–2 hours)

If you want a quick, high ROI first pass:

1. Add management API smoke tests to cover each management method once on sqlite.
2. Add 2–3 long-poll edge tests around the timeout boundary.
3. Add 1 instrumentation equivalence test that wraps sqlite provider and runs a tiny workflow.
