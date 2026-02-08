# Provider Implementation Guide

**For:** Developers and LLMs implementing custom storage backends for Duroxide  
**Reference implementations:**
- SQLite: `src/providers/sqlite.rs` (bundled)
- PostgreSQL: [duroxide-pg](https://github.com/affandar/duroxide-pg) (external)

> **🤖 AI Assistant Users:** Install the [duroxide-provider-implementation](skills/duroxide-provider-implementation.md) skill for your AI coding assistant. See [docs/skills/README.md](skills/README.md) for instructions on VS Code Copilot, Claude Code, and Cursor.

---

## Table of Contents

1. [Understanding the Provider Role](#understanding-the-provider-role)
2. [Core Concepts](#core-concepts)
3. [The Provider Trait at a Glance](#the-provider-trait-at-a-glance)
4. [Building Your First Provider: The Simplest Path](#building-your-first-provider-the-simplest-path)
5. [The Contract: What the Runtime Expects](#the-contract-what-the-runtime-expects)
6. [Detailed Method Implementations](#detailed-method-implementations)
7. [Advanced Topics](#advanced-topics)
8. [Schema Recommendations](#schema-recommendations)
9. [Testing Your Provider](#testing-your-provider)
10. [Common Pitfalls](#common-pitfalls)
11. [Validation Checklist](#validation-checklist)

---

## Understanding the Provider Role

### What is a Provider?

A **Provider** is a storage backend that Duroxide uses to persist orchestration state. Think of it as a database adapter with specific semantics for durable execution.

The provider is responsible for:
- **Storing event history** — The append-only log of what happened
- **Managing work queues** — Pending work items waiting to be processed
- **Providing atomic operations** — Ensuring consistency during orchestration turns

The provider is **NOT** responsible for:
- Understanding what events mean
- Making orchestration decisions
- Generating IDs or timestamps
- Interpreting workflow logic

**Key Principle:** The provider is "dumb storage." It stores and retrieves data exactly as instructed. All orchestration logic lives in the runtime.

### Runtime vs. Provider: Who Does What?

| Responsibility | Runtime | Provider |
|----------------|---------|----------|
| Generate event IDs | ✅ | ❌ |
| Generate execution IDs | ✅ | ❌ |
| Decide what to schedule next | ✅ | ❌ |
| Interpret event history | ✅ | ❌ |
| Store events | ❌ | ✅ |
| Manage queue visibility | ❌ | ✅ |
| Lock/unlock work items | ❌ | ✅ |
| Ensure atomic commits | ❌ | ✅ |

### The Two-Queue Architecture

Duroxide uses two work queues to separate concerns:

```
┌─────────────────────────────────────────────────────────────────────┐
│                        DUROXIDE RUNTIME                              │
│                                                                      │
│  ┌─────────────────────┐          ┌─────────────────────┐           │
│  │ Orchestration       │          │ Worker              │           │
│  │ Dispatcher          │          │ Dispatcher          │           │
│  │                     │          │                     │           │
│  │ • Fetches turns     │          │ • Fetches activities│           │
│  │ • Runs replay       │          │ • Executes work     │           │
│  │ • Commits results   │          │ • Reports results   │           │
│  └──────────┬──────────┘          └──────────┬──────────┘           │
│             │                                │                       │
└─────────────┼────────────────────────────────┼───────────────────────┘
              │                                │
              ▼                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│                          PROVIDER                                    │
│                                                                      │
│  ┌─────────────────────┐          ┌─────────────────────┐           │
│  │ Orchestrator Queue  │          │ Worker Queue        │           │
│  │                     │          │                     │           │
│  │ • StartOrchestration│          │ • ActivityExecute   │           │
│  │ • ActivityCompleted │          │                     │           │
│  │ • TimerFired        │          │                     │           │
│  │ • ExternalEvent     │          │                     │           │
│  │ • SubOrchCompleted  │          │                     │           │
│  └─────────────────────┘          └─────────────────────┘           │
│                                                                      │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │                      Event History                               ││
│  │  [OrchestrationStarted] → [ActivityScheduled] → [ActivityCompleted] → ...
│  └─────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────┘
```

**Orchestrator Queue:** Holds work that triggers orchestration turns (completions, timer fires, external events, new orchestrations).

**Worker Queue:** Holds work that needs to be executed by activities.

**Event History:** The append-only log of everything that has happened to an orchestration instance.

### Data Flow: A Single Orchestration Turn

Let's trace what happens when an orchestration schedules an activity:

```
┌─────────────────────────────────────────────────────────────────────┐
│ TURN 1: Orchestration starts, schedules an activity                  │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  1. Client calls start_orchestration("order-123", "ProcessOrder")    │
│     └─► Provider enqueues StartOrchestration to orchestrator queue   │
│                                                                      │
│  2. Orchestration Dispatcher fetches from orchestrator queue         │
│     └─► Provider returns locked OrchestrationItem with:              │
│         • instance: "order-123"                                      │
│         • messages: [StartOrchestration]                             │
│         • history: [] (empty - new instance)                         │
│                                                                      │
│  3. Runtime runs the orchestration, which calls schedule_activity()  │
│     └─► Runtime creates events: [OrchestrationStarted, ActivityScheduled]
│     └─► Runtime creates action: ActivityExecute                      │
│                                                                      │
│  4. Runtime commits the turn via ack_orchestration_item()            │
│     └─► Provider atomically:                                         │
│         • Appends events to history                                  │
│         • Enqueues ActivityExecute to worker queue                   │
│         • Deletes processed messages from orchestrator queue         │
│         • Releases instance lock                                     │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│ ACTIVITY EXECUTION: Worker processes the activity                    │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  5. Worker Dispatcher fetches from worker queue                      │
│     └─► Provider returns locked ActivityExecute                      │
│                                                                      │
│  6. Worker executes the activity (calls user code)                   │
│     └─► Activity returns Ok("processed")                             │
│                                                                      │
│  7. Worker acks the work item with completion                        │
│     └─► Provider atomically:                                         │
│         • Deletes ActivityExecute from worker queue                  │
│         • Enqueues ActivityCompleted to orchestrator queue           │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│ TURN 2: Orchestration receives completion, finishes                  │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  8. Orchestration Dispatcher fetches from orchestrator queue         │
│     └─► Provider returns locked OrchestrationItem with:              │
│         • instance: "order-123"                                      │
│         • messages: [ActivityCompleted]                              │
│         • history: [OrchestrationStarted, ActivityScheduled]         │
│                                                                      │
│  9. Runtime replays orchestration, delivers completion               │
│     └─► Orchestration resumes, returns Ok("done")                    │
│     └─► Runtime creates events: [ActivityCompleted, OrchCompleted]   │
│                                                                      │
│ 10. Runtime commits final turn                                       │
│     └─► Provider appends final events                                │
│     └─► Instance is now complete                                     │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Core Concepts

### Event History: The Append-Only Log

Every orchestration instance has an **event history**—a sequence of events that records everything that happened:

```
Instance "order-123", Execution 1:
────────────────────────────────────────────────────────────
[1] OrchestrationStarted { name: "ProcessOrder", input: "..." }
[2] ActivityScheduled { name: "ValidateOrder", input: "..." }
[3] ActivityCompleted { source_event_id: 2, result: "valid" }
[4] ActivityScheduled { name: "ChargePayment", input: "..." }
[5] ActivityCompleted { source_event_id: 4, result: "charged" }
[6] OrchestrationCompleted { output: "Order processed" }
```

**Key properties:**
- **Append-only:** Events are never modified or deleted (during normal operation)
- **Ordered by event_id:** Events have monotonically increasing IDs within an execution
- **Immutable content:** Once written, event data never changes

**Why append-only?** This enables deterministic replay. The runtime can recreate any orchestration state by replaying history from the beginning.

### Work Queues: Pending Work

Work queues hold items waiting to be processed. Each item represents something that needs to happen:

**Orchestrator Queue items:**
- `StartOrchestration` — Start a new orchestration
- `ActivityCompleted` / `ActivityFailed` — Activity finished
- `TimerFired` — A timer expired
- `ExternalEvent` — External system sent an event
- `SubOrchCompleted` / `SubOrchFailed` — Child orchestration finished
- `CancelInstance` — Cancellation requested

**Worker Queue items:**
- `ActivityExecute` — Execute an activity

### Peek-Lock Semantics

Both queues use **peek-lock** semantics (also called "receive and delete"):

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Peek-Lock Lifecycle                              │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  1. FETCH (peek + lock)                                              │
│     ┌─────────────────────────────────────────────────────────────┐ │
│     │ Item in queue, visible, unlocked                             │ │
│     │ → Dispatcher fetches item                                    │ │
│     │ → Provider locks item with unique token                      │ │
│     │ → Item becomes invisible to other dispatchers                │ │
│     └─────────────────────────────────────────────────────────────┘ │
│                              │                                       │
│              ┌───────────────┼───────────────┐                      │
│              ▼               ▼               ▼                      │
│         SUCCESS          FAILURE        LOCK EXPIRES                │
│                                                                      │
│  2a. ACK (success)       2b. ABANDON       2c. AUTO-UNLOCK          │
│     ┌──────────────┐     ┌──────────────┐  ┌──────────────┐         │
│     │ Processing   │     │ Processing   │  │ Dispatcher   │         │
│     │ succeeded    │     │ failed       │  │ crashed      │         │
│     │ → Delete item│     │ → Unlock     │  │ → Lock       │         │
│     │ → Maybe      │     │ → Maybe delay│  │   expires    │         │
│     │   enqueue    │     │ → Item       │  │ → Item       │         │
│     │   completion │     │   becomes    │  │   becomes    │         │
│     │              │     │   visible    │  │   visible    │         │
│     └──────────────┘     └──────────────┘  └──────────────┘         │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

**Why peek-lock?**
- **At-least-once delivery:** If a dispatcher crashes, the lock expires and another dispatcher can retry
- **No lost work:** Items aren't deleted until explicitly acked
- **Graceful retry:** Failed items can be retried with optional backoff

### Turns and Replay

An orchestration executes in **turns**. Each turn:

1. Fetches pending messages for the instance
2. Loads the event history
3. **Replays** the orchestration from the beginning using history
4. Continues execution until the orchestration blocks (waiting for something)
5. Commits new events and dispatches new work

**Replay** is the magic that makes orchestrations durable. When an orchestration calls `schedule_activity()`, the runtime checks:
- **If history has the result:** Return it immediately (replay)
- **If history doesn't have the result:** Schedule the work and suspend

This means orchestration code runs multiple times, but from the orchestration's perspective, it feels like continuous execution.

### Long Polling vs Short Polling

The `fetch_*` methods receive a `poll_timeout` parameter. How your provider handles this determines its polling behavior:

**Short-polling providers** (SQLite, PostgreSQL):
- Ignore `poll_timeout` and return immediately
- Return `None` if no work is available
- The dispatcher handles waiting between polls

**Long-polling providers** (Redis with BLPOP, Azure Service Bus, SQS):
- MAY block up to `poll_timeout` waiting for work to arrive
- Return early if work becomes available
- Reduces latency and unnecessary database queries

```rust
// Short-polling (SQLite style) - ignores poll_timeout
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    _poll_timeout: Duration,  // Ignored
) -> Result<Option<...>, ProviderError> {
    // Query immediately, return None if nothing available
    self.try_fetch_and_lock().await
}

// Long-polling (Redis style) - uses poll_timeout
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,  // Used
) -> Result<Option<...>, ProviderError> {
    // Block up to poll_timeout waiting for work
    match self.blpop("worker_queue", poll_timeout).await {
        Some(item) => self.lock_item(item, lock_timeout).await,
        None => Ok(None),  // Timeout expired, no work
    }
}
```

**Which should you implement?**
- Start with **short-polling** — it's simpler and works everywhere
- Add **long-polling** if your storage backend supports blocking reads (BLPOP, LISTEN/NOTIFY, change streams)
- The runtime works correctly with either approach

---

## The Provider Trait at a Glance

Here's every method you need to implement, organized by complexity:

### Simple Methods (Start Here)

| Method | Purpose | Complexity |
|--------|---------|------------|
| `read()` | Load history for latest execution | ⭐ Easy |
| `append_with_execution()` | Append events to history | ⭐ Easy |
| `enqueue_for_worker()` | Add item to worker queue | ⭐ Easy |
| `enqueue_orchestrator_work()` | Add item to orchestrator queue | ⭐ Easy |

### Queue Operations (Medium Complexity)

| Method | Purpose | Complexity |
|--------|---------|------------|
| `fetch_work_item()` | Fetch and lock from worker queue | ⭐⭐ Medium |
| `ack_work_item()` | Delete from worker queue, maybe enqueue completion | ⭐⭐ Medium |
| `abandon_work_item()` | Release lock without deleting | ⭐⭐ Medium |
| `renew_work_item_lock()` | Extend lock for long-running activity | ⭐⭐ Medium |

### Orchestration Operations (Complex)

| Method | Purpose | Complexity |
|--------|---------|------------|
| `fetch_orchestration_item()` | Fetch turn with instance locking | ⭐⭐⭐ Complex |
| `ack_orchestration_item()` | Atomic commit of turn results | ⭐⭐⭐ Complex |
| `abandon_orchestration_item()` | Release orchestration lock | ⭐⭐ Medium |
| `renew_orchestration_item_lock()` | Extend orchestration lock | ⭐⭐ Medium |

### Optional Methods

| Method | Purpose |
|--------|---------|
| `latest_execution_id()` | Performance optimization |
| `read_with_execution()` | Read specific execution's history |
| `list_instances()` | Management API |
| `list_executions()` | Management API |

### Minimal Skeleton

```rust
use async_trait::async_trait;
use duroxide::providers::{
    Provider, ProviderError, WorkItem, OrchestrationItem, 
    ExecutionMetadata, ExecutionState, ScheduledActivityIdentifier
};
use duroxide::Event;
use std::time::Duration;

pub struct MyProvider {
    // Your storage connection
}

#[async_trait]
impl Provider for MyProvider {
    // === History ===
    
    async fn read(&self, instance: &str) -> Vec<Event> {
        todo!("Load events for latest execution")
    }
    
    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), ProviderError> {
        todo!("Append events to history")
    }
    
    // === Worker Queue ===
    
    async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> {
        todo!("Add to worker queue")
    }
    
    async fn fetch_work_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Duration,
    ) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError> {
        todo!("Fetch and lock from worker queue")
    }
    
    async fn ack_work_item(
        &self, 
        token: &str, 
        completion: Option<WorkItem>
    ) -> Result<(), ProviderError> {
        todo!("Delete from worker queue")
    }
    
    async fn abandon_work_item(
        &self, 
        token: &str, 
        delay: Option<Duration>,
        ignore_attempt: bool,
    ) -> Result<(), ProviderError> {
        todo!("Release lock, make visible again")
    }
    
    async fn renew_work_item_lock(
        &self, 
        token: &str, 
        extend_for: Duration
    ) -> Result<ExecutionState, ProviderError> {
        todo!("Extend lock timeout")
    }
    
    // === Orchestrator Queue ===
    
    async fn enqueue_orchestrator_work(
        &self, 
        item: WorkItem, 
        delay: Option<Duration>
    ) -> Result<(), ProviderError> {
        todo!("Add to orchestrator queue")
    }
    
    async fn fetch_orchestration_item(
        &self,
        lock_timeout: Duration,
        poll_timeout: Duration,
        filter: Option<&DispatcherCapabilityFilter>,
    ) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError> {
        todo!("Fetch turn with instance lock, applying capability filter")
    }
    
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
        cancelled_activities: Vec<ScheduledActivityIdentifier>,
    ) -> Result<(), ProviderError> {
        todo!("Atomic commit")
    }
    
    async fn abandon_orchestration_item(
        &self, 
        lock_token: &str, 
        delay: Option<Duration>,
        ignore_attempt: bool,
    ) -> Result<(), ProviderError> {
        todo!("Release orchestration lock")
    }
    
    async fn renew_orchestration_item_lock(
        &self, 
        token: &str, 
        extend_for: Duration
    ) -> Result<(), ProviderError> {
        todo!("Extend orchestration lock")
    }
}
```

---

## Building Your First Provider: The Simplest Path

Let's build a provider incrementally. We'll start with the simplest pieces and add complexity.

### Step 1: Event History Storage

The foundation is storing and retrieving event history.

**Schema:**
```sql
CREATE TABLE history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    event_data TEXT NOT NULL,  -- JSON-serialized Event
    
    UNIQUE(instance_id, execution_id, event_id)
);

CREATE TABLE instances (
    instance_id TEXT PRIMARY KEY,
    current_execution_id INTEGER NOT NULL DEFAULT 1,
    orchestration_name TEXT,
    orchestration_version TEXT,
    status TEXT DEFAULT 'Running',
    output TEXT
);
```

**Implementation:**
```rust
async fn read(&self, instance: &str) -> Vec<Event> {
    // 1. Find the latest execution_id for this instance
    let execution_id = sqlx::query_scalar!(
        "SELECT current_execution_id FROM instances WHERE instance_id = ?",
        instance
    )
    .fetch_optional(&self.pool)
    .await?
    .unwrap_or(1);
    
    // 2. Load all events for that execution, ordered by event_id
    let rows = sqlx::query!(
        "SELECT event_data FROM history 
         WHERE instance_id = ? AND execution_id = ?
         ORDER BY event_id ASC",
        instance, execution_id
    )
    .fetch_all(&self.pool)
    .await?;
    
    // 3. Deserialize and return
    rows.into_iter()
        .map(|row| serde_json::from_str(&row.event_data).unwrap())
        .collect()
}

async fn append_with_execution(
    &self,
    instance: &str,
    execution_id: u64,
    new_events: Vec<Event>,
) -> Result<(), ProviderError> {
    // Insert each event (database enforces uniqueness)
    for event in new_events {
        let event_data = serde_json::to_string(&event).unwrap();
        
        sqlx::query!(
            "INSERT INTO history (instance_id, execution_id, event_id, event_data)
             VALUES (?, ?, ?, ?)",
            instance, execution_id, event.event_id, event_data
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ProviderError::permanent("append", e.to_string()))?;
    }
    
    Ok(())
}
```

### Step 2: Worker Queue

The worker queue holds activities waiting to be executed.

**Schema:**
```sql
CREATE TABLE worker_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item TEXT NOT NULL,         -- JSON-serialized WorkItem
    visible_at INTEGER NOT NULL,     -- Unix timestamp (ms) when item becomes visible
    lock_token TEXT,                 -- NULL = unlocked
    locked_until INTEGER,            -- Unix timestamp (ms) when lock expires
    attempt_count INTEGER DEFAULT 0, -- For poison message detection
    
    -- For activity cancellation (lock stealing)
    instance_id TEXT,
    execution_id INTEGER,
    activity_id INTEGER
);

CREATE INDEX idx_worker_visible ON worker_queue (visible_at) WHERE lock_token IS NULL;
CREATE INDEX idx_worker_lock ON worker_queue (lock_token);
CREATE INDEX idx_worker_activity ON worker_queue (instance_id, execution_id, activity_id);
```

**Implementation:**
```rust
async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> {
    let item_json = serde_json::to_string(&item).unwrap();
    let now = current_time_ms();
    
    // Extract identity for cancellation support
    let (instance_id, execution_id, activity_id) = match &item {
        WorkItem::ActivityExecute { instance, execution_id, activity_id, .. } => {
            (Some(instance.clone()), Some(*execution_id), Some(*activity_id))
        }
        _ => (None, None, None)
    };
    
    sqlx::query!(
        "INSERT INTO worker_queue (work_item, visible_at, instance_id, execution_id, activity_id)
         VALUES (?, ?, ?, ?, ?)",
        item_json, now, instance_id, execution_id, activity_id
    )
    .execute(&self.pool)
    .await
    .map_err(|e| ProviderError::permanent("enqueue_for_worker", e.to_string()))?;
    
    Ok(())
}

async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    _poll_timeout: Duration,  // Ignored for SQLite (short-polling)
) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError> {
    let now = current_time_ms();
    let lock_token = uuid::Uuid::new_v4().to_string();
    let locked_until = now + lock_timeout.as_millis() as i64;
    
    // Atomically find and lock one item
    let result = sqlx::query!(
        "UPDATE worker_queue
         SET lock_token = ?, locked_until = ?, attempt_count = attempt_count + 1
         WHERE id = (
             SELECT id FROM worker_queue
             WHERE visible_at <= ? AND (lock_token IS NULL OR locked_until <= ?)
             ORDER BY id LIMIT 1
         )
         RETURNING work_item, attempt_count, instance_id",
        lock_token, locked_until, now, now
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(|e| ProviderError::retryable("fetch_work_item", e.to_string()))?;
    
    match result {
        None => Ok(None),
        Some(row) => {
            let item: WorkItem = serde_json::from_str(&row.work_item).unwrap();
            let attempt_count = row.attempt_count as u32;
            
            // Check if instance is cancelled (for cooperative cancellation)
            let state = self.get_execution_state(&row.instance_id).await;
            
            Ok(Some((item, lock_token, attempt_count, state)))
        }
    }
}

async fn ack_work_item(
    &self, 
    token: &str, 
    completion: Option<WorkItem>
) -> Result<(), ProviderError> {
    let mut tx = self.pool.begin().await
        .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    
    // Delete the work item
    let deleted = sqlx::query!(
        "DELETE FROM worker_queue WHERE lock_token = ?",
        token
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    
    // If no rows deleted, lock was stolen (activity cancelled)
    if deleted.rows_affected() == 0 {
        return Err(ProviderError::permanent(
            "ack_work_item", 
            "Lock token not found - activity was cancelled"
        ));
    }
    
    // Enqueue completion if provided
    if let Some(completion) = completion {
        let item_json = serde_json::to_string(&completion).unwrap();
        let now = current_time_ms();
        
        sqlx::query!(
            "INSERT INTO orchestrator_queue (work_item, visible_at, instance_id)
             VALUES (?, ?, ?)",
            item_json, now, extract_instance(&completion)
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    }
    
    tx.commit().await
        .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    
    Ok(())
}
```

### Step 3: Orchestrator Queue (Similar Pattern)

The orchestrator queue follows the same peek-lock pattern, but with instance-level locking.

**Schema:**
```sql
CREATE TABLE orchestrator_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    work_item TEXT NOT NULL,
    visible_at INTEGER NOT NULL,
    lock_token TEXT,
    locked_until INTEGER,
    attempt_count INTEGER DEFAULT 0
);

CREATE TABLE instance_locks (
    instance_id TEXT PRIMARY KEY,
    lock_token TEXT NOT NULL,
    locked_until INTEGER NOT NULL
);

CREATE INDEX idx_orch_visible ON orchestrator_queue (visible_at, instance_id);
```

The orchestrator queue is more complex because:
- All messages for one instance are fetched together (instance-level locking)
- History must be loaded along with messages
- Lock applies to the entire instance, not just one message

See [Detailed Method Implementations](#fetch_orchestration_item---fetching-an-orchestration-turn) for the full implementation.

### Step 4: The Atomic Commit

The `ack_orchestration_item()` method is the heart of the provider. It must atomically:

1. Validate the lock is still held
2. Append new events to history
3. Update instance metadata
4. Enqueue new work items
5. Delete cancelled activities (lock stealing)
6. Delete processed messages
7. Release the instance lock

All of this must succeed or fail together. See [Detailed Method Implementations](#ack_orchestration_item---the-atomic-commit) for the full implementation.

---

## The Contract: What the Runtime Expects

### ID Generation: The Runtime's Job

**The provider MUST NOT generate `execution_id` or `event_id` values.**

All IDs come from the runtime:
- **`execution_id`**: Passed explicitly to `ack_orchestration_item()`
- **`event_id`**: Set in each `Event` within `history_delta`

```rust
// ✅ CORRECT: Store runtime-provided IDs
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,  // Runtime provides this
    history_delta: Vec<Event>,  // Each event has event_id set
    ...
) {
    for event in &history_delta {
        // event.event_id is already set by runtime - just store it
        db.insert_event(instance, execution_id, event.event_id, event);
    }
}

// ❌ WRONG: Never generate IDs
let execution_id = db.max_execution_id() + 1;  // NO!
let event_id = db.max_event_id() + 1;  // NO!
```

**Why?** The runtime maintains determinism by controlling ID assignment. If the provider generated IDs, replays could produce different IDs, breaking correctness.

### Error Classification

Provider methods return `Result<..., ProviderError>`. The runtime uses error classification to decide how to handle failures:

```rust
// Retryable: Transient failures, runtime will retry with backoff
ProviderError::retryable("operation", "Database busy")

// Permanent: Unrecoverable, runtime will fail the orchestration
ProviderError::permanent("operation", "Duplicate event detected")
```

**Retryable errors** (runtime will retry):
- Database busy/locked
- Connection timeout
- Network failure
- Temporary resource exhaustion

**Permanent errors** (runtime will fail):
- Duplicate events
- Missing lock token (already processed or cancelled)
- Data corruption
- Invalid input

### Atomicity Requirements

Several operations must be atomic (all-or-nothing):

| Operation | Must Be Atomic |
|-----------|----------------|
| `ack_work_item()` | Delete + enqueue completion |
| `ack_orchestration_item()` | All 7 steps (see above) |
| `fetch_orchestration_item()` | Lock instance + tag messages |

Use database transactions to ensure atomicity.

---

## Detailed Method Implementations

### fetch_orchestration_item() — Fetching an Orchestration Turn

This is the most complex method. It must:

1. Find an instance with pending work (applying capability filter if provided)
2. Acquire an instance-level lock (prevent concurrent processing)
3. Tag all visible messages with the lock token
4. Load instance metadata
5. Load event history for current execution
6. Return everything together

#### Capability Filtering Contract

`fetch_orchestration_item` accepts an optional `DispatcherCapabilityFilter`. When provided, the provider **MUST** apply the filter before acquiring the lock or loading history. The correct implementation order is:

1. Find a candidate instance from the queue.
2. Check the execution's `duroxide_version_major/minor/patch` against the filter. **Skip if incompatible.**
3. Only then acquire the instance lock.
4. Only then load and deserialize history.

This ordering is critical — it ensures the provider never loads or deserializes history events from an incompatible execution (which may contain unknown event types from a newer duroxide version).

When `filter` is `None`, behaviour is unchanged (legacy/drain mode).

#### History Deserialization Contract

Even with filtering, a provider may encounter undeserializable history events in edge cases (e.g., `filter=None`, corrupted data, bugs). The contract:

1. **MUST NOT** silently drop events that fail to deserialize. Silent dropping leads to incomplete history and confusing nondeterminism errors.
2. **MUST** return `ProviderError::permanent(...)` when history deserialization fails. The `attempt_count` must have been incremented before the error is returned so the existing max-attempts poison machinery can terminate the orchestration.

The required implementation pattern:
1. Acquire instance lock and increment `attempt_count` (commit atomically).
2. Load history events.
3. If any event fails to deserialize → return `ProviderError::permanent("Failed to deserialize event at position N: {error}")`.
4. On the next fetch cycle, the item is fetched again with a higher `attempt_count`.
5. Once `attempt_count > max_attempts`, the runtime poisons the orchestration with a clear error.

**Pseudocode:**
```
BEGIN TRANSACTION

-- Step 1: Find instance with work that's not locked
instance_id = SELECT DISTINCT q.instance_id
              FROM orchestrator_queue q
              LEFT JOIN instance_locks il ON q.instance_id = il.instance_id
              WHERE q.visible_at <= now()
                AND (il.instance_id IS NULL OR il.locked_until <= now())
              LIMIT 1

IF instance_id IS NULL:
    RETURN None  -- No work available

-- Step 2: Acquire instance lock atomically
lock_token = generate_uuid()
locked_until = now() + lock_timeout

INSERT INTO instance_locks (instance_id, lock_token, locked_until)
VALUES (instance_id, lock_token, locked_until)
ON CONFLICT(instance_id) DO UPDATE
SET lock_token = excluded.lock_token, locked_until = excluded.locked_until
WHERE locked_until <= now()  -- Only if lock expired

IF rows_affected == 0:
    ROLLBACK
    RETURN None  -- Another dispatcher got the lock

-- Step 3: Tag all visible messages with our lock token
UPDATE orchestrator_queue
SET lock_token = lock_token, locked_until = locked_until,
    attempt_count = attempt_count + 1
WHERE instance_id = instance_id
  AND visible_at <= now()
  AND (lock_token IS NULL OR locked_until <= now())

-- Step 4: Fetch tagged messages
messages = SELECT work_item, attempt_count FROM orchestrator_queue
           WHERE lock_token = lock_token
           ORDER BY id

-- Step 5: Load metadata (or derive from messages for new instance)
metadata = SELECT * FROM instances WHERE instance_id = instance_id
IF metadata IS NULL:
    -- New instance: extract from StartOrchestration message
    start_msg = messages.find(|m| m.is_start_orchestration())
    orchestration_name = start_msg.orchestration
    execution_id = 1
ELSE:
    orchestration_name = metadata.orchestration_name
    execution_id = metadata.current_execution_id

-- Step 6: Load history
history = SELECT event_data FROM history
          WHERE instance_id = instance_id AND execution_id = execution_id
          ORDER BY event_id

COMMIT

RETURN OrchestrationItem {
    instance: instance_id,
    orchestration_name,
    execution_id,
    version: orchestration_version,
    history: deserialize(history),
    messages: deserialize(messages),
}
```

**Critical behaviors:**
- Only messages present at fetch time are tagged
- Messages arriving after fetch are NOT tagged (will be fetched next turn)
- Lock token must be unique (use UUID)
- Lock must be atomic (use `ON CONFLICT` or similar)

### ack_orchestration_item() — The Atomic Commit

This method commits an orchestration turn. It must be fully atomic.

**Pseudocode:**
```
BEGIN TRANSACTION

-- Step 1: Validate lock is still held
lock = SELECT * FROM instance_locks 
       WHERE instance_id = instance AND lock_token = token AND locked_until > now()
IF lock IS NULL:
    ROLLBACK
    RETURN Error("Lock expired or invalid")

-- Step 2: Create or update instance metadata
INSERT INTO instances (instance_id, orchestration_name, orchestration_version, 
                       current_execution_id, status, output)
VALUES (instance, metadata.name, metadata.version, execution_id, 
        metadata.status, metadata.output)
ON CONFLICT(instance_id) DO UPDATE
SET current_execution_id = MAX(current_execution_id, excluded.current_execution_id),
    orchestration_name = COALESCE(excluded.orchestration_name, orchestration_name),
    orchestration_version = COALESCE(excluded.orchestration_version, orchestration_version),
    status = excluded.status,
    output = excluded.output

-- Step 3: Append events to history
FOR event IN history_delta:
    INSERT INTO history (instance_id, execution_id, event_id, event_data)
    VALUES (instance, execution_id, event.event_id, serialize(event))
    -- Database enforces uniqueness, rejects duplicates

-- Step 4: Enqueue worker items (activities)
FOR item IN worker_items:
    INSERT INTO worker_queue (work_item, visible_at, instance_id, execution_id, activity_id)
    VALUES (serialize(item), now(), item.instance, item.execution_id, item.activity_id)

-- Step 5: Enqueue orchestrator items (timers, completions)
FOR item IN orchestrator_items:
    INSERT INTO orchestrator_queue (work_item, visible_at, instance_id)
    VALUES (serialize(item), now() + item.delay, item.instance)

-- Step 6: Cancel activities via lock stealing
-- ⚠️ MUST happen AFTER step 4 (enqueue) to handle same-turn schedule+cancel
FOR cancelled IN cancelled_activities:
    DELETE FROM worker_queue 
    WHERE instance_id = cancelled.instance 
      AND execution_id = cancelled.execution_id 
      AND activity_id = cancelled.activity_id

-- Step 7: Delete processed messages
DELETE FROM orchestrator_queue WHERE lock_token = token

-- Step 8: Release instance lock
DELETE FROM instance_locks WHERE instance_id = instance AND lock_token = token

COMMIT
```

**Critical ordering:** Step 4 (enqueue) must happen before Step 6 (cancel). When an activity is scheduled and immediately dropped in the same turn, both lists contain the same activity. INSERT-then-DELETE is correct; DELETE-then-INSERT would leave a stale entry.

### abandon_orchestration_item() — Releasing Without Commit

When an orchestration turn fails (e.g., unregistered handler, panic), the runtime abandons the work without committing. This releases the lock so another dispatcher can retry.

**Pseudocode:**
```
-- Release the instance lock
DELETE FROM instance_locks 
WHERE instance_id = instance AND lock_token = token

-- Clear lock_token from tagged messages (make them visible again)
UPDATE orchestrator_queue
SET lock_token = NULL, 
    locked_until = NULL,
    visible_at = CASE 
        WHEN delay IS NOT NULL THEN now() + delay  -- Backoff delay
        ELSE visible_at 
    END,
    attempt_count = CASE 
        WHEN ignore_attempt THEN MAX(0, attempt_count - 1)  -- Undo increment
        ELSE attempt_count 
    END
WHERE lock_token = token
```

**Parameters:**
- `delay`: Optional backoff before retry (e.g., 1 second for transient failures)
- `ignore_attempt`: If true, don't count this attempt toward poison message detection

**Implementation:**
```rust
async fn abandon_orchestration_item(
    &self,
    lock_token: &str,
    delay: Option<Duration>,
    ignore_attempt: bool,
) -> Result<(), ProviderError> {
    let now = current_time_ms();
    let visible_at = delay.map(|d| now + d.as_millis() as i64);
    
    // Release instance lock
    sqlx::query!(
        "DELETE FROM instance_locks WHERE lock_token = ?",
        lock_token
    )
    .execute(&self.pool)
    .await?;
    
    // Make messages visible again
    if let Some(visible_at) = visible_at {
        if ignore_attempt {
            sqlx::query!(
                "UPDATE orchestrator_queue 
                 SET lock_token = NULL, locked_until = NULL, 
                     visible_at = ?, attempt_count = MAX(0, attempt_count - 1)
                 WHERE lock_token = ?",
                visible_at, lock_token
            )
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query!(
                "UPDATE orchestrator_queue 
                 SET lock_token = NULL, locked_until = NULL, visible_at = ?
                 WHERE lock_token = ?",
                visible_at, lock_token
            )
            .execute(&self.pool)
            .await?;
        }
    } else {
        sqlx::query!(
            "UPDATE orchestrator_queue 
             SET lock_token = NULL, locked_until = NULL
             WHERE lock_token = ?",
            lock_token
        )
        .execute(&self.pool)
        .await?;
    }
    
    Ok(())
}
```

### renew_orchestration_item_lock() — Extending Turn Time

Extends the lock for long-running orchestration turns. Unlike activity lock renewal, this returns `()` rather than `ExecutionState`—orchestration cancellation comes via `CancelInstance` messages in the fetched work items, not via lock renewal.

```rust
async fn renew_orchestration_item_lock(
    &self,
    token: &str,
    extend_for: Duration,
) -> Result<(), ProviderError> {
    let now = current_time_ms();
    let new_locked_until = now + extend_for.as_millis() as i64;
    
    // Update instance lock - only if still valid
    let result = sqlx::query!(
        "UPDATE instance_locks 
         SET locked_until = ?
         WHERE lock_token = ? AND locked_until > ?",
        new_locked_until, token, now
    )
    .execute(&self.pool)
    .await?;
    
    if result.rows_affected() == 0 {
        return Err(ProviderError::permanent(
            "renew_orchestration_item_lock",
            "Lock expired or not found"
        ));
    }
    
    // Also extend the message locks
    sqlx::query!(
        "UPDATE orchestrator_queue SET locked_until = ? WHERE lock_token = ?",
        new_locked_until, token
    )
    .execute(&self.pool)
    .await?;
    
    Ok(())
}
```

### abandon_work_item() — Releasing Activity Lock

When an activity fails and should be retried (e.g., transient error), release the lock without deleting:

```rust
async fn abandon_work_item(
    &self,
    token: &str,
    delay: Option<Duration>,
    ignore_attempt: bool,
) -> Result<(), ProviderError> {
    let now = current_time_ms();
    let visible_at = delay.map(|d| now + d.as_millis() as i64).unwrap_or(now);
    
    let query = if ignore_attempt {
        sqlx::query!(
            "UPDATE worker_queue 
             SET lock_token = NULL, locked_until = NULL, 
                 visible_at = ?, attempt_count = MAX(0, attempt_count - 1)
             WHERE lock_token = ?",
            visible_at, token
        )
    } else {
        sqlx::query!(
            "UPDATE worker_queue 
             SET lock_token = NULL, locked_until = NULL, visible_at = ?
             WHERE lock_token = ?",
            visible_at, token
        )
    };
    
    query.execute(&self.pool).await?;
    
    // Silently succeed even if token not found (idempotent)
    Ok(())
}
```

**Note:** Unlike `ack_work_item()`, this method is idempotent—it succeeds even if the token doesn't exist.

### enqueue_orchestrator_work() — Adding to Orchestrator Queue

Add a work item to the orchestrator queue with optional delay (for timers):

```rust
async fn enqueue_orchestrator_work(
    &self,
    item: WorkItem,
    delay: Option<Duration>,
) -> Result<(), ProviderError> {
    let now = current_time_ms();
    let visible_at = delay.map(|d| now + d.as_millis() as i64).unwrap_or(now);
    let instance_id = extract_instance_from_workitem(&item);
    let item_json = serde_json::to_string(&item)
        .map_err(|e| ProviderError::permanent("enqueue", e.to_string()))?;
    
    sqlx::query!(
        "INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
         VALUES (?, ?, ?)",
        instance_id, item_json, visible_at
    )
    .execute(&self.pool)
    .await
    .map_err(|e| ProviderError::retryable("enqueue_orchestrator_work", e.to_string()))?;
    
    Ok(())
}

fn extract_instance_from_workitem(item: &WorkItem) -> String {
    match item {
        WorkItem::StartOrchestration { instance, .. } => instance.clone(),
        WorkItem::ActivityCompleted { instance, .. } => instance.clone(),
        WorkItem::ActivityFailed { instance, .. } => instance.clone(),
        WorkItem::TimerFired { instance, .. } => instance.clone(),
        WorkItem::ExternalRaised { instance, .. } => instance.clone(),
        WorkItem::SubOrchCompleted { instance, .. } => instance.clone(),
        WorkItem::SubOrchFailed { instance, .. } => instance.clone(),
        WorkItem::ContinueAsNew { instance, .. } => instance.clone(),
        WorkItem::CancelInstance { instance, .. } => instance.clone(),
        // ActivityExecute goes to worker queue, not here
        _ => panic!("Unexpected work item type for orchestrator queue"),
    }
}
```

**Important:** This method does NOT create the instance. Instance creation happens in `ack_orchestration_item()` when the runtime provides metadata.

### read() and append_with_execution() — History Operations

These are the simplest methods. See [Step 1: Event History Storage](#step-1-event-history-storage) for implementations.

Key points:
- `read()` returns events for the **latest** execution, ordered by `event_id`
- `append_with_execution()` stores events with **runtime-provided** IDs
- Both are relatively simple compared to queue operations

### fetch_work_item() — Complete Implementation

Fetch and lock a single item from the worker queue:

```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    _poll_timeout: Duration,  // For long-polling providers
) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError> {
    let now = current_time_ms();
    let lock_token = uuid::Uuid::new_v4().to_string();
    let locked_until = now + lock_timeout.as_millis() as i64;
    
    // Atomically find, lock, and increment attempt_count
    let result = sqlx::query!(
        r#"
        UPDATE worker_queue
        SET lock_token = ?1, 
            locked_until = ?2, 
            attempt_count = attempt_count + 1
        WHERE id = (
            SELECT id FROM worker_queue
            WHERE visible_at <= ?3 
              AND (lock_token IS NULL OR locked_until <= ?3)
            ORDER BY id 
            LIMIT 1
        )
        RETURNING id, work_item, attempt_count, instance_id, execution_id
        "#,
        lock_token, locked_until, now
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(|e| ProviderError::retryable("fetch_work_item", e.to_string()))?;
    
    match result {
        None => Ok(None),  // Queue is empty
        Some(row) => {
            let item: WorkItem = serde_json::from_str(&row.work_item)
                .map_err(|e| ProviderError::permanent("fetch_work_item", e.to_string()))?;
            let attempt_count = row.attempt_count as u32;
            
            // Check if the orchestration is cancelled/completed
            let execution_state = if let Some(instance_id) = &row.instance_id {
                self.get_execution_state(instance_id, row.execution_id).await
            } else {
                ExecutionState::Running
            };
            
            Ok(Some((item, lock_token, attempt_count, execution_state)))
        }
    }
}

async fn get_execution_state(&self, instance_id: &str, execution_id: Option<i64>) -> ExecutionState {
    // Check instance status - if terminal, activity should be cancelled
    let status = sqlx::query_scalar!(
        "SELECT status FROM instances WHERE instance_id = ?",
        instance_id
    )
    .fetch_optional(&self.pool)
    .await
    .ok()
    .flatten();
    
    match status.as_deref() {
        Some("Completed") | Some("Failed") => ExecutionState::Terminated,
        _ => ExecutionState::Running,
    }
}
```

**Return value:**
- `None` — Queue is empty (normal, not an error)
- `Some((item, token, attempts, state))` — Successfully locked an item
  - `item`: The work item to execute
  - `token`: Lock token for ack/abandon/renew
  - `attempts`: How many times this item has been fetched (for poison detection)
  - `state`: Execution state (Running or Terminated for cancellation)

### ack_work_item() — Complete Implementation

Delete from worker queue and optionally enqueue completion:

```rust
async fn ack_work_item(
    &self,
    token: &str,
    completion: Option<WorkItem>,
) -> Result<(), ProviderError> {
    let mut tx = self.pool.begin().await
        .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    
    // Delete the locked item
    let deleted = sqlx::query!(
        "DELETE FROM worker_queue WHERE lock_token = ?",
        token
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    
    // If nothing deleted, lock was stolen (activity cancelled)
    if deleted.rows_affected() == 0 {
        tx.rollback().await.ok();
        return Err(ProviderError::permanent(
            "ack_work_item",
            "Lock token not found - activity was cancelled via lock stealing"
        ));
    }
    
    // Enqueue completion to orchestrator queue
    if let Some(completion) = completion {
        let instance_id = extract_instance_from_workitem(&completion);
        let item_json = serde_json::to_string(&completion)
            .map_err(|e| ProviderError::permanent("ack_work_item", e.to_string()))?;
        let now = current_time_ms();
        
        sqlx::query!(
            "INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
             VALUES (?, ?, ?)",
            instance_id, item_json, now
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    }
    
    tx.commit().await
        .map_err(|e| ProviderError::retryable("ack_work_item", e.to_string()))?;
    
    Ok(())
}
```

**Critical:** Returns a **permanent error** when the token is not found. This signals to the worker that the activity was cancelled via lock stealing.

### renew_work_item_lock() — Complete Implementation

Extend the lock for a long-running activity:

```rust
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_for: Duration,
) -> Result<ExecutionState, ProviderError> {
    let now = current_time_ms();
    let new_locked_until = now + extend_for.as_millis() as i64;
    
    // Only renew if lock is still valid (not expired, not stolen)
    let result = sqlx::query!(
        r#"
        UPDATE worker_queue
        SET locked_until = ?1
        WHERE lock_token = ?2 AND locked_until > ?3
        RETURNING instance_id, execution_id
        "#,
        new_locked_until, token, now
    )
    .fetch_optional(&self.pool)
    .await
    .map_err(|e| ProviderError::retryable("renew_work_item_lock", e.to_string()))?;
    
    match result {
        None => {
            // Lock not found or expired = activity was cancelled
            Err(ProviderError::permanent(
                "renew_work_item_lock",
                "Lock not found or expired - activity was cancelled"
            ))
        }
        Some(row) => {
            // Check if orchestration is in terminal state
            let state = if let Some(instance_id) = &row.instance_id {
                self.get_execution_state(instance_id, row.execution_id).await
            } else {
                ExecutionState::Running
            };
            Ok(state)
        }
    }
}
```

**Return value:** Returns `ExecutionState` so the worker can detect if the orchestration terminated (for cooperative cancellation).

---

## Advanced Topics

### Activity Cancellation via Lock Stealing

When an orchestration terminates or a `select()` determines a loser, in-flight activities should be cancelled. This is implemented via **lock stealing**:

1. Runtime identifies activities to cancel (via `cancelled_activities` in ack)
2. Provider deletes those entries from worker queue (same transaction as ack)
3. Worker's next lock renewal fails (entry gone)
4. Worker triggers activity's cancellation token
5. Activity checks `ctx.is_cancelled()` and exits gracefully

```rust
// In ack_orchestration_item():
for cancelled in cancelled_activities {
    sqlx::query!(
        "DELETE FROM worker_queue 
         WHERE instance_id = ? AND execution_id = ? AND activity_id = ?",
        cancelled.instance, cancelled.execution_id, cancelled.activity_id
    )
    .execute(&mut *tx)
    .await?;
}
```

### Lock Renewal for Long-Running Activities

Activities that run longer than the lock timeout need lock renewal. The runtime automatically calls `renew_work_item_lock()`:

```rust
async fn renew_work_item_lock(
    &self, 
    token: &str, 
    extend_for: Duration
) -> Result<ExecutionState, ProviderError> {
    let now = current_time_ms();
    let new_locked_until = now + extend_for.as_millis() as i64;
    
    let result = sqlx::query!(
        "UPDATE worker_queue 
         SET locked_until = ?
         WHERE lock_token = ? AND locked_until > ?
         RETURNING instance_id",
        new_locked_until, token, now
    )
    .fetch_optional(&self.pool)
    .await?;
    
    match result {
        Some(row) => {
            // Return execution state for cancellation detection
            let state = self.get_execution_state(&row.instance_id).await;
            Ok(state)
        }
        None => {
            // Entry missing = lock was stolen (activity cancelled)
            Err(ProviderError::permanent(
                "renew_work_item_lock",
                "Lock not found - activity was cancelled"
            ))
        }
    }
}
```

### Multi-Execution Support (continue_as_new)

Orchestrations can call `continue_as_new()` to start a fresh execution with new input. This creates a new `execution_id` while keeping the same `instance_id`:

```
Instance "order-123":
  Execution 1: [OrchestrationStarted, ..., OrchestrationContinuedAsNew]
  Execution 2: [OrchestrationStarted, ..., OrchestrationContinuedAsNew]
  Execution 3: [OrchestrationStarted, ..., OrchestrationCompleted]  ← current
```

Providers must:
- Track `current_execution_id` per instance
- Load history for the current execution only
- Support incrementing execution_id via metadata updates

---

## Schema Recommendations

### Recommended Table Structure

```sql
-- Instance metadata
CREATE TABLE instances (
    instance_id TEXT PRIMARY KEY,
    orchestration_name TEXT NOT NULL,
    orchestration_version TEXT,
    current_execution_id INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'Running',  -- Running, Completed, Failed
    output TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Event history (append-only)
CREATE TABLE history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    event_data TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    
    UNIQUE(instance_id, execution_id, event_id)
);

-- Orchestrator queue
CREATE TABLE orchestrator_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    work_item TEXT NOT NULL,
    visible_at INTEGER NOT NULL,
    lock_token TEXT,
    locked_until INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0
);

-- Instance locks (separate from queue for clarity)
CREATE TABLE instance_locks (
    instance_id TEXT PRIMARY KEY,
    lock_token TEXT NOT NULL,
    locked_until INTEGER NOT NULL
);

-- Worker queue
CREATE TABLE worker_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item TEXT NOT NULL,
    visible_at INTEGER NOT NULL,
    lock_token TEXT,
    locked_until INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    instance_id TEXT,
    execution_id INTEGER,
    activity_id INTEGER
);

-- Indexes
CREATE INDEX idx_orch_queue_fetch ON orchestrator_queue (visible_at, instance_id) 
    WHERE lock_token IS NULL;
CREATE INDEX idx_worker_queue_fetch ON worker_queue (visible_at) 
    WHERE lock_token IS NULL;
CREATE INDEX idx_worker_activity ON worker_queue (instance_id, execution_id, activity_id);
CREATE INDEX idx_history_lookup ON history (instance_id, execution_id, event_id);
```

---

## Testing Your Provider

Use the built-in validation test suite:

```rust
#[tokio::test]
async fn test_my_provider() {
    let provider = MyProvider::new(...);
    
    // Run all provider validation tests
    duroxide::provider_validation::run_all_validations(provider).await;
}
```

The validation suite tests:
- Basic CRUD operations
- Peek-lock semantics
- Atomicity guarantees
- Lock expiration
- Concurrent access
- Error handling
- **Capability filtering** — version-based fetch filtering, NULL compatibility, boundary versions, ContinueAsNew isolation
- **Deserialization contract** — corrupted history handling, attempt_count increment, poison path

The capability filtering tests (`provider_validations::capability_filtering`) validate:
- Filter-before-lock ordering (incompatible items never locked)
- NULL pinned version treated as always compatible
- Boundary version correctness at range edges
- ContinueAsNew creates independent pinned versions (not inherited)
- Deserialization errors produce permanent errors with incremented attempt_count
- Filter applied before history deserialization (corrupted history + excluded version = no error)

See `tests/sqlite_provider_validations.rs` for examples of wiring these tests for a specific provider.

---

## Common Pitfalls

### 1. Generating IDs in the Provider

**Wrong:**
```rust
let event_id = self.max_event_id() + 1;  // ❌
```

**Right:**
```rust
let event_id = event.event_id();  // ✅ Use runtime-provided ID
```

### 2. Non-Atomic Ack Operations

**Wrong:**
```rust
self.delete_message(token).await?;  // What if we crash here?
self.enqueue_completion(completion).await?;  // Completion lost!
```

**Right:**
```rust
let mut tx = self.pool.begin().await?;
self.delete_message_tx(&mut tx, token).await?;
self.enqueue_completion_tx(&mut tx, completion).await?;
tx.commit().await?;  // All or nothing
```

### 3. Creating Instances in enqueue_orchestrator_work

**Wrong:**
```rust
async fn enqueue_orchestrator_work(&self, item: WorkItem, ...) {
    // ❌ Don't create instance here!
    self.create_instance_if_not_exists(item.instance()).await?;
    self.enqueue(item).await?;
}
```

**Right:**
```rust
async fn enqueue_orchestrator_work(&self, item: WorkItem, ...) {
    // ✅ Just enqueue - instance created in ack_orchestration_item
    self.enqueue(item).await?;
}
```

### 4. Wrong Ordering of Enqueue and Cancel

**Wrong:**
```rust
// ❌ Cancel before enqueue = stale entry remains
for c in cancelled_activities { delete(c); }
for w in worker_items { insert(w); }
```

**Right:**
```rust
// ✅ Enqueue before cancel = correct for same-turn schedule+cancel
for w in worker_items { insert(w); }
for c in cancelled_activities { delete(c); }
```

### 5. Not Validating Lock on Renewal

**Wrong:**
```rust
async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) {
    // ❌ Doesn't check if lock is still valid
    update_locked_until(token, now() + extend_for);
    Ok(())
}
```

**Right:**
```rust
async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) {
    // ✅ Only renew if lock is still valid
    let result = UPDATE ... WHERE lock_token = token AND locked_until > now();
    if result.rows_affected == 0 {
        Err(ProviderError::permanent("renew", "Lock expired or stolen"))
    } else {
        Ok(...)
    }
}
```

---

## Validation Checklist

Before considering your provider complete:

### Core Operations
- [ ] `read()` returns events ordered by event_id
- [ ] `read()` returns empty Vec for non-existent instance
- [ ] `append_with_execution()` stores events with runtime-provided IDs
- [ ] `append_with_execution()` rejects duplicate event_ids

### Worker Queue
- [ ] `fetch_work_item()` returns `None` when queue empty
- [ ] `fetch_work_item()` locks item with unique token
- [ ] `fetch_work_item()` increments attempt_count
- [ ] `ack_work_item()` atomically deletes + enqueues completion
- [ ] `ack_work_item()` fails when token not found (lock stolen)
- [ ] `abandon_work_item()` makes item visible again
- [ ] `renew_work_item_lock()` extends lock timeout
- [ ] `renew_work_item_lock()` fails when token not found

### Orchestrator Queue
- [ ] `fetch_orchestration_item()` acquires instance-level lock
- [ ] `fetch_orchestration_item()` tags all visible messages
- [ ] `fetch_orchestration_item()` loads correct history
- [ ] `fetch_orchestration_item()` applies capability filter BEFORE acquiring lock and loading history
- [ ] `fetch_orchestration_item()` returns `Ok(None)` for incompatible items (not an error)
- [ ] `fetch_orchestration_item()` treats NULL pinned version as always compatible
- [ ] `fetch_orchestration_item()` returns `ProviderError::permanent` on history deserialization failure (not silent drop)
- [ ] `ack_orchestration_item()` is fully atomic
- [ ] `ack_orchestration_item()` validates lock before committing
- [ ] `ack_orchestration_item()` handles cancelled_activities correctly
- [ ] `ack_orchestration_item()` enqueues before cancelling (ordering)
- [ ] `ack_orchestration_item()` stores `pinned_duroxide_version` unconditionally from `ExecutionMetadata` when provided (no write-once guard — the runtime enforces this invariant)

### Concurrency
- [ ] Lock acquisition is atomic (no check-then-set)
- [ ] Expired locks can be acquired by other dispatchers
- [ ] Instance locks don't block other instances

### Error Handling
- [ ] Retryable errors for transient failures
- [ ] Permanent errors for unrecoverable failures
- [ ] Lock renewal failure returns error (not silent success)

---

## Performance Considerations

### Indexes (Critical for Performance)

```sql
-- Orchestrator queue (hot path)
CREATE INDEX idx_orch_visible ON orchestrator_queue(visible_at, lock_token);
CREATE INDEX idx_orch_instance ON orchestrator_queue(instance_id);

-- Worker queue
CREATE INDEX idx_worker_lock ON worker_queue(lock_token);

-- History (for read operations)
CREATE INDEX idx_history_lookup ON history(instance_id, execution_id, event_id);
```

### Connection Pooling

- Use connection pools for concurrent dispatcher access
- Recommended pool size: 5-10 connections
- SQLite example: `SqlitePoolOptions::new().max_connections(10)`

### Lock Timeout

- Recommended: 30 seconds
- Too short: False retries under load
- Too long: Slow recovery from crashes

### Runtime Polling Configuration

The default runtime polling interval (`dispatcher_min_poll_interval`) is 10ms, optimized for low-latency local providers like SQLite.

For remote database providers, configure a longer polling interval:

```rust
let runtime = DuroxideRuntime::builder()
    .with_provider(my_remote_provider)
    .with_options(RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(100),
        ..Default::default()
    })
    .build()
    .await?;
```

**Guidelines:**
- **Local SQLite:** 10ms (default)
- **Local PostgreSQL:** 10-50ms
- **Remote PostgreSQL:** 50-200ms
- **Cloud-hosted databases:** 100-500ms

---

## Implementing ProviderAdmin (Optional)

The `ProviderAdmin` trait provides management and observability features. While optional, implementing it enables instance discovery, metrics, and deletion capabilities.

### When to Implement

**Implement if:** Production use, need instance discovery, want metrics/monitoring, need debugging capabilities.

**Skip if:** Building a minimal/test provider, or storage backend doesn't support efficient queries.

### The Trait

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Send + Sync {
    // Discovery
    async fn list_instances(&self) -> Result<Vec<String>, ProviderError>;
    async fn list_instances_by_status(&self, status: &str) -> Result<Vec<String>, ProviderError>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ProviderError>;

    // History access
    async fn read_history_with_execution_id(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError>;
    async fn read_history(&self, instance: &str) -> Result<Vec<Event>, ProviderError>;
    async fn latest_execution_id(&self, instance: &str) -> Result<u64, ProviderError>;

    // Metadata
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, ProviderError>;
    async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, ProviderError>;

    // System metrics
    async fn get_system_metrics(&self) -> Result<SystemMetrics, ProviderError>;
    async fn get_queue_depths(&self) -> Result<QueueDepths, ProviderError>;

    // Instance hierarchy (for cascade deletion)
    async fn get_parent_id(&self, instance: &str) -> Result<Option<String>, ProviderError>;
    async fn list_children(&self, instance: &str) -> Result<Vec<String>, ProviderError>;
    async fn get_instance_tree(&self, instance: &str) -> Result<InstanceTree, ProviderError>;

    // Deletion
    async fn delete_instance(&self, instance: &str, force: bool) -> Result<DeleteInstanceResult, ProviderError>;
    async fn delete_instances_atomic(&self, instance_ids: &[&str], force: bool) -> Result<DeleteInstanceResult, ProviderError>;
    async fn delete_instance_bulk(&self, filter: InstanceFilter) -> Result<DeleteInstanceResult, ProviderError>;

    // Pruning (execution history cleanup)
    async fn prune_executions(&self, instance: &str, options: PruneOptions) -> Result<PruneResult, ProviderError>;
    async fn prune_executions_bulk(&self, filter: InstanceFilter, options: PruneOptions) -> Result<PruneResult, ProviderError>;
}
```

### Enabling ProviderAdmin

```rust
#[async_trait::async_trait]
impl Provider for MyProvider {
    // ... core Provider methods ...
    
    fn as_management_capability(&self) -> Option<&dyn ProviderAdmin> {
        Some(self)  // Enable management features
    }
}
```

### Client Auto-Discovery

```rust
let client = Client::new(provider);

if client.has_management_capability() {
    let instances = client.list_all_instances().await?;
    let metrics = client.get_system_metrics().await?;
}
```

### Key Types

```rust
pub struct InstanceInfo {
    pub instance_id: String,
    pub orchestration_name: String,
    pub orchestration_version: String,
    pub current_execution_id: u64,
    pub status: String,              // "Running", "Completed", "Failed"
    pub output: Option<String>,
    pub created_at: String,
}

pub struct SystemMetrics {
    pub total_instances: usize,
    pub total_executions: usize,
    pub running_instances: usize,
    pub completed_instances: usize,
    pub failed_instances: usize,
    pub total_events: usize,
}

pub struct DeleteInstanceResult {
    pub instances_deleted: u64,
    pub executions_deleted: u64,
    pub events_deleted: u64,
    pub queue_messages_deleted: u64,
}

pub struct PruneResult {
    pub instances_processed: u64,
    pub executions_deleted: u64,
    pub events_deleted: u64,
}
```

### Provider Contracts

| Contract | Requirement |
|----------|-------------|
| **Current execution protected** | `current_execution_id` MUST NEVER be pruned |
| **Running protected** | Running executions MUST NEVER be pruned |
| **Atomic** | All deletions MUST be atomic (single transaction) |
| **Cascade delete** | Deleting parent MUST delete all descendants |
| **Force semantics** | `force=false` rejects Running instances |

See `src/providers/sqlite.rs` for complete implementation examples.

---

## Getting Help

- **Reference implementations:**
  - SQLite (bundled): `src/providers/sqlite.rs`
  - PostgreSQL (external): [duroxide-pg](https://github.com/affandar/duroxide-pg)
- **Validation tests:** `tests/sqlite_provider_validations.rs`
- **Provider trait docs:** `src/providers/mod.rs`
- **Testing guide:** `docs/provider-testing-guide.md`
