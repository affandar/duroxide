use crate::Event;
use std::any::Any;

/// Orchestration item containing all data needed to process an instance atomically.
///
/// This represents a locked batch of work for a single orchestration instance.
/// The provider must guarantee that no other process can modify this instance
/// until `ack_orchestration_item()` or `abandon_orchestration_item()` is called.
///
/// # Fields
///
/// * `instance` - Unique identifier for the orchestration instance (e.g., "order-123")
/// * `orchestration_name` - Name of the orchestration being executed (e.g., "ProcessOrder")
/// * `execution_id` - Current execution ID (starts at 1, increments with ContinueAsNew)
/// * `version` - Orchestration version string (e.g., "1.0.0")
/// * `history` - Complete event history for the current execution (ordered by event_id)
/// * `messages` - Batch of WorkItems to process (may include Start, completions, external events)
/// * `lock_token` - Unique token that must be used to ack or abandon this batch
///
/// # Implementation Notes
///
/// - The provider must ensure `history` contains ALL events for `execution_id` in order
/// - For multi-execution instances (ContinueAsNew), only return the LATEST execution's history
/// - The `lock_token` must be unique and prevent concurrent processing of the same instance
/// - All messages in the batch should belong to the same instance
/// - The lock should expire after a timeout (e.g., 30s) to handle worker crashes
///
/// # Example from SQLite Provider
///
/// ```text
/// // 1. Find available instance (check instance_locks for active locks)
/// SELECT q.instance_id FROM orchestrator_queue q
/// LEFT JOIN instance_locks il ON q.instance_id = il.instance_id
/// WHERE q.visible_at <= now() 
///   AND (il.instance_id IS NULL OR il.locked_until <= now())
/// ORDER BY q.id LIMIT 1
///
/// // 2. Atomically acquire instance-level lock
/// INSERT INTO instance_locks (instance_id, lock_token, locked_until, locked_at)
/// VALUES (?, ?, ?, ?)
/// ON CONFLICT(instance_id) DO UPDATE
/// SET lock_token = excluded.lock_token, locked_until = excluded.locked_until
/// WHERE locked_until <= excluded.locked_at
///
/// // 3. Lock all visible messages for that instance
/// UPDATE orchestrator_queue SET lock_token = ?, locked_until = ?
/// WHERE instance_id = ? AND visible_at <= now()
///
/// // 4. Load instance metadata
/// SELECT orchestration_name, orchestration_version, current_execution_id
/// FROM instances WHERE instance_id = ?
///
/// // 5. Load history for current execution
/// SELECT event_data FROM history
/// WHERE instance_id = ? AND execution_id = ?
/// ORDER BY event_id
///
/// // 6. Return OrchestrationItem with lock_token
/// ```
#[derive(Debug, Clone)]
pub struct OrchestrationItem {
    pub instance: String,
    pub orchestration_name: String,
    pub execution_id: u64,
    pub version: String,
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
}

/// Execution metadata computed by the runtime to be persisted by the provider.
///
/// The runtime inspects the `history_delta` and `orchestrator_items` to compute this metadata.
/// **Providers must NOT inspect event contents themselves** - they should blindly store this metadata.
///
/// This design ensures the provider remains a pure storage abstraction without orchestration knowledge.
///
/// # Fields
///
/// * `status` - New execution status: `Some("Completed")`, `Some("Failed")`, `Some("ContinuedAsNew")`, or `None`
///   - `None` means the execution is still running (no status update needed)
///   - Provider should UPDATE the execution status column when `Some(...)`
///
/// * `output` - The terminal value to store (depends on status):
///   - `Completed`: The orchestration's successful result
///   - `Failed`: The error message
///   - `ContinuedAsNew`: The input that was passed to continue_as_new()
///   - `None`: No output (execution still running)
///
/// # Example Usage in Provider
///
/// ```text
/// async fn ack_orchestration_item(..., metadata: ExecutionMetadata) {
///     // Store metadata without understanding what it means
///     if let Some(status) = &metadata.status {
///         UPDATE executions SET status = ?, output = ? WHERE instance_id = ? AND execution_id = ?
///     }
/// }
/// ```
///
/// # ContinueAsNew Handling
///
/// ContinueAsNew is handled entirely by the runtime. Providers must NOT try to
/// synthesize new executions in `fetch_orchestration_item`.
///
/// Runtime behavior:
/// - When an orchestration calls `continue_as_new(input)`, the runtime stamps
///   `OrchestrationContinuedAsNew` into the current execution's history and enqueues
///   a `WorkItem::ContinueAsNew`.
/// - When processing that work item, the runtime starts a fresh execution with
///   `execution_id = current + 1`, passes an empty `existing_history`, and stamps an
///   `OrchestrationStarted { event_id: 1, .. }` event for the new execution.
/// - The runtime then calls `ack_orchestration_item(lock_token, execution_id, ...)` with
///   the explicit execution id to persist history and queue operations.
///
/// Provider responsibilities:
/// - Use the explicit `execution_id` given to `ack_orchestration_item`.
/// - Idempotently create the execution row (`INSERT OR IGNORE`).
/// - Update `instances.current_execution_id = MAX(current_execution_id, execution_id)`.
/// - Append all `history_delta` events to the specified `execution_id`.
/// - Update `executions.status, executions.output` from `ExecutionMetadata` when provided.
#[derive(Debug, Clone, Default)]
pub struct ExecutionMetadata {
    /// New status for the execution ('Completed', 'Failed', 'ContinuedAsNew', or None to keep current)
    pub status: Option<String>,
    /// Output/error/input to store (for Completed/Failed/ContinuedAsNew)
    pub output: Option<String>,
    /// Orchestration name (for new instances or updates)
    pub orchestration_name: Option<String>,
    /// Orchestration version (for new instances or updates)
    pub orchestration_version: Option<String>,
}

/// Provider-backed work queue items the runtime consumes continually.
///
/// WorkItems represent messages that flow through provider-managed queues.
/// They are serialized/deserialized using serde_json for storage.
///
/// # Queue Routing
///
/// Different WorkItem types go to different queues:
/// - `StartOrchestration, ContinueAsNew, ActivityCompleted/Failed, TimerFired, ExternalRaised, SubOrchCompleted/Failed, CancelInstance` → **Orchestrator queue**
///   - TimerFired items use `visible_at = fire_at_ms` for delayed visibility
/// - `ActivityExecute` → **Worker queue**
///
/// # Instance ID Extraction
///
/// Most WorkItems have an `instance` field. Sub-orchestration completions use `parent_instance`.
/// Providers need to extract the instance ID for routing. SQLite example:
///
/// ```ignore
/// let instance = match &item {
///     WorkItem::StartOrchestration { instance, .. } |
///     WorkItem::ActivityCompleted { instance, .. } |
///     WorkItem::CancelInstance { instance, .. } => instance,
///     WorkItem::SubOrchCompleted { parent_instance, .. } => parent_instance,
///     _ => return Err("unexpected item type"),
/// };
/// ```
///
/// # Execution ID Tracking
///
/// WorkItems for activities, timers, and sub-orchestrations include `execution_id`.
/// This allows providers to route completions to the correct execution when ContinueAsNew creates multiple executions.
///
/// **Critical:** Completions with mismatched execution_id should still be enqueued (the runtime filters them).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum WorkItem {
    /// Start a new orchestration instance
    /// - Instance metadata is created by runtime via ack_orchestration_item metadata (not on enqueue)
    /// - `execution_id`: The execution ID for this start (usually INITIAL_EXECUTION_ID=1)
    /// - `version`: None means runtime will resolve from registry
    StartOrchestration {
        instance: String,
        orchestration: String,
        input: String,
        version: Option<String>,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
        execution_id: u64,
    },

    /// Execute an activity (goes to worker queue)
    /// - `id`: event_id from ActivityScheduled (for correlation)
    /// - Worker will enqueue ActivityCompleted or ActivityFailed
    ActivityExecute {
        instance: String,
        execution_id: u64,
        id: u64, // scheduling_event_id from ActivityScheduled
        name: String,
        input: String,
    },

    /// Activity completed successfully (goes to orchestrator queue)
    /// - `id`: source_event_id referencing the ActivityScheduled event
    /// - Triggers next orchestration turn
    ActivityCompleted {
        instance: String,
        execution_id: u64,
        id: u64, // source_event_id referencing ActivityScheduled
        result: String,
    },

    /// Activity failed with error (goes to orchestrator queue)
    /// - `id`: source_event_id referencing the ActivityScheduled event
    /// - Triggers next orchestration turn
    ActivityFailed {
        instance: String,
        execution_id: u64,
        id: u64, // source_event_id referencing ActivityScheduled
        details: crate::ErrorDetails,
    },

    /// Timer fired (goes to orchestrator queue with delayed visibility)
    /// - Created directly by runtime when timer is scheduled
    /// - Enqueued to orchestrator queue with `visible_at = fire_at_ms`
    /// - Orchestrator dispatcher processes when `visible_at <= now()`
    TimerFired {
        instance: String,
        execution_id: u64,
        id: u64, // source_event_id referencing TimerCreated
        fire_at_ms: u64,
    },

    /// External event raised (goes to orchestrator queue)
    /// - Matched by `name` to ExternalSubscribed events
    /// - `data`: JSON payload from external system
    ExternalRaised {
        instance: String,
        name: String,
        data: String,
    },

    /// Sub-orchestration completed (goes to parent's orchestrator queue)
    /// - Routes to `parent_instance`, not the child
    /// - `parent_id`: event_id from parent's SubOrchestrationScheduled event
    SubOrchCompleted {
        parent_instance: String,
        parent_execution_id: u64,
        parent_id: u64, // source_event_id referencing SubOrchestrationScheduled
        result: String,
    },

    /// Sub-orchestration failed (goes to parent's orchestration queue)
    /// - Routes to `parent_instance`, not the child
    /// - `parent_id`: event_id from parent's SubOrchestrationScheduled event
    SubOrchFailed {
        parent_instance: String,
        parent_execution_id: u64,
        parent_id: u64, // source_event_id referencing SubOrchestrationScheduled
        details: crate::ErrorDetails,
    },

    /// Request orchestration cancellation (goes to orchestrator queue)
    /// - Runtime will append OrchestrationCancelRequested event
    /// - Eventually results in OrchestrationFailed with "canceled: {reason}"
    CancelInstance { instance: String, reason: String },

    /// Continue orchestration as new execution (goes to orchestrator queue)
    /// - Signals the end of current execution and start of next
    /// - Runtime will create Event::OrchestrationStarted for next execution
    /// - Provider should create new execution (see ExecutionMetadata.create_next_execution)
    ContinueAsNew {
        instance: String,
        orchestration: String,
        input: String,
        version: Option<String>,
    },
}

