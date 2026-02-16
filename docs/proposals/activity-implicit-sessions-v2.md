# Activity Implicit Sessions (v2)

**Status:** Accepted  
**Created:** 2026-02-14  
**Revised:** 2026-02-16  
**Supersedes:** `activity-implicit-sessions.md`

## Summary

Add session-based worker affinity so activities with the same `session_id` are routed to the same worker process. An orchestration stamps a `session_id` on activities via `ctx.schedule_activity_on_session(name, input, session_id)`. The runtime guarantees all activities with the same `session_id` are dispatched to the same runtime process for as long as the session is owned.

Sessions are a pure routing/affinity mechanism. No ordering guarantees, no transactional semantics, no provider-managed state. In-memory state management and checkpointing are the application's responsibility.

**Key changes from v1:**
- `schedule_activity_on_session(name, input, session_id)` replaces `.on_session()` builder pattern
- Two-timeout model: `session_lock_timeout` (heartbeat lease) + `session_idle_timeout` (inactivity expiry)
- Idle timeout managed by the session renewal thread (stops heartbeating idle sessions)
- Dedicated `cleanup_orphaned_sessions()` provider method (runs on a separate interval)
- `renew_session_lock()` provider method (renamed from `heartbeat_sessions`)
- User-supplied `worker_node_id` for stable identity across restarts
- Session features are **required** for all provider implementations

---

## Motivation

Some workloads need routing to a *specific* worker process — not because of hardware, but because of **in-memory state**.

**Primary use case: durable-copilot-sdk**

The durable-copilot-sdk keeps `CopilotSession` objects in memory on the worker process (see `session-manager.ts`). These hold conversation history, auth tokens, and the copilot CLI child process. Each durable turn is an activity. All turns for the same conversation should route to the same worker, or the session context must be expensively recreated.

Today this works by running exactly 1 worker replica. Sessions would let us scale to N workers while keeping turns for the same conversation on the same worker.

**Other use cases:**
- ML model loaded in memory — inference activities route to the worker holding the model
- Database connection pools — queries route to the worker with the right pool
- File cache locality — processing activities route to the worker with cached data
- Any stateful activity sequence where recreating state is expensive

---

## Design Principles

1. **Sessions are implicit.** No `create_session` / `close_session` APIs. A session comes into existence when a work item with that `session_id` is enqueued and is cleaned up when drained.

2. **Sessions are pure affinity.** No ordering guarantees (multiple session activities can execute concurrently on the owning worker), no provider-managed session state, no transactional semantics. The runtime routes; the app manages everything else.

3. **Sessions are re-claimable.** If a worker dies and its session lock expires, any other worker can claim the session. There is no "session permanently invalidated" concept. This eliminates the stranded-work-item problem.

4. **Two-timeout model.** Session lock timeout (heartbeat lease) controls crash recovery speed. Session idle timeout controls how long affinity persists after inactivity. These are independent concerns.

5. **App-managed state lifecycle.** The application manages its own in-memory state (creation, checkpointing, eviction) using the `session_id` as a key. Duroxide does not provide hydration, dehydration, or notification callbacks.

6. **Required for all providers.** Session support is a mandatory part of the Provider trait, not an optional extension.

---

## API Design

### OrchestrationContext — `schedule_activity_on_session()`

```rust
// Generate a session ID (deterministic for replay)
let session_id = ctx.new_guid().await?;

// Route activity to session-owning worker
let result = ctx.schedule_activity_on_session("run_turn", &input, &session_id).await?;
```

`schedule_activity_on_session` is a dedicated method (not a builder modifier). The `session_id` is baked into `Action::CallActivity` at emit time, which is cleaner than retroactive mutation of an already-emitted action.

```rust
impl OrchestrationContext {
    /// Schedule an activity routed to the worker owning the given session.
    ///
    /// If no worker owns the session, any worker can claim it on first fetch.
    /// Once claimed, all subsequent activities with the same `session_id` route
    /// to the claiming worker until the session unpins (idle timeout or worker death).
    pub fn schedule_activity_on_session(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        session_id: impl Into<String>,
    ) -> DurableFuture<Result<String, String>>

    /// Typed version with serde serialization/deserialization.
    pub fn schedule_activity_on_session_typed<In, Out>(
        &self,
        name: impl Into<String>,
        input: &In,
        session_id: impl Into<String>,
    ) -> impl Future<Output = Result<Out, String>>
    where
        In: serde::Serialize,
        Out: serde::de::DeserializeOwned,
}
```

Activities without `schedule_activity_on_session` work exactly as they do today — any worker can pick them up.

### ActivityContext — `session_id()` getter

