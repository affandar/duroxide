# Activity Sessions

**Status:** Proposal (v2 — refined through design iteration)  
**Created:** 2026-02-13  
**Updated:** 2026-02-14  
**Depends on:** Activity Tags proposal (for `tag` column on `worker_queue` + `fetch_work_item` tag filtering)  

## Summary

Add a session API to `OrchestrationContext` that provides **worker affinity for groups of activities**. Workers **compete** to claim unclaimed sessions when they pick up the first work item for that session. Once claimed, only the owning worker can process activities for that session. Sessions have explicit lifecycle (create/close), lock refresh tied to worker lifetime, and clean failure semantics when the owning worker dies.

## Motivation

Activity tags (see `activity-tags.md`) solve routing to worker *classes* (e.g., all GPU activities → GPU workers). But some workloads need routing to a *specific* worker instance — not because of hardware, but because of **in-memory state**.

**Primary use case: durable-copilot-sdk**

The Copilot SDK keeps `CopilotSession` objects in memory on the worker process. These hold conversation history, auth tokens, and connection state. Each durable turn is an activity. All turns for the same conversation must route to the same worker, or the session context is lost.

Today this works by running exactly 1 worker replica. Activity sessions would let us scale to N workers while keeping turns for the same conversation on the same worker.

**Other use cases:**
- ML model loaded in memory — inference activities route to the worker holding the model
- Database connection pools — queries route to the worker with the right pool
- File cache locality — processing activities route to the worker with cached data
- Any stateful activity sequence where recreating state is expensive

## Design Principles

1. **Sessions are created by orchestrations, claimed by workers.** The orchestration decides when to create/close a session. Workers compete to claim unclaimed sessions — the orchestration never knows (or needs to know) which worker owns the session.

2. **Implemented as builtin system activities.** `create_session` and `close_session` follow the same pattern as `new_guid()` and `utc_now()` — they're registered as builtin activities (`__duroxide_syscall:create_session`, etc.) that execute on the worker and call provider methods. Results are recorded as `ActivityCompleted` events, making them deterministic on replay.

3. **Idempotent operations.** Since activities have at-least-once delivery guarantees, `create_session` with a duplicate ID is a no-op (returns the existing session_id), and `close_session` on a non-existent session is a no-op. No `SessionError` type needed — only infrastructure errors are possible.

4. **Exclusive routing after claim.** Once a worker claims a session, ONLY that worker can pick up work items for that session. Other workers skip them entirely.

5. **Session lock = death detection, not re-claim.** The claiming worker holds a lock on the session with a timeout + periodic refresh. The lock is tied to the **worker lifecycle**, not any single activity. If the worker dies and the lock expires, the session is **permanently invalidated** — it is NOT re-claimable by another worker. Re-claiming is pointless because the in-memory state (the whole reason for sessions) died with the worker. Instead, all pending work items for the dead session are failed with `session_lost`, and the orchestration is responsible for creating a new session and re-hydrating state on the new worker.

6. **Provider-optional.** Providers that don't implement sessions return `ProviderError` on any session method call. The runtime handles this gracefully.

## API Design

### OrchestrationContext Methods

```rust
/// Create a new session with a generated ID.
/// Returns the session_id.
/// Implemented as builtin activity: __duroxide_syscall:create_session
ctx.create_session() -> DurableFuture<Result<String, String>>

/// Create a new session with a specific ID.
/// If session already exists, this is a no-op and returns the same session_id.
/// Implemented as builtin activity: __duroxide_syscall:create_session_with_id
ctx.create_session_with_id(session_id: impl Into<String>) -> DurableFuture<Result<String, String>>

/// Schedule an activity routed to the session's worker.
/// The session_id is set on the WorkItem::ActivityExecute.
/// When a worker picks up the item, it claims the session if unclaimed,
/// or processes it if it already owns the session.
ctx.schedule_activity_on(
    session_id: impl Into<String>,
    name: impl Into<String>,
    input: impl Into<String>,
) -> DurableFuture<Result<String, String>>

/// Close a session. Non-existent session is a no-op.
/// Implemented as builtin activity: __duroxide_syscall:close_session
ctx.close_session(session_id: impl Into<String>) -> DurableFuture<Result<String, String>>
```

### Usage in Orchestration (Rust)