/// Provider abstraction for durable orchestration execution (persistence + queues).
///
/// # Overview
///
/// A Provider is responsible for:
/// 1. **Persistence**: Storing orchestration history (append-only event log)
/// 2. **Queueing**: Managing two work queues (orchestrator, worker)
/// 3. **Locking**: Implementing peek-lock semantics to prevent concurrent processing
/// 4. **Atomicity**: Ensuring transactional consistency across operations
///
/// # Architecture: Two-Queue Model
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────┐
/// │                     RUNTIME (2 Dispatchers)                 │
/// ├─────────────────────────────────────────────────────────────┤
/// │                                                             │
/// │  [Orchestration Dispatcher]  ←─── fetch_orchestration_item │
/// │           ↓                                                 │
/// │      Process Turn                                           │
/// │           ↓                                                 │
/// │  ack_orchestration_item ────┬──► Orchestrator Queue        │
/// │                             │   (TimerFired with delayed   │
/// │                             │    visibility for timers)   │
/// │                             └──► Worker Queue              │
/// │                                                             │
/// │  [Worker Dispatcher]  ←────────── dequeue_worker_peek_lock │
/// │       Execute Activity                                      │
/// │           ↓                                                 │
/// │  Completion ────────────────────► Orchestrator Queue        │
/// │                                                             │
/// └─────────────────────────────────────────────────────────────┘
///                           ↕
///              ┌────────────────────────┐
///              │   PROVIDER (Storage)   │
///              │  ┌──────────────────┐  │
///              │  │ History (Events) │  │
///              │  ├──────────────────┤  │
///              │  │ Orch Queue       │  │
///              │  │ Worker Queue     │  │
///              │  └──────────────────┘  │
///              └────────────────────────┘
/// ```
///
/// # Design Principles
///
/// **Providers should be storage abstractions, NOT orchestration engines:**
/// - ✅ Store and retrieve events as opaque data (don't inspect Event contents)
/// - ✅ Manage queues and locks (generic queue operations)
/// - ✅ Provide ACID guarantees where possible
/// - ❌ DON'T interpret orchestration semantics (use ExecutionMetadata from runtime)
/// - ❌ DON'T create events (runtime creates all events)
/// - ❌ DON'T make orchestration decisions (runtime decides control flow)
///
/// # Multi-Execution Support (ContinueAsNew)
///
/// Orchestration instances can have multiple executions (execution_id 1, 2, 3, ...):
/// - Each execution has its own event history
/// - `read()` should return the LATEST execution's history
/// - Provider tracks current_execution_id to know which execution is active
/// - When metadata.create_next_execution=true, create new execution row
///
/// **Example:**
/// ```text
/// Instance "order-123":
///   Execution 1: [OrchestrationStarted, ActivityScheduled, OrchestrationContinuedAsNew]
///   Execution 2: [OrchestrationStarted, ActivityScheduled, OrchestrationCompleted]
///   
///   read("order-123") → Returns Execution 2's events (latest)
///   read_with_execution("order-123", 1) → Returns Execution 1's events
///   latest_execution_id("order-123") → Returns Some(2)
/// ```
///
/// # Concurrency Model
///
/// The runtime runs 3 background dispatchers polling your queues:
/// 1. **Orchestration Dispatcher**: Polls fetch_orchestration_item() continuously
/// 2. **Work Dispatcher**: Polls dequeue_worker_peek_lock() continuously
/// 3. **Timer Dispatcher**: Polls dequeue_timer_peek_lock() continuously
///
/// **Your implementation must be thread-safe** and support concurrent access from multiple dispatchers.
///
/// # Peek-Lock Pattern (Critical)
///
/// All dequeue operations use peek-lock semantics:
/// 1. **Peek**: Select and lock a message (message stays in queue)
/// 2. **Process**: Runtime processes the locked message
/// 3. **Ack**: Delete message from queue (success) OR
/// 4. **Abandon**: Release lock for retry (failure)
///
/// Benefits:
/// - At-least-once delivery (messages survive crashes)
/// - Automatic retry on worker failure (lock expires)
/// - Prevents duplicate processing (locked messages invisible to others)
///
/// # Transactional Guarantees
///
/// **`ack_orchestration_item()` is the atomic boundary:**
/// - ALL operations in ack must succeed or fail together
/// - History append + queue enqueues + lock release = atomic
/// - If commit fails, entire turn is retried
/// - This ensures exactly-once semantics for orchestration turns
///
/// # Queue Message Flow
///
/// **Orchestrator Queue:**
/// - Inputs: StartOrchestration, ActivityCompleted/Failed, TimerFired, ExternalRaised, SubOrchCompleted/Failed, CancelInstance, ContinueAsNew
/// - Output: Processed by orchestration dispatcher → ack_orchestration_item
/// - Batching: All messages for an instance processed together
/// - Timers: TimerFired items are enqueued with `visible_at = fire_at_ms` for delayed visibility
///
/// **Worker Queue:**
/// - Inputs: ActivityExecute (from ack_orchestration_item)
/// - Output: Processed by work dispatcher → ActivityCompleted/Failed to orch queue
/// - Batching: One message at a time (activities executed independently)
///
/// **Note:** There is no separate timer queue. Timers are handled by enqueuing TimerFired items
/// directly to the orchestrator queue with delayed visibility (`visible_at` set to `fire_at_ms`).
/// The orchestrator dispatcher processes them when `visible_at <= now()`.
///
/// # Instance Metadata Management
///
/// Providers typically maintain metadata about instances:
/// - instance_id (primary key)
/// - orchestration_name, orchestration_version
/// - current_execution_id (for multi-execution support)
/// - status, output (optional, for quick queries)
/// - created_at, updated_at timestamps
///
/// This metadata is updated via:
/// - `enqueue_orchestrator_work()` with StartOrchestration
/// - `ack_orchestration_item()` with history changes
/// - `ExecutionMetadata` for status/output updates
///
/// # Required vs Optional Methods
///
/// **REQUIRED** (must implement):
/// - fetch_orchestration_item, ack_orchestration_item, abandon_orchestration_item
/// - read, append_with_execution
/// - enqueue_worker_work, dequeue_worker_peek_lock, ack_worker
/// - enqueue_orchestrator_work
///
/// **OPTIONAL** (has defaults):
/// - latest_execution_id, read_with_execution
/// - list_instances, list_executions
///
/// # Recommended Database Schema (SQL Example)
///
/// ```sql
/// -- Instance metadata
/// CREATE TABLE instances (
///     instance_id TEXT PRIMARY KEY,
///     orchestration_name TEXT NOT NULL,
///     orchestration_version TEXT,  -- NULLable, set by runtime via metadata
///     current_execution_id INTEGER DEFAULT 1,
///     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
/// );
///
/// -- Execution tracking
/// CREATE TABLE executions (
///     instance_id TEXT NOT NULL,
///     execution_id INTEGER NOT NULL,
///     status TEXT DEFAULT 'Running',
///     output TEXT,
///     started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
///     completed_at TIMESTAMP,
///     PRIMARY KEY (instance_id, execution_id)
/// );
///
/// -- Event history (append-only)
/// CREATE TABLE history (
///     instance_id TEXT NOT NULL,
///     execution_id INTEGER NOT NULL,
///     event_id INTEGER NOT NULL,
///     event_type TEXT NOT NULL,
///     event_data TEXT NOT NULL,  -- JSON serialized Event
///     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
///     PRIMARY KEY (instance_id, execution_id, event_id)
/// );
///
/// -- Orchestrator queue (peek-lock)
/// CREATE TABLE orchestrator_queue (
///     id INTEGER PRIMARY KEY AUTOINCREMENT,
///     instance_id TEXT NOT NULL,
///     work_item TEXT NOT NULL,  -- JSON serialized WorkItem
///     visible_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
///     lock_token TEXT,
///     locked_until TIMESTAMP,
///     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
/// );
///
/// -- Worker queue (peek-lock)
/// CREATE TABLE worker_queue (
///     id INTEGER PRIMARY KEY AUTOINCREMENT,
///     work_item TEXT NOT NULL,
///     lock_token TEXT,
///     locked_until TIMESTAMP,
///     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
/// );
///
/// -- Instance-level locks (CRITICAL: Prevents concurrent processing)
/// CREATE TABLE instance_locks (
///     instance_id TEXT PRIMARY KEY,
///     lock_token TEXT NOT NULL,
///     locked_until INTEGER NOT NULL,  -- Unix timestamp (milliseconds)
///     locked_at INTEGER NOT NULL      -- Unix timestamp (milliseconds)
/// );
/// ```
///
/// # Implementing a New Provider: Checklist
///
/// 1. ✅ **Storage Layer**: Choose backing store (PostgreSQL, Redis, DynamoDB, etc.)
/// 2. ✅ **Serialization**: Use serde_json for Event and WorkItem (or compatible format)
/// 3. ✅ **Instance Locking**: Implement instance-level locks to prevent concurrent processing
/// 4. ✅ **Locking**: Implement peek-lock with unique tokens and expiration
/// 5. ✅ **Transactions**: Ensure `ack_orchestration_item` is atomic
/// 6. ✅ **Indexes**: Add indexes on `instance_id`, `lock_token`, `visible_at` for orchestrator queue
/// 7. ✅ **Delayed Visibility**: Support `visible_at` timestamps for TimerFired items in orchestrator queue
/// 8. ✅ **Testing**: Use `tests/sqlite_provider_validations.rs` as a template, or see `docs/provider-implementation-guide.md`
/// 9. ✅ **Multi-execution**: Support execution_id partitioning for ContinueAsNew
/// 10. ⚠️ **DO NOT**: Inspect event contents (use ExecutionMetadata)
/// 11. ⚠️ **DO NOT**: Create events (runtime owns event creation)
/// 12. ⚠️ **DO NOT**: Make orchestration decisions (runtime owns logic)
///
/// # Example: Minimal Redis Provider Sketch
///
/// ```ignore
/// struct RedisProvider {
///     client: redis::Client,
///     lock_timeout_ms: u64,
/// }
///
/// impl Provider for RedisProvider {
///     async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
///         // 1. RPOPLPUSH from "orch_queue" to "orch_processing"
///         let instance = self.client.rpoplpush("orch_queue", "orch_processing")?;
///         
///         // 2. Load history from "history:{instance}:{exec_id}" (sorted set)
///         let exec_id = self.client.get(&format!("instance:{instance}:exec_id")).unwrap_or(1);
///         let history = self.client.zrange(&format!("history:{instance}:{exec_id}"), 0, -1);
///         
///         // 3. Generate lock token and set expiration
///         let lock_token = uuid::Uuid::new_v4().to_string();
///         self.client.setex(&format!("lock:{lock_token}"), self.lock_timeout_ms, &instance);
///         
///         Some(OrchestrationItem { instance, history, lock_token, ... })
///     }
///     
///     async fn ack_orchestration_item(..., metadata: ExecutionMetadata) -> Result<(), String> {
///         let mut pipe = redis::pipe();
///         pipe.atomic(); // Use Redis transaction
///         
///         // Append history (ZADD to sorted set with event_id as score)
///         for event in history_delta {
///             pipe.zadd(&format!("history:{instance}:{exec_id}"), event_json, event.event_id());
///         }
///         
///         // Update metadata (HSET)
///         if let Some(status) = &metadata.status {
///             pipe.hset(&format!("exec:{instance}:{exec_id}"), "status", status);
///             pipe.hset(&format!("exec:{instance}:{exec_id}"), "output", &metadata.output);
///         }
///         
///         // Create next execution if needed
///         if metadata.create_next_execution {
///             pipe.incr(&format!("instance:{instance}:exec_id"), 1);
///         }
///         
///         // Enqueue worker/orchestrator items
///         for item in worker_items { pipe.lpush("worker_queue", serialize(item)); }
///         for item in orch_items { 
///             let visible_at = match &item {
///                 WorkItem::TimerFired { fire_at_ms, .. } => *fire_at_ms,
///                 _ => now()
///             };
///             pipe.zadd("orch_queue", serialize(item), visible_at); 
///         }
///         
///         // Release lock
///         pipe.del(&format!("lock:{lock_token}"));
///         pipe.lrem("orch_processing", 1, instance);
///         
///         pipe.execute()?;
///         Ok(())
///     }
/// }
/// ```
///
/// # Design Principles
///
/// **Providers should be storage abstractions, NOT orchestration engines:**
/// - ✅ Store and retrieve events as opaque data (don't inspect Event contents)
/// - ✅ Manage queues and locks (generic queue operations)
/// - ✅ Provide ACID guarantees where possible
/// - ❌ DON'T interpret orchestration semantics (use ExecutionMetadata from runtime)
/// - ❌ DON'T create events (runtime creates all events)
/// - ❌ DON'T make orchestration decisions (runtime decides control flow)
///
/// # Multi-Execution Support (ContinueAsNew)
///
/// Orchestration instances can have multiple executions (execution_id 1, 2, 3, ...):
/// - Each execution has its own event history
/// - `read()` should return the LATEST execution's history
/// - Provider tracks current_execution_id to know which execution is active
/// - When metadata.create_next_execution=true, create new execution row
///
/// **Example:**
/// ```text
/// Instance "order-123":
///   Execution 1: [OrchestrationStarted, ActivityScheduled, OrchestrationContinuedAsNew]
///   Execution 2: [OrchestrationStarted, ActivityScheduled, OrchestrationCompleted]
///   
///   read("order-123") → Returns Execution 2's events (latest)
///   read_with_execution("order-123", 1) → Returns Execution 1's events
///   latest_execution_id("order-123") → Returns Some(2)
///   list_executions("order-123") → Returns vec!\[1, 2\]
/// ```
///
/// # Concurrency Model
///
/// The runtime runs 3 background dispatchers polling your queues:
/// 1. **Orchestration Dispatcher**: Polls fetch_orchestration_item() continuously
/// 2. **Work Dispatcher**: Polls dequeue_worker_peek_lock() continuously
/// 3. **Timer Dispatcher**: Polls dequeue_timer_peek_lock() continuously
///
/// **Your implementation must be thread-safe** and support concurrent access from multiple dispatchers.
///
/// # Peek-Lock Pattern (Critical)
///
/// All dequeue operations use peek-lock semantics:
/// 1. **Peek**: Select and lock a message (message stays in queue)
/// 2. **Process**: Runtime processes the locked message
/// 3. **Ack**: Delete message from queue (success) OR
/// 4. **Abandon**: Release lock for retry (failure)
///
/// Benefits:
/// - At-least-once delivery (messages survive crashes)
/// - Automatic retry on worker failure (lock expires)
/// - Prevents duplicate processing (locked messages invisible to others)
///
/// # Transactional Guarantees
///
/// **`ack_orchestration_item()` is the atomic boundary:**
/// - ALL operations in ack must succeed or fail together
/// - History append + queue enqueues + lock release = atomic
/// - If commit fails, entire turn is retried
/// - This ensures exactly-once semantics for orchestration turns
///
/// # Queue Message Flow
///
/// **Orchestrator Queue:**
/// - Inputs: StartOrchestration, ActivityCompleted/Failed, TimerFired, ExternalRaised, SubOrchCompleted/Failed, CancelInstance, ContinueAsNew
/// - Output: Processed by orchestration dispatcher → ack_orchestration_item
/// - Batching: All messages for an instance processed together
/// - Ordering: FIFO per instance preferred
/// - Timers: TimerFired items are enqueued with `visible_at = fire_at_ms` for delayed visibility
///
/// **Worker Queue:**
/// - Inputs: ActivityExecute (from ack_orchestration_item)
/// - Output: Processed by work dispatcher → ActivityCompleted/Failed to orch queue
/// - Batching: One message at a time (activities executed independently)
/// - Ordering: FIFO preferred but not required
///
/// **Note:** There is no separate timer queue. Timers are handled by enqueuing TimerFired items
/// directly to the orchestrator queue with delayed visibility (`visible_at` set to `fire_at_ms`).
/// The orchestrator dispatcher processes them when `visible_at <= now()`.
///
/// # Instance Metadata Management
///
/// Providers typically maintain metadata about instances:
/// - instance_id (primary key)
/// - orchestration_name, orchestration_version (from StartOrchestration)
/// - current_execution_id (for multi-execution support)
/// - status, output (optional, from ExecutionMetadata)
/// - created_at, updated_at timestamps
///
/// This metadata is updated via:
/// - `enqueue_orchestrator_work()` with StartOrchestration (creates instance)
/// - `ack_orchestration_item()` with ExecutionMetadata (updates status/output)
/// - `ack_orchestration_item()` with metadata.create_next_execution (increments current_execution_id)
///
/// # Error Handling Philosophy
///
/// - **Transient errors** (busy, timeout): Return error, let runtime retry
/// - **Invalid tokens**: Return Ok (idempotent - already processed)
/// - **Corruption** (missing instance, invalid data): Return error
/// - **Concurrency conflicts**: Handle via locks, return error if deadlock
///
/// # Required vs Optional Methods
///
/// **REQUIRED** (must implement):
/// - fetch_orchestration_item, ack_orchestration_item, abandon_orchestration_item
/// - read, append_with_execution
/// - enqueue_worker_work, dequeue_worker_peek_lock, ack_worker
/// - enqueue_orchestrator_work
///
/// **OPTIONAL** (has defaults):
/// - latest_execution_id, read_with_execution
/// - list_instances, list_executions
///
/// # Testing Your Provider
///
/// See `tests/sqlite_provider_validations.rs` for comprehensive provider validation tests:
/// - Basic enqueue/dequeue operations
/// - Transactional atomicity
/// - Instance locking correctness (including multi-threaded tests)
/// - Lock expiration and redelivery
/// - Multi-execution support
/// - Execution status and output persistence
///
/// All tests use the Provider trait directly (not runtime), so they're portable to new providers.
/// See `docs/provider-implementation-guide.md` for detailed implementation guidance including instance locks.
///
/// Core provider trait for runtime orchestration operations.
///
/// This trait defines the essential methods required for durable orchestration execution.
/// It focuses on runtime-critical operations: fetching work items, processing history,
/// and managing queues. Management and observability features are provided through
/// optional capability traits.
///
/// # Capability Discovery
///
/// Providers can implement additional capability traits (like `ManagementCapability`)
/// to expose richer features. The `Client` automatically discovers these capabilities
/// through the `as_management_capability()` method.
///
/// # Implementation Guide for LLMs
///
/// When implementing a new provider, focus on these core methods:
///
/// ## Required Methods (9 total)
///
/// 1. **Orchestration Processing (3 methods)**
///    - `fetch_orchestration_item()` - Atomic batch processing
///    - `ack_orchestration_item()` - Commit processing results
///    - `abandon_orchestration_item()` - Release locks and retry
///
/// 2. **History Access (1 method)**
///    - `read()` - Get event history for status checks
///
/// 3. **Worker Queue (2 methods)**
///    - `dequeue_worker_peek_lock()` - Get activity work items
///    - `ack_worker()` - Acknowledge activity completion
///
/// 4. **Orchestrator Queue (1 method)**
///    - `enqueue_orchestrator_work()` - Enqueue control messages (including TimerFired with delayed visibility)
///
/// ## Optional Management Methods (2 methods)
///
/// These are included for backward compatibility but will be moved to
/// `ManagementCapability` in future versions:
///
/// - `list_instances()` - List all instance IDs
/// - `list_executions()` - List execution IDs for an instance
///
/// # Capability Detection
///
/// Implement `as_management_capability()` to expose management features:
///
/// ```ignore
/// impl Provider for MyProvider {
///     // ... implement required methods
///     
///     fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
///         Some(self as &dyn ManagementCapability)
///     }
/// }
///
/// impl ManagementCapability for MyProvider {
///     // ... implement management methods
/// }
/// ```
///
/// # Testing Your Provider
///
/// See `tests/sqlite_provider_validations.rs` for comprehensive provider validation tests:
/// - Basic enqueue/dequeue operations
/// - Transactional atomicity
/// - Instance locking correctness (including multi-threaded tests)
/// - Lock expiration and redelivery
/// - Multi-execution support
/// - Execution status and output persistence
///
/// All tests use the Provider trait directly (not runtime), so they're portable to new providers.
/// See `docs/provider-implementation-guide.md` for detailed implementation guidance including instance locks.
///
#[async_trait::async_trait]
pub trait Provider: Any + Send + Sync {
    // ===== Core Atomic Orchestration Methods (REQUIRED) =====
    // These three methods form the heart of reliable orchestration execution.
    //
    // ⚠️ CRITICAL ID GENERATION CONTRACT:
    //
    // The provider MUST NOT generate execution_id or event_id values.
    // All IDs are generated by the runtime and passed to the provider:
    //   - execution_id: Passed to ack_orchestration_item()
    //   - event_id: Set in each Event in the history_delta
    //
    // The provider's role is to STORE these IDs, not generate them.

