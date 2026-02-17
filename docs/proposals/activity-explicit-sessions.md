# Activity Sessions (Explicit Lifecycle, App-Managed State)

**Status:** Proposal (v2 — refined through design iteration)  
**Created:** 2026-02-14  
**Updated:** 2026-02-14  

> **Revision note:** v3 corrections have been folded into this document. See `docs/proposals/activity-explicit-sessions-v3.md` for the compact correction summary.

## Summary

Add first-class **activity sessions** to duroxide that provide worker-process affinity for groups of activities. Session lifecycle is explicitly controlled by orchestration code via `open_session()` / `close_session()`. The runtime manages session identity, attachment, and lease renewal. Application code manages all in-memory state (hydration, checkpointing, dehydration).

Sessions are a pure **affinity and routing** mechanism. No ordering guarantees, no infrastructure-managed session state, no automatic recovery interception (Phase 1).

## Lifecycle Diagram: Copilot SDK Example

Three nested lifecycles govern a copilot agent conversation:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                             │
│  ORCHESTRATION INSTANCE  (instance_id = "conv-42")                         │
│  ══════════════════════════════════════════════════                          │
│  Maps 1:1 with a logical copilot conversation.                              │
│  Lives from first user message until conversation ends.                     │
│  Survives ContinueAsNew (same instance_id, new execution_id).              │
│  Has NO worker affinity — it's pure orchestration logic.                    │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                                                                       │  │
│  │  SESSION "X"  (open_session → close_session)                          │  │
│  │  ═══════════════════════════════════════════                           │  │
│  │  Maps to a period of INTERACTIVE activity with the Copilot SDK.       │  │
│  │  Active during back-and-forth turns and short waits (<5 min).         │  │
│  │  Closed when a long wait begins (timer >5min, long event wait).       │  │
│  │  Reopened when the trigger fires (timer/event arrives).               │  │
│  │                                                                       │  │
│  │  ┌─────────────────────────────────────┐                              │  │
│  │  │                                     │                              │  │
│  │  │  WORKER ATTACHMENT  (session lock)  │                              │  │
│  │  │  ═══════════════════════════════════ │                              │  │
│  │  │  Worker A claims session on first   │                              │  │
│  │  │  activity fetch. Holds lock via     │                              │  │
│  │  │  background renewal task. Lock      │                              │  │
│  │  │  survives between turns.            │                              │  │
│  │  │                                     │                              │  │
│  │  │  Can transfer to Worker B if:       │                              │  │
│  │  │  • Worker A crashes                 │                              │  │
│  │  │  • Worker A shuts down              │                              │  │
│  │  │  • Session lock timeout expires     │                              │  │
│  │  │    (only if timeout is not None)    │                              │  │
│  │  │                                     │                              │  │
│  │  └─────────────────────────────────────┘                              │  │
│  │                                                                       │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Timeline: A Copilot Conversation

```
time  Orchestration Instance ("conv-42")          Session        Worker
═══════════════════════════════════════════════════════════════════════════

t=0   User sends first message
      open_session() ─────────────────────────▶  Session 1      
      schedule_activity_on_session(hydrate,1) ────────────────▶  Worker A claims
                                                                 CopilotSession created

t=1   schedule_activity_on_session(run_turn(msg1),1) ──────────▶  Worker A executes
      ◀── Ok(response1)

t=5   schedule_wait("user_message")
      ... user typing (30 seconds) ...
      (no items in queue, but Worker A's                         Worker A: session
       session renewal task keeps lock alive)                    renewal task running

t=35  User sends second message
      schedule_activity_on_session(run_turn(msg2),1) ──────────▶  Worker A (same!)
      ◀── Ok(response2)                                          In-memory state warm

t=40  LLM says "wait 3 hours"
      schedule_activity_on_session(checkpoint,1) ─────────────▶  Worker A checkpoints
      schedule_activity_on_session(dehydrate,1) ───────────────▶  Worker A tears down
      close_session(1) ──────────────────────▶  Session 1 ended  Renewal task stops
      schedule_timer(3h)

      ─── 3 hours pass, no process, no memory, no cost ───

t=3h  Timer fires
      open_session() ─────────────────────────▶  Session 2
      schedule_activity_on_session(hydrate,2) ────────────────▶  Worker B claims
                                                                 (A may be gone)
                                                                 CopilotSession from
                                                                 checkpoint

t=3h  schedule_activity_on_session(run_turn("wait complete"),2) ▶ Worker B executes
      ◀── Ok(response3)

      ... more interactive turns on Worker B ...

t=4h  ──── Worker B crashes! ────
                                                                 Renewal task dies
                                                  Session 2      Lock expires after
                                                  lock expires   session_lock_timeout

t=4h  User sends message
      schedule_activity_on_session(run_turn(msg),2) ──────────▶  Worker C claims
      ◀── Err("session_lost")                                    No local state!
      schedule_activity_on_session(hydrate,2) ─────────────────▶  Worker C hydrates
                                                                 from last checkpoint
      schedule_activity_on_session(run_turn(msg),2) ───────────▶  Worker C executes
      ◀── Ok(response)

t=5h  Conversation ends
      schedule_activity_on_session(dehydrate,2) ──────────────▶  Worker C cleans up
      close_session(2) ──────────────────────▶  Session 2 ended
      return Ok("conversation complete")

═══════════════════════════════════════════════════════════════════════════
```

### Key Observations

**Orchestration instance** = the entire conversation lifetime. One instance per conversation. Survives `ContinueAsNew`. Has no worker affinity.

**Session** = a period of interactive activity. Opened when the conversation is active (turns arriving, short waits). Closed when entering a long idle period (timer > threshold, external event expected to take a long time). Multiple sessions can exist sequentially within one orchestration instance. Each session gets a unique ID.

**Worker attachment** = which physical process holds the session. Characterized by the session lock. Stays on the same worker as long as the worker is alive and the session is open. Can transfer on crash, shutdown, or lock expiry.

**Session lost is an app-level concern (Phase 1).** When a session migrates to a new worker, the activity finds no in-memory state. Rather than returning an error to the orchestration, **each activity should attempt to re-hydrate the session itself** — load state from a checkpoint store using the `session_id` as a key. Only if re-hydration fails should the activity return an error. This keeps the orchestration code clean:

```rust
// ACTIVITY-SIDE: each session-bound activity handles cold state internally
"run_turn" => {
    let session_id = ctx.session_id().unwrap();
    
    // Try local cache first, then re-hydrate from checkpoint store
    let state = LOCAL_CACHE.get(session_id)
        .or_else(|| rehydrate_from_store(session_id))  // transparent recovery
        .ok_or("session_lost: cannot rehydrate")?;      // only if store has nothing
    
    state.process(input).await
}

// ORCHESTRATION-SIDE: clean, no error-matching boilerplate
let session_id = ctx.open_session().await?;
ctx.schedule_activity_on_session("hydrate", &config, &session_id).await?;

loop {
    let event = ctx.schedule_wait("user_message").await;
    let result = ctx.schedule_activity_on_session("run_turn", &event, &session_id)
        .await?;  // activities self-heal on migration
}
```

