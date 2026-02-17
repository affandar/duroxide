# Activity Implicit Sessions

> **⚠️ Superseded** by [Activity Implicit Sessions v2](activity-implicit-sessions-v2.md).
> This document is retained for historical reference. The v2 proposal is the accepted design.

**Status:** Draft  
**Created:** 2026-02-14  

## Summary

Add a session API to `OrchestrationContext` that provides **worker-process affinity for groups of activities**. An orchestration stamps a `session_id` on activities via `.on_session(id)`. The runtime guarantees all activities with the same `session_id` are dispatched to the same runtime process. Sessions are implicit — they exist when work items reference them and disappear when drained.

Sessions are a pure routing/affinity mechanism. No ordering guarantees, no transactional semantics, no provider-managed state. In-memory state management and checkpointing are the application's responsibility.

## Motivation

Some workloads need routing to a *specific* worker process — not because of hardware, but because of **in-memory state**.

**Primary use case: durable-copilot-sdk**

The durable-copilot-sdk keeps `CopilotSession` objects in memory on the worker process. These hold conversation history, auth tokens, and the copilot CLI child process. Each durable turn is an activity. All turns for the same conversation should route to the same worker, or the session context must be expensively recreated.

Today this works by running exactly 1 worker replica. Sessions would let us scale to N workers while keeping turns for the same conversation on the same worker.

**Other use cases:**
- ML model loaded in memory — inference activities route to the worker holding the model
- Database connection pools — queries route to the worker with the right pool
- File cache locality — processing activities route to the worker with cached data
- Any stateful activity sequence where recreating state is expensive

## Design Principles

1. **Sessions are implicit.** No `create_session` / `close_session` APIs. A session comes into existence when a work item with that `session_id` is enqueued and is cleaned up when the last work item for it is drained. This mirrors Azure Service Bus session semantics where sessions exist as long as messages reference them.

2. **Sessions are pure affinity.** No ordering guarantees (multiple session activities can execute concurrently on the owning worker), no provider-managed session state, no transactional semantics. The runtime routes; the app manages everything else.

3. **Sessions are re-claimable.** If a worker dies and its session lock expires, any other worker can claim the session. There is no "session permanently invalidated" concept. This eliminates the stranded-work-item problem entirely — work is never stuck.

4. **Session lock = activity-driven.** A session lock is extended only when work for *that specific session* is fetched or actively processed (lock renewed). If a worker stops touching a session, the lock expires and another worker can claim it. A worker doing unrelated work does not keep sessions alive.

5. **App-managed state lifecycle.** The application manages its own in-memory state (creation, checkpointing, eviction) using the `session_id` as a key. Duroxide does not provide hydration, dehydration, or notification callbacks. The app should use patterns like LRU caches and checkpoint-per-turn to handle worker death and idle expiry.

## API Design

### OrchestrationContext — `.on_session()` modifier

```rust
// Generate a session ID (deterministic for replay)
let session_id = ctx.new_guid().await?;

// Route activity to session-owning worker
let result = ctx.schedule_activity("run_turn", &input)
    .on_session(&session_id)
    .await?;
```

`.on_session()` is a modifier on `DurableFuture`, like `.with_tag()` in the activity tags proposal. Activities without `.on_session()` work exactly as they do today — any worker can pick them up.

### ActivityContext — `session_id()` getter

```rust
impl ActivityContext {
    /// Returns the session_id if this activity was scheduled via .on_session().
    /// Returns None for regular activities.
    pub fn session_id(&self) -> Option<&str>;
}
```

The activity uses this to look up or create process-local state:

```rust
"run_turn" => {
    let session_id = ctx.session_id().unwrap();
    let state = LOCAL_CACHE.get(session_id)
        .ok_or("unknown_session")?;
    let result = state.process(input).await;
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

    /// How long a session lock persists after the last activity for that
    /// session is fetched or renewed. If no activity for a session occurs
    /// within this duration, the lock expires and another worker can claim it.
    /// Default: 5 minutes.
    session_lock_timeout: Duration,
}
```

## Usage Patterns