    /// Fetch the next orchestration work item atomically.
    ///
    /// # What This Does
    ///
    /// 1. **Select an instance to process**: Find the next available message in orchestrator queue that is not locked
    /// 2. **Acquire instance-level lock**: Atomically claim the instance lock to prevent concurrent processing
    /// 3. **Lock ALL messages** for that instance (batch processing)
    /// 4. **Load history**: Get complete event history for the current execution
    /// 5. **Load metadata**: Get orchestration_name, version, execution_id
    /// 6. **Return locked batch**: Provider must prevent other processes from touching this instance
    ///
    /// # ⚠️ CRITICAL: Instance-Level Locking
    ///
    /// **You MUST acquire an instance-level lock BEFORE fetching messages.** This prevents concurrent
    /// dispatchers from processing the same instance simultaneously, which would cause race conditions
    /// and data corruption.
    ///
    /// **Required Implementation Pattern:**
    /// 1. Find available instance (join with `instance_locks` table to check lock status)
    /// 2. Atomically acquire instance lock (INSERT into `instance_locks` with conflict handling)
    /// 3. Verify lock acquisition succeeded (check rows_affected)
    /// 4. Only then proceed to lock and fetch messages
    ///
    /// See `docs/provider-implementation-guide.md` for detailed implementation guidance.
    ///
    /// # Peek-Lock Semantics
    ///
    /// - Messages remain in queue until `ack_orchestration_item()` is called
    /// - Generate a unique `lock_token` (e.g., UUID)
    /// - Set `locked_until` timestamp (e.g., now + 30 seconds)
    /// - If lock expires before ack, messages become available again (automatic retry)
    ///
    /// # Visibility Filtering
    ///
    /// Only return messages where `visible_at <= now()`:
    /// - Normal messages: visible immediately
    /// - Delayed messages: `visible_at = now + delay_ms` (used for timer backpressure)
    ///
    /// # Instance Batching
    ///
    /// **CRITICAL:** All messages in the batch must belong to the SAME instance.
    ///
    /// SQLite implementation:
    /// 1. Find first visible unlocked instance: `SELECT q.instance_id FROM orchestrator_queue q LEFT JOIN instance_locks il ON q.instance_id = il.instance_id WHERE q.visible_at <= now() AND (il.instance_id IS NULL OR il.locked_until <= now()) ORDER BY q.id LIMIT 1`
    /// 2. Atomically acquire instance lock: `INSERT INTO instance_locks (instance_id, lock_token, locked_until, locked_at) VALUES (?, ?, ?, ?) ON CONFLICT(instance_id) DO UPDATE SET ... WHERE locked_until <= ?`
    /// 3. Lock ALL messages for that instance: `UPDATE orchestrator_queue SET lock_token = ? WHERE instance_id = ? AND visible_at <= now()`
    /// 4. Fetch all locked messages: `SELECT * FROM orchestrator_queue WHERE lock_token = ?`
    ///
    /// # History Loading
    ///
    /// - For new instances (no history yet): return empty Vec
    /// - For existing instances: return ALL events for current execution_id, ordered by event_id
    /// - For multi-execution instances: return ONLY the LATEST execution's history
    ///
    /// SQLite example:
    /// ```text
    /// // Get current execution ID
    /// let exec_id = SELECT current_execution_id FROM instances WHERE instance_id = ?
    ///
    /// // Load history for that execution
    /// SELECT event_data FROM history
    /// WHERE instance_id = ? AND execution_id = ?
    /// ORDER BY event_id
    /// ```
    ///
    /// # Return Value
    ///
    /// - `Some(OrchestrationItem)` - Work is available and locked
    /// - `None` - No work available (dispatcher will sleep and retry)
    ///
    /// # Error Handling
    ///
    /// Don't panic on transient errors - return None and let dispatcher retry.
    /// Only panic on unrecoverable errors (e.g., corrupted database schema).
    ///
    /// # Concurrency
    ///
    /// This method is called continuously by the orchestration dispatcher.
    /// Must be thread-safe and handle concurrent calls gracefully.
    ///
/// # Example Implementation Pattern
///
/// ```ignore
/// async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
///     let tx = begin_transaction()?;
///     
///     // Step 1: Find next available instance (check instance_locks)
///     let instance_id = SELECT q.instance_id FROM orch_queue q
///         LEFT JOIN instance_locks il ON q.instance_id = il.instance_id
///         WHERE q.visible_at <= now() 
///           AND (il.instance_id IS NULL OR il.locked_until <= now())
///         ORDER BY q.id LIMIT 1;
///     
///     if instance_id.is_none() { return None; }
///     
///     // Step 2: Atomically acquire instance lock
///     let lock_token = generate_uuid();
///     let lock_result = INSERT INTO instance_locks (instance_id, lock_token, locked_until, locked_at)
///         VALUES (?, ?, ?, ?)
///         ON CONFLICT(instance_id) DO UPDATE
///         SET lock_token = excluded.lock_token, locked_until = excluded.locked_until
///         WHERE locked_until <= excluded.locked_at;
///     
///     if lock_result.rows_affected == 0 {
///         // Lock acquisition failed - another dispatcher has the lock
///         return None;
///     }
///     
///     // Step 3: Lock all messages for this instance
///     UPDATE orch_queue SET lock_token = ?, locked_until = now() + 30s
///         WHERE instance_id = ? AND visible_at <= now();
///     
///     // Step 4: Fetch locked messages
///     let messages = SELECT work_item FROM orch_queue WHERE lock_token = ?;
///     
///     // Step 5: Load instance metadata
///     let (name, version, exec_id) = SELECT ... FROM instances WHERE instance_id = ?;
///     
///     // Step 6: Load history for current execution
///     let history = SELECT event_data FROM history
///         WHERE instance_id = ? AND execution_id = ?
///         ORDER BY event_id;
///     
///     commit_transaction();
///     Some(OrchestrationItem { instance, orchestration_name, execution_id, version, history, messages, lock_token })
/// }
/// ```
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;