```rust
async fn my_orchestration(ctx: &OrchestrationContext) -> Result<String, String> {
    // Create session — returns session_id
    let session_id = ctx.create_session().await?;
    
    loop {
        // All activities route to the same worker
        let result = ctx.schedule_activity_on(&session_id, "runAgentTurn", &input).await;
        
        match result {
            Ok(output) => { /* process */ },
            Err(e) if e.contains("session_lost") => {
                // Worker died — create new session, re-hydrate
                let new_session = ctx.create_session().await?;
                // Continue with new session
            },
            Err(e) => return Err(e),
        }
        
        ctx.schedule_timer(60_000).await?;
    }
    
    ctx.close_session(&session_id).await?;
    Ok("done".into())
}
```

### Usage from Node.js SDK

```javascript
function* myOrchestration(ctx) {
    const sessionId = yield ctx.createSession();
    
    const result = yield ctx.scheduleActivityOn(sessionId, "runAgentTurn", input);
    
    yield ctx.closeSession(sessionId);
    return "done";
}
```

### Usage from Python SDK

```python
def my_orchestration(ctx):
    session_id = yield ctx.create_session()
    
    result = yield ctx.schedule_activity_on(session_id, "run_agent_turn", input)
    
    yield ctx.close_session(session_id)
    return "done"
```

## How It Works: End-to-End Flow

### 1. Session Creation

```
Orchestration                     Runtime                          Provider
    |                                |                                |
    |-- ctx.create_session() ------->|                                |
    |                                |-- emit Action::CallActivity    |
    |                                |   name: __duroxide_syscall:    |
    |                                |         create_session         |
    |                                |   input: ""                    |
    |                                |                                |
    |                                |-- (action → event →            |
    |                                |    ActivityScheduled →          |
    |                                |    WorkItem::ActivityExecute)   |
    |                                |                                |
    |                      [Worker picks up work item]                |
    |                                |                                |
    |                                |-- builtin handler executes --->|
    |                                |   generate guid                |
    |                                |   provider.create_session(id)  |
    |                                |                                |
    |                                |                  INSERT INTO sessions
    |                                |                  (session_id, worker_id=NULL,
    |                                |                   locked_until=NULL)
    |                                |                                |
    |                                |<-- Ok(session_id) ------------|
    |                                |                                |
    |                                |-- ActivityCompleted { result: session_id }
    |                                |   enqueued to orchestrator_queue
    |                                |                                |
    |<-- Ok("generated-uuid") ------|                                |
```

For `create_session_with_id("X")`:
- Same flow, but `input = "X"`
- Provider: `INSERT INTO sessions ... ON CONFLICT (session_id) DO NOTHING`
- Returns the session_id regardless (idempotent)

### 2. Activity Routing + Session Claim

```
Orchestration                     Runtime                          Provider
    |                                |                                |
    |-- ctx.schedule_activity_on --->|                                |
    |   (session_id, "doWork", data) |                                |
    |                                |-- emit Action::CallActivity    |
    |                                |   name: "doWork"               |
    |                                |   input: data                  |
    |                                |   session_id: Some("X")        |
    |                                |                                |
    |                                |-- WorkItem::ActivityExecute    |
    |                                |   { ..., session_id: Some("X")}|
    |                                |                                |
    |                                |   ack_orchestration_item       |
    |                                |   enqueues to worker_queue     |
    |                                |   with session_id = "X"        |
    |                                |                                |
    |              [Worker W fetches work item]                       |
    |                                |                                |
    |                                |   fetch_work_item(worker_id=W) |
    |                                |                                |
    |                                |   SELECT FROM worker_queue     |
    |                                |   WHERE session_id IS NULL     |
    |                                |      OR session IN (my sessions)
    |                                |      OR session IN (unclaimed) |
    |                                |                                |
    |                                |   [Found item for session "X", |
    |                                |    session unclaimed]          |
    |                                |                                |
    |                                |   ATOMICALLY:                  |
    |                                |   UPDATE sessions              |
    |                                |   SET worker_id = W,           |
    |                                |       locked_until = now+30s   |
    |                                |   WHERE session_id = "X"       |
    |                                |     AND worker_id IS NULL      |
    |                                |                                |
    |                                |   [Worker W now owns session,  |
    |                                |    spawns session lock refresh] |
    |                                |                                |
    |                                |-- execute activity "doWork" -->|
    |                                |                                |
    |                                |<-- ActivityCompleted ---------|
    |<-- Ok(result) ----------------|                                |
```