```rust
impl ActivityContext {
    /// Returns the session_id if this activity was scheduled via schedule_activity_on_session.
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
    /// Lock timeout for session heartbeat lease.
    /// Controls crash recovery speed — if a worker dies, its sessions become
    /// claimable after this duration.
    /// Default: 30 seconds
    pub session_lock_timeout: Duration,

    /// Buffer time before session lock expiration to trigger renewal.
    /// Uses the same formula as worker_lock_renewal_buffer.
    /// Default: 5 seconds
    pub session_lock_renewal_buffer: Duration,

    /// How long a session stays pinned after the last activity is
    /// fetched, renewed, or completed. The session renewal thread
    /// stops heartbeating idle sessions, so their locks naturally expire.
    /// Default: 5 minutes
    pub session_idle_timeout: Duration,

    /// How often orphaned session rows are swept from the sessions table.
    /// Runs on the same background thread as session lock renewal.
    /// Default: 5 minutes
    pub session_cleanup_interval: Duration,

    /// Maximum number of sessions this worker will own concurrently.
    /// Worker skips unclaimed sessions when at capacity.
    /// Session activities and non-session activities share the same
    /// worker_concurrency slots.
    /// Default: 10
    pub max_sessions_per_worker: usize,

    /// Stable worker identity for session ownership.
    /// If set, used as the worker_id for session claims. Allows a restarted
    /// worker to reclaim its own sessions without waiting for lock expiry.
    /// Example: Kubernetes StatefulSet pod name.
    /// If None, uses ephemeral runtime_id-based identity (sessions cannot
    /// survive restarts).
    /// Default: None
    pub worker_node_id: Option<String>,
}
```

---

## Two-Timeout Model

### `session_lock_timeout` (heartbeat lease — default 30s)

A heartbeat lease renewed periodically by a background task via `renew_session_lock()`. If the worker crashes and stops heartbeating, the lock expires in ≤ 30s, allowing another worker to claim the session quickly.

Analogous to `worker_lock_timeout` for activity work items.

### `session_idle_timeout` (inactivity expiry — default 5 min)

Tracks `last_activity_at` — the last time work flowed through the session (activity fetched, lock renewed, or ack'd). The session renewal thread checks idle timeout and **stops heartbeating idle sessions**. Once heartbeating stops, the lock naturally expires after `session_lock_timeout`.

**Total unpin latency for idle sessions:** `session_idle_timeout` + up to `session_lock_timeout` (idle detection + lock expiry).

### How They Interact

A session is **owned** when `locked_until > now`. Idle timeout is managed by the renewal thread, not the fetch query. This gives a single ownership concept everywhere:

- **`fetch_work_item`** claimability check: `locked_until < $now` (one condition)
- **`renew_session_lock`** filters: `WHERE last_activity_at + $idle_timeout > $now` (stops renewing idle sessions)
- **Crash recovery:** Worker dies → heartbeat stops → lock expires in ≤ `session_lock_timeout`
- **Idle unpin:** Worker alive, no activity flow → renewal thread stops heartbeating → lock expires in ≤ `session_lock_timeout`
- **Active session:** Activity lock renewals piggyback `last_activity_at = now` → renewal thread keeps heartbeating → lock stays fresh

### `last_activity_at` Updates (piggybacked on existing provider calls)

| Provider method | When | Effect |
|---|---|---|
| `fetch_work_item` | Returns a session-bound item | `last_activity_at = now` |
| `renew_work_item_lock` | For a session-bound item | `last_activity_at = now` |
| `ack_work_item` | For a session-bound item | `last_activity_at = now` |

No new methods needed for `last_activity_at` tracking.

---

## Session Background Task

A single task per runtime (not per worker slot, not per session). Renewal runs on a tight interval; cleanup runs on a longer interval within the same loop.

```
Session Manager Task (1 per runtime)
├── Every ~25s (session_lock_timeout - buffer): renew_session_lock()
└── Every ~5min (session_cleanup_interval):     cleanup_orphaned_sessions()
```

The task uses a tick counter to determine when cleanup should run:

```rust
// Single background task
tokio::spawn(async move {
    let renewal_interval = calculate_renewal_interval(
        session_lock_timeout, session_lock_renewal_buffer
    );
    let mut interval = tokio::time::interval(renewal_interval);
    let cleanup_every_n_ticks =
        (cleanup_interval.as_millis() / renewal_interval.as_millis()).max(1);
    let mut ticks_since_cleanup = 0u64;

    loop {
        interval.tick().await;
        if shutdown.load(Ordering::Relaxed) { break; }

        // Always renew (skips idle sessions internally)
        let _ = store.renew_session_lock(
            &worker_id, session_lock_timeout, session_idle_timeout
        ).await;

        // Periodically cleanup
        ticks_since_cleanup += 1;
        if ticks_since_cleanup >= cleanup_every_n_ticks {
            ticks_since_cleanup = 0;
            let _ = store.cleanup_orphaned_sessions(session_idle_timeout).await;
        }
    }
});
```

---

## Data Model Changes

### New Table: `sessions`

```sql
CREATE TABLE sessions (
    session_id     TEXT PRIMARY KEY,
    worker_id      TEXT NOT NULL,
    locked_until   INTEGER NOT NULL,    -- ms since epoch (heartbeat lease)
    last_activity_at INTEGER NOT NULL   -- ms since epoch (last work flow)
);
```

### Modified Table: `worker_queue`

```sql
ALTER TABLE worker_queue ADD COLUMN session_id TEXT;
CREATE INDEX idx_worker_queue_session ON worker_queue(session_id);
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

---

## Provider Contract Changes

All changes are **required** for all provider implementations.

### Modified Methods

**`fetch_work_item`** — gains session routing parameters:
```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
    worker_id: &str,                 // NEW
    session_lock_timeout: Duration,  // NEW — for upsert locked_until
    max_sessions: usize,             // NEW — capacity limit
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

The provider must:
1. Select eligible work items: non-session + owned-session + claimable-session (under capacity)
2. If the item brings a new session claim, atomically upsert the `sessions` row
3. If the item is for an already-owned session, update `last_activity_at = now`
4. Return the work item as normal

Fetch query logic:
```sql
SELECT * FROM worker_queue q
WHERE q.visible_at <= $now
  AND (q.lock_token IS NULL OR q.locked_until <= $now)
  AND (
    q.session_id IS NULL                                        -- non-session
    OR q.session_id IN (                                        -- owned session
        SELECT s.session_id FROM sessions s
        WHERE s.worker_id = $worker_id AND s.locked_until > $now
    )
    OR (                                                        -- claimable session
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

On fetching a session-bound item, atomically upsert the sessions row:
```sql
INSERT INTO sessions (session_id, worker_id, locked_until, last_activity_at)
VALUES ($sid, $worker_id, $now + $session_lock_timeout, $now)
ON CONFLICT (session_id) DO UPDATE
SET worker_id = $worker_id,
    locked_until = $now + $session_lock_timeout,
    last_activity_at = $now
WHERE sessions.locked_until <= $now OR sessions.worker_id = $worker_id;
```

**`renew_work_item_lock`** — signature unchanged, implementation adds `last_activity_at` piggyback:
```rust
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_for: Duration,
) -> Result<(), ProviderError>;