    /// Acknowledge successful orchestration processing atomically.
    ///
    /// This is the most critical method - it commits all changes from an orchestration turn atomically.
    ///
    /// # What This Does (ALL must be atomic)
    ///
    /// 1. **Validate lock token**: Verify instance lock is still valid and matches lock_token
    /// 2. **Remove instance lock**: Delete from `instance_locks` table (processing complete)
    /// 3. **Append new events** to history for current execution
    /// 4. **Update execution metadata** (status, output) using pre-computed metadata
    /// 5. **Create new execution** if metadata.create_next_execution=true (ContinueAsNew)
    /// 6. **Enqueue worker_items** to worker queue (activity executions)
    /// 7. **Enqueue orchestrator_items** to orchestrator queue (completions, new instances, TimerFired with delayed visibility)
    /// 8. **Delete acknowledged messages** from orchestrator queue (release lock)
    ///
    /// # Atomicity Requirements
    ///
    /// **CRITICAL:** All 8 operations above must succeed or fail together.
    ///
    /// - **If ANY operation fails**: Roll back the entire transaction
    /// - **If commit succeeds**: All changes are durable and visible
    ///
    /// This prevents:
    /// - Duplicate activity execution (history saved but worker item lost)
    /// - Lost completions (worker item enqueued but history not saved)
    /// - Orphaned locks (messages deleted but history append failed)
    /// - Stale instance locks (instance lock not removed)
    ///
    /// # Parameters
    ///
    /// * `lock_token` - Token from fetch_orchestration_item() - identifies locked messages
    /// * `history_delta` - New events to append (runtime assigns event_ids, provider stores as-is)
    /// * `worker_items` - Activity executions to enqueue (WorkItem::ActivityExecute)
    /// * `orchestrator_items` - Orchestrator messages to enqueue (completions, starts, TimerFired with delayed visibility)
    /// * `metadata` - Pre-computed execution state (DO NOT inspect events yourself!)
    ///
    /// # Event Storage (history_delta)
    ///
    /// **Important:** Store events exactly as provided. DO NOT:
    /// - Modify event_id (runtime assigns these)
    /// - Inspect event contents to make decisions
    /// - Filter or reorder events
    ///
    /// SQLite example:
    /// ```ignore
    /// for event in &history_delta {
    ///     let event_json = serde_json::to_string(&event)?;
    ///     let event_type = extract_discriminant_name(&event); // For indexing only
    ///     INSERT INTO history (instance_id, execution_id, event_id, event_data, event_type)
    ///     VALUES (?, ?, event.event_id(), event_json, event_type)
    /// }
    /// ```
    ///
    /// # Metadata Storage (ExecutionMetadata)
    ///
    /// **The runtime has ALREADY inspected events and computed metadata.**
    /// Provider just stores it:
    ///
    /// ```ignore
    /// // Update execution status/output from metadata
    /// if let Some(status) = &metadata.status {
    ///     UPDATE executions
    ///     SET status = ?, output = ?, completed_at = CURRENT_TIMESTAMP
    ///     WHERE instance_id = ? AND execution_id = ?
    /// }
    ///
    /// // Create next execution if requested
    /// if metadata.create_next_execution {
    ///     if let Some(next_id) = metadata.next_execution_id {
    ///         INSERT INTO executions (instance_id, execution_id, status)
    ///         VALUES (?, next_id, 'Running')
    ///         
    ///         UPDATE instances SET current_execution_id = next_id
    ///     }
    /// }
    /// ```
    ///
    /// # Queue Item Enqueuing
    ///
/// Worker items and orchestrator items must be enqueued within the same transaction:
///
/// ```ignore
/// // Worker queue (no special handling)
/// for item in worker_items {
///     INSERT INTO worker_queue (work_item) VALUES (serde_json::to_string(&item))
/// }
///
/// // Orchestrator queue (may have delayed visibility for TimerFired)
/// for item in orchestrator_items {
///     let visible_at = match &item {
///         WorkItem::TimerFired { fire_at_ms, .. } => *fire_at_ms,  // Delayed visibility
///         _ => now()  // Immediate visibility
///     };
///     
///     INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
///     VALUES (extract_instance(&item), serde_json::to_string(&item), visible_at)
/// }
/// ```
    ///
    /// # Lock Release
    ///
    /// Delete all messages with this lock_token:
    /// ```text
    /// DELETE FROM orchestrator_queue WHERE lock_token = ?
    /// ```
    ///
    /// # Error Handling
    ///
    /// - Return `Ok(())` if all operations succeeded
    /// - Return `Err(msg)` if any operation failed (transaction rolled back)
    /// - On error, runtime will call `abandon_orchestration_item()` to release lock
    ///
    /// # Special Cases
    ///
    /// **StartOrchestration in orchestrator_items:**
    /// - May need to create instance metadata row (INSERT OR IGNORE)
    /// - Should create execution row with ID=1 if new instance
    ///
    /// **Empty history_delta:**
    /// - Valid case (e.g., terminal instance being acked with no changes)
    /// - Still process queues and release lock
    ///
    /// # Parameters
    ///
    /// * `lock_token` - Token from `fetch_orchestration_item` identifying the batch
    /// * `execution_id` - **The execution ID this history belongs to** (runtime decides this)
    /// * `history_delta` - Events to append to the specified execution
    /// * `worker_items` - Activity work items to enqueue
    /// * `orchestrator_items` - Orchestration work items to enqueue (StartOrchestration, ContinueAsNew, TimerFired, etc.)
    ///   - TimerFired items should be enqueued with `visible_at = fire_at_ms` for delayed visibility
    /// * `metadata` - Pre-computed execution metadata (status, output)
    ///
    /// # TimerFired Items
    ///
    /// When `TimerFired` items are present in `orchestrator_items`, they should be enqueued with
    /// delayed visibility using the `fire_at_ms` field for the `visible_at` timestamp.
    /// This allows timers to fire at the correct logical time.
    ///
    /// # execution_id Parameter
    ///
    /// The `execution_id` parameter tells the provider **which execution this history belongs to**.
    /// The runtime is responsible for:
    /// - Deciding when to create new executions (e.g., for ContinueAsNew)
    /// - Managing execution ID sequencing
    /// - Ensuring each execution has its own isolated history
    ///
    /// The provider should:
    /// - **Create the execution record if it doesn't exist** (idempotent INSERT OR IGNORE)
    /// - Append `history_delta` to the specified `execution_id`
    /// - Update `instances.current_execution_id` if this execution_id is newer
    /// - NOT inspect WorkItems to decide execution IDs
    ///
    /// # SQLite Implementation Pattern
    ///
    /// ```text
    /// async fn ack_orchestration_item(execution_id, ...) -> Result<(), String> {
    ///     let tx = begin_transaction()?;
    ///     
    ///     // Step 1: Validate lock token and get instance_id from instance_locks
    ///     let instance_id = SELECT instance_id FROM instance_locks
    ///         WHERE lock_token = ? AND locked_until > now();
    ///     if instance_id.is_none() {
    ///         return Err("Invalid or expired lock token");
    ///     }
    ///     
    ///     // Step 2: Remove instance lock (processing complete)
    ///     DELETE FROM instance_locks WHERE instance_id = ? AND lock_token = ?;
    ///     
    ///     // Step 3: Create execution record if it doesn't exist (idempotent)
    ///     INSERT OR IGNORE INTO executions (instance_id, execution_id, status)
    ///     VALUES (?, ?, 'Running');
    ///     
    ///     // Step 4: Append history to the SPECIFIED execution_id
    ///     for event in history_delta {
    ///         INSERT INTO history (...) VALUES (instance_id, execution_id, event.event_id(), ...)
    ///     }
    ///     
    ///     // Step 5: Update execution metadata (no event inspection!)
    ///     if let Some(status) = &metadata.status {
    ///         UPDATE executions SET status = ?, output = ?, completed_at = NOW()
    ///         WHERE instance_id = ? AND execution_id = ?
    ///     }
    ///     
    ///     // Step 6: Update current_execution_id if this is a newer execution
    ///     UPDATE instances SET current_execution_id = GREATEST(current_execution_id, ?)
    ///     WHERE instance_id = ?;
    ///     
///     // Step 7: Enqueue worker/orchestrator items
///     // Worker items go to worker queue
///     for item in worker_items {
///         INSERT INTO worker_queue (work_item) VALUES (serialize(item))
///     }
///     
///     // Orchestrator items go to orchestrator queue (TimerFired uses fire_at_ms for visible_at)
///     for item in orchestrator_items {
///         let visible_at = match &item {
///             WorkItem::TimerFired { fire_at_ms, .. } => *fire_at_ms,
///             _ => now()
///         };
///         INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
///         VALUES (extract_instance(&item), serialize(item), visible_at)
///     }
///     
///     // Step 8: Release lock: DELETE FROM orch_queue WHERE lock_token = ?;
    ///     
    ///     commit_transaction()?;
    ///     Ok(())
    /// }
    /// ```
    async fn ack_orchestration_item(
        &self,
        _lock_token: &str,
        _execution_id: u64,
        _history_delta: Vec<Event>,
        _worker_items: Vec<WorkItem>,
        _orchestrator_items: Vec<WorkItem>,
        _metadata: ExecutionMetadata,
    ) -> Result<(), String>;