### 3. Subsequent Activities (Same Session, Already Claimed)

```
Worker W fetches next work item:

SELECT FROM worker_queue
WHERE session_id IS NULL
   OR session_id IN (SELECT session_id FROM sessions WHERE worker_id = 'W')
   -- unclaimed sessions no longer match "X" because it's claimed by W

Worker W sees work item for session "X" → already owns it → no claim needed → execute directly.

Worker Y fetches work item:
-- "X" is not in Y's sessions, and not unclaimed → Y skips it.
```

### 4. Session Close

```
Orchestration                     Runtime                          Provider
    |                                |                                |
    |-- ctx.close_session("X") ----->|                                |
    |                                |-- emit Action::CallActivity    |
    |                                |   name: __duroxide_syscall:    |
    |                                |         close_session          |
    |                                |   input: "X"                   |
    |                                |                                |
    |                      [Worker picks up work item]                |
    |                                |                                |
    |                                |-- builtin handler ----------->|
    |                                |   provider.close_session("X")  |
    |                                |                                |
    |                                |            DELETE FROM sessions
    |                                |            WHERE session_id="X"
    |                                |            (no-op if not found)|
    |                                |                                |
    |                                |<-- Ok("") -------------------|
    |                                |                                |
    |               [Worker stops session lock refresh for "X"]       |
    |                                |                                |
    |<-- Ok("") --------------------|                                |
```

## Worker Identity

### Worker ID

Each runtime process needs a stable, unique worker ID for session ownership. Two modes:

**Auto-generated (default):** The runtime generates a unique ID at startup. Format: `{hostname}-{pid}-{random}` or similar. Stable for the lifetime of the process.

**User-supplied:** The user provides a fixed node ID via `RuntimeOptions`. Useful for:
- Kubernetes pods with stable identities (StatefulSet)
- Testing with predictable IDs

Note: even with a fixed node ID, a restarted process loses all in-memory state. A session claimed by the previous incarnation is still invalidated — the orchestration must create a new session and re-hydrate.

```rust
RuntimeOptions {
    // ... existing options ...
    
    /// Fixed worker node ID for session affinity. If not set, a unique ID
    /// is generated per runtime instance.
    worker_node_id: Option<String>,
}
```

The worker_id used for session operations is:
- `worker_node_id` if provided by the user
- Otherwise, `runtime_id` (the existing 4-char hex, but we may want to make this longer/more unique)

### Worker ID in fetch_work_item

`fetch_work_item` gains a `worker_id` parameter:

```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
    worker_id: &str,              // NEW
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

The worker dispatcher passes its worker_id to every fetch call. Providers that don't support sessions ignore it and return any available work item (backward compatible).

## Provider Changes

### New Provider Trait Methods

```rust
/// Create a session record. Idempotent — duplicate session_id is a no-op.
/// Session is created with worker_id = NULL (unclaimed).
async fn create_session(
    &self,
    session_id: &str,
    instance_id: &str,
    now_ms: u64,
) -> Result<(), ProviderError>;

/// Close/delete a session. Idempotent — non-existent session is a no-op.
async fn close_session(
    &self,
    session_id: &str,
) -> Result<(), ProviderError>;

/// Renew the session lock. Called periodically by the worker that owns the session.
/// Returns error if the session doesn't exist, is closed, or is owned by a different worker.
async fn renew_session_lock(
    &self,
    session_id: &str,
    worker_id: &str,
    lock_timeout: Duration,
    now_ms: u64,
) -> Result<(), ProviderError>;
```

Default implementations return `ProviderError::permanent("not_supported", "sessions not implemented")`.

### Modified Provider Methods

**`fetch_work_item`** — gains `worker_id` parameter, query changes:

```sql
-- Fetch next available work item for this worker
SELECT q.id, q.work_item, q.attempt_count, q.session_id
FROM worker_queue q
WHERE q.visible_at <= $now
  AND (q.lock_token IS NULL OR q.locked_until <= $now)
  AND (
    q.session_id IS NULL                                          -- no session (regular activity)
    OR q.session_id IN (                                          -- session I already own
        SELECT s.session_id FROM sessions s WHERE s.worker_id = $worker_id
    )
    OR q.session_id IN (                                          -- unclaimed session
        SELECT s.session_id FROM sessions s WHERE s.worker_id IS NULL
    )
  )