This is simpler in Phase 1 than littering the orchestration with `match Err("session_lost") => hydrate → retry` blocks. Phase 2 may add `SessionRecoveryPolicy` as a runtime-level convenience, but for now the app-level "try cache, fallback to store" pattern inside each activity is the recommended approach.

## Motivation

Some workloads need routing to a *specific* worker process — not because of hardware, but because of **in-memory state**.

**Primary use case: durable-copilot-sdk**

The durable-copilot-sdk keeps `CopilotSession` objects in memory on the worker process. These hold conversation history, auth tokens, and the copilot CLI child process. Each durable turn is an activity. All turns for the same conversation should route to the same worker, or the session context must be expensively recreated.

**Requirements from the copilot scenario:**

1. **Low thrashing** — turns for the same conversation stay on the same worker. Dehydrate/rehydrate is expensive. The system should actively resist migration.
2. **Orchestration-controlled dehydration** — when the LLM says "wait 3 hours," the orchestration decides to checkpoint, close the session, and release the worker.
3. **Orchestration awareness of worker crashes** — if the worker dies, the orchestration needs to know so it can re-hydrate on a new worker.

**Other use cases:**
- ML model loaded in memory — inference activities route to the worker holding the model
- Database connection pools — queries route to the worker with the right pool
- File cache locality — processing activities route to the worker with cached data

## Design Principles

1. **Explicit lifecycle.** The orchestration controls when a session exists via `open_session()` / `close_session()`. Sessions persist between activity turns without relying on work items being in the queue.

2. **Instance-scoped.** A session belongs to an orchestration instance, not a single execution. Sessions survive `ContinueAsNew`.

3. **Pure affinity.** No ordering guarantees (multiple session activities can execute concurrently), no provider-managed session state. The runtime routes; the app manages everything else.

4. **Lazy attachment.** Workers compete to claim a session when they first fetch an activity for it. The session is not pre-assigned to any worker.

5. **Re-claimable.** If a worker dies or the session lock expires, another worker can claim the session. There is no "permanently invalidated" concept.

6. **App-managed state.** Hydration, checkpointing, and dehydration are app-level activities. The runtime does not provide `get/set_session_state`. Phase 2 may add convenience interception for `SessionLost` errors.

---

## API Design

### Orchestration-side

```rust
// Open a session with a generated ID — returns a deterministic session_id
let session_id = ctx.open_session().await?;

// Open a session with a specific ID — idempotent (create if not exists)
// If the session already exists and is open, this is a no-op returning the same ID.
// Useful for ContinueAsNew where the session_id is carried in the input.
let session_id = ctx.open_session_with_id("my-session-123").await?;

// Route activities to the session's worker
let result = ctx.schedule_activity_on_session("run_turn", &input, &session_id)
    .await?;

// Typed variant (same codec behavior as schedule_activity_typed)
let output: TurnResult = ctx
    .schedule_activity_on_session_typed("run_turn", &typed_input, &session_id)
    .await?;

// Close the session — orchestrator-side operation
// Marks session as ended, cancels in-flight session activities.
// No-op if session is already closed or doesn't exist.
ctx.close_session(&session_id).await?;
```

### Activity-side

```rust
async fn run_turn(ctx: ActivityContext, input: String) -> Result<String, String> {
    let session_id = ctx.session_id().expect("session-bound activity");

    // App-level state lookup
    let state = LOCAL_CACHE.get(session_id)
        .ok_or_else(|| "session_lost: state missing on this worker".to_string())?;

    let result = state.process(&input).await;
    Ok(result)
}
```

### RuntimeOptions

```rust
RuntimeOptions {
    /// Maximum number of distinct sessions this runtime will own concurrently,
    /// spanning all `worker_concurrency` slots.
    /// Default: 100. Set to 0 to never accept session work.
    max_sessions_per_runtime: usize,

    /// Session lock duration for liveness detection.
    /// The session lock in the provider expires after this duration.
    /// The background renewal task renews it every duration/2.
    /// If the worker crashes, the lock expires after at most this duration,
    /// allowing another worker to claim the session.
    /// Keep this short for fast crash recovery.
    /// Default: 2 * worker_lock_timeout (e.g., 60s).
    session_lock_duration: Duration,

    /// Session idle timeout. How long a session remains attached to a worker
    /// after the last activity for that session completes.
    ///
    /// - `Some(duration)` — if no activity for this session is fetched or
    ///   renewed within this duration, the worker voluntarily stops renewing
    ///   the session lock. The lock then expires (after session_lock_duration)
    ///   and another worker can claim it.
    ///   Example: Some(Duration::from_secs(300)) = 5 minutes.
    ///
    /// - `None` — no idle timeout. The worker holds the session indefinitely
    ///   as long as it is alive. Only broken by:
    ///   1. close_session() from the orchestration
    ///   2. Worker crash (renewal stops, lock expires after session_lock_duration)
    ///   3. Worker graceful shutdown
    ///   Recommended for the copilot use case where sessions should never
    ///   migrate due to inactivity.
    session_idle_timeout: Option<Duration>,

    /// Maximum number of sessions a single orchestration can have open
    /// concurrently. Enforced by the replay engine — exceeding this
    /// fails the orchestration with a Configuration error.
    /// Default: 10.
    max_sessions_per_orchestration: usize,
}
```

---

## Usage Patterns

### Copilot agent: hydrate → turns → checkpoint → dehydrate

```rust
async fn copilot_agent(ctx: &OrchestrationContext) -> Result<String, String> {
    let session_id = ctx.open_session().await?;

    // Hydrate on whichever worker claims the session
    ctx.schedule_activity_on_session("hydrate_copilot", &config, &session_id).await?;

    loop {
        let event = ctx.schedule_wait("user_message").await;

        match ctx.schedule_activity_on_session("run_turn", &event, &session_id).await
        {
            Ok(result) => {
                // Checkpoint after every turn (app's choice)
                ctx.schedule_activity_on_session("checkpoint", "", &session_id).await?;
            }
            Err(e) if e.contains("session_lost") => {
                // Worker died or session lock expired. Re-hydrate.
                ctx.schedule_activity_on_session("hydrate_copilot", &config, &session_id).await?;
                continue;
            }
            Err(e) => return Err(e),
        }

        // Long wait: checkpoint, close session, timer, reopen
        if needs_long_wait {
            ctx.schedule_activity_on_session("dehydrate_copilot", "", &session_id).await?;
            ctx.close_session(&session_id).await?;
            ctx.schedule_timer(Duration::from_secs(3600)).await;
            let session_id = ctx.open_session().await?;
            ctx.schedule_activity_on_session("hydrate_copilot", &config, &session_id).await?;
        }

        // History management
        ctx.continue_as_new(updated_state).await?;
    }
}
```

### Worker-side state management