// Implementation: if the work item has a session_id, also update last_activity_at
// UPDATE sessions SET last_activity_at = $now
// WHERE session_id = (SELECT session_id FROM worker_queue WHERE lock_token = $token)
//   AND worker_id = $worker_id;
```

**`ack_work_item`** — signature unchanged, implementation adds `last_activity_at` piggyback:
```rust
async fn ack_work_item(
    &self,
    token: &str,
    completion: Option<WorkItem>,
) -> Result<(), ProviderError>;

// Implementation: if the work item has a session_id, update last_activity_at = now
```

**`ack_orchestration_item`** — signature unchanged. When enqueuing `WorkItem::ActivityExecute` to `worker_queue`, store the `session_id` on the row.

### New Methods

**`renew_session_lock`** — heartbeat all non-idle sessions owned by this worker:
```rust
async fn renew_session_lock(
    &self,
    worker_id: &str,
    extend_for: Duration,
    idle_timeout: Duration,
) -> Result<usize, ProviderError>;
```

```sql
UPDATE sessions SET locked_until = $now + $extend_for
WHERE worker_id = $worker_id
  AND locked_until > $now
  AND last_activity_at + $idle_timeout > $now;
```

Returns count of sessions renewed.

**`cleanup_orphaned_sessions`** — sweep orphaned session rows:
```rust
async fn cleanup_orphaned_sessions(
    &self,
    idle_timeout: Duration,
) -> Result<usize, ProviderError>;
```

```sql
DELETE FROM sessions
WHERE locked_until < $now
  AND NOT EXISTS (SELECT 1 FROM worker_queue WHERE session_id = sessions.session_id);
```

Returns count of rows deleted. Any worker can sweep any worker's orphans.

**Race safety with `ack_orchestration_item`:** If cleanup runs concurrently with an `ack_orchestration_item` that is inserting a new work item for the same session, cleanup might delete the session row before the work item insert commits. This is harmless — the next `fetch_work_item` sees no session row and treats it as an unclaimed session, creating a fresh row via upsert. For SQLite specifically, this race cannot occur (writes are serialized).

---

## Runtime Changes

### Worker Dispatcher

- Resolve `worker_id` from `RuntimeOptions::worker_node_id`. If set: `format!("work-{worker_idx}-{node_id}")`. Otherwise: existing ephemeral `format!("work-{worker_idx}-{runtime_id}")`.
- Pass `worker_id`, `session_lock_timeout`, and `max_sessions_per_worker` to `fetch_work_item`.
- Extract `session_id` from `WorkItem::ActivityExecute` into `ActivityWorkContext`.
- Spawn a single "session manager" background task per runtime for lock renewal + cleanup.

### Orchestration Dispatcher

No changes. The orchestration dispatcher passes `session_id` through as data, from `Action` → `Event` → `WorkItem`. It has no awareness of session ownership or routing.

### Replay Engine

The replay engine flows `session_id` from `Action::CallActivity` through to `EventKind::ActivityScheduled` and into `WorkItem::ActivityExecute`, identical to how any other field (name, input) flows through:

- `action_to_event`: include `session_id` from action in event
- `update_action_event_id`: preserve `session_id`
- `action_matches_event_kind`: include `session_id` in match comparison

On replay, `session_id` is restored from the `ActivityScheduled` event in history.

### Execution Layer

`Action::CallActivity` → `WorkItem::ActivityExecute` conversion copies `session_id` through.

---

## Session Lifecycle

```
t=0     Orchestration calls schedule_activity_on_session("RunTurn", input, "session-X")
        → emit_action(Action::CallActivity { session_id: Some("session-X"), .. })

t=1     Replay engine: action → ActivityScheduled event (with session_id) + pending_action
        Execution layer: pending_action → WorkItem::ActivityExecute { session_id: Some("session-X") }
        ack_orchestration_item: INSERT worker_queue (session_id = "session-X")

t=2     Worker A fetch_work_item → session "session-X" not yet in sessions table → claim it
        → UPSERT sessions (session_id="session-X", worker_id="A", locked_until=t+30s, last_activity_at=t)