ORDER BY q.id
LIMIT 1
FOR UPDATE OF q SKIP LOCKED;
```

When the fetched item has an unclaimed session, **atomically claim it** in the same transaction:

```sql
-- Claim unclaimed session
UPDATE sessions
SET worker_id = $worker_id,
    locked_until = $now_ms + $lock_timeout_ms
WHERE session_id = $session_id
  AND worker_id IS NULL;
```

If the UPDATE affects 0 rows (race — another worker claimed first), the work item fetch should be abandoned and retried. The `FOR UPDATE SKIP LOCKED` on worker_queue makes this unlikely but not impossible (two workers could fetch different items for the same unclaimed session).

**`ack_orchestration_item`** — when enqueuing `WorkItem::ActivityExecute` with `session_id`:

```sql
-- Enqueue work item with session_id
INSERT INTO worker_queue (work_item, visible_at, instance_id, execution_id, activity_id, session_id)
VALUES ($work_item, $now, $instance, $exec_id, $activity_id, $session_id);
```

### New Table: `sessions`

```sql
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    worker_id TEXT,                -- NULL = unclaimed
    instance_id TEXT NOT NULL,     -- orchestration that created it
    locked_until BIGINT,          -- NULL = unclaimed, else lock expiry (ms since epoch)
    created_at_ms BIGINT NOT NULL
);
```

### Modified Table: `worker_queue`

```sql
ALTER TABLE worker_queue ADD COLUMN session_id TEXT;
-- Index for session-aware fetch
CREATE INDEX idx_worker_queue_session ON worker_queue (session_id);
```

## Session Lock Lifecycle

### Lock Refresh

When a worker claims a session, it spawns a **session lock refresh task**. This is similar to activity lock refresh but operates at the worker lifecycle level:

```
Worker claims session "X"
  → sets locked_until = now + 30s
  → spawns refresh task:
      loop every 15s:
        provider.renew_session_lock("X", worker_id, 30s)
        if Err → session lost, trigger cascading failure