```typescript
const sessionCache = new LRUCache<string, CopilotSession>({
    max: 50,
    ttl: 10 * 60 * 1000,  // LRU eviction as safety net
    dispose: (session) => session.destroy(),
});

registerActivity("hydrate_copilot", async (ctx, config) => {
    const session = await loadFromCheckpointStore(ctx.sessionId!)
        ?? await createFresh(config);
    sessionCache.set(ctx.sessionId!, session);
    return "hydrated";
});

registerActivity("run_turn", async (ctx, input) => {
    const session = sessionCache.get(ctx.sessionId!);
    if (!session) throw new Error("session_lost");
    return await session.sendAndWait(input);
});

registerActivity("checkpoint", async (ctx) => {
    const session = sessionCache.get(ctx.sessionId!);
    if (!session) throw new Error("session_lost");
    await saveToCheckpointStore(ctx.sessionId!, session);
    return "ok";
});

registerActivity("dehydrate_copilot", async (ctx) => {
    const session = sessionCache.get(ctx.sessionId!);
    if (session) {
        await saveToCheckpointStore(ctx.sessionId!, session);
        await session.destroy();
        sessionCache.delete(ctx.sessionId!);
    }
    return "dehydrated";
});
```

---

## Session Lock Renewal

### How session locks work

The worker spawns a **session lock renewal background task** when it claims a session. This is independent of any specific activity.

**Session lock timeout** defaults to 2x the activity lock timeout (e.g., 60s if `worker_lock_timeout` is 30s).

**Renewal triggers:**

1. **Activity lock renewal** — Every time `renew_work_item_lock` is called for an activity in this session, the provider piggybacks a session lock extension. This means while any activity is in-flight for a session, the session lock stays alive.

2. **Post-activity grace period** — After the last activity for a session completes, the session lock has `session_lock_timeout` remaining from its last renewal. The worker continues to own the session for this duration even with no active work.

3. **Worker-spawned session renewal task** — The worker spawns a dedicated background task per session that renews the session lock periodically (at `session_lock_timeout / 2` intervals). This keeps the session alive between activity turns — solving the "idle between turns" problem without a keepalive activity.

### Lifecycle of a session lock

```
t=0     Worker A fetches first item for session "X" → claims session
        → Session lock set to t + session_lock_timeout (60s)
        → Worker spawns session renewal background task for "X"

t=5     Activity in-flight. renew_work_item_lock piggybacks session renewal.
        → Session lock extended.

t=10    Activity completes. No more items in queue for "X".
        → Session renewal background task still running.
        → Renews session lock every ~30s.

t=25    Orchestration waits on schedule_wait("user_message").
        → No items in queue. Renewal task keeps extending lock.

t=40    User sends message. Orchestration schedules schedule_activity_on_session(run_turn, "X").
        → Worker A fetches it (still owns session). In-memory state available.
        → No migration, no re-hydration.

t=100   Orchestration calls close_session("X").
        → But first, app should schedule a dehydrate activity to clean up:
          schedule_activity_on_session(dehydrate, "X") → Worker A checkpoints & evicts cache.
        → Then close_session("X") marks session as ended in provider.
        → Worker A's renewal task receives error on next renewal attempt → stops.
        → In-memory state already cleaned up by the dehydrate activity.
        → (If app skips the dehydrate, state lingers until LRU TTL evicts it.
           There is no runtime hook to notify the worker of close_session.)
```

### Session lock expiry (no close, worker alive but idle)

```
t=0     Worker A claims session "X". Renewal task running.
t=10    Last activity completes.
t=40    Renewal task renews lock. Still alive.
...
t=∞     As long as worker is alive and renewal task runs, session stays claimed.
        Session only expires if:
        1. Worker crashes (renewal task dies, lock expires after session_lock_duration)
        2. Orchestration calls close_session()
        3. Worker shuts down gracefully (renewal tasks cancelled)
        4. session_idle_timeout elapses (if not None)
```

**Two independent knobs:**

- **`session_lock_duration`** (e.g., 60s) — how long the lock row lives in the DB before expiry. Renewed every ~30s by the background task. Controls **crash recovery latency**: if a worker dies, another worker can claim the session after at most 60s. Keep this short.

- **`session_idle_timeout`** (e.g., `None` or `Some(5 min)`) — how long the worker voluntarily holds a session after the last activity completes. Controls **voluntary release**:
  - `None`: worker holds forever. Session never migrates while worker is alive. Recommended for copilot.
  - `Some(5 min)`: if no session activity for 5 minutes, the worker stops renewing the lock. Lock expires after `session_lock_duration`. Session becomes claimable.

These are independent concerns:
- Lock duration = liveness probe frequency. Short = fast crash detection.
- Idle timeout = affinity stickiness. Long/None = less thrashing.

---

## Core Semantics

### Session identity and scope

- `session_id` is either generated by the runtime (`open_session()`) or supplied by the user (`open_session_with_id(id)`). Both are recorded as a `SessionOpened` event in history.
- `open_session_with_id(id)` is idempotent: if a session with that ID already exists and is open, it's a no-op. If it was previously closed, it reopens it.
- `close_session(id)` is idempotent: closing an already-closed or non-existent session is a no-op.
- Session belongs to orchestration **instance_id**.
- Session remains valid across `ContinueAsNew` executions of that instance. The app carries the session_id in the `continue_as_new` input and calls `open_session_with_id(id)` in the new execution to re-associate with the existing session (no-op if still open).

### Attachment and lease

- Session attachment is lazy: the first worker to fetch a session-bound activity claims the session.
- Workers race to attach when a session is unclaimed (lock expired or never claimed).
- One active attachment at a time — the `sessions` table enforces this atomically.
- Attachment lease is renewed by the worker's session renewal background task.
- Lease loss (crash, shutdown) allows another worker to attach on next fetch.

### open_session / open_session_with_id behavior

