# Worker Sessions for Orchestrations (Stateful, Movable, App-Managed)

**Status:** Proposal  
**Created:** 2026-02-15  
**Author:** AI Assistant

## Summary

Add first-class **worker sessions** to orchestrations for long-running stateful operations (for example, loaded caches/models for inferencing).

Session lifecycle is controlled by orchestration code:

- `ctx.start_session(...)`
- `ctx.end_session(...)`

Sessions:

- are scoped to an orchestration **instance** (not a single execution),
- can span multiple executions (`ContinueAsNew`),
- can include many activities,
- can be attached to one worker at a time and moved after worker failure,
- do **not** require infrastructure-managed session state machine for app data.

The infrastructure only manages session identity + attachment/lease.  
Application orchestration/activity code manages actual app session state (rehydrate/rebuild/transfer).

---

## Goals

1. Support worker-local warm state across multiple activities in one orchestration instance.
2. Preserve existing orchestration durability semantics.
3. Support worker failure with session mobility (lazy re-attach on other worker).
4. Keep user code ergonomic via infrastructure-level recovery interception.

## Non-Goals

1. Infrastructure-managed durable session data model.
2. Strong exactly-once guarantees for arbitrary side effects inside activity code.
3. Session idle timeout auto-expiry (session ends only via orchestration call).

---

## API Shape

### Orchestration-side

```rust
let session = ctx.start_session(StartSessionOptions::default()).await?;

let a = ctx.schedule_activity("Infer", req_a)
    .in_session(&session)
    .into_activity();
let b = ctx.schedule_activity("Infer", req_b)
    .in_session(&session)
    .into_activity();

let (ra, rb) = ctx.join2(a, b).await?;

ctx.end_session(&session).await?;
```

### Activity-side

```rust
async fn infer(ctx: ActivityContext, input: String) -> Result<String, ActivityError> {
    let session = ctx.session().expect("session-bound activity");

    // Application-owned state lookup.
    // If missing on this worker (cold attachment), signal typed error.
    let local_state = get_local_state(&session.session_id)
        .ok_or_else(|| ActivityError::session_lost("state missing on attached worker"))?;

    Ok(run_inference(local_state, input))
}
```

---

## Core Semantics

### Session identity and scope

- `session_id` is deterministic and durable in orchestration history.
- Session belongs to orchestration **instance_id**.
- Session remains valid across `ContinueAsNew` executions of that instance.

### Attachment and lease

- Session attachment is lazy (first session-bound activity fetch/execute path).
- Competing consumers: workers race to attach when needed.
- One active attachment at a time.
- Attachment lease is renewed by active worker; lease loss allows another worker to attach.

### EndSession behavior

`ctx.end_session(...)` takes effect at orchestration ack transaction commit:

1. Persist `SessionEnded` event.
2. Mark session as ended in provider control-plane storage.
3. Cancel in-flight session activities via existing cancellation/lock-steal mechanism.
4. Ignore late completions from ended session work.

This matches current behavior for dropped/cancelled paths and stale completions.

---

## Simplicity Decision: No Infrastructure Session-Reset State Machine

To keep v1 simple:

- Infrastructure does not track a separate "session reset required" state.
- Activity code attempts to load local session state.
- Missing local state returns typed `SessionLost`.
- Recovery is handled by infrastructure interception policy (below), not repeated orchestration boilerplate.

---

## Infrastructure-Level Recovery Interception

To avoid littering orchestration code with repeated "if SessionLost then rebuild then retry" checks:

Define explicit recovery policy for selected activities:

```rust
SessionRecoveryPolicy {
    on_error: SessionErrorKind::Lost,
    rebuild_activity: "RebuildSession".to_string(),
    max_auto_retries: 1,
    singleflight_per_session: true,
}
```

Behavior:

1. Execute original activity.
2. If activity returns typed `SessionLost`:
   - run configured rebuild activity in same session context,
   - retry original activity once.
3. Return normal success/failure completion.

Safety constraints:

- Rebuild activity itself must not recursively trigger same interception.
- Rebuild should be idempotent.
- Singleflight on worker avoids redundant concurrent rebuild storms for same session.

---

## Exactly-Once Discussion

Strict exactly-once execution cannot be guaranteed across crash/retry boundaries for arbitrary side effects.

Practical guarantee in this model:

- **At-least-once** rebuild execution across failures/retries.
- **Effectively-once** behavior when rebuild is idempotent and side effects are guarded.
- Deterministic orchestration history and stale completion filtering remain intact.

This aligns with existing activity reliability semantics.

---

## Edge Cases and Races

### 1) Concurrent session activities all cold

- Multiple concurrent calls return `SessionLost`.
- Singleflight rebuild ensures one rebuild per session attachment on a worker.
- Waiting calls retry after rebuild.

### 2) Worker dies during rebuild

- Rebuild may rerun on another worker after lease loss/redelivery.
- Idempotent rebuild required.

### 3) Old worker finishes late after lease loss

- Late ack/completion is rejected or ignored as stale.
- No cross-attachment corruption should be accepted into active flow.

### 4) EndSession races with in-flight work

- EndSession commit closes session and triggers cancellation.
- Late completions are ignored.

### 5) Rebuild policy loop

- If rebuild fails with `SessionLost` or other errors, fail activity path.
- No recursive auto-recovery loop for rebuild activity.

### 6) Runtime cancellation or shutdown during recovery flow

- Existing cancellation token behavior applies to both rebuild and retried activity.

---

## Orthogonality With Existing Features

### ContinueAsNew

- Session is instance-scoped, so it can continue across executions.
- Execution-id filtering for completions remains unchanged.
- Session handle must be carried in orchestration state/input as needed by user code.

### Versioning / Capability Filtering

- Session-related fields/events/work-item metadata should be backward-compatible and version-gated.
- Older runtimes must not process session-bound semantics they do not understand.

### External Events

- External events remain orthogonal.
- They may trigger turns that schedule session-bound activities; recovery interception handles cold state.

### Timers

- Timer behavior unchanged.
- Timer-triggered turns may schedule session activities and follow same recovery interception path.

### Async blocks / join / select

- Semantics unchanged.
- Higher concurrency can increase simultaneous cold-state errors; singleflight is important.

### Sub-orchestrations

- Default: session handles are instance-local and not implicitly shared with child orchestrations.
- Sharing semantics, if needed later, should be explicit and separately specified.

### Cancellation, poison handling, retries

- Existing mechanisms remain primary.
- Recovery interception runs within normal activity execution boundaries and retry rules.

---

## Configuration

Add runtime-level guardrail:

- `max_attached_sessions_per_worker: usize`

When at cap:

- worker may continue processing already-attached sessions,
- worker should not attach new sessions until capacity frees.

No idle timeout:

- sessions are ended explicitly by orchestration only.

---

## Suggested Minimal Implementation Phases

### Phase 1: Session primitives

1. Add session start/end APIs and history events.
2. Add session metadata to session-bound activity scheduling.
3. Add provider session control-plane table (attachment + lease + ended state).
4. Add lazy attach + competing consumer behavior.

### Phase 2: Recovery interception

1. Add typed `SessionLost` activity error kind.
2. Add explicit recovery policy registration.
3. Add auto rebuild-and-retry path with singleflight.
4. Add observability metrics.

### Phase 3: Hardening

1. Add max sessions per worker enforcement.
2. Expand race-condition tests and stress tests.
3. Validate compatibility in mixed-version deployments.

---

## Test Matrix (High Priority)

1. Session survives ContinueAsNew, remains usable.
2. EndSession cancels in-flight activities and ignores late completions.
3. Worker death + lazy reattach + rebuild + retry succeeds.
4. Concurrent cold hits trigger one rebuild (singleflight).
5. Rebuild activity excluded from recursive interception.
6. Max sessions per worker limit enforced without starvation regressions.
7. Timers/external events/async joins with session activities preserve deterministic behavior.
8. Mixed-version nodes do not process unsupported session semantics.

