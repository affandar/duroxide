# Activity Sessions (Explicit Lifecycle, App-Managed State) — v3

**Status:** Proposal (v3)  
**Created:** 2026-02-14  
**Updated:** 2026-02-15

> **Read order note:** This document is an add-on/revision to `docs/proposals/activity-explicit-sessions.md` (v2). Read v2 first for the full narrative and examples, then apply v3 as the implementation-ready corrections and source of truth where they differ.

## Summary

Add first-class **activity sessions** to duroxide that provide worker-process affinity for groups of activities.

Sessions are:
- Explicitly controlled by orchestration code (`open_session`, `close_session`)
- Pure affinity/routing (not ordering)
- Re-claimable after crash/shutdown
- Backed by provider lock rows
- Application-managed for in-memory state hydration/checkpoint/dehydration

This v3 resolves API/runtime mismatches identified in v2 review and aligns to current duroxide architecture.

---

## Design Goals

1. **Strong affinity** for session-bound activities while worker is healthy.
2. **Explicit lifecycle** controlled by orchestration.
3. **Deterministic replay compatibility** with existing command/history model.
4. **Safe mixed-version behavior** (no silent semantic corruption).
5. **Minimal surface area for MVP** (Phase 1) with clear extension points.

## Non-Goals (Phase 1)

- No runtime-managed session state blob
- No implicit ordering among session activities
- No automatic runtime-level recovery policy/interception
- No cross-instance session sharing

---

## Key Corrections from v2

1. **API shape fixed for current scheduling model**
   - Existing `schedule_activity(...)` emits `Action` immediately.
   - Therefore `.on_session()` chained after `schedule_activity()` is not compatible without deep refactor.
   - v3 introduces explicit session-bound scheduling methods.

2. **ContinueAsNew semantics made consistent**
   - Sessions are instance-scoped and survive ContinueAsNew.
   - Terminal cleanup closes sessions for `Completed`/`Failed`/`Cancelled`, **not** for `ContinuedAsNew`.

3. **`close_session` semantics made consistent**
   - Idempotent and no-op if unknown/already closed (provider + replay semantics aligned).

4. **Error classification unified**
   - Provider lacking session capability is an application failure (`AppErrorKind::OrchestrationFailed`), not nondeterminism.

5. **Session identity/key fixed**
   - Session row keyed by `(instance_id, session_id)` to enforce instance scope and avoid global ID collisions.

6. **Worker ID uniqueness updated**
    - Keep existing `runtime_id` format as **4-char hex**.

---

## Orchestration API (v3)

### Required methods

```rust
impl OrchestrationContext {
    pub async fn open_session(&self) -> Result<String, String>;
    pub async fn open_session_with_id(&self, session_id: impl Into<String>) -> Result<String, String>;
    pub async fn close_session(&self, session_id: &str) -> Result<(), String>;

    pub fn schedule_activity_on_session(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        session_id: &str,
    ) -> DurableFuture<Result<String, String>>;

    pub fn schedule_activity_on_session_typed<In: serde::Serialize, Out: serde::de::DeserializeOwned>(
        &self,
        name: impl Into<String>,
        input: &In,
        session_id: &str,
    ) -> impl Future<Output = Result<Out, String>>;
}
```

```rust
// Open generated session id
let session_id = ctx.open_session().await?;

// Open caller-provided session id (idempotent)
let session_id = ctx.open_session_with_id("copilot-main").await?;

// Session-bound activity scheduling (explicit API)
let output = ctx
    .schedule_activity_on_session("run_turn", input, &session_id)
    .await?;

// Optional typed variant
let output: TurnResult = ctx
    .schedule_activity_on_session_typed("run_turn", &typed_input, &session_id)
    .await?;

// Close session (idempotent)
ctx.close_session(&session_id).await?;
```

### Why explicit `schedule_activity_on_session`?

This matches current duroxide internals where action emission happens at schedule call time and avoids a broad `DurableFuture` builder refactor in MVP.

### API parity note