    /// Abandon orchestration processing (used for errors/retries).
    ///
    /// Called when orchestration processing fails (e.g., database busy, runtime crash).
    /// The messages must be made available for reprocessing.
    ///
    /// # What This Does
    ///
    /// 1. **Clear lock_token** from messages (make available again)
    /// 2. **Remove instance lock** from `instance_locks` table
    /// 3. **Optionally delay** retry by setting visibility timestamp
    /// 4. **Preserve message order** (don't reorder or modify messages)
    ///
    /// # Parameters
    ///
    /// * `lock_token` - Token from fetch_orchestration_item()
    /// * `delay_ms` - Optional delay before messages become visible again
    ///   - `None`: immediate retry (visible_at = now)
    ///   - `Some(ms)`: delayed retry (visible_at = now + ms)
    ///
    /// # Implementation Pattern
    ///
/// ```ignore
/// async fn abandon_orchestration_item(lock_token: &str, delay_ms: Option<u64>) -> Result<(), String> {
///     let visible_at = if let Some(delay) = delay_ms {
///         now() + delay
///     } else {
///         now()
///     };
///     
///     BEGIN TRANSACTION
///         // Get instance_id from instance_locks
///         let instance_id = SELECT instance_id FROM instance_locks WHERE lock_token = ?;
///         
///         IF instance_id IS NULL:
///             // Invalid lock token - return error
///             ROLLBACK
///             RETURN Err("Invalid lock token")
///         
///         // Clear lock_token from messages
///         UPDATE orchestrator_queue
///         SET lock_token = NULL, locked_until = NULL, visible_at = ?
///         WHERE lock_token = ?;
///         
///         // Remove instance lock
///         DELETE FROM instance_locks WHERE lock_token = ?;
///     COMMIT
///     
///     Ok(())
/// }
/// ```
    ///
    /// # Use Cases
    ///
    /// - Database contention (SQLITE_BUSY) → delay_ms = Some(50) for backoff
    /// - Orchestration turn failed → delay_ms = None for immediate retry
    /// - Runtime shutdown during processing → messages auto-recover when lock expires
    ///
/// # Error Handling
///
/// **Invalid lock tokens MUST return an error.** Unlike `ack_orchestration_item()`, this method
/// should not be idempotent for invalid tokens. Invalid tokens indicate a programming error or
/// state corruption and should be surfaced as errors.
///
/// Return `Err("Invalid lock token")` if the lock token is not found in `instance_locks`.
    async fn abandon_orchestration_item(&self, _lock_token: &str, _delay_ms: Option<u64>) -> Result<(), String>;