### Basic: Stateful Activity Sequence

```rust
async fn my_orchestration(ctx: &OrchestrationContext) -> Result<String, String> {
    let session_id = ctx.new_guid().await?;

    // All three activities execute on the same worker
    ctx.schedule_activity("load_model", "llama-7b")
        .on_session(&session_id).await?;

    let a = ctx.schedule_activity("inference", "question 1")
        .on_session(&session_id).await?;

    let b = ctx.schedule_activity("inference", "question 2")
        .on_session(&session_id).await?;

    Ok(format!("{a}, {b}"))
}
```

### Copilot SDK: Hydrate/Checkpoint/Dehydrate

```rust
async fn copilot_agent(ctx: &OrchestrationContext) -> Result<String, String> {
    let session_id = ctx.new_guid().await?;

    // Hydrate state on whichever worker claims the session
    ctx.schedule_activity("hydrate_session", &config)
        .on_session(&session_id).await?;

    loop {
        let event = ctx.schedule_wait("user_message").await;

        match ctx.schedule_activity("run_turn", &event)
            .on_session(&session_id).await
        {
            Ok(result) => {
                // Checkpoint after every turn (app's choice)
                ctx.schedule_activity("checkpoint_state", "")
                    .on_session(&session_id).await?;
            }
            Err(e) if e.contains("unknown_session") => {
                // Worker lost session (died, lock expired). Re-hydrate.
                ctx.schedule_activity("hydrate_session", &config)
                    .on_session(&session_id).await?;
                continue;
            }
            Err(e) => return Err(e),
        }

        // Long wait — checkpoint, then durable timer
        if needs_long_wait {
            ctx.schedule_activity("checkpoint_state", "")
                .on_session(&session_id).await?;
            ctx.schedule_timer(Duration::from_secs(3600)).await;
            // After timer: next loop iteration's run_turn may land on different worker.
            // If it does → "unknown_session" → re-hydrate from checkpoint.
        }
    }
}
```

### Worker-Side State Management

```typescript
// App-level LRU cache — no duroxide involvement
const sessionCache = new LRUCache<string, CopilotSession>({
    max: 50,
    ttl: 10 * 60 * 1000,  // 10 min idle eviction
    dispose: (session) => session.destroy(),
});

// Activities use session_id as cache key
registerActivity("hydrate_session", async (ctx, config) => {
    const sessionId = ctx.sessionId!;
    const state = await loadFromCheckpointStore(sessionId) ?? freshState(config);
    sessionCache.set(sessionId, state);
    return "hydrated";
});

registerActivity("run_turn", async (ctx, input) => {
    const state = sessionCache.get(ctx.sessionId!);
    if (!state) throw new Error("unknown_session");
    return await state.process(input);
});

registerActivity("checkpoint_state", async (ctx) => {
    const state = sessionCache.get(ctx.sessionId!);
    if (!state) throw new Error("unknown_session");
    await saveToCheckpointStore(ctx.sessionId!, state);
    return "checkpointed";
});
```

## How It Works

### Session Claim and Routing

When a worker calls `fetch_work_item`, the provider returns work items based on:

1. **Non-session items** (`session_id IS NULL`) — always eligible
2. **Items for sessions this worker owns** — session row exists with this worker's ID and non-expired lock
3. **Items for unclaimed/expired sessions** — if the worker is below `max_sessions`, it can claim the session atomically during fetch

```sql
SELECT * FROM worker_queue q
WHERE q.visible_at <= $now
  AND (q.lock_token IS NULL OR q.locked_until <= $now)
  AND (
    q.session_id IS NULL
    OR q.session_id IN (
        SELECT s.session_id FROM sessions s
        WHERE s.worker_id = $worker_id AND s.locked_until > $now
    )
    OR (
        q.session_id NOT IN (
            SELECT s.session_id FROM sessions s
            WHERE s.locked_until > $now
        )
        AND (SELECT COUNT(*) FROM sessions
             WHERE worker_id = $worker_id AND locked_until > $now) < $max_sessions
    )
  )
ORDER BY q.id LIMIT 1
```