t=3     Activity executing. spawn_activity_manager renews work item lock every ~25s.
        → renew_work_item_lock piggybacks: UPDATE sessions SET last_activity_at = now

t=5     Activity completes. ack_work_item piggybacks: last_activity_at = now.
        → Session row: worker_id="A", locked_until refreshed by heartbeat, last_activity_at=t+5

t=6     Session manager background task renews lock: locked_until = now + 30s
        → Session stays owned by A, last_activity_at=t+5

t=310   5 minutes since t+5. Renewal thread detects last_activity_at + 5min < now.
        → Stops heartbeating session "session-X".

t=340   locked_until expires (30s after last renewal). Session claimable by any worker.

t=500   Next activity for "session-X": any worker can claim it.
        Cleanup task sweeps the expired row if no work items reference it.
```

---

## Orthogonality Matrix — Interaction with Framework Features

### Continue-As-New

ContinueAsNew terminates the current execution (execution_id + 1) and cancels in-flight activities via lock stealing. **Sessions survive ContinueAsNew** — the session row persists, and the new execution can schedule activities on the same `session_id` (passed through the `continue_as_new` input). Activities route to the same worker.

**Edge case — ContinueAsNew during session activity execution:** The in-flight activity is cancelled via lock stealing. The worker detects this through failed `renew_work_item_lock`. Meanwhile, the new execution may schedule a replacement activity on the same session. The worker is still the session owner, so it picks up the new activity. The cancelled activity's completion (if it races) has a stale `execution_id` and is ignored.

### Poison Message Handling

A session-bound activity that exceeds `max_attempts` is handled identically to non-session activities — `handle_poison_message` acks with `ActivityFailed(Poison)`. The session itself is NOT poisoned; only the specific activity fails. The session remains owned by the worker, and subsequent session activities work fine.

**Edge case — session item keeps getting abandoned:** Each abandon returns the item to the queue. Since the session is owned by this worker, the same worker fetches it again. The poison limit prevents infinite loops. No special handling needed.

### Activity Cancellation (Lock Stealing)

Lock stealing deletes worker queue entries atomically in `ack_orchestration_item`. Session-bound activities are deleted like any other. The session row is unaffected — session remains owned. If the orchestration schedules a replacement activity on the same session, it routes to the same worker.

### Sub-Orchestrations

Sub-orchestrations are independent instances with their own context. They can call `schedule_activity_on_session` with a `session_id` passed from the parent via input. **Sessions are NOT scoped to an orchestration instance** — they are a worker-queue-level routing concept. Multiple orchestration instances can share the same `session_id`.

### Instance Deletion

`delete_instance` (force=true) deletes worker queue entries atomically. Session rows are NOT deleted (deletion doesn't know about sessions). The session becomes orphaned — `last_activity_at` stops advancing, renewal thread stops heartbeating, lock expires, `cleanup_orphaned_sessions` sweeps it.

### Abandon / Retry

When a session-bound activity is abandoned, it returns to the queue with `session_id` still set. The session is still owned by this worker, so the same worker picks it up again. This is correct — abandon is for transient failures, and the session state is still on this worker.

### Unregistered Activity Handling (Rolling Upgrades)

If a worker receives a session-bound activity it doesn't have registered, it abandons with backoff. The item returns to the queue. Since the session is pinned, the same worker fetches it again, and it eventually poisons.

**Important caveat:** Unlike non-session activities which can fail over to an upgraded worker during rolling deployments, session-bound activities are stuck on the owning worker until the session unpins. **Rolling upgrade guidance:** Session-bound activity handlers must be registered on all workers before orchestrations schedule activities on sessions. When changing session-bound activity handler code, the upgrade window should complete within `session_idle_timeout`, or accept that in-flight sessions may run old code until they naturally unpin.

---

## Session Migration Problem

When a session unpins (idle timeout or worker death) and a new worker claims it, the application's in-memory state on the old worker is stale. This is the fundamental tradeoff of implicit sessions.

### Recommended Pattern: Reactive Detection

```rust
match ctx.schedule_activity_on_session("run_turn", &event, &session_id).await {
    Ok(result) => { /* success */ }
    Err(e) if e.contains("unknown_session") => {
        // Session migrated. Re-hydrate on new worker.
        ctx.schedule_activity_on_session("hydrate", &config, &session_id).await?;
    }
    Err(e) => return Err(e),
}
```

Worker side uses an LRU cache with TTL-based eviction:
```rust
// Bounded in-memory state — evicts stale sessions automatically
let session_cache = LruCache::new(NonZeroUsize::new(50).unwrap());
```

### Alternative: Keepalive Activity

For scenarios where session migration must be prevented during waits:

```rust
loop {
    let user_input = ctx.schedule_wait("user_message");
    let keepalive = ctx.schedule_activity_on_session(
        "session_keepalive", "", &session_id
    );

    match ctx.select2(user_input, keepalive).await {
        Either2::First(event) => {
            ctx.schedule_activity_on_session("run_turn", &event, &session_id).await?;
        }
        Either2::Second(_) => { /* keepalive ended unexpectedly, re-hydrate */ }
    }
}
```

The keepalive activity loops sleeping + checking cancellation. Its lock renewal piggybacks `last_activity_at`, preventing idle unpin. **Cost:** One worker concurrency slot consumed per waiting session.

---

## Backward Compatibility

| Scenario | Behavior |
|---|---|
| Existing `schedule_activity` calls | Unchanged — `session_id = None`, no session routing |
| Existing RuntimeOptions | New fields have defaults (30s lock, 5min idle, 5min cleanup, 10 max, None node_id) |
| Existing provider implementations | **Must update** — new `fetch_work_item` signature, 2 new methods |
| Old `ActivityScheduled` events without `session_id` | Deserialize with `session_id = None` via `#[serde(default)]` |
| Old `WorkItem::ActivityExecute` without `session_id` | Same — backward-compatible serde |