`schedule_activity_on_session_typed` must preserve the same JSON codec semantics and error behavior as existing `schedule_activity_typed`:
- Serialize input using the same codec path
- Deserialize output using the same codec path
- Return decode failures as `Err(String)` with deterministic behavior
- Introduce no session-specific serialization differences

---

## Activity API (v3)

```rust
async fn run_turn(ctx: ActivityContext, input: String) -> Result<String, String> {
    let session_id = ctx.session_id().ok_or("missing session_id")?;

    // app-managed self-healing
    let state = LOCAL_CACHE
        .get(session_id)
        .or_else(|| rehydrate_from_checkpoint(session_id))
        .ok_or("session_lost: cannot rehydrate")?;

    state.process(input).await
}
```

`ActivityContext` additions:
- `session_id() -> Option<&str>`

---

## RuntimeOptions (v3)

```rust
RuntimeOptions {
    // Existing fields ...

    // Max sessions this worker may own concurrently
    max_sessions_per_worker: usize, // default: 100

    // DB lease duration; recovery latency bound after crash
    session_lock_duration: Duration, // default: 2 * worker_lock_timeout

    // Voluntary release after inactivity
    session_idle_timeout: Option<Duration>, // default: None (copilot-friendly)

    // Guardrail at orchestration level
    max_sessions_per_orchestration: usize, // default: 10
}
```

---

## Core Semantics

### Session scope and identity

- Session belongs to orchestration `instance_id`.
- Primary identity: `(instance_id, session_id)`.
- `open_session_with_id(id)` is idempotent for the instance.

### Open/close behavior

- `open_session`: create if absent; reopen if closed; idempotent.
- `close_session`: idempotent no-op if absent/closed.

### ContinueAsNew

- Open sessions carry across executions of the same instance.
- On `ContinueAsNew`, runtime preserves open session set.
- Orchestration code carries session IDs in input for business logic use.
- No mandatory re-open call in new execution.

### Terminal cleanup

- On `Completed` / `Failed` / `Cancelled`: close all open sessions for that instance.
- On `ContinuedAsNew`: **do not** close sessions.

---

## Provider Contract (v3)

### Capability

```rust
fn supports_sessions(&self) -> bool { false }
```

If an orchestration executes session API on unsupported provider:
- Fail orchestration with application error (`OrchestrationFailed`, retryable=false).

### Work fetch split

```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;

async fn fetch_session_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
    worker_id: &str,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

### Session lock methods

```rust
async fn renew_session_lock(
    &self,
    instance_id: &str,
    session_id: &str,
    worker_id: &str,
    extend_for: Duration,
) -> Result<(), ProviderError>;

async fn release_session_lock(
    &self,
    instance_id: &str,
    session_id: &str,
    worker_id: &str,
) -> Result<(), ProviderError>;
```

### Important behavior

- `fetch_work_item` returns only non-session work.
- `fetch_session_work_item` may return non-session work and claimable/owned session work.
- `renew_work_item_lock` piggybacks session extension for session-bound items.

---

## Data Model (v3)

### New table: `sessions`

```sql
CREATE TABLE sessions (
    instance_id   TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    worker_id     TEXT,
    locked_until  INTEGER,
    PRIMARY KEY (instance_id, session_id)
);