On fetching an item for an unclaimed/expired session, the provider atomically upserts:

```sql
INSERT INTO sessions (session_id, worker_id, locked_until)
VALUES ($sid, $worker_id, $now + $session_lock_timeout)
ON CONFLICT (session_id) DO UPDATE
SET worker_id = $worker_id, locked_until = $now + $session_lock_timeout
WHERE sessions.locked_until <= $now OR sessions.worker_id IS NULL;
```

### Session Lock Renewal

Session locks are renewed as a **side effect** of existing provider operations. No new background tasks, no new provider methods.

**When `fetch_work_item` returns a session-bound item:**
```sql
UPDATE sessions SET locked_until = $now + $session_lock_timeout
WHERE session_id = $sid AND worker_id = $worker_id;
```

**When `renew_work_item_lock` is called for a session-bound item:**
```sql
-- Provider looks up session_id from the work item being renewed
UPDATE sessions SET locked_until = $now + $session_lock_timeout
WHERE session_id = $resolved_session_id AND worker_id = $worker_id;
```

This means:
- A session lock stays alive as long as activities for that session are being fetched or executed
- A session with no activity for longer than `session_lock_timeout` expires
- A worker doing unrelated work does not keep sessions alive
- No new background tasks or timers in the runtime

### Session Lifecycle

```
t=0     Orchestration schedules activity.on_session("X")
        → ack_orchestration_item enqueues to worker_queue with session_id="X"
        → No session row yet. Item sits in queue.

t=1     Worker A fetch_work_item → sees unclaimed session "X" → claims it
        → INSERT INTO sessions (session_id="X", worker_id="A", locked_until=t+5min)
        → Returns work item. Session "X" now owned by Worker A.

t=2     Activity executing. spawn_activity_manager renews work item lock.
        → renew_work_item_lock piggbacks: UPDATE sessions SET locked_until=t+7min

t=3     Activity completes. ack_work_item deletes work item from worker_queue.
        → No more items for session "X" in queue.
        → Session row stays (locked_until=t+7min). Session is idle.

t=5     Orchestration schedules another activity.on_session("X")
        → Worker A fetch_work_item → session "X" still owned by A (lock not expired)
        → Session lock extended. Work continues on A.

t=60    No activity for session "X" for 55 minutes. Lock expired at t+7min.
        → Session row has locked_until in the past.
        → Next item for "X" can be claimed by any worker.
        → Session row can be lazily cleaned up.
```

### Session Cleanup

Session rows are transient bookkeeping. Cleanup options (provider choice):

**Option A — Lazy cleanup in `ack_work_item`:**
```sql
DELETE FROM sessions
WHERE session_id = $sid
  AND NOT EXISTS (SELECT 1 FROM worker_queue WHERE session_id = $sid);
```

**Option B — Periodic sweep:**
```sql
DELETE FROM sessions
WHERE locked_until < $now
  AND NOT EXISTS (SELECT 1 FROM worker_queue WHERE session_id = $sid);
```

Orphaned session rows (expired lock, no items) are harmless — they just take up a row. The next claim will overwrite them via the upsert.

## Data Model Changes

### New Table: `sessions`

```sql
CREATE TABLE sessions (
    session_id   TEXT PRIMARY KEY,
    worker_id    TEXT NOT NULL,
    locked_until BIGINT NOT NULL   -- ms since epoch
);
```

### Modified Table: `worker_queue`

```sql
ALTER TABLE worker_queue ADD COLUMN session_id TEXT;
CREATE INDEX idx_worker_queue_session ON worker_queue (session_id);
```

### Modified Types

**`Action::CallActivity`** — gains `session_id`:
```rust
Action::CallActivity {
    scheduling_event_id: u64,
    name: String,
    input: String,
    session_id: Option<String>,   // NEW
}
```

**`EventKind::ActivityScheduled`** — gains `session_id`:
```rust
ActivityScheduled {
    name: String,
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    session_id: Option<String>,   // NEW
}
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
    session_id: Option<String>,   // NEW
}
```

**`DurableFuture<T>`** — gains `.on_session()` modifier:
```rust
impl<T> DurableFuture<T> {
    pub fn on_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}
```