**Migration sequence for providers:**
1. Deploy schema migration (add `session_id` column to `worker_queue`, create `sessions` table)
2. Implement updated `fetch_work_item`, `renew_session_lock`, `cleanup_orphaned_sessions`
3. All existing activities have `session_id = None` and are completely unaffected

---

## Implementation Steps

### Step 1: Data model changes
Add `session_id: Option<String>` to `Action::CallActivity` (`src/lib.rs`), `EventKind::ActivityScheduled` (`src/lib.rs`), and `WorkItem::ActivityExecute` (`src/providers/mod.rs`). All with `#[serde(skip_serializing_if, default)]`.

### Step 2: `schedule_activity_on_session` method
Add to `OrchestrationContext` in `src/lib.rs`. Emits `Action::CallActivity { session_id: Some(..) }`. Update existing `schedule_activity` to pass `session_id: None`. Add typed variant.

### Step 3: Replay engine plumbing
In `src/runtime/replay_engine.rs`: update `action_to_event`, `update_action_event_id`, `action_matches_event_kind` to flow `session_id`. In `src/runtime/execution.rs`: copy `session_id` in Action → WorkItem conversion.

### Step 4: `ActivityContext` gains `session_id()` getter
Add `session_id: Option<String>` to `ActivityWorkContext` in `src/runtime/dispatchers/worker.rs`. Flow from `WorkItem::ActivityExecute`. Add `pub fn session_id(&self) -> Option<&str>` to `ActivityContext`.

### Step 5: `RuntimeOptions` additions
Add `session_lock_timeout`, `session_lock_renewal_buffer`, `session_idle_timeout`, `session_cleanup_interval`, `max_sessions_per_worker`, `worker_node_id` to `RuntimeOptions` in `src/runtime/mod.rs`.

Add startup validation in `Runtime::start_with_options`: if `session_idle_timeout <= worker_lock_timeout - worker_lock_renewal_buffer`, return an error and refuse to start. This prevents misconfiguration where piggyback `last_activity_at` updates fire less frequently than the idle timeout, causing premature session unpin during activity execution.

### Step 6: Provider trait changes
In `src/providers/mod.rs`:
- Update `fetch_work_item` signature (add `worker_id`, `session_lock_timeout`, `max_sessions`)
- Add `renew_session_lock(worker_id, extend_for, idle_timeout) -> Result<usize>`
- Add `cleanup_orphaned_sessions(idle_timeout) -> Result<usize>`

### Step 7: SQLite migration
New file `migrations/20240107000000_add_sessions.sql`:
- `ALTER TABLE worker_queue ADD COLUMN session_id TEXT`
- `CREATE INDEX idx_worker_queue_session ON worker_queue(session_id)`
- `CREATE TABLE sessions (...)`

### Step 8: SQLite provider implementation
Update `src/providers/sqlite.rs`:
- `ack_orchestration_item`: store `session_id` on worker_queue row
- `fetch_work_item`: session-aware query + upsert
- `renew_work_item_lock`: piggyback `last_activity_at`
- `ack_work_item`: piggyback `last_activity_at`
- `renew_session_lock`: bulk UPDATE with idle filter
- `cleanup_orphaned_sessions`: DELETE orphans

### Step 9: Worker dispatcher changes
In `src/runtime/dispatchers/worker.rs`:
- Resolve `worker_id` from `worker_node_id`
- Pass session params to `fetch_work_item`
- Extract `session_id` into `ActivityWorkContext`
- Spawn single session manager background task

### Step 10: Provider validation tests
New module `src/provider_validation/sessions.rs` (25 tests). Wire into `src/provider_validations.rs` and `tests/sqlite_provider_validations.rs`.

### Step 11: Integration tests
New `tests/session_tests.rs` (12 tests) + `tests/session_multi_worker_tests.rs` (22 tests: 12 homogeneous + 10 heterogeneous config) + `tests/scenarios/sessions.rs` (3 tests) + replay engine tests (4 tests) + unit tests (5 tests).

### Step 12: Documentation updates
See Documentation section below. Additionally, mark the activity tags proposal (`docs/proposals/activity-tags.md` or similar) as "Rejected — we went a different direction with activity implicit sessions."

---

## Test Plan

### Provider Validation Tests (`src/provider_validation/sessions.rs`) — 25 tests

Required for all providers. Wired through `ProviderFactory`.