CREATE INDEX idx_sessions_worker ON sessions(worker_id);
CREATE INDEX idx_sessions_locked_until ON sessions(locked_until);
```

### Worker queue

```sql
ALTER TABLE worker_queue ADD COLUMN session_id TEXT;
CREATE INDEX idx_worker_queue_session ON worker_queue(session_id);
```

---

## Event/Action/WorkItem changes (v3)

### EventKind additions

```rust
SessionOpened { session_id: String }
SessionClosed { session_id: String }
```

### Action additions

```rust
OpenSession  { scheduling_event_id: u64, session_id: String }
CloseSession { scheduling_event_id: u64, session_id: String }
CallActivity {
  scheduling_event_id: u64,
  name: String,
  input: String,
  session_id: Option<String>,
}
```

### WorkItem additions

```rust
ActivityExecute {
  instance: String,
  execution_id: u64,
  id: u64,
  name: String,
  input: String,
  #[serde(default)]
  #[serde(skip_serializing_if = "Option::is_none")]
  session_id: Option<String>,
}
```

---

## Replay and Validation Rules (v3)

- Replay engine tracks open sessions set per instance execution context.
- `schedule_activity_on_session(..., id)` with unknown/closed session => application failure.
- Exceeding `max_sessions_per_orchestration` => application failure.
- Session open/close action ordering mismatches history => nondeterminism.
- `close_session` called on unknown session is allowed and deterministic (records action/event; provider no-op).

---

## Worker Dispatcher Semantics (v3)

1. Maintain owned sessions map and per-session renewal tasks.
2. At/over session capacity:
   - Keep processing non-session work.
   - Keep processing already-owned session work.
   - Do not claim new sessions.
3. Graceful shutdown:
   - Release owned session locks explicitly.
   - Stop renewal tasks.

---

## Mixed-Version Safety (v3)

Phase 1 rollout requirement:
- Session functionality is enabled only when all active workers in deployment are session-capable **or** provider-level compatibility gate prevents legacy workers from fetching session-bound items.

Minimum guarantee:
- Legacy workers must not execute session-bound `ActivityExecute` silently.

Practical path:
- Add provider capability-aware fetch routing and optional queue filtering by worker capability.
- Document rolling upgrade order explicitly in migration guide.

---

## Runtime ID Uniqueness (v3)

Keep current runtime ID generation as **4-char hex**.

- Format: 4 lowercase hex chars.
- Entropy: 16-bit value derived from startup timestamp.
- Purpose: lightweight per-runtime worker ID suffix.

Example:

```rust
let runtime_id = format!("{:04x}", nanos_low_16); // e.g. "7a3f"
```

---

## Test Plan (v3, prioritized)

### P0 (must pass for MVP)

1. Session lifecycle: open/open_with_id/close idempotency.
2. Routing affinity: same session routes to same worker across turns.
3. Crash reclaim: worker A crash => worker B claims after lock expiry.
4. Graceful shutdown release: immediate re-claim by other worker.
5. ContinueAsNew carry: sessions remain usable after CAN without reopen.
6. Terminal cleanup: complete/fail/cancel close sessions; CAN does not.
7. Capability failure: provider without sessions fails orchestration cleanly.
8. Serialization compatibility (`session_id` optional serde behavior).
9. Typed API success path: `schedule_activity_on_session_typed` serializes input and deserializes output correctly.
10. Typed API decode failure: invalid output payload returns deterministic decode error.

### P1 (important)

1. Capacity behavior (owned sessions still processed at capacity).
2. close during in-flight activity cancels via existing mechanism.
3. Late completion after close ignored safely.
4. Multi-session interleaving with select/join.
5. Single-thread runtime (`current_thread`) renewal behavior.

### P2 (hardening)

1. Mixed-version cluster compatibility scenarios.
2. High-churn open/close cycles (leak detection).
3. Session migration rate under stress.

---

## Open Questions (v3)

1. Default `session_idle_timeout`: `None` (affinity-first) vs bounded idle release.
2. Composition with future activity tags/pools (`on_session` equivalent API layering).
3. Whether to expose session ownership observability metrics in MVP or Phase 2.

---

## Implementation Plan (v3)

### Phase 1 (MVP)

1. Add session API methods on `OrchestrationContext` and `ActivityContext`.
2. Add session events/actions/work item optional field.
3. Add provider trait methods + sqlite implementation + migrations.
4. Add worker session renewal/release tasks.
5. Add replay session validation and CAN carry semantics.
6. Add provider validation + scenario tests (P0/P1).

### Phase 2

1. Runtime-level recovery policy convenience (`SessionLost` strategy).
2. Observability expansions (ownership, migrations, renewal failures).

### Phase 3

1. Stress and mixed-version hardening.
2. Documentation and migration playbooks.