    // ===== Basic History Access (REQUIRED) =====

    /// Read the full history for an instance.
    ///
    /// # What This Does
    ///
    /// Returns ALL events for the LATEST execution of an instance, ordered by event_id.
    ///
    /// # Multi-Execution Behavior
    ///
    /// For instances with multiple executions (from ContinueAsNew):
    /// - Find the latest execution_id (MAX(execution_id))
    /// - Return events for ONLY that execution
    /// - DO NOT return events from earlier executions
    ///
    /// # Return Value
    ///
    /// - Empty Vec if instance doesn't exist
    /// - Empty Vec if instance exists but has no events (shouldn't happen normally)
    /// - Vec of events ordered by event_id ascending (event_id 1, 2, 3, ...)
    ///
    /// # Implementation Pattern
    ///
    /// ```ignore
    /// async fn read(&self, instance: &str) -> Vec<Event> {
    ///     // Get latest execution ID
    ///     let exec_id = SELECT COALESCE(MAX(execution_id), 1)
    ///         FROM executions WHERE instance_id = ?;
    ///     
    ///     // Load events for that execution
    ///     let rows = SELECT event_data FROM history
    ///         WHERE instance_id = ? AND execution_id = ?
    ///         ORDER BY event_id;
    ///     
    ///     rows.into_iter()
    ///         .filter_map(|row| serde_json::from_str(&row.event_data).ok())
    ///         .collect()
    /// }
    /// ```
    ///
    /// # Usage
    ///
    /// Called by:
    /// - Client.get_orchestration_status() - to determine current state
    /// - Tests and debugging tools
    /// - NOT called in hot path (fetch_orchestration_item loads history internally)
    async fn read(&self, instance: &str) -> Vec<Event>;

    // ===== Worker Queue Operations (REQUIRED) =====
    // Worker queue processes activity executions.

    /// Enqueue an activity execution request.
    ///
    /// # What This Does
    ///
    /// Add a WorkItem::ActivityExecute to the worker queue for background processing.
    ///
    /// # Implementation
    ///
    /// ```ignore
    /// async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
    ///     INSERT INTO worker_queue (work_item)
    ///     VALUES (serde_json::to_string(&item)?)
    /// }
    /// ```
    ///
    /// # Locking
    ///
    /// New messages should have lock_token = NULL (available for dequeue).
    ///
    /// # Ordering
    ///
    /// FIFO order preferred but not strictly required.
    async fn enqueue_worker_work(&self, _item: WorkItem) -> Result<(), String>;