**`open_session()`** generates a deterministic session_id (via the replay engine's ID allocation) and persists a `SessionOpened` event. On replay, the session_id is restored from history.

**`open_session_with_id(id)`** uses the caller-supplied ID. **Idempotent:** if a session with that ID already exists and is open, this is a no-op — it returns the same session_id. If the session was previously closed, it reopens it (sets `ended = false`, clears worker attachment). This is useful for `ContinueAsNew` where the orchestration carries the session_id in its input and re-opens it in each execution.

Both variants persist a `SessionOpened` event and create (or update) the session row in the provider's `sessions` table with `ended = false`, `worker_id = NULL`.

### close_session behavior

`close_session()` is an **orchestrator-side operation** that takes effect at the orchestration ack transaction commit.

**Idempotent:** if the session is already closed or doesn't exist, this is a no-op. A `SessionClosed` event is still persisted for replay determinism, but the provider operation is a no-op.

When closing an active session:

1. Persist `SessionClosed` event in history.
2. Delete session row from provider `sessions` table. The worker's renewal task will fail on next attempt (row gone) and stop.
3. Cancel in-flight session activities via existing cancellation/lock-steal mechanism.
4. Ignore late completions from closed session work.

**Note:** `close_session` does NOT run an activity on the session's worker. The worker does not receive an explicit "teardown" signal. If the app needs to checkpoint/dehydrate before closing, it should schedule a `dehydrate` activity before calling `close_session`:

```rust
ctx.schedule_activity_on_session("dehydrate", "", &session_id).await?;
ctx.close_session(&session_id).await?;
```

The worker cleans up in-memory state via its LRU cache eviction or by detecting that the session renewal task has stopped (next renewal attempt fails → session is gone).

---

## Session Migration

Session migration occurs when a session's worker attachment changes. This is handled entirely in the **app layer** — duroxide provides the routing infrastructure but stays out of state management decisions. The app's activities detect cold state and self-heal.

Migration happens in three scenarios:

### 1. Worker crash

Worker dies → renewal task dies → session lock expires after `session_lock_duration` → next session-bound activity is fetched by a different worker → that worker claims the session.

The app handles this transparently inside activities:

```rust
// Activity self-heals: try cache, fallback to checkpoint store
"run_turn" => {
    let session_id = ctx.session_id().unwrap();
    let state = LOCAL_CACHE.get(session_id)
        .or_else(|| rehydrate_from_store(session_id))
        .ok_or("session_lost: cannot rehydrate")?;
    state.process(input).await
}
```

### 2. Orchestration-controlled migration (long wait)

The orchestration decides to release the worker before a long idle period:

```rust
ctx.schedule_activity_on_session("dehydrate", "", &session_id).await?;
ctx.close_session(&session_id).await?;
ctx.schedule_timer(Duration::from_secs(3600)).await;
let session_id = ctx.open_session().await?;
ctx.schedule_activity_on_session("hydrate", &config, &session_id).await?;
```

No migration surprise — the orchestration explicitly manages the transition.

### 3. Worker shutdown (graceful)

On graceful shutdown, the worker **actively releases** all session locks by calling `provider.release_session_lock(session_id, worker_id)` for each owned session, then cancels renewal tasks. This immediately makes sessions claimable by other workers — no need to wait for `session_lock_duration` to expire.

From the orchestration's perspective, this is the same as a crash: the next activity on the session runs on a new worker, the app detects cold state, and self-heals.

---

## Data Model Changes

### New Table: `sessions`

```sql
CREATE TABLE sessions (
    instance_id   TEXT NOT NULL,     -- orchestration instance that owns it
    session_id    TEXT NOT NULL,
    worker_id     TEXT,              -- NULL = unclaimed, non-NULL = attached
    locked_until  INTEGER,           -- NULL = unclaimed, else lock expiry (ms)
    PRIMARY KEY (instance_id, session_id)
);

CREATE INDEX idx_sessions_worker ON sessions(worker_id);
CREATE INDEX idx_sessions_locked_until ON sessions(locked_until);
```

Note: there is no `ended` column. When `close_session` is called, the session row is **deleted**. A session row exists only while the session is open. This simplifies queries — "session exists" = "session is open". The `renew_session_lock` method returns an error when the row is missing, which signals the worker's renewal task to stop.

### Modified Table: `worker_queue`

```sql
ALTER TABLE worker_queue ADD COLUMN session_id TEXT;
CREATE INDEX idx_worker_queue_session ON worker_queue (session_id);
```

### New Events

```rust
EventKind::SessionOpened {
    session_id: String,
}

EventKind::SessionClosed {
    session_id: String,
}
```

### Modified Types

**`Action` enum** — new variants:
```rust
Action::OpenSession { scheduling_event_id: u64, session_id: String }  // generated or user-supplied
Action::CloseSession { scheduling_event_id: u64, session_id: String }  // idempotent (no-op if already closed)
Action::CallActivity { scheduling_event_id: u64, name: String, input: String, session_id: Option<String> }
```

**`WorkItem::ActivityExecute`** — gains `session_id`:
```rust
ActivityExecute {
    instance: String,
    execution_id: u64,
    id: u64,
    name: String,
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    session_id: Option<String>,
}
```

**`OrchestrationContext`** — gains session-bound scheduling methods:
```rust
impl OrchestrationContext {
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

**API parity note:** `schedule_activity_on_session_typed` must preserve the same JSON codec semantics and error behavior as existing `schedule_activity_typed` — same codec path, same decode-failure behavior, no session-specific serialization differences.

**`ActivityContext`** — gains `session_id()`:
```rust
impl ActivityContext {
    pub fn session_id(&self) -> Option<&str>;
}
```

---

## Provider Contract Changes

### Session Support as Optional Capability

Session support is **optional** in the provider. Providers advertise session support via a capability flag:

```rust
pub trait Provider: Any + Send + Sync {
    /// Whether this provider supports activity sessions.
    /// Default: false.
    fn supports_sessions(&self) -> bool { false }
    
    // ... existing methods ...
}
```

The runtime checks this at startup and when processing session actions:
- If the orchestration calls `open_session()` and the provider doesn't support sessions, the orchestration is failed with:
  ```
  ErrorDetails::Application {
      kind: AppErrorKind::OrchestrationFailed,
      message: "Provider does not support sessions".to_string(),
      retryable: false,
  }
  ```
- The worker dispatcher only uses session-aware fetch if `supports_sessions()` is true.

### Work Item Fetching: Two Variants

Instead of overloading `fetch_work_item` with session parameters, two separate methods:

**`fetch_work_item`** — unchanged, fetches only non-session work items:
```rust
/// Fetch next available NON-SESSION work item. Unchanged from current contract.
/// Only returns items where session_id IS NULL.
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

**`fetch_session_work_item`** — new, fetches session-bound or non-session work items:
```rust
/// Fetch next available work item, including session-bound items.
/// Returns items where:
/// - session_id IS NULL (regular work), OR
/// - session_id belongs to a session this worker owns, OR  
/// - session_id belongs to an unclaimed/expired session (worker claims it atomically)
///
/// Only available when supports_sessions() returns true.
async fn fetch_session_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
    worker_id: &str,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

The worker dispatcher chooses which variant to call:
- If the provider doesn't support sessions: always `fetch_work_item`
- If the provider supports sessions and worker is below `max_sessions_per_runtime`: `fetch_session_work_item`
- If the provider supports sessions but worker is at session capacity: `fetch_work_item` (only non-session work)

The `max_sessions_per_runtime` limit is tracked runtime-side (the worker knows how many sessions it owns based on active renewal tasks), not passed to the provider.

**`renew_work_item_lock`** — signature unchanged, but implementation piggybacks session lock renewal:
```rust
async fn renew_work_item_lock(&self, token: &str, extend_for: Duration)
    -> Result<(), ProviderError>;
// If the work item has a session_id, also extend session lock
```

**`ack_orchestration_item`** — when processing `OpenSession` / `CloseSession` actions:
- `OpenSession`: insert session row with `worker_id = NULL` (idempotent — `INSERT ... ON CONFLICT DO NOTHING`)
- `CloseSession`: delete session row (idempotent — no-op if not found). Cancel in-flight session activities.
- When enqueuing `WorkItem::ActivityExecute` to worker_queue, store `session_id`

### New Methods

**`renew_session_lock`** — called by the worker's session renewal background task:
```rust
async fn renew_session_lock(
    &self,
    instance_id: &str,
    session_id: &str,
    worker_id: &str,
    extend_for: Duration,
) -> Result<(), ProviderError>;
// Returns error if session row doesn't exist (closed), or claimed by different worker
```

**`release_session_lock`** — called during graceful worker shutdown:
```rust
async fn release_session_lock(
    &self,
    instance_id: &str,
    session_id: &str,
    worker_id: &str,
) -> Result<(), ProviderError>;
// Sets worker_id = NULL, locked_until = NULL. Immediate release, no expiry wait.
// No-op if session doesn't exist or is owned by a different worker.
```

---

## Runtime Changes

### Worker Dispatcher

- Calls `fetch_session_work_item` (if provider supports sessions and worker is below session capacity) or `fetch_work_item` (otherwise)
- Tracks owned sessions runtime-side (based on active renewal tasks)
- When a new session is claimed (first fetch for that session), the worker spawns a `session_lock_renewal_task` for it
- The `session_lock_renewal_task` calls `provider.renew_session_lock()` periodically
- On renewal failure (session closed/deleted or claimed by another worker), the task stops
- On graceful shutdown, calls `provider.release_session_lock()` for each owned session before stopping

### Orchestration Dispatcher

- Processes `Action::OpenSession` → persists `SessionOpened` event, creates session row via provider
- Processes `Action::CloseSession` → persists `SessionClosed` event, deletes session row, cancels in-flight activities
- **On orchestration terminal** (Completed, Failed, Cancelled): closes all open sessions for the instance. The orchestration dispatcher queries for open sessions by `instance_id` and deletes them, same as `close_session`. In-flight session activities are cancelled via the existing lock-steal mechanism. This prevents session leaks when orchestrations complete without explicitly closing sessions.
- **On ContinuedAsNew**: sessions are **not** closed. They survive across executions of the same instance (see ContinueAsNew section).

### Replay Engine

- `open_session()` emits `Action::OpenSession`, recorded as `SessionOpened` event. On replay, the session_id is restored from history.
- `close_session()` emits `Action::CloseSession`, recorded as `SessionClosed` event.
- `session_id` on `Action::CallActivity` flows through to `ActivityScheduled` event and `WorkItem::ActivityExecute`.

#### Session History Validation

The replay engine tracks open sessions in a `HashSet<String>` during execution. This set is validated at several points. All session validation errors are **application errors** (not nondeterminism) — they represent bugs in the orchestration code, not replay divergence.

**1. `close_session` is idempotent**

`close_session` is idempotent at both the provider and replay engine levels. Calling `close_session(id)` for a session that was never opened or is already closed is a no-op — a `SessionClosed` event is still recorded for replay determinism, but no error is raised. This keeps orchestration code simple and avoids fragile open/close bookkeeping.

**2. `schedule_activity_on_session` referencing a non-open session**

If the orchestration calls `schedule_activity_on_session(..., id)` and `id` is not in the open sessions set, the replay engine returns `TurnResult::Failed` with:
```
ErrorDetails::Application {
    kind: AppErrorKind::OrchestrationFailed,
    message: "schedule_activity_on_session called for session '{id}' which is not open",
    retryable: false,
}
```

**3. Max sessions per orchestration exceeded**

The replay engine enforces a per-orchestration session limit (configurable, e.g., 10). When `open_session` would exceed this limit, the replay engine returns `TurnResult::Failed` with:
```
ErrorDetails::Application {
    kind: AppErrorKind::OrchestrationFailed,
    message: "max sessions per orchestration exceeded (limit: {max}, open: {current})",
    retryable: false,
}
```

This prevents runaway orchestrations from opening unbounded sessions.

**4. Provider does not support sessions**

If the orchestration calls `open_session()` and the provider's `supports_sessions()` returns false, the replay engine returns `TurnResult::Failed` with:
```
ErrorDetails::Application {
    kind: AppErrorKind::OrchestrationFailed,
    message: "Provider does not support sessions".to_string(),
    retryable: false,
}
```

**5. Replay: `SessionOpened` / `SessionClosed` determinism**

During replay, the replay engine applies `match_and_bind_schedule()` to `SessionOpened` and `SessionClosed` events, exactly as it does for `ActivityScheduled`, `TimerCreated`, etc. If the replayed orchestration code emits actions that don't match the persisted history (e.g., opens a session that wasn't opened before, or opens in different order), this produces a **nondeterminism** error (the only nondeterminism case — it's a replay divergence, not a runtime validation).

#### ContinueAsNew: Carrying Open Sessions

When an orchestration calls `continue_as_new`, the set of currently open sessions must be carried to the new execution. The runtime handles this automatically:

1. At `ContinueAsNew` time, the replay engine captures the set of open session IDs.
2. This set is persisted as part of the `OrchestrationContinuedAsNew` event (or as a separate mechanism passed through the provider).
3. When the new execution starts, the replay engine initializes its open sessions set from the carried-over data.
4. The sessions are immediately available — the user's orchestration code does **not** need to call `open_session_with_id()` again. The sessions are already open and can be used with `schedule_activity_on_session()` directly.

The user only needs to carry the session_id string(s) in the `continue_as_new` input so the orchestration code knows which IDs to reference. The runtime handles the session bookkeeping transparently.

This ensures:
- Sessions survive `ContinueAsNew` without the worker losing its lock.
- The replay engine in the new execution has correct knowledge of which sessions are open.
- The provider's `sessions` table is not modified during `ContinueAsNew` — the session row persists with its existing worker attachment.
- No redundant `open_session_with_id()` calls needed.

#### RuntimeOptions for Session Limits

```rust
RuntimeOptions {
    /// Maximum number of sessions a single orchestration instance can have
    /// open at the same time. Enforced by the replay engine.
    /// Default: 10.
    max_sessions_per_orchestration: usize,
}
```

---

## Orthogonality With Existing Features

### ContinueAsNew

Session is instance-scoped, so it survives across executions. The set of open sessions is carried over automatically by the runtime to the new execution — no additional `open_session` calls needed. The user carries the session_id string(s) in the `continue_as_new` input so the orchestration code knows which IDs to use with `schedule_activity_on_session()`. Execution-id filtering for completions remains unchanged.

### External Events / Timers

Orthogonal. They may trigger orchestration turns that schedule session-bound activities. If session has migrated, the activity detects cold state and returns an error for the orchestration to handle.

### Sub-orchestrations

Sessions are instance-local and not implicitly shared with child orchestrations. Sharing semantics can be specified separately if needed later.

### Cancellation / Poison Handling

Existing mechanisms remain primary. `close_session` cancels session activities via the existing lock-steal mechanism.

### Versioning / Capability Filtering

Session-related events and work-item fields use `#[serde(skip_serializing_if = "Option::is_none")]` and `#[serde(default)]` for backward compatibility. Older runtimes that don't understand sessions will skip session-bound work items (the session_id filter in `fetch_work_item` won't match).

---

## Configuration

```rust
RuntimeOptions {
    /// Maximum distinct sessions this runtime will own concurrently,
    /// spanning all worker slots. Default: 100.
    max_sessions_per_runtime: usize,

    /// Session lock duration for liveness detection. Controls crash recovery speed.
    /// Lock renewed every duration/2. Default: 2 * worker_lock_timeout (e.g., 60s).
    session_lock_duration: Duration,

    /// Session idle timeout. How long to hold a session after last activity.
    /// - Some(duration): release after idle period. e.g. Some(5 min).
    /// - None: hold forever while worker is alive. Recommended for copilot.
    session_idle_timeout: Option<Duration>,

    /// Max sessions per orchestration instance. Enforced by replay engine. Default: 10.
    max_sessions_per_orchestration: usize,
}
```

---

## Implementation Phases

### Phase 1: Session primitives (MVP)

1. Add `open_session()` / `close_session()` to `OrchestrationContext`
2. Add `SessionOpened` / `SessionClosed` event kinds
3. Add `OpenSession` / `CloseSession` action variants
4. Add `session_id: Option<String>` to `Action::CallActivity`, `EventKind::ActivityScheduled`, `WorkItem::ActivityExecute`
5. Add `schedule_activity_on_session()` and `schedule_activity_on_session_typed()` to `OrchestrationContext`
6. Add `session_id()` getter to `ActivityContext`
7. Add `max_sessions_per_runtime` and `session_lock_timeout` to `RuntimeOptions`
8. Add `sessions` table and `worker_queue.session_id` column (SQLite migration)
9. Add `renew_session_lock` to Provider trait
10. Implement session-aware `fetch_work_item` query
11. Implement session lock piggyback in `renew_work_item_lock`
12. Implement session lock renewal background task in worker dispatcher
13. Implement `open_session` / `close_session` processing in orchestration dispatcher
14. Update replay engine to flow session events and session_id on activities
15. Add provider validation tests
16. Add integration tests (routing, re-claim, max capacity, ContinueAsNew)

### Phase 2: Recovery interception (convenience)

1. Add typed `SessionLost` error kind to `ErrorDetails`
2. Add `SessionRecoveryPolicy` configuration (rebuild activity, max retries, singleflight)
3. Add auto rebuild-and-retry path in worker dispatcher
4. Add singleflight gate per session per worker
5. Add observability metrics for session migration events

### Phase 3: Hardening

1. Stress tests for session migration under load
2. Mixed-version deployment validation
3. Session leak detection (sessions never closed)
4. Observability: session count per worker, migration rate, lock renewal metrics

---

## Open Questions

1. **`session_lock_duration` default** — `2 * worker_lock_timeout` (e.g., 60s) means after a crash, sessions are stuck for up to 60s before another worker can claim them. Acceptable?

2. **`session_idle_timeout` default** — `None` (hold forever) is simplest and right for copilot. But should the default be `Some(5 min)` to prevent accidental session accumulation on workers?

3. **Interaction with activity tags** — If both features ship, can an activity use both `schedule_activity_on_session()` and `.with_tag()`? Semantically: tag selects the worker pool, session selects the worker within the pool.

### Note: `runtime_id` stays as 4-char hex

The current `runtime_id` is a 4-char hex derived from nanoseconds (`{:04x}` of `nanos & 0xFFFF`). Sessions use `runtime_id` as the `worker_id` for lock ownership. The 16-bit value is kept as-is for now; collision risk is low for typical deployment sizes. Revisit if Kubernetes StatefulSet pod names or high-density deployments become a requirement.

---

## Test Specification

### 1. Session Lifecycle — Basic

| # | Test | Description |
|---|------|-------------|
| 1.1 | `open_session_returns_unique_id` | `open_session()` returns a non-empty session_id. Two calls return different IDs. |
| 1.2 | `open_session_with_id_returns_given_id` | `open_session_with_id("my-id")` returns `"my-id"`. |
| 1.3 | `open_session_with_id_idempotent` | Calling `open_session_with_id("X")` twice returns `"X"` both times, no error. |
| 1.4 | `close_session_succeeds` | `open_session()` then `close_session(id)` completes without error. |
| 1.5 | `close_session_idempotent` | `close_session(id)` on an already-closed session is a no-op, no error. |
| 1.6 | `close_session_nonexistent_noop` | `close_session("never-opened")` is a no-op, no error. |
| 1.7 | `open_close_open_same_id` | Open session "X", close it, open "X" again. Second open succeeds and session is usable. |
| 1.8 | `multiple_sessions_open_concurrently` | Open 3 sessions with different IDs. All are independently usable with `schedule_activity_on_session()`. |
| 1.9 | `session_events_in_history` | After `open_session` and `close_session`, history contains `SessionOpened` and `SessionClosed` events with correct session_ids. |

### 2. Session Routing — Happy Path

| # | Test | Description |
|---|------|-------------|
| 2.1 | `session_activity_routes_to_same_worker` | Open session. Schedule 3 sequential `schedule_activity_on_session` calls. All execute on the same worker (assert via `ActivityContext.worker_id()`). |
| 2.2 | `session_activity_routes_consistently_across_turns` | Open session. Schedule session activity, await. Wait for external event. Schedule another session activity on same session. Same worker. |
| 2.3 | `different_sessions_can_route_to_different_workers` | Open session A and session B. Schedule session activities on each. They MAY execute on different workers (non-deterministic, but the system allows it). |
| 2.4 | `non_session_activity_unaffected` | Open session. Schedule one `schedule_activity_on_session` and one `schedule_activity`. Non-session activity can execute on any worker. |
| 2.5 | `session_id_available_in_activity_context` | Activity scheduled via `schedule_activity_on_session(... "X")` has `ctx.session_id() == Some("X")`. |
| 2.6 | `non_session_activity_has_none_session_id` | Activity scheduled via `schedule_activity` has `ctx.session_id() == None`. |

### 3. Session Routing — Multi-Worker

| # | Test | Description |
|---|------|-------------|
| 3.1 | `two_workers_claim_different_sessions` | Two runtimes sharing a provider. Session A claimed by worker 1, session B by worker 2. Activities route correctly. |
| 3.2 | `session_exclusively_owned` | Two workers compete for same session. Only one claims it. All activities for that session go to the owner. |
| 3.3 | `max_sessions_per_runtime_enforced` | Worker with `max_sessions_per_runtime: 2`. Open 3 sessions. Third session's activities are picked up by a different worker (if available) or wait. |
| 3.4 | `at_capacity_worker_still_processes_nonsession` | Worker at session capacity still fetches and processes non-session activities. |
| 3.5 | `at_capacity_worker_still_processes_owned_sessions` | Worker at session capacity still processes activities for sessions it already owns. |

### 4. Session Lock Renewal

| # | Test | Description |
|---|------|-------------|
| 4.1 | `session_lock_renewed_during_activity` | While a session-bound activity is running, the session lock is extended (check `locked_until` in DB increases). |
| 4.2 | `session_lock_renewed_between_turns` | Session lock stays alive between two activity turns separated by a `schedule_wait`. No migration. |
| 4.3 | `session_lock_renewal_stops_on_close` | After `close_session()`, the renewal task stops. Next renewal attempt returns error (session row deleted). |
| 4.4 | `session_lock_renewal_stops_on_shutdown` | Graceful shutdown calls `release_session_lock()`. Session becomes immediately claimable. |
| 4.5 | `idle_timeout_none_keeps_session_forever` | With `session_idle_timeout: None`, session stays owned after long idle period with no activities. |
| 4.6 | `idle_timeout_releases_after_duration` | With `session_idle_timeout: Some(2s)`, session lock is released after 2s of no activity for that session. Another worker can claim it. |

### 5. Session Migration — Worker Crash

| # | Test | Description |
|---|------|-------------|
| 5.1 | `crash_then_reclaim` | Worker A claims session, runs activity. Kill worker A. Wait for `session_lock_duration` to expire. Worker B picks up next activity for that session. Verify B executes it. |
| 5.2 | `crash_app_detects_cold_state` | After crash + reclaim, activity on new worker can detect missing in-memory state (returns error or re-hydrates). |
| 5.3 | `crash_inflight_activity_retried` | Worker A crashes mid-activity. Activity work item lock expires. Work item is re-fetched by worker B (which claims the session). Activity executes again. |
| 5.4 | `crash_multiple_sessions_all_reclaimed` | Worker A owns 3 sessions. Kill it. All 3 sessions become claimable after lock expiry. |

### 6. Session Migration — Graceful Shutdown

| # | Test | Description |
|---|------|-------------|
| 6.1 | `graceful_shutdown_releases_locks_immediately` | Shutdown worker A. Sessions are immediately claimable (no waiting for lock expiry). Worker B claims on next fetch. |
| 6.2 | `graceful_shutdown_inflight_activities_cancelled` | Shutdown worker A while activity is running. Activity is cancelled. Work item becomes available for another worker. |

### 7. Session Migration — Orchestration-Controlled

| # | Test | Description |
|---|------|-------------|
| 7.1 | `close_reopen_different_worker` | Open session, run activity on worker A. Close session. Open new session, next activity may go to worker A or B (no guarantee). |
| 7.2 | `dehydrate_close_timer_open_hydrate` | Full copilot pattern: dehydrate → close → timer(1s) → open → hydrate. Verify hydrate runs (potentially on different worker). |
| 7.3 | `close_cancels_inflight_session_activities` | Open session. Schedule long-running activity on session. Close session before activity completes. Activity is cancelled. |
| 7.4 | `close_ignores_late_completions` | Open session. Activity completes after close_session is processed. Late completion is dropped/ignored. Orchestration doesn't see it. |

### 8. History Validation — Error Cases

| # | Test | Description |
|---|------|-------------|
| 8.1 | `close_without_open_is_noop` | Call `close_session("X")` without prior `open_session`. No error, `SessionClosed` event recorded, provider no-op. |
| 8.2 | `session_activity_without_open_fails` | Schedule `schedule_activity_on_session(..., "X")` without opening session "X". Orchestration fails. |
| 8.3 | `session_activity_after_close_fails` | Open "X", close "X", then `schedule_activity_on_session(..., "X")`. Orchestration fails. |
| 8.4 | `max_sessions_exceeded_fails` | With `max_sessions_per_orchestration: 2`, open 3 sessions. Third open fails orchestration. |
| 8.5 | `max_sessions_close_then_reopen_within_limit` | Open 2 (at limit), close 1 (now at 1), open another (now at 2). Succeeds. |
| 8.6 | `provider_no_session_support_fails` | Provider returns `supports_sessions() = false`. Orchestration calls `open_session()`. Fails with `AppErrorKind::OrchestrationFailed`. |

### 9. Replay Determinism

| # | Test | Description |
|---|------|-------------|
| 9.1 | `session_open_replay_matches` | Run orchestration that opens a session and schedules activity. Replay the same orchestration. Actions match history. No nondeterminism error. |
| 9.2 | `session_close_replay_matches` | Open + close + activity. Replay. Deterministic. |
| 9.3 | `replay_divergence_open_mismatch` | History has `SessionOpened`. Replay code does NOT call `open_session()`. Nondeterminism error. |
| 9.4 | `replay_divergence_extra_open` | History has no session events. Replay code calls `open_session()`. Nondeterminism error. |
| 9.5 | `replay_divergence_open_order` | History has `open(A)` then `open(B)`. Replay code opens B then A. Nondeterminism error. |
| 9.6 | `replay_session_id_restored` | Open session, schedule activity, continue-as-new. Replay the continued execution. session_id is correctly restored from carried-over data. |

### 10. ContinueAsNew

| # | Test | Description |
|---|------|-------------|
| 10.1 | `session_survives_continue_as_new` | Open session, schedule session activity (succeeds on worker A), continue_as_new. In new execution, `schedule_activity_on_session(same_id)`. Executes on same worker A. |
| 10.2 | `session_no_reopen_needed` | After continue_as_new, sessions are already in the open set. `schedule_activity_on_session(id)` works without calling `open_session_with_id()` again. |
| 10.3 | `multiple_sessions_survive_can` | Open 3 sessions. ContinueAsNew. All 3 usable in new execution. |
| 10.4 | `close_session_after_can` | Open session, ContinueAsNew, close session in new execution. Works. Session row deleted. |
| 10.5 | `can_with_no_open_sessions` | ContinueAsNew with no sessions open. New execution starts with empty session set. open_session works normally. |
| 10.6 | `session_lock_maintained_across_can` | Worker A owns session. ContinueAsNew. Worker A's renewal task is still running. Session stays on worker A. |

### 11. Orchestration Terminal — Session Cleanup

| # | Test | Description |
|---|------|-------------|
| 11.1 | `completed_orchestration_closes_all_sessions` | Open 2 sessions, don't close them. Orchestration returns Ok. Both session rows are deleted from provider. |
| 11.2 | `failed_orchestration_closes_all_sessions` | Open session, orchestration panics/fails. Session row is cleaned up. |
| 11.3 | `cancelled_orchestration_closes_all_sessions` | Open session. External cancel. Session row deleted, in-flight session activities cancelled. |
| 11.4 | `terminal_cleanup_stops_renewal_tasks` | Worker A holds session. Orchestration completes. Renewal task on worker A stops (next renewal returns error — row gone). |

### 12. Mixed Session + Non-Session Activities

| # | Test | Description |
|---|------|-------------|
| 12.1 | `session_and_nonsession_in_same_orchestration` | Open session. Schedule session activity AND non-session activity. Both complete. Session one on session worker, non-session one on any worker. |
| 12.2 | `nonsession_activity_between_session_activities` | Session activity → non-session activity → session activity. Session activities on same worker. Non-session on any. |
| 12.3 | `fan_out_mixed` | `join` with 2 session activities and 2 non-session activities. All 4 complete. Session ones on same worker. |
| 12.4 | `select_session_vs_timer` | `select2` between session activity and timer. Timer wins. Session activity cancelled (select loser). |
| 12.5 | `select_session_vs_nonsession` | `select2` between session activity and non-session activity. Whichever completes first wins. Loser cancelled. |
| 12.6 | `sub_orchestration_does_not_inherit_session` | Parent opens session. Schedules sub-orchestration. Sub-orchestration tries `schedule_activity_on_session(parent_session_id)` — fails (session is instance-local). |

### 13. Multiple Sessions — Interleaving

| # | Test | Description |
|---|------|-------------|
| 13.1 | `two_sessions_same_worker` | Open session A and B. Both claimed by same worker (if only one worker). Activities interleave correctly. |
| 13.2 | `two_sessions_different_workers` | Two workers. Session A on worker 1, session B on worker 2. Concurrent activities route correctly. |
| 13.3 | `close_one_keep_other` | Open A and B. Close A. Schedule activity on B. Works. Schedule on A. Fails (closed). |
| 13.4 | `concurrent_activities_different_sessions` | `join` with activity on session A and activity on session B. Both complete on their respective workers. |

### 14. Provider Contract

| # | Test | Description |
|---|------|-------------|
| 14.1 | `fetch_work_item_skips_session_items` | Enqueue session-bound and non-session items. `fetch_work_item()` only returns non-session items. |
| 14.2 | `fetch_session_work_item_returns_both` | `fetch_session_work_item()` returns session-bound items (for owned/claimable sessions) AND non-session items. |
| 14.3 | `fetch_session_claims_unclaimed` | Enqueue item for unclaimed session. `fetch_session_work_item()` returns it and atomically creates session row with worker_id set. |
| 14.4 | `fetch_session_skips_other_workers_session` | Session "X" owned by worker A. Worker B calls `fetch_session_work_item()`. Item for "X" is not returned. |
| 14.5 | `fetch_session_reclaims_expired_lock` | Session "X" owned by worker A, lock expired. Worker B calls `fetch_session_work_item()`. Claims "X", returns the item. |
| 14.6 | `renew_session_lock_extends` | Claim session. Call `renew_session_lock()`. `locked_until` in DB is extended. |
| 14.7 | `renew_session_lock_fails_if_deleted` | Close session (deletes row). Call `renew_session_lock()`. Returns error. |
| 14.8 | `renew_session_lock_fails_if_stolen` | Session claimed by worker A. Worker B claims it (lock expired). Worker A calls `renew_session_lock()`. Returns error (different worker_id). |
| 14.9 | `release_session_lock_clears_attachment` | Call `release_session_lock()`. Session row has `worker_id = NULL`, `locked_until = NULL`. |
| 14.10 | `release_session_lock_noop_if_not_owner` | Worker B calls `release_session_lock()` for session owned by worker A. No-op. |
| 14.11 | `ack_orchestration_creates_session_row` | `ack_orchestration_item` with `OpenSession` action. Session row created in DB with `worker_id = NULL`. |
| 14.12 | `ack_orchestration_deletes_session_row` | `ack_orchestration_item` with `CloseSession` action. Session row deleted from DB. |
| 14.13 | `ack_orchestration_stores_session_id_on_workitem` | `ack_orchestration_item` enqueues `ActivityExecute` with `session_id`. Worker queue row has session_id column set. |
| 14.14 | `renew_work_item_lock_piggybacks_session` | Renew lock on a session-bound work item. Session `locked_until` is also extended. |
| 14.15 | `supports_sessions_false_default` | Default `Provider` trait impl returns `supports_sessions() = false`. |

### 15. Edge Cases and Race Conditions

| # | Test | Description |
|---|------|-------------|
| 15.1 | `two_workers_race_to_claim_same_session` | Two workers call `fetch_session_work_item()` simultaneously for unclaimed session. Only one succeeds. Loser gets a different item or None. |
| 15.2 | `close_during_inflight_activity` | Activity running on session. Orchestration processes close_session in same ack. Activity's work item is cancelled (lock stolen). |
| 15.3 | `open_session_with_id_after_close_in_same_turn` | `close_session("X")` then `open_session_with_id("X")` in the same orchestration turn. Session is reopened. Provider: delete then insert in same transaction. |
| 15.4 | `activity_completes_after_session_close` | Session closed. Activity ack arrives late. Completion is dropped (session row gone, or filtered by dispatchers). |
| 15.5 | `continue_as_new_during_inflight_session_activity` | Session activity in-flight. Orchestration calls `continue_as_new`. Activity is a select loser (cancelled). Session survives to next execution. |
| 15.6 | `session_lock_expires_during_activity` | Session idle timeout fires while a long-running activity is still executing. The activity's work item lock keeps the session alive via piggyback renewal. Session should NOT expire. |
| 15.7 | `rapid_open_close_cycles` | Open and close 100 sessions in a loop. No resource leaks. Provider session table is clean. |
| 15.8 | `session_activity_on_worker_with_no_registry` | Session activity routed to worker that doesn't have the activity registered. Existing unregistered-activity handling (abandon with backoff → poison) applies. |
| 15.9 | `poison_message_in_session` | Session-bound activity exceeds max_attempts. Poison handling fires. Session itself is unaffected (other activities on same session still work). |
| 15.10 | `session_survives_orchestration_lock_timeout` | Orchestration lock expires and is re-acquired. Session state (open set) is reconstructed from history. No session loss. |
| 15.11 | `empty_session_id_string_rejected` | `open_session_with_id("")` — empty string. Should fail with application error. |
| 15.12 | `very_long_session_id_handled` | `open_session_with_id("a]".repeat(1000))` — 1000-char session ID. Should work (or be rejected with a clear limit). |

### 16. Serialization / Backward Compatibility

| # | Test | Description |
|---|------|-------------|
| 16.1 | `session_id_none_omitted_in_json` | `WorkItem::ActivityExecute` with `session_id: None` serializes without `session_id` field (skip_serializing_if). |
| 16.2 | `session_id_present_in_json` | `WorkItem::ActivityExecute` with `session_id: Some("X")` serializes with `"session_id":"X"`. |
| 16.3 | `old_workitem_without_session_id_deserializes` | JSON without `session_id` field deserializes to `session_id: None` (serde default). |
| 16.4 | `old_event_without_session_id_deserializes` | `ActivityScheduled` event from pre-sessions era (no `session_id` field) deserializes with `session_id: None`. |
| 16.5 | `session_events_serialize_roundtrip` | `SessionOpened` and `SessionClosed` events serialize and deserialize correctly. |

### 17. Observability

| # | Test | Description |
|---|------|-------------|
| 17.1 | `session_open_logged` | `open_session()` produces a tracing event with session_id. |
| 17.2 | `session_close_logged` | `close_session()` produces a tracing event with session_id. |
| 17.3 | `session_claim_logged` | When a worker claims a session, a tracing event is emitted with session_id and worker_id. |
| 17.4 | `session_migration_logged` | When a session is re-claimed by a different worker, a tracing event is emitted. |
| 17.5 | `session_renewal_failure_logged` | When `renew_session_lock` fails, a warning is logged with session_id. |

### 18. Single-Threaded Runtime

| # | Test | Description |
|---|------|-------------|
| 18.1 | `sessions_work_in_current_thread` | `#[tokio::test(flavor = "current_thread")]` with 1x1 concurrency. Open session, run session-bound activity. Completes. |
| 18.2 | `session_renewal_works_single_thread` | Session renewal background task runs correctly on single-threaded runtime (no multi-threaded scheduling required). |