| # | Test | Description |
|---|---|---|
| 1 | `test_session_item_claimable_by_any_worker_initially` | Work item with `session_id` enqueued, no session row → any worker can fetch and claim |
| 2 | `test_session_item_pinned_after_claim` | After Worker A claims session, Worker B cannot fetch items for that session |
| 3 | `test_session_item_pinned_worker_can_fetch` | After Worker A claims session, Worker A can fetch more items for same session |
| 4 | `test_non_session_item_unaffected` | Work item without `session_id` is fetchable by any worker regardless of sessions |
| 5 | `test_session_claim_creates_session_row` | Fetching a session-bound item upserts a sessions row with correct fields |
| 6 | `test_session_lock_expiry_allows_reclaim` | After `locked_until` passes, Worker B can claim the session |
| 7 | `test_session_idle_expiry_allows_reclaim` | Worker A stops heartbeating idle session → lock expires → Worker B claims |
| 8 | `test_renew_session_lock_extends_all_owned` | `renew_session_lock(worker_A)` extends `locked_until` for all A's sessions |
| 9 | `test_renew_session_lock_skips_idle_sessions` | Sessions where `last_activity_at + idle_timeout < now` are NOT renewed |
| 10 | `test_renew_session_lock_skips_other_workers` | `renew_session_lock(worker_A)` does NOT extend Worker B's sessions |
| 11 | `test_renew_session_lock_skips_expired_locks` | Sessions with `locked_until < now` are NOT renewed |
| 12 | `test_renew_work_item_lock_updates_last_activity_at` | Piggyback updates `last_activity_at` for session-bound items |
| 13 | `test_ack_work_item_updates_last_activity_at` | Piggyback updates `last_activity_at` for session-bound items |
| 14 | `test_fetch_work_item_updates_last_activity_at` | Sets `last_activity_at = now` when returning session-bound item |
| 15 | `test_max_sessions_capacity_respected` | Worker at capacity skips unclaimed session items |
| 16 | `test_max_sessions_allows_owned_session_items` | Worker at capacity can still fetch items for already-owned sessions |
| 17 | `test_session_id_stored_on_worker_queue` | `ack_orchestration_item` stores `session_id` on worker_queue row |
| 18 | `test_cleanup_removes_expired_no_work` | Expired lock + no work items → deleted |
| 19 | `test_cleanup_removes_idle_no_work` | Past idle timeout + no work items → deleted |
| 20 | `test_cleanup_preserves_sessions_with_work` | Expired/idle sessions with pending work items → NOT deleted |
| 21 | `test_cleanup_preserves_active_sessions` | Fresh lock + recent activity → NOT deleted |
| 22 | `test_cleanup_returns_deleted_count` | Return value matches actual deletions |
| 23 | `test_session_reclaim_after_lock_expiry_upserts` | Worker B claiming expired session updates row, doesn't duplicate |
| 24 | `test_session_id_serde_backward_compat` | Old `WorkItem::ActivityExecute` without `session_id` → `None` |
| 25 | `test_multiple_sessions_per_worker` | Worker claims multiple distinct sessions independently |

### Multi-Worker E2E Tests (`tests/session_multi_worker_tests.rs`) — 12 tests

Two or more runtimes with different `worker_node_id` against the same store.

| # | Test | Description |
|---|---|---|
| 1 | `test_session_pins_to_claiming_worker` | 5 activities on same session → all execute on whichever runtime claimed first |
| 2 | `test_non_session_work_distributes_across_workers` | Session activities pin; non-session activities distribute freely |
| 3 | `test_session_handoff_on_worker_death` | Runtime A claims session → shutdown A → after lock expiry → runtime B claims and executes |
| 4 | `test_session_handoff_on_idle_timeout` | Session goes idle → idle + lock expire → new activity claimed by either runtime |
| 5 | `test_session_no_handoff_while_active` | Activity runs longer than `session_lock_timeout` → lock renewal prevents theft |
| 6 | `test_concurrent_session_claim_race` | Two runtimes start simultaneously, one session-bound item → exactly one claims |
| 7 | `test_max_sessions_overflow_to_other_worker` | Runtime at `max_sessions` → overflow session goes to other runtime |
| 8 | `test_session_pin_survives_continue_as_new` | ContinueAsNew with same session_id → both executions on same runtime |
| 9 | `test_session_and_non_session_isolation` | Session activities go to owner; non-session activities go to either runtime |
| 10 | `test_handoff_with_inflight_activity_retry` | Worker dies mid-activity → session + activity locks expire → other runtime picks up |
| 11 | `test_multiple_sessions_pinned_to_different_workers` | N sessions distribute across runtimes; each session's activities stay on owner |
| 12 | `test_session_reclaim_after_restart_with_same_node_id` | Same `worker_node_id` after restart → reclaims session without waiting for expiry |

### Heterogeneous Configuration Tests (`tests/session_multi_worker_tests.rs`) — 10 tests

Two runtimes with **different session-related settings** against the same store.