    /// Dequeue a single work item with peek-lock semantics.
    ///
    /// # What This Does
    ///
    /// 1. Find next unlocked worker queue item
    /// 2. Lock it with a unique token
    /// 3. Return item + token (item stays in queue until ack)
    ///
    /// # Implementation Pattern
    ///
    /// ```ignore
    /// async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
    ///     let tx = begin_transaction()?;
    ///     
    ///     // Find next available item
    ///     let row = SELECT id, work_item FROM worker_queue
    ///         WHERE lock_token IS NULL OR locked_until <= now()
    ///         ORDER BY id LIMIT 1;
    ///     
    ///     if row.is_none() { return None; }
    ///     
    ///     // Lock it
    ///     let lock_token = generate_uuid();
    ///     UPDATE worker_queue
    ///     SET lock_token = ?, locked_until = now() + 30s
    ///     WHERE id = ?;
    ///     
    ///     let item = serde_json::from_str(&row.work_item)?;
    ///     commit_transaction();
    ///     Some((item, lock_token))
    /// }
    /// ```
    ///
    /// # Return Value
    ///
    /// - `Some((WorkItem, String))` - Item is locked and ready to process
    /// - `None` - No work available
    ///
    /// # Concurrency
    ///
    /// Called continuously by work dispatcher. Must prevent double-dequeue.
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)>;

    /// Acknowledge successful processing of a work item.
    ///
    /// # What This Does
    ///
    /// Atomically acknowledge worker item and enqueue completion to orchestrator queue.
    ///
    /// # Purpose
    ///
    /// Ensures completion delivery and worker ack happen atomically.
    /// Prevents lost completions if enqueue succeeds but ack fails.
    /// Prevents duplicate work if ack succeeds but enqueue fails.
    ///
    /// # Parameters
    ///
    /// * `token` - Lock token from dequeue_worker_peek_lock
    /// * `completion` - WorkItem::ActivityCompleted or WorkItem::ActivityFailed
    ///
    /// # Implementation
    ///
    /// ```ignore
    /// async fn ack_worker(&self, token: &str, completion: WorkItem) -> Result<(), String> {
    ///     BEGIN TRANSACTION
    ///         DELETE FROM worker_queue WHERE lock_token = ?token
    ///         INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
    ///         VALUES (completion.instance, serialize(completion), now)
    ///     COMMIT
    /// }
    /// ```
    async fn ack_worker(&self, token: &str, completion: WorkItem) -> Result<(), String>;

    // ===== Multi-Execution Support (REQUIRED for ContinueAsNew) =====
    // These methods enable orchestrations to continue with new input while maintaining history.

    /// Get the latest execution ID for an instance.
    ///
    /// # What This Does
    ///
    /// Returns the current (highest) execution_id for an instance.
    ///
    /// # Implementation
    ///
    /// ```ignore
    /// async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
    ///     SELECT MAX(execution_id) FROM executions WHERE instance_id = ?
    ///     // Returns None if no executions, Some(max_id) otherwise
    /// }
    /// ```
    ///
    /// # Default Implementation
    ///
    /// The default implementation uses `read()` and returns Some(1) if history exists.
    /// Override for better performance if you track execution IDs separately.
    ///
    /// # Return Value
    ///
    /// - `None` if instance doesn't exist or no execution ID can be determined
    /// - `Some(n)` for instances with executions (derived from history)
    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        let h = self.read(instance).await;
        if h.is_empty() {
            None
        } else {
            // Count OrchestrationStarted events to determine latest execution_id
            let count = h
                .iter()
                .filter(|e| matches!(e, crate::Event::OrchestrationStarted { .. }))
                .count() as u64;
            if count > 0 { Some(count) } else { None }
        }
    }

    /// Read history for a specific execution.
    ///
    /// # What This Does
    ///
    /// Returns events for a specific execution_id, not just the latest.
    ///
    /// # Use Cases
    ///
    /// - Debugging: inspect execution 1 after ContinueAsNew created execution 2
    /// - Testing: verify execution history isolation
    /// - NOT used in normal runtime operation
    ///
    /// # Implementation
    ///
    /// ```ignore
    /// async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
    ///     SELECT event_data FROM history
    ///     WHERE instance_id = ? AND execution_id = ?
    ///     ORDER BY event_id
    /// }
    /// ```
    ///
    /// # Default Implementation
    ///
    /// Falls back to `read()` (ignores execution_id parameter).
    /// Override if you need execution-specific history access.
    async fn read_with_execution(&self, instance: &str, _execution_id: u64) -> Vec<Event> {
        self.read(instance).await
    }

    /// Append events to a specific execution.
    ///
    /// # What This Does
    ///
    /// Add new events to the history log for a specific execution.
    ///
    /// # CRITICAL: Event ID Assignment
    ///
    /// **The runtime assigns event_ids BEFORE calling this method.**
    /// - DO NOT modify event.event_id()
    /// - DO NOT renumber events
    /// - Store events exactly as provided
    ///
    /// # Duplicate Detection
    ///
    /// Events with duplicate (instance_id, execution_id, event_id) should:
    /// - Either: Reject with error (let runtime handle)
    /// - Or: IGNORE (idempotent append)
    /// - NEVER: Overwrite existing event (corrupts history)
    ///
    /// # Implementation Pattern
    ///
    /// ```ignore
    /// async fn append_with_execution(instance: &str, execution_id: u64, new_events: Vec<Event>) -> Result<(), String> {
    ///     for event in &new_events {
    ///         // Validate event_id was set
    ///         if event.event_id() == 0 {
    ///             return Err("event_id must be set by runtime");
    ///         }
    ///         
    ///         let event_json = serde_json::to_string(&event)?;
    ///         let event_type = discriminant_name(&event); // For indexing
    ///         
    ///         INSERT INTO history (instance_id, execution_id, event_id, event_data, event_type)
    ///         VALUES (?, ?, event.event_id(), event_json, event_type)
    ///         // Use PRIMARY KEY (instance_id, execution_id, event_id) to prevent duplicates
    ///     }
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Ordering
    ///
    /// Events must be retrievable in event_id order (use ORDER BY event_id in reads).
    ///
    /// # Concurrency
    ///
    /// Usually called within a transaction (from ack_orchestration_item).
    /// If called standalone, must be thread-safe.
    async fn append_with_execution(
        &self,
        instance: &str,
        _execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String>;

    // ===== Optional Management APIs =====
    // These have default implementations and are primarily used for testing/debugging.

    /// Enqueue a work item to the orchestrator queue.
    ///
    /// # Purpose
    ///
    /// Used by runtime for:
    /// - External events: `Client.raise_event()` → enqueues WorkItem::ExternalRaised
    /// - Cancellation: `Client.cancel_instance()` → enqueues WorkItem::CancelInstance
    /// - Testing: Direct queue manipulation
    ///
    /// **Note:** In normal operation, orchestrator items are enqueued via `ack_orchestration_item`.
    ///
    /// # Parameters
    ///
    /// * `item` - WorkItem to enqueue (usually ExternalRaised or CancelInstance)
    /// * `delay_ms` - Optional visibility delay
    ///   - `None`: Immediate visibility (common case)
    ///   - `Some(ms)`: Delay visibility by ms milliseconds
    ///   - Providers without delayed_visibility support can ignore this (treat as None)
    ///
    /// # Implementation Pattern
    ///
    /// ```ignore
    /// async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
    ///     let instance = extract_instance(&item);  // See WorkItem docs for extraction
    ///     let work_json = serde_json::to_string(&item)?;
    ///     
    ///     let visible_at = if let Some(delay) = delay_ms {
    ///         now() + delay
    ///     } else {
    ///         now()
    ///     };
    ///     
    ///     // For StartOrchestration, may need to create instance first
    ///     if let WorkItem::StartOrchestration { orchestration, version, .. } = &item {
    ///         INSERT OR IGNORE INTO instances (instance_id, orchestration_name, orchestration_version)
    ///         VALUES (instance, orchestration, version.unwrap_or("1.0.0"));
    ///         
    ///         INSERT OR IGNORE INTO executions (instance_id, execution_id, status)
    ///         VALUES (instance, 1, 'Running');
    ///     }
    ///     
    ///     INSERT INTO orchestrator_queue (instance_id, work_item, visible_at, lock_token, locked_until)
    ///     VALUES (instance, work_json, visible_at, NULL, NULL);
    ///     
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Special Cases
    ///
    /// **StartOrchestration:**
    /// - Must create instance metadata if doesn't exist
    /// - Must create execution with ID=1
    ///
    /// **ExternalRaised:**
    /// - Can arrive before instance is started (race condition)
    /// - Should still enqueue - runtime will handle gracefully
    ///
    /// # Error Handling
    ///
    /// Return Err if storage fails. Return Ok if item was enqueued successfully.
    async fn enqueue_orchestrator_work(&self, _item: WorkItem, _delay_ms: Option<u64>) -> Result<(), String>;

    /// List all known instance IDs.
    ///
    /// # Purpose
    ///
    /// Used for:
    /// - Testing: Find all instances created during a test
    /// - Debugging: Inspect what instances exist
    /// - Admin tools: Build dashboards
    ///
    /// # Implementation
    ///
    /// ```ignore
    /// async fn list_instances(&self) -> Vec<String> {
    ///     SELECT instance_id FROM instances ORDER BY created_at
    /// }
    /// ```
    ///
    /// # Default Implementation
    ///
    /// Returns empty Vec. Override if you want to support instance listing.
    ///
    /// # Not Required
    ///
    /// This is purely for observability - runtime doesn't call this.
    async fn list_instances(&self) -> Vec<String> {
        Vec::new()
    }

    /// List all execution IDs for an instance.
    ///
    /// # Purpose
    ///
    /// Used for:
    /// - Testing: Verify ContinueAsNew created multiple executions
    /// - Debugging: See execution history for an instance
    /// - NOT used by runtime in normal operation
    ///
    /// # Implementation
    ///
    /// ```ignore
    /// async fn list_executions(&self, instance: &str) -> Vec<u64> {
    ///     SELECT execution_id FROM executions
    ///     WHERE instance_id = ?
    ///     ORDER BY execution_id
    /// }
    /// ```
    ///
    /// # Default Implementation
    ///
    /// Returns `[1]` if instance exists (from read()), empty Vec otherwise.
    /// Override for proper multi-execution support.
    ///
    /// # Return Value
    ///
    /// Vec of execution IDs in ascending order: `[1]`, `[1, 2]`, `[1, 2, 3]`, etc.
    async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let h = self.read(instance).await;
        if h.is_empty() { Vec::new() } else { vec![1] }
    }

    // - Add timeout parameter to dequeue_worker_peek_lock and dequeue_timer_peek_lock
    // - Add refresh_worker_lock(token, extend_ms) and refresh_timer_lock(token, extend_ms)
    // - Provider should auto-abandon messages if lock expires without ack
    // This would enable graceful handling of worker crashes and long-running activities

    // ===== Capability Discovery =====

    /// Check if this provider implements management capabilities.
    ///
    /// # Purpose
    ///
    /// This method enables automatic capability discovery by the `Client`.
    /// When a provider implements `ManagementCapability`, it should return
    /// `Some(self as &dyn ManagementCapability)` to expose management features.
    ///
    /// # Default Implementation
    ///
    /// Returns `None` (no management capabilities). Override to expose capabilities:
    ///
    /// ```ignore
    /// impl Provider for MyProvider {
    ///     fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
    ///         Some(self as &dyn ManagementCapability)
    ///     }
    /// }
    /// ```
    ///
    /// # Usage
    ///
    /// The `Client` automatically discovers capabilities:
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let instances = client.list_all_instances().await?;
    /// }
    /// ```
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        None
    }
}

/// Management and observability provider interface.
pub mod management;