```

The refresh task runs independently of any specific activity. Multiple activities for the same session may execute sequentially, but the session lock persists across all of them.

### When Lock Refresh Stops

1. **Orchestration closes session** — `close_session("X")` deletes the session row → next refresh fails → task stops
2. **Worker shuts down** — refresh task is cancelled during graceful shutdown
3. **Worker crashes** — refresh task dies with the process → lock expires after timeout → session is **permanently invalidated** (not reclaimable — in-memory state is gone)

### Session Lock Failure Cascade

If the lock refresh fails (returns error from provider):

1. Worker marks session "X" as lost locally
2. **All in-progress activities** for session "X" are cancelled (via their cancellation tokens)
3. **All pending work items** in worker_queue for session "X" owned by this worker:
   - Converted to `ActivityFailed` with error: `"session_lost: session X lock expired"`
   - Enqueued to orchestrator_queue for the owning orchestration instance
   - Deleted from worker_queue
4. Orchestration receives `ActivityFailed(session_lost)` → can create a new session and retry

### ⚠️ Open Problem: In-Flight Work on Worker Death

When a worker **crashes** (as opposed to a graceful shutdown or lock refresh failure detected by the worker itself), there is an open question about how pending and in-flight work gets cleaned up:

1. **In-progress activities on the dead worker** — These are actively executing but the process is gone. The activity lock on the worker_queue row will eventually expire. On retry, the work item still has `session_id = "X"`, but:
   - Session "X" is invalidated (lock expired, worker gone)
   - No living worker owns session "X"
   - The work item matches neither "session I own" nor "unclaimed session" in any worker's fetch query
   - **Result: the work item is stranded** — no worker will ever pick it up

2. **Pending work items in worker_queue** — Items enqueued for session "X" but not yet picked up by any worker. Same stranding problem as above.

3. **Work items enqueued AFTER worker death but BEFORE session invalidation** — The orchestration doesn't know the worker is dead yet. It may schedule more activities on the dead session during the lock timeout window. These also get stranded.

**Possible solutions (needs further design):**

- **Periodic reaper** — A background task (in the runtime or provider) periodically scans for sessions with expired locks. For each expired session: mark as invalidated, convert all worker_queue items with that session_id to `ActivityFailed(session_lost)` in orchestrator_queue, delete them from worker_queue.

- **Detection during fetch** — When any worker's `fetch_work_item` encounters a work item for a session with an expired lock, it triggers cleanup for that session as a side effect.

- **Provider-level trigger** — The provider could run cleanup on a timer (e.g., PostgreSQL `pg_cron`, or a background tokio task in the runtime that calls a provider cleanup method).

The key challenge is that the dead worker can't clean up after itself. Some **external** mechanism must detect the expired session lock and fail the stranded work items. This is analogous to how activity lock expiry + retry handles individual activity failures, but sessions add the complexity that retried items can't be picked up by anyone.

**This needs careful design to avoid:**
- Work items stuck forever in worker_queue
- Orchestrations waiting forever for ActivityCompleted that will never arrive
- Race conditions between the reaper and a worker that's slow (not dead) renewing its lock

### Activity-Initiated Session Abort

An activity can also abort its session. This is useful when the activity detects that in-memory state is corrupted or the session should be abandoned:

```rust
// In activity handler:
activity_ctx.abort_session().await;
// → Same cascade as lock failure: all other activities for this session fail
```

Provider method:

```rust
/// Abort a session from within an activity. Deletes the session and
/// converts all pending work items for this session to ActivityFailed.
async fn abort_session(
    &self,
    session_id: &str,
) -> Result<(), ProviderError>;
```

## Poison Message Handling

Activities have at-least-once delivery. The existing poison message mechanism (attempt_count > max_attempts → `ActivityFailed` with `ErrorDetails::Poison`) applies to session activities too.

For `create_session` / `close_session` builtin activities, since they're idempotent, repeated execution is safe. The only failure mode is infrastructure errors (DB down), which would increment attempt_count and eventually poison.

For user activities routed via sessions:
- If the activity fails and is retried, it stays in the same session (session_id on the work item is preserved)
- The same worker picks it up again (exclusive routing)
- If the worker died between attempts, the session lock expires, and the retry will fail because no worker owns the session → eventually poisons or orchestration handles session_lost

## Session Visibility in Activities

The `ActivityContext` should expose the session_id so activities can use it to look up in-memory state:

```rust
impl ActivityContext {
    /// Returns the session_id if this activity was scheduled via schedule_activity_on.
    /// Returns None for regular activities.
    pub fn session_id(&self) -> Option<&str>;
}
```

This flows from the `WorkItem::ActivityExecute { session_id }` through to `ActivityContext` construction.

**Use case in durable-copilot-sdk:**
```javascript
// Activity handler
async function runAgentTurn(ctx, input) {
    const sessionId = ctx.sessionId;  // from activity context
    const copilotSession = sessionManager.get(sessionId);
    // ... use copilotSession for this turn
}
```

## Changes to WorkItem::ActivityExecute

```rust
WorkItem::ActivityExecute {
    instance: String,
    execution_id: u64,
    id: u64,
    name: String,
    input: String,
    session_id: Option<String>,   // NEW: for session routing
}
```

## Open Questions

1. **Session survival across `continueAsNew`** — When an orchestration continues as new, should the session survive? The new execution is a new instance_id. Options:
   - Session dies with the orchestration instance (strict) — new instance must create a new session
   - Session ID is passed in `continueAsNew` input — new instance creates session with same ID (idempotent, no-op, same worker keeps it)
   - Add `ctx.transfer_session(session_id)` that re-associates the session with the new instance_id

2. **Worker selection on claim** — First worker to fetch an unclaimed session's work item claims it. This is effectively random (whoever polls first). Should there be a bias mechanism (e.g., prefer least-loaded worker)?

3. **Max sessions per worker** — Should there be a configurable limit? If a worker accumulates too many sessions, new unclaimed-session work items could be skipped until another worker picks them up.

4. **Session timeout** — Should unclaimed sessions expire? If a session is created but no activity is ever scheduled on it, it sits in the table forever. Could add a TTL.

5. **Multiple sessions per orchestration** — The API allows creating multiple sessions (for different worker affinity needs). Is this a feature or should we restrict to one?

6. **Session-aware retry backoff** — If a session-routed activity fails and is retried, should the retry delay account for session lock timeout? E.g., if the worker died, the retry should wait until the session lock expires before another worker can claim it.