## Provider Contract Changes

### Modified Methods

**`fetch_work_item`** — gains `worker_id` and `max_sessions`:
```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
    worker_id: &str,            // NEW
    max_sessions: usize,        // NEW
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

The provider must:
1. Select a work item eligible for this worker (non-session, owned session, or claimable session within capacity)
2. If the item brings a new session claim, atomically upsert the `sessions` row
3. If the item is for an already-owned session, extend the session lock
4. Return the work item as normal

**`renew_work_item_lock`** — signature unchanged, but implementation must piggyback session lock renewal:
```rust
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_for: Duration,
) -> Result<(), ProviderError>;
// Implementation: if the work item has a session_id, also extend session lock
```

**`ack_orchestration_item`** — when enqueuing `WorkItem::ActivityExecute` to `worker_queue`, store the `session_id` on the row. No signature change.

**`ack_work_item`** — optionally clean up session rows when the last item for a session is acked. No signature change.

### No New Methods

There are no `create_session`, `close_session`, `renew_session_lock`, `get_session_state`, or `set_session_state` methods. Session management is entirely a side effect of existing fetch/renew/ack operations.

## Runtime Changes

### Worker Dispatcher

The worker dispatcher loop structure is unchanged. The only difference is passing `worker_id` and `max_sessions` to `fetch_work_item`:

```rust
// Before
store.fetch_work_item(lock_timeout, poll_timeout)

// After
store.fetch_work_item(lock_timeout, poll_timeout, &worker_id, max_sessions)
```

No new background tasks. No session tracker. No session lock renewal loop.

### Orchestration Dispatcher

No changes. The orchestration dispatcher passes `session_id` through as data, from `Action` → `Event` → `WorkItem`. It has no awareness of session ownership or routing.

### Replay Engine

The replay engine flows `session_id` from `Action::CallActivity` through to `EventKind::ActivityScheduled` and into `WorkItem::ActivityExecute`, identical to how any other field (name, input) flows through. On replay, `session_id` is restored from the `ActivityScheduled` event in history.

## Known Limitations

### No FIFO ordering

Multiple activities for the same session can execute concurrently on the owning worker. If the application needs ordered execution, it should schedule activities sequentially in the orchestration (await each before scheduling the next).

### Checkpoint gap on worker death

If a worker dies, any in-memory state modified since the last checkpoint is lost. The application is responsible for checkpoint frequency. Common patterns:

- Checkpoint after every turn (one extra activity per turn)
- Checkpoint inside the activity before returning (transparent to orchestration)
- Accept the loss (if the underlying system has its own durability, e.g., Copilot SDK's events.jsonl on shared storage)

## The Session Migration Problem

The most significant design tension with implicit sessions is **silent session migration**. Because sessions are pure affinity with no explicit lifecycle, neither the orchestration nor the worker receives a signal when session affinity is lost and re-established elsewhere.

### The Problem

Consider a copilot agent orchestration:

```
t=0     Worker A claims session "X", hydrates CopilotSession into memory
t=1     run_turn activity completes successfully on Worker A
t=2     Orchestration waits for user input (schedule_wait). No work items in queue.
t=7     session_lock_timeout (5 min) expires. Session "X" is now unclaimed.
        Worker A still holds CopilotSession in memory — no signal to clean up.

--- minutes later ---