/// SQLite-backed provider with full transactional support.
pub mod sqlite;

// Re-export management types for convenience
pub use management::{ExecutionInfo, InstanceInfo, ManagementProvider, QueueDepths, SystemMetrics};

/// Management capability trait for observability and administrative operations.
///
/// This trait provides rich management and observability features that extend
/// the core `Provider` functionality. Providers can implement this trait to
/// expose administrative capabilities to the `Client`.
///
/// # Automatic Discovery
///
/// The `Client` automatically discovers this capability through the
/// `Provider::as_management_capability()` method. When available, management
/// methods become accessible through the client.
///
/// # Implementation Guide for LLMs
///
/// When implementing a new provider, you can optionally implement this trait
/// to expose management features:
///
/// ```ignore
/// impl Provider for MyProvider {
///     // ... implement required Provider methods
///     
///     fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
///         Some(self as &dyn ManagementCapability)
///     }
/// }
///
/// impl ManagementCapability for MyProvider {
///     async fn list_instances(&self) -> Result<Vec<String>, String> {
///         // Query your storage for all instance IDs
///         Ok(vec!["instance-1".to_string(), "instance-2".to_string()])
///     }
///     
///     async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
///         // Query instance metadata from your storage
///         Ok(InstanceInfo {
///             instance_id: instance.to_string(),
///             orchestration_name: "ProcessOrder".to_string(),
///             orchestration_version: "1.0.0".to_string(),
///             current_execution_id: 1,
///             status: "Running".to_string(),
///             output: None,
///             created_at: 1234567890,
///             updated_at: 1234567890,
///         })
///     }
///     
///     // ... implement other management methods
/// }
/// ```
///
/// # Required Methods (8 total)
///
/// 1. **Instance Discovery (2 methods)**
///    - `list_instances()` - List all instance IDs
///    - `list_instances_by_status()` - Filter instances by status
///
/// 2. **Execution Inspection (3 methods)**
///    - `list_executions()` - List execution IDs for an instance
///    - `read_execution()` - Read history for a specific execution
///    - `latest_execution_id()` - Get the latest execution ID
///
/// 3. **Metadata Access (2 methods)**
///    - `get_instance_info()` - Get comprehensive instance information
///    - `get_execution_info()` - Get detailed execution information
///
/// 4. **System Metrics (2 methods)**
///    - `get_system_metrics()` - Get system-wide metrics
///    - `get_queue_depths()` - Get current queue depths
///
/// # Default Implementations
///
/// All methods have default implementations that return empty results or errors,
/// making this trait optional for providers that don't need management features.
///
/// # Usage
///
/// ```ignore
/// let client = Client::new(provider);
///
/// // Check if management features are available
/// if client.has_management_capability() {
///     let instances = client.list_all_instances().await?;
///     let metrics = client.get_system_metrics().await?;
///     println!("System has {} instances", metrics.total_instances);
/// } else {
///     println!("Management features not available");
/// }
/// ```
#[async_trait::async_trait]
pub trait ManagementCapability: Any + Send + Sync {
    // ===== Instance Discovery =====

    /// List all known instance IDs.
    ///
    /// # Returns
    ///
    /// Vector of instance IDs, typically sorted by creation time (newest first).
    ///
    /// # Use Cases
    ///
    /// - Admin dashboards showing all workflows
    /// - Bulk operations across instances
    /// - Testing (verify instance creation)
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn list_instances(&self) -> Result<Vec<String>, String> {
    ///     SELECT instance_id FROM instances ORDER BY created_at DESC
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns empty Vec if not supported.
    async fn list_instances(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    /// List instances matching a status filter.
    ///
    /// # Parameters
    ///
    /// * `status` - Filter by execution status: "Running", "Completed", "Failed", "ContinuedAsNew"
    ///
    /// # Returns
    ///
    /// Vector of instance IDs with the specified status.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn list_instances_by_status(&self, status: &str) -> Result<Vec<String>, String> {
    ///     SELECT i.instance_id FROM instances i
    ///     JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
    ///     WHERE e.status = ?
    ///     ORDER BY i.created_at DESC
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns empty Vec if not supported.
    async fn list_instances_by_status(&self, _status: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    // ===== Execution Inspection =====

    /// List all execution IDs for an instance.
    ///
    /// # Returns
    ///
    /// Vector of execution IDs in ascending order: `[1]`, `[1, 2]`, `[1, 2, 3]`, etc.
    ///
    /// # Multi-Execution Context
    ///
    /// When an orchestration uses ContinueAsNew, multiple executions exist:
    /// - Execution 1: Original execution
    /// - Execution 2: First ContinueAsNew
    /// - Execution 3: Second ContinueAsNew
    /// - etc.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String> {
    ///     SELECT execution_id FROM executions
    ///     WHERE instance_id = ?
    ///     ORDER BY execution_id
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns `[1]` if instance exists, empty Vec otherwise.
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String> {
        // Default assumes single execution if instance exists
        if let Ok(info) = self.get_instance_info(instance).await {
            Ok(vec![info.current_execution_id])
        } else {
            Ok(Vec::new())
        }
    }

    /// Read the full event history for a specific execution within an instance.
    ///
    /// # Parameters
    ///
    /// * `instance` - The ID of the orchestration instance.
    /// * `execution_id` - The specific execution ID to read history for.
    ///
    /// # Returns
    ///
    /// Vector of events in chronological order (oldest first).
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String> {
    ///     SELECT event_data FROM history
    ///     WHERE instance_id = ? AND execution_id = ?
    ///     ORDER BY event_id
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns empty vector.
    async fn read_execution(&self, _instance: &str, _execution_id: u64) -> Result<Vec<Event>, String> {
        Ok(Vec::new())
    }

    /// Get the latest (current) execution ID for an instance.
    ///
    /// # Parameters
    ///
    /// * `instance` - The ID of the orchestration instance.
    ///
    /// # Implementation Pattern
    ///
    /// ```ignore
    /// async fn latest_execution_id(&self, instance: &str) -> Result<u64, String> {
    ///     SELECT COALESCE(MAX(execution_id), 1) FROM executions WHERE instance_id = ?
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns 1 (assumes single execution).
    async fn latest_execution_id(&self, _instance: &str) -> Result<u64, String> {
        Ok(1)
    }

    // ===== Instance Metadata =====

    /// Get comprehensive information about an instance.
    ///
    /// # Parameters
    ///
    /// * `instance` - The ID of the orchestration instance.
    ///
    /// # Returns
    ///
    /// Detailed instance information including status, output, and metadata.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
    ///     SELECT i.*, e.status, e.output
    ///     FROM instances i
    ///     JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
    ///     WHERE i.instance_id = ?
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns an error indicating not implemented.
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        Err(format!("get_instance_info not implemented for instance {instance}"))
    }

    /// Get detailed information about a specific execution.
    ///
    /// # Parameters
    ///
    /// * `instance` - The ID of the orchestration instance.
    /// * `execution_id` - The specific execution ID.
    ///
    /// # Returns
    ///
    /// Detailed execution information including status, output, and event count.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, String> {
    ///     SELECT e.*, COUNT(h.event_id) as event_count
    ///     FROM executions e
    ///     LEFT JOIN history h ON e.instance_id = h.instance_id AND e.execution_id = h.execution_id
    ///     WHERE e.instance_id = ? AND e.execution_id = ?
    ///     GROUP BY e.execution_id
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns an error indicating not implemented.
    async fn get_execution_info(&self, _instance: &str, _execution_id: u64) -> Result<ExecutionInfo, String> {
        Err("get_execution_info not implemented".to_string())
    }

    // ===== System Metrics =====

    /// Get system-wide metrics for the orchestration engine.
    ///
    /// # Returns
    ///
    /// System metrics including instance counts, execution counts, and status breakdown.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_system_metrics(&self) -> Result<SystemMetrics, String> {
    ///     SELECT
    ///         COUNT(*) as total_instances,
    ///         SUM(CASE WHEN e.status = 'Running' THEN 1 ELSE 0 END) as running_instances,
    ///         SUM(CASE WHEN e.status = 'Completed' THEN 1 ELSE 0 END) as completed_instances,
    ///         SUM(CASE WHEN e.status = 'Failed' THEN 1 ELSE 0 END) as failed_instances
    ///     FROM instances i
    ///     JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns default `SystemMetrics`.
    async fn get_system_metrics(&self) -> Result<SystemMetrics, String> {
        Ok(SystemMetrics::default())
    }

    /// Get the current depths of the internal work queues.
    ///
    /// # Returns
    ///
    /// Queue depths for orchestrator and worker queues.
    ///
    /// **Note:** Timer queue depth is not applicable since timers are handled via
    /// delayed visibility in the orchestrator queue.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
    ///     SELECT
    ///         (SELECT COUNT(*) FROM orchestrator_queue WHERE lock_token IS NULL) as orchestrator_queue,
    ///         (SELECT COUNT(*) FROM worker_queue WHERE lock_token IS NULL) as worker_queue
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns default `QueueDepths` (timer_queue will be 0).
    async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
        Ok(QueueDepths::default())
    }
}