| # | Test | Description |
|---|---|---|
| 1 | `test_different_session_lock_timeout_recovery` | A has `session_lock_timeout=5s`, B has `session_lock_timeout=60s`. A claims session, A dies. Session becomes claimable in ≤5s (A's lock). B claims and operates normally with its own 60s lock. Verify recovery uses the dead worker's lock duration, not the claimer's. |
| 2 | `test_different_session_idle_timeout` | A has `session_idle_timeout=3s`, B has `session_idle_timeout=10min`. A claims session, activity completes. A stops heartbeating after 3s idle. Lock expires. B claims session on next activity. Verify each worker applies its own idle policy to its own sessions. |
| 3 | `test_different_max_sessions_load_distribution` | A has `max_sessions=2`, B has `max_sessions=100`. Create 5 sessions. A claims 2, then starts skipping unclaimed session items. B absorbs remaining 3. Verify non-session work still distributes to both. |
| 4 | `test_zero_max_sessions_non_session_worker` | A has `max_sessions=0` (non-session worker), B has `max_sessions=100`. Session-bound activities all go to B. Non-session activities distribute to both A and B. A never claims a session. |
| 5 | `test_session_adopts_claimer_lock_characteristics` | A (lock=10s) claims session → goes idle → unpins. B (lock=60s) claims same session. B dies. Session is claimable in ≤60s (B's lock, not A's). Verify the lock timeout in effect is the current owner's. |
| 6 | `test_different_cleanup_intervals_no_conflict` | A runs cleanup every 5s, B every 10min. Both running. Sessions owned by A and B. A's aggressive cleanup does NOT delete B's active sessions (cleanup only sweeps sessions with expired locks). |
| 7 | `test_mixed_worker_node_id_and_ephemeral` | A has `worker_node_id="stable-A"`, B has `worker_node_id=None` (ephemeral). Both claim sessions. A restarts with same node_id → reclaims its sessions immediately. B restarts → gets new ID → cannot reclaim, waits for lock expiry. |
| 8 | `test_asymmetric_concurrency_no_bottleneck` | A has `worker_concurrency=1`, B has `worker_concurrency=4`. A holds one session. Non-session work from the same orchestration flows to B's 4 slots without being blocked by A's session. |
| 9 | `test_worker_lock_timeout_vs_session_idle_timeout_safe` | A has `worker_lock_timeout=30s` (renewal ~25s), `session_idle_timeout=5min`. Activity runs for 3 minutes. Piggyback `last_activity_at` every ~25s keeps session alive despite no new scheduling. Session does NOT unpin during execution. |
| 10 | `test_worker_lock_timeout_exceeds_session_idle_timeout_errors` | A has `worker_lock_timeout=10min`, `session_idle_timeout=5min`. `Runtime::start_with_options` returns an error refusing to start. Verify the error message mentions both values. Validates the startup invariant check. |

### Single-Runtime E2E Tests (`tests/session_tests.rs`) — 12 tests

| # | Test | Description |
|---|---|---|
| 1 | `test_session_basic_affinity` | 3 activities on same session → all on same worker slot |
| 2 | `test_session_different_sessions_different_workers` | Two sessions → can run on different workers |
| 3 | `test_non_session_activity_alongside_session` | Mix of `schedule_activity` and `schedule_activity_on_session` |
| 4 | `test_session_survives_continue_as_new` | ContinueAsNew passes session_id → same worker |
| 5 | `test_session_activity_cancellation_via_select` | select2 loser cancelled → session still owned → next activity works |
| 6 | `test_session_poison_message` | Session activity poisons → orchestration receives ActivityFailed(Poison) |
| 7 | `test_session_with_sub_orchestration` | Sub-orchestration uses parent's session_id → same worker |
| 8 | `test_session_id_in_event_history` | `ActivityScheduled` event contains `session_id` |
| 9 | `test_session_id_preserved_on_replay` | Stop → restart → replay uses stored `session_id` |
| 10 | `test_schedule_activity_on_session_typed` | Typed variant with serde round-trip |
| 11 | `test_session_id_getter_on_activity_context` | `ActivityContext::session_id()` returns correct value |
| 12 | `test_session_id_none_for_regular_activity` | Regular activity → `session_id()` returns `None` |

### Scenario Tests (`tests/scenarios/sessions.rs`) — 3 tests

| # | Test | Description |
|---|---|---|
| 1 | `test_copilot_session_pattern` | Generate session_id → sequential activities → ContinueAsNew → same session |
| 2 | `test_session_migration_on_idle` | Two runtimes, session idles → migrates on next activity |
| 3 | `test_session_recovery_after_worker_death` | Runtime A claims → dies → runtime B picks up after lock expiry |

### Replay Engine Tests — 4 tests

| # | Test | Where |
|---|---|---|
| 1 | `test_action_to_event_includes_session_id` | `tests/replay_engine/action_to_event.rs` |
| 2 | `test_action_to_event_session_id_none` | same |
| 3 | `test_action_matches_event_with_session_id` | `tests/replay_engine/nondeterminism.rs` |
| 4 | `test_nondeterminism_session_id_mismatch` | same |

### Unit Tests — 5 tests

| # | Test | Where |
|---|---|---|
| 1 | `test_activity_scheduled_session_id_serde` | `tests/unit_tests.rs` |
| 2 | `test_activity_scheduled_session_id_none_omitted` | same |
| 3 | `test_activity_scheduled_missing_session_id_deserializes` | same |
| 4 | `test_work_item_activity_execute_session_id_serde` | same |
| 5 | `test_runtime_options_session_defaults` | `tests/runtime_options_test.rs` |

### Total: 71 tests

---

## Documentation Updates

### Must Update

| Doc | Changes |
|---|---|
| `docs/provider-implementation-guide.md` | New `fetch_work_item` signature. New methods `renew_session_lock`, `cleanup_orphaned_sessions`. `sessions` table schema. `worker_queue.session_id` column. Session-aware fetch pseudocode. Piggyback behavior for `renew_work_item_lock` and `ack_work_item`. |
| `docs/provider-testing-guide.md` | New "sessions" test category (25 tests). Updated total count. Wiring instructions. |
| `docs/provider-observability.md` | Instrumentation for `renew_session_lock` and `cleanup_orphaned_sessions`. |
| `docs/ORCHESTRATION-GUIDE.md` | `schedule_activity_on_session` API reference. `session_id()` getter. Usage patterns. Rolling upgrade caveat. Keepalive pattern. |
| `docs/sqlite-provider-design.md` | `sessions` table schema. Updated `worker_queue` schema. |
| `docs/metrics-specification.md` | New session metrics. Session labels on activity metrics. |
| `docs/observability-guide.md` | New RuntimeOptions fields. Session log events. Session claim/migration tracing. |
| `docs/architecture.md` | Session manager background task in worker dispatcher. |
| `docs/execution-model.md` | Session routing as optional dispatch modifier. |

### Should Update

| Doc | Changes |
|---|---|
| `docs/durable-futures-internals.md` | `Action::CallActivity` gains `session_id`. Replay matching includes `session_id`. |
| `docs/continue-as-new.md` | Sessions survive ContinueAsNew. |
| `docs/migration-guide.md` | Provider migration: 2 new methods, updated signature, schema migration. |
| `docs/versioning-best-practices.md` | Rolling upgrade caveat for session-pinned activities. |

### Update Proposal

| Doc | Changes |
|---|---|
| `docs/proposals/activity-implicit-sessions.md` | Add note: "Superseded by activity-implicit-sessions-v2.md" |

---

## Configuration Invariants

### `session_idle_timeout` must be >> `worker_lock_timeout`

The `last_activity_at` timestamp is piggybacked on `renew_work_item_lock`, which fires every `worker_lock_timeout - worker_lock_renewal_buffer` seconds. If this interval exceeds `session_idle_timeout`, the session renewal thread will see the session as idle and stop heartbeating while an activity is still running — causing premature unpin.

**Invariant:** `session_idle_timeout > worker_lock_timeout - worker_lock_renewal_buffer`

With defaults: `300s > 30s - 5s = 25s` → 12x safety margin.

**Runtime behavior:** At startup, if this invariant is violated, the runtime **must return an error** and refuse to start:
```rust
if options.session_idle_timeout <= options.worker_lock_timeout - options.worker_lock_renewal_buffer {
    return Err(format!(
        "session_idle_timeout ({}s) must be greater than worker lock renewal interval ({}s). \
         Sessions would unpin during long-running activity execution. \
         Increase session_idle_timeout or decrease worker_lock_timeout.",
        options.session_idle_timeout.as_secs(),
        (options.worker_lock_timeout - options.worker_lock_renewal_buffer).as_secs(),
    ));
}
```

This check is in `Runtime::start_with_options` alongside existing validation. It prevents a subtle misconfiguration that would cause sessions to unpin mid-activity with no obvious symptoms.

### `max_sessions_per_worker = 0` creates a non-session worker

This is a valid configuration. The fetch query's claimable clause evaluates `COUNT(*) < 0` which is always false. The worker only processes non-session work items. Useful for heterogeneous clusters where some workers should never hold sessions.

### Heterogeneous settings in a cluster

Multiple runtimes in the same cluster may have different session settings. Each runtime applies its own settings to its own behavior:

| Setting | Scope |
|---|---|
| `session_lock_timeout` | Applied by owning worker when heartbeating. Recovery time = dead worker's lock timeout. |
| `session_idle_timeout` | Applied by owning worker's renewal thread. Idle detection = owning worker's setting. |
| `max_sessions_per_worker` | Per-worker capacity (default 10). No global coordination. Session and non-session activities share the same `worker_concurrency` slots. |
| `session_cleanup_interval` | Each runtime sweeps independently. Any worker can sweep any worker's expired sessions. |
| `worker_node_id` | Per-worker identity. Mixed stable + ephemeral is valid. |

When a session migrates from Worker A to Worker B, it adopts B's lock/idle characteristics going forward.

---

## Resolved Questions

1. **`session_idle_timeout` default** — 5 minutes. Matches the copilot-sdk use case. Users can increase for longer-idle workloads.

2. **`max_sessions_per_worker` default** — 10. Session activities and non-session activities share the same `worker_concurrency` slots. 10 is a reasonable default that avoids overcommitting worker capacity to sessions.

3. **Activity tags proposal** — Will not be implemented. We went a different direction. The `docs/proposals/activity-tags.md` proposal (if it exists) should be marked as "Rejected — superseded by activity implicit sessions."

4. **Multiple sessions per orchestration** — Supported by design. The API allows multiple sessions (different `new_guid()` calls). Useful for multi-tenant scenarios.

5. **Observability** — Session claims and migrations must be traced. Workers should log structured events for:
   - Session claimed: `session_id`, `worker_id`, `is_new_claim` (true) vs `is_reclaim` (true, from expired lock)
   - Session renewed: periodic bulk count at DEBUG level
   - Session idle unpin: `session_id`, `worker_id`, `idle_duration`
   - Session orphan cleanup: count of rows deleted

   These logs must allow reconstructing the full migration timeline: "session X was on worker A from t0-t5, then migrated to worker B at t6 because A's lock expired."