t=15    User sends message. Orchestration schedules run_turn.on_session("X").
t=15    Worker B claims session "X" (Worker A's lock expired).
t=15    run_turn on Worker B: no local state → returns Err("unknown_session")
t=15    Orchestration catches error, schedules hydrate.on_session("X") on Worker B
t=15    Worker B hydrates from last checkpoint (e.g., t=1). Resumes.
```

**Two problems:**

1. **Worker A leaks memory.** It still holds the CopilotSession. It never learns that session "X" migrated. The state sits in memory until the app's LRU cache evicts it.

2. **State loss.** If Worker A modified in-memory state after the last checkpoint (t=1) but before the session expired (t=7), that work is lost. Worker B hydrates from the t=1 checkpoint.

### Mitigation Options

#### Option A: Reactive detection via "unknown_session" error

The simplest approach. Don't try to detect migration proactively. Let the next activity fail and re-hydrate.

```rust
match ctx.schedule_activity("run_turn", &event)
    .on_session(&session_id).await
{
    Ok(result) => { /* success */ }
    Err(e) if e.contains("unknown_session") => {
        // Session migrated. Re-hydrate on new worker.
        ctx.schedule_activity("hydrate", &config)
            .on_session(&session_id).await?;
    }
    Err(e) => return Err(e),
}
```

Worker side uses an LRU cache with TTL-based eviction to bound memory leaks:
```typescript
const sessionCache = new LRUCache<string, CopilotSession>({
    max: 50,
    ttl: 10 * 60 * 1000,  // evict after 10 min idle
    dispose: (session) => session.destroy(),
});
```

**Tradeoff:** One wasted activity round-trip on migration. Memory leak bounded by LRU TTL but not eliminated. State since last checkpoint is lost.

#### Option B: Session keepalive activity

The orchestration runs a long-lived "parking" activity on the session in parallel with the wait. This activity does nothing except keep the session lock alive (via work-item lock renewal → piggybacked session lock renewal):

```rust
loop {
    let user_input = ctx.schedule_wait("user_message");
    let keepalive = ctx.schedule_activity("session_keepalive", "")
        .on_session(&session_id);

    match ctx.select2(user_input, keepalive).await {
        Either2::First(event) => {
            // Keepalive is cancelled (select loser). Session lock was maintained
            // the entire time because the keepalive activity was in-flight.
            ctx.schedule_activity("run_turn", &event)
                .on_session(&session_id).await?;
        }
        Either2::Second(_) => {
            // Keepalive ended unexpectedly (worker died, lock lost).
            // Re-hydrate on next iteration.
        }
    }
}
```

Worker side:
```rust
"session_keepalive" => {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        if ctx.is_cancelled() { return Ok("done"); }
    }
}
```

**How it works:** As long as the keepalive activity is in-flight, `spawn_activity_manager` renews its work-item lock periodically, which piggybacks session lock renewal. The session stays claimed by this worker for the entire duration of the wait. If the worker dies, the keepalive dies, both locks expire, and the session becomes claimable.

**Tradeoff:** One worker concurrency slot is consumed per active session while waiting. With `worker_concurrency: 10` and 5 sessions, half the slots are parked. The app controls this cost by choosing which waits need keepalive (long timer → maybe not worth it; waiting for user input that usually comes in seconds → worth it).

#### Option C: Provider-raised migration event

When a session is re-claimed by a different worker, the provider enqueues a duroxide external event to the orchestration. This requires the sessions table to track which orchestration instance owns the session:

```sql
CREATE TABLE sessions (
    session_id   TEXT PRIMARY KEY,
    worker_id    TEXT NOT NULL,
    instance_id  TEXT,              -- orchestration that uses this session
    locked_until BIGINT NOT NULL
);
```

On re-claim (inside `fetch_work_item`):
```sql
-- Before overwriting, read the old owner
SELECT instance_id FROM sessions WHERE session_id = $sid;

-- After claiming, enqueue event to the orchestration
INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
VALUES ($old_instance_id, 
        '{"ExternalRaised":{"instance":"...","name":"__session_migrated","data":"X"}}',
        $now);
```

Orchestration side:
```rust
let user_input = ctx.schedule_wait("user_message");
let migration = ctx.schedule_wait("__session_migrated");

match ctx.select2(user_input, migration).await {
    Either2::First(event) => {
        ctx.schedule_activity("run_turn", &event)
            .on_session(&session_id).await?;
    }
    Either2::Second(migrated_session_id) => {
        // Proactive notification: session moved to another worker.
        // Re-hydrate before next turn.
        ctx.schedule_activity("hydrate", &config)
            .on_session(&session_id).await?;
    }
}
```

**Tradeoff:** No wasted concurrency slots. Proactive notification. But:
- Adds `instance_id` to sessions table (coupling provider to orchestration identity).
- The `__session_migrated` event is generated by the provider, not the orchestration. This is a new pattern — events today come from `Client::raise_event()` or the runtime. Replay implications need careful thought: on replay, the event is already in history so it works, but the provider must not re-generate it.
- The `.on_session()` modifier would need to carry the `instance_id` through to the provider so it can be stored in the sessions table.

### Comparison

| | Option A: Reactive | Option B: Keepalive | Option C: Provider event |
|---|---|---|---|
| **Complexity** | None | Activity + select pattern | Provider + sessions table + event |
| **Detection latency** | One failed activity | Immediate (lock maintained) | On re-claim |
| **Concurrency cost** | None | 1 slot per waiting session | None |
| **Memory leak on old worker** | Bounded by LRU TTL | Prevented (select cancels keepalive cleanly) | Worker still needs LRU cleanup |
| **State loss** | Last checkpoint gap | No gap (lock never lost) | Last checkpoint gap |
| **Provider changes** | None | None | `instance_id` on sessions, event generation |
| **New duroxide primitives** | None | None | Provider-generated external events |

**Recommendation:** Option A is sufficient for most use cases — the app already needs to handle the "unknown_session" case for worker death. Option B is the right choice when maintaining session affinity during waits is important and the concurrency cost is acceptable. Option C is the most complete solution but adds significant provider complexity and should only be pursued if demand justifies it.

## Backward Compatibility

- Activities without `.on_session()` are completely unaffected (session_id = None)
- `fetch_work_item` gains required parameters — providers must update their implementation
- The `session_id` field on events uses `#[serde(skip_serializing_if = "Option::is_none")]` and `#[serde(default)]` for backward-compatible serialization — old events without the field deserialize with `session_id = None`
- Old providers that don't support sessions will need updating to accept the new `fetch_work_item` parameters. Default trait implementations can return `ProviderError::permanent("sessions not supported")` if `worker_id` is non-empty — but this is a breaking change to the trait signature.

## Implementation Order

1. Add `session_id: Option<String>` to `Action::CallActivity`, `EventKind::ActivityScheduled`, `WorkItem::ActivityExecute`
2. Add `.on_session()` modifier to `DurableFuture`
3. Add `session_id()` getter to `ActivityContext`
4. Add `max_sessions_per_runtime` and `session_lock_timeout` to `RuntimeOptions`
5. Update `Provider::fetch_work_item` signature with `worker_id` and `max_sessions`
6. Add SQLite migration: `sessions` table, `worker_queue.session_id` column
7. Implement session-aware fetch query in SQLite provider
8. Implement session lock piggyback in `renew_work_item_lock`
9. Update replay engine to flow `session_id` through action → event → work item
10. Update worker dispatcher to pass `worker_id` and `max_sessions`
11. Add `ActivityWorkContext.session_id` and flow to `ActivityContext`
12. Add provider validation tests
13. Add integration tests (session routing, re-claim on expiry, max capacity)
14. Update documentation

## Open Questions

1. **`session_lock_timeout` default** — 5 minutes feels right for the copilot use case (turns happen within seconds). For longer-idle workloads, users would increase this. Is 5 minutes a good default?

2. **`max_sessions_per_runtime` default** — 100 is arbitrary. Should this be proportional to `worker_concurrency`? Or is a flat limit simpler?

3. **Interaction with activity tags** — If both features ship, can an activity have both `.on_session()` and `.with_tag()`? Semantically: tag selects the worker pool, session selects the worker within the pool. This is composable but the `fetch_work_item` query becomes complex.

4. **Worker ID stability** — The worker_id is currently `"work-{idx}-{runtime_id}"` where `runtime_id` is a 4-char hex regenerated on each start. This means a restarted process gets a new worker_id and cannot reclaim its own sessions. Should `RuntimeOptions` accept a user-supplied `worker_node_id` for stable identity (e.g., Kubernetes StatefulSet pod names)?

5. **Multiple sessions per orchestration** — The API allows an orchestration to create multiple sessions (different `new_guid()` calls). Is this useful or should it be restricted?
