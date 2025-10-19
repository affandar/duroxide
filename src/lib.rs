//! # Duroxide: Deterministic Task Orchestration in Rust
//!
//! Duroxide is a framework for building reliable, long-running workflows that can survive
//! failures and restarts. It's inspired by Microsoft's Durable Task Framework and provides
//! a replay-driven programming model for deterministic orchestration.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use duroxide::providers::sqlite::SqliteProvider;
//! use duroxide::runtime::registry::ActivityRegistry;
//! use duroxide::runtime::{self};
//! use duroxide::{OrchestrationContext, OrchestrationRegistry, Client};
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. Create a storage provider
//! let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await.unwrap());
//!
//! // 2. Register activities (your business logic)
//! let activities = ActivityRegistry::builder()
//!     .register("Greet", |name: String| async move {
//!         Ok(format!("Hello, {}!", name))
//!     })
//!     .build();
//!
//! // 3. Define your orchestration
//! let orchestration = |ctx: OrchestrationContext, name: String| async move {
//!     let greeting = ctx.schedule_activity("Greet", name)
//!         .into_activity().await?;
//!     Ok(greeting)
//! };
//!
//! // 4. Register and start the runtime
//! let orchestrations = OrchestrationRegistry::builder()
//!     .register("HelloWorld", orchestration)
//!     .build();
//!
//! let rt = runtime::Runtime::start_with_store(
//!     store.clone(), Arc::new(activities), orchestrations
//! ).await;
//!
//! // 5. Create a client and start an orchestration instance
//! let client = Client::new(store.clone());
//! client.start_orchestration("inst-1", "HelloWorld", "World").await?;
//! let result = client.wait_for_orchestration("inst-1", std::time::Duration::from_secs(5)).await
//!     .map_err(|e| format!("Wait error: {:?}", e))?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Key Concepts
//!
//! - **Orchestrations**: Long-running workflows written as async functions (coordination logic)
//! - **Activities**: Single-purpose work units (can do anything - DB, API, polling, etc.)
//! - **Timers**: Use `ctx.schedule_timer(ms)` for orchestration-level delays and timeouts
//! - **Deterministic Replay**: Orchestrations are replayed from history to ensure consistency
//! - **Durable Futures**: Composable futures for activities, timers, and external events
//! - **ContinueAsNew (Multi-Execution)**: An orchestration can end the current execution and
//!   immediately start a new one with fresh input. Each execution has its own isolated history
//!   that starts with `OrchestrationStarted { event_id: 1 }`.
//!
//! ## ⚠️ Important: Orchestrations vs Activities
//!
//! **Orchestrations = Coordination (control flow, business logic)**
//! **Activities = Execution (single-purpose work units)**
//!
//! ```rust,no_run
//! # use duroxide::OrchestrationContext;
//! # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
//! // ✅ CORRECT: Orchestration-level delay using timer
//! ctx.schedule_timer(5000).into_timer().await;  // Wait 5 seconds
//!
//! // ✅ ALSO CORRECT: Activity can poll/sleep as part of its work
//! // Example: Activity that provisions a VM and polls for readiness
//! // activities.register("ProvisionVM", |config| async move {
//! //     let vm = create_vm(config).await?;
//! //     while !vm_ready(&vm).await {
//! //         tokio::time::sleep(Duration::from_secs(5)).await;  // ✅ OK - part of provisioning
//! //     }
//! //     Ok(vm.id)
//! // });
//!
//! // ❌ WRONG: Activity that ONLY sleeps (use timer instead)
//! // ctx.schedule_activity("Sleep5Seconds", "").into_activity().await;
//! # Ok(())
//! # }
//! ```
//!
//! **Put in Activities (single-purpose execution units):**
//! - Database operations
//! - API calls (can include retries/polling)
//! - Data transformations
//! - File I/O
//! - VM provisioning (with internal polling)
//!
//! **Put in Orchestrations (coordination and business logic):**
//! - Control flow (if/else, match, loops)
//! - Business decisions
//! - Multi-step workflows
//! - Error handling and compensation
//! - Timeouts and deadlines (use timers)
//! - Waiting for external events
//!
//! ## ContinueAsNew (Multi-Execution) Semantics
//!
//! ContinueAsNew (CAN) allows an orchestration to end its current execution and start a new
//! one with fresh input (useful for loops, pagination, long-running workflows).
//!
//! - Orchestration calls `ctx.continue_as_new(new_input)`
//! - Runtime stamps `OrchestrationContinuedAsNew` in the CURRENT execution's history
//! - Runtime enqueues a `WorkItem::ContinueAsNew`
//! - When processing that work item, the runtime starts a NEW execution with:
//!   - `execution_id = previous_execution_id + 1`
//!   - `existing_history = []` (fresh history)
//!   - `OrchestrationStarted { event_id: 1, input = new_input }` is stamped automatically
//! - Each execution's history is independent; `duroxide::Client::read_execution_history(instance, id)`
//!   returns events for that execution only
//!
//! Provider responsibilities are strictly storage-level (see below). The runtime owns all
//! orchestration semantics, including execution boundaries and starting the new execution.
//!
//! ## Provider Responsibilities (At a Glance)
//!
//! Providers are pure storage abstractions. The runtime computes orchestration semantics
//! and passes explicit instructions to the provider.
//!
//! - `fetch_orchestration_item()`
//!   - Return a locked batch of work for ONE instance
//!   - Include full history for the CURRENT `execution_id`
//!   - Do NOT create/synthesize new executions here (even for ContinueAsNew)
//!
//! - `ack_orchestration_item(lock_token, execution_id, history_delta, ..., metadata)`
//!   - Atomic commit of one orchestration turn
//!   - Idempotently `INSERT OR IGNORE` execution row for the explicit `execution_id`
//!   - `UPDATE instances.current_execution_id = MAX(current_execution_id, execution_id)`
//!   - Append `history_delta` to the specified execution
//!   - Update `executions.status` and `executions.output` from `metadata` (no event inspection)
//!
//! - Worker/Timer queues
//!   - Peek-lock semantics (dequeue with lock token; ack by deleting)
//!   - Orchestrator, Worker, Timer queues are independent but committed atomically with history
//!
//! See `docs/provider-implementation-guide.md` and `src/providers/sqlite.rs` for a complete,
//! production-grade provider implementation.
//!
//! ## ⚠️ Critical: DurableFuture Conversion Pattern
//!
//! **All schedule methods return `DurableFuture` - you MUST convert before awaiting:**
//!
//! ```rust,no_run
//! # use duroxide::OrchestrationContext;
//! # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
//! // ✅ CORRECT patterns:
//! let result = ctx.schedule_activity("Task", "input").into_activity().await?;
//! ctx.schedule_timer(5000).into_timer().await;
//! let event = ctx.schedule_wait("Event").into_event().await;
//! let sub_result = ctx.schedule_sub_orchestration("Sub", "input").into_sub_orchestration().await?;
//!
//! // ❌ WRONG - These won't compile:
//! // let result = ctx.schedule_activity("Task", "input").await;  // Missing .into_activity()!
//! // ctx.schedule_timer(5000).await;                            // Missing .into_timer()!
//! // let event = ctx.schedule_wait("Event").await;              // Missing .into_event()!
//! # Ok(())
//! # }
//! ```
//!
//! **Why this pattern?** `DurableFuture` is a unified type that can represent any async operation.
//! The `.into_*()` methods convert it to the specific awaitable type you need.
//!
//! ## Common Patterns
//!
//! ### Function Chaining
//! ```rust,no_run
//! # use duroxide::OrchestrationContext;
//! async fn chain_example(ctx: OrchestrationContext) -> Result<String, String> {
//!     let step1 = ctx.schedule_activity("Step1", "input").into_activity().await?;
//!     let step2 = ctx.schedule_activity("Step2", &step1).into_activity().await?;
//!     Ok(step2)
//! }
//! ```
//!
//! ### Fan-Out/Fan-In
//! ```rust,no_run
//! # use duroxide::{OrchestrationContext, DurableOutput};
//! async fn fanout_example(ctx: OrchestrationContext) -> Vec<String> {
//!     let futures = vec![
//!         ctx.schedule_activity("Process", "item1"),
//!         ctx.schedule_activity("Process", "item2"),
//!         ctx.schedule_activity("Process", "item3"),
//!     ];
//!     let results = ctx.join(futures).await;
//!     results.into_iter().map(|r| match r {
//!         DurableOutput::Activity(Ok(s)) => s,
//!         _ => "error".to_string(),
//!     }).collect()
//! }
//! ```
//!
//! ### Human-in-the-Loop
//! ```rust,no_run
//! # use duroxide::{OrchestrationContext, DurableOutput};
//! async fn approval_example(ctx: OrchestrationContext) -> String {
//!     let timer = ctx.schedule_timer(30000); // 30 second timeout
//!     let approval = ctx.schedule_wait("ApprovalEvent");
//!     
//!     let (_, result) = ctx.select2(timer, approval).await;
//!     match result {
//!         DurableOutput::External(data) => data,
//!         DurableOutput::Timer => "timeout".to_string(),
//!         _ => "error".to_string(),
//!     }
//! }
//! ```
//!
//! ## Examples
//!
//! See the `examples/` directory for complete, runnable examples:
//! - `hello_world.rs` - Basic orchestration setup
//! - `fan_out_fan_in.rs` - Parallel processing pattern
//! - `timers_and_events.rs` - Human-in-the-loop workflows
//!
//! Run examples with: `cargo run --example <name>`
//!
//! ## Architecture
//!
//! This crate provides:
//! - **Public data model**: `Event`, `Action` for history and decisions
//! - **Orchestration driver**: `run_turn`, `run_turn_with`, and `Executor`
//! - **OrchestrationContext**: Schedule activities, timers, and external events
//! - **DurableFuture**: Unified futures that can be composed with `join`/`select`
//! - **Runtime**: In-process execution engine with dispatchers and workers
//! - **Providers**: Pluggable storage backends (filesystem, in-memory)
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// Public orchestration primitives and executor

pub mod client;
pub mod futures;
pub mod runtime;
// Re-export descriptor type for public API ergonomics
pub use runtime::OrchestrationDescriptor;
pub mod providers;

// Re-export key runtime types for convenience
pub use client::Client;
pub use runtime::{
    OrchestrationHandler, OrchestrationRegistry, OrchestrationRegistryBuilder, OrchestrationStatus, RuntimeOptions,
};

// Re-export management types for convenience
pub use providers::{ExecutionInfo, InstanceInfo, ManagementCapability, QueueDepths, SystemMetrics};

// System call operation constants
pub(crate) const SYSCALL_OP_GUID: &str = "guid";
pub(crate) const SYSCALL_OP_UTCNOW_MS: &str = "utcnow_ms";
pub(crate) const SYSCALL_OP_TRACE_PREFIX: &str = "trace:";

use crate::_typed_codec::Codec;
// LogLevel is now defined locally in this file
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

// Internal codec utilities for typed I/O (kept private; public API remains ergonomic)
mod _typed_codec {
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::Value;
    pub trait Codec {
        fn encode<T: Serialize>(v: &T) -> Result<String, String>;
        fn decode<T: DeserializeOwned>(s: &str) -> Result<T, String>;
    }
    pub struct Json;
    impl Codec for Json {
        fn encode<T: Serialize>(v: &T) -> Result<String, String> {
            // If the value is a JSON string, return raw content to preserve historic behavior
            match serde_json::to_value(v) {
                Ok(Value::String(s)) => Ok(s),
                Ok(val) => serde_json::to_string(&val).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        fn decode<T: DeserializeOwned>(s: &str) -> Result<T, String> {
            // Try parse as JSON first
            match serde_json::from_str::<T>(s) {
                Ok(v) => Ok(v),
                Err(_) => {
                    // Fallback: treat raw string as JSON string value
                    let val = Value::String(s.to_string());
                    serde_json::from_value(val).map_err(|e| e.to_string())
                }
            }
        }
    }
}

/// Initial execution ID for new orchestration instances.
/// All orchestrations start with execution_id = 1.
pub const INITIAL_EXECUTION_ID: u64 = 1;

/// Initial event ID for new executions.
/// The first event (OrchestrationStarted) always has event_id = 1.
pub const INITIAL_EVENT_ID: u64 = 1;

/// Append-only orchestration history entries persisted by a provider and
/// consumed during replay. All events have a monotonically increasing event_id
/// representing their position in history. Scheduling and completion events
/// are linked via source_event_id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    /// Orchestration instance was created and started by name with input.
    /// Version is required; parent linkage is present when this is a child orchestration.
    OrchestrationStarted {
        event_id: u64,
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },
    /// Orchestration completed with a final result.
    OrchestrationCompleted { event_id: u64, output: String },
    /// Orchestration failed with a final error.
    OrchestrationFailed { event_id: u64, error: String },
    /// Activity was scheduled. event_id is THE id for this operation.
    ActivityScheduled {
        event_id: u64,
        name: String,
        input: String,
        execution_id: u64,
    },
    /// Activity completed successfully with a result.
    ActivityCompleted {
        event_id: u64,
        source_event_id: u64,
        result: String,
    },
    /// Activity failed with an error string.
    ActivityFailed {
        event_id: u64,
        source_event_id: u64,
        error: String,
    },

    /// Timer was created and will logically fire at `fire_at_ms`.
    TimerCreated {
        event_id: u64,
        fire_at_ms: u64,
        execution_id: u64,
    },
    /// Timer fired at logical time `fire_at_ms`.
    TimerFired {
        event_id: u64,
        source_event_id: u64,
        fire_at_ms: u64,
    },

    /// Subscription to an external event by name was recorded.
    ExternalSubscribed { event_id: u64, name: String },
    /// An external event was raised. No source_event_id - matched by name.
    ExternalEvent { event_id: u64, name: String, data: String },

    /// Fire-and-forget orchestration scheduling (detached).
    OrchestrationChained {
        event_id: u64,
        name: String,
        instance: String,
        input: String,
    },

    /// Sub-orchestration was scheduled with deterministic child instance id.
    SubOrchestrationScheduled {
        event_id: u64,
        name: String,
        instance: String,
        input: String,
        execution_id: u64,
    },
    /// Sub-orchestration completed and returned a result to the parent.
    SubOrchestrationCompleted {
        event_id: u64,
        source_event_id: u64,
        result: String,
    },
    /// Sub-orchestration failed and returned an error to the parent.
    SubOrchestrationFailed {
        event_id: u64,
        source_event_id: u64,
        error: String,
    },

    /// Orchestration continued as new with fresh input (terminal for this execution).
    OrchestrationContinuedAsNew { event_id: u64, input: String },

    /// Cancellation has been requested for the orchestration (terminal will follow deterministically).
    OrchestrationCancelRequested { event_id: u64, reason: String },

    /// System call executed synchronously during orchestration turn (single event for schedule+completion).
    SystemCall {
        event_id: u64,
        op: String,
        value: String,
        execution_id: u64,
    },
}

// Event type name for SystemCall (used by providers for persistence)
pub(crate) const EVENT_TYPE_SYSTEM_CALL: &str = "SystemCall";

impl Event {
    /// Get the event_id (position in history) for any event.
    pub fn event_id(&self) -> u64 {
        match self {
            Event::OrchestrationStarted { event_id, .. } => *event_id,
            Event::OrchestrationCompleted { event_id, .. } => *event_id,
            Event::OrchestrationFailed { event_id, .. } => *event_id,
            Event::ActivityScheduled { event_id, .. } => *event_id,
            Event::ActivityCompleted { event_id, .. } => *event_id,
            Event::ActivityFailed { event_id, .. } => *event_id,
            Event::TimerCreated { event_id, .. } => *event_id,
            Event::TimerFired { event_id, .. } => *event_id,
            Event::ExternalSubscribed { event_id, .. } => *event_id,
            Event::ExternalEvent { event_id, .. } => *event_id,
            Event::OrchestrationChained { event_id, .. } => *event_id,
            Event::SubOrchestrationScheduled { event_id, .. } => *event_id,
            Event::SubOrchestrationCompleted { event_id, .. } => *event_id,
            Event::SubOrchestrationFailed { event_id, .. } => *event_id,
            Event::OrchestrationContinuedAsNew { event_id, .. } => *event_id,
            Event::OrchestrationCancelRequested { event_id, .. } => *event_id,
            Event::SystemCall { event_id, .. } => *event_id,
        }
    }

    /// Set the event_id (used by runtime when adding events to history).
    pub(crate) fn set_event_id(&mut self, id: u64) {
        match self {
            Event::OrchestrationStarted { event_id, .. } => *event_id = id,
            Event::OrchestrationCompleted { event_id, .. } => *event_id = id,
            Event::OrchestrationFailed { event_id, .. } => *event_id = id,
            Event::ActivityScheduled { event_id, .. } => *event_id = id,
            Event::ActivityCompleted { event_id, .. } => *event_id = id,
            Event::ActivityFailed { event_id, .. } => *event_id = id,
            Event::TimerCreated { event_id, .. } => *event_id = id,
            Event::TimerFired { event_id, .. } => *event_id = id,
            Event::ExternalSubscribed { event_id, .. } => *event_id = id,
            Event::ExternalEvent { event_id, .. } => *event_id = id,
            Event::OrchestrationChained { event_id, .. } => *event_id = id,
            Event::SubOrchestrationScheduled { event_id, .. } => *event_id = id,
            Event::SubOrchestrationCompleted { event_id, .. } => *event_id = id,
            Event::SubOrchestrationFailed { event_id, .. } => *event_id = id,
            Event::OrchestrationContinuedAsNew { event_id, .. } => *event_id = id,
            Event::OrchestrationCancelRequested { event_id, .. } => *event_id = id,
            Event::SystemCall { event_id, .. } => *event_id = id,
        }
    }

    /// Get the source_event_id if this is a completion event.
    /// Returns None for lifecycle and scheduling events.
    /// Note: ExternalEvent does not have source_event_id (matched by name).
    pub fn source_event_id(&self) -> Option<u64> {
        match self {
            Event::ActivityCompleted { source_event_id, .. } => Some(*source_event_id),
            Event::ActivityFailed { source_event_id, .. } => Some(*source_event_id),
            Event::TimerFired { source_event_id, .. } => Some(*source_event_id),
            Event::SubOrchestrationCompleted { source_event_id, .. } => Some(*source_event_id),
            Event::SubOrchestrationFailed { source_event_id, .. } => Some(*source_event_id),
            _ => None,
        }
    }
}

/// Log levels for orchestration context logging.
#[derive(Debug, Clone)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Declarative decisions produced by an orchestration turn. The host/provider
/// is responsible for materializing these into corresponding `Event`s.
#[derive(Debug, Clone)]
pub enum Action {
    /// Schedule an activity invocation. scheduling_event_id is the event_id of the ActivityScheduled event.
    CallActivity {
        scheduling_event_id: u64,
        name: String,
        input: String,
    },
    /// Create a timer that will fire after the requested delay. scheduling_event_id is the event_id of the TimerCreated event.
    CreateTimer { scheduling_event_id: u64, delay_ms: u64 },
    /// Subscribe to an external event by name. scheduling_event_id is the event_id of the ExternalSubscribed event.
    WaitExternal { scheduling_event_id: u64, name: String },
    /// Start a detached orchestration (no result routing back to parent).
    StartOrchestrationDetached {
        scheduling_event_id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
    },
    /// Start a sub-orchestration by name and child instance id. scheduling_event_id is the event_id of the SubOrchestrationScheduled event.
    StartSubOrchestration {
        scheduling_event_id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
    },

    /// Continue the current orchestration as a new execution with new input (terminal for current execution).
    /// Optional version string selects the target orchestration version for the new execution.
    ContinueAsNew { input: String, version: Option<String> },

    /// System call executed synchronously (no worker dispatch needed).
    SystemCall {
        scheduling_event_id: u64,
        op: String,
        value: String,
    },
}

#[derive(Debug)]
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,

    // Event ID generation
    next_event_id: u64,

    // Track claimed scheduling events (to prevent collision)
    claimed_scheduling_events: std::collections::HashSet<u64>,

    // Track consumed completions by event_id (FIFO enforcement)
    consumed_completions: std::collections::HashSet<u64>,

    // Track consumed external events (by name) since they're searched, not cursor-based
    consumed_external_events: std::collections::HashSet<String>,

    // Execution metadata
    execution_id: u64,

    // Turn metadata
    turn_index: u64,
    logging_enabled_this_poll: bool,
    // When set, indicates a nondeterminism condition detected by futures during polling
    nondeterminism_error: Option<String>,
}

impl CtxInner {
    fn new(history: Vec<Event>, execution_id: u64) -> Self {
        // Compute next event_id based on maximum event_id in history
        // (skip event_id=0 which are placeholders)
        let next_event_id = history
            .iter()
            .map(|e| e.event_id())
            .filter(|id| *id > 0)
            .max()
            .map(|max_id| max_id + 1)
            .unwrap_or(1);

        Self {
            history,
            actions: Vec::new(),
            next_event_id,
            claimed_scheduling_events: Default::default(),
            consumed_completions: Default::default(),
            consumed_external_events: Default::default(),
            execution_id,
            turn_index: 0,
            logging_enabled_this_poll: false,
            nondeterminism_error: None,
        }
    }

    fn record_action(&mut self, a: Action) {
        // Scheduling a new action means this poll is producing new decisions
        self.logging_enabled_this_poll = true;
        self.actions.push(a);
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    // Note: deterministic GUID generation was removed from public API.
}

/// User-facing orchestration context for scheduling and replay-safe helpers.
#[derive(Clone)]
pub struct OrchestrationContext {
    inner: Arc<Mutex<CtxInner>>,
}

impl OrchestrationContext {
    /// Construct a new context from an existing history vector.
    pub fn new(history: Vec<Event>, execution_id: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CtxInner::new(history, execution_id))),
        }
    }

    /// Returns the current logical time in milliseconds based on the last
    /// `TimerFired` event in history.

    fn take_actions(&self) -> Vec<Action> {
        std::mem::take(&mut self.inner.lock().unwrap().actions)
    }

    // Turn metadata
    /// The zero-based turn counter assigned by the host for diagnostics.
    pub fn turn_index(&self) -> u64 {
        self.inner.lock().unwrap().turn_index
    }
    pub(crate) fn set_turn_index(&self, idx: u64) {
        self.inner.lock().unwrap().turn_index = idx;
    }

    // Replay-safe logging control
    /// Indicates whether logging is enabled for the current poll. This is
    /// flipped on when a decision is recorded to minimize log noise.
    pub fn is_logging_enabled(&self) -> bool {
        self.inner.lock().unwrap().logging_enabled_this_poll
    }
    // log_buffer removed - not used

    /// Emit a structured trace entry.
    /// Creates a system call event for deterministic replay and logs to tracing.
    pub fn trace(&self, level: impl Into<String>, message: impl Into<String>) {
        let level_str = level.into();
        let msg = message.into();

        // Schedule and poll system call synchronously for deterministic replay
        // Format: "trace:{level}:{message}"
        // Note: Actual logging happens inside the System future during first execution only
        let op = format!("{}{}:{}", SYSCALL_OP_TRACE_PREFIX, level_str, msg);
        let mut fut = self.schedule_system_call(&op);
        // Poll immediately to record the event synchronously
        let _ = poll_once(&mut fut);
    }

    /// Convenience wrapper for INFO level tracing.
    pub fn trace_info(&self, message: impl Into<String>) {
        self.trace("INFO", message.into())
    }
    /// Convenience wrapper for WARN level tracing.
    pub fn trace_warn(&self, message: impl Into<String>) {
        self.trace("WARN", message.into())
    }
    /// Convenience wrapper for ERROR level tracing.
    pub fn trace_error(&self, message: impl Into<String>) {
        self.trace("ERROR", message.into())
    }
    /// Convenience wrapper for DEBUG level tracing.
    pub fn trace_debug(&self, message: impl Into<String>) {
        self.trace("DEBUG", message.into())
    }

    /// Schedule a system call operation (internal helper).
    pub(crate) fn schedule_system_call(&self, op: &str) -> DurableFuture {
        DurableFuture(Kind::System {
            op: op.to_string(),
            claimed_event_id: Cell::new(None),
            value: RefCell::new(None),
            ctx: self.clone(),
        })
    }

    /// Generate a new deterministic GUID.
    /// Returns a future that resolves to a String GUID.
    pub fn new_guid(&self) -> impl Future<Output = Result<String, String>> {
        self.schedule_system_call(SYSCALL_OP_GUID).into_activity()
    }

    /// Generate a new deterministic GUID as a DurableFuture.
    /// This variant returns a DurableFuture that can be used with join/select.
    pub fn new_guid_future(&self) -> DurableFuture {
        self.schedule_system_call(SYSCALL_OP_GUID)
    }

    /// Get the current UTC time in milliseconds since epoch.
    /// Returns a future that resolves to a u64 timestamp.
    pub fn utcnow_ms(&self) -> impl Future<Output = Result<u64, String>> {
        let fut = self.schedule_system_call(SYSCALL_OP_UTCNOW_MS).into_activity();
        async move {
            let s = fut.await?;
            s.parse::<u64>().map_err(|e| e.to_string())
        }
    }

    /// Get the current UTC time as a DurableFuture.
    /// This variant returns a DurableFuture that can be used with join/select.
    /// The result will be a String representation of milliseconds since epoch.
    pub fn utcnow_ms_future(&self) -> DurableFuture {
        self.schedule_system_call(SYSCALL_OP_UTCNOW_MS)
    }

    pub fn continue_as_new(&self, input: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap();
        let input: String = input.into();
        inner.record_action(Action::ContinueAsNew { input, version: None });
    }

    pub fn continue_as_new_typed<In: serde::Serialize>(&self, input: &In) {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        self.continue_as_new(payload);
    }

    /// ContinueAsNew to a specific target version (string is parsed as semver later).
    pub fn continue_as_new_versioned(&self, version: impl Into<String>, input: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.record_action(Action::ContinueAsNew {
            input: input.into(),
            version: Some(version.into()),
        });
    }
}

// Unified future/output that allows joining different orchestration primitives

/// Output of a `DurableFuture` when awaited via unified composition.
pub use crate::futures::{DurableFuture, DurableOutput, JoinFuture, SelectFuture};

// NOTE: Current replay model strictly consumes the next history event for each await.
// This breaks down in races (e.g., select(timer, external)) where the host may append
// multiple completions in one turn, and the "loser" event can end up ahead of the next
// awaited operation, causing a replay mismatch. We will refactor to correlate by stable
// IDs and buffer completions so futures resolve by correlation rather than head-of-queue
// order, matching Durable Task semantics where multiple results can be present out of
// arrival order without corrupting replay.

/// A unified future for activities, timers, and external events that carries a
/// correlation ID. Useful for composing with `futures::select`/`join`.
use crate::futures::Kind;

// Internal tag to classify DurableFuture kinds for history indexing
use crate::futures::AggregateDurableFuture;
// KindTag no longer needed - cursor model doesn't use it for matching

// DurableFuture's Future impl lives in crate::futures

impl DurableFuture {
    /// Converts this unified future into a future that resolves only for
    /// an activity completion or failure.
    /// Await an activity result as a raw String (back-compat API).
    pub fn into_activity(self) -> impl Future<Output = Result<String, String>> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = Result<String, String>;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::Activity(v)) => Poll::Ready(v),
                    Poll::Ready(other) => {
                        panic!("into_activity used on non-activity future: {other:?}")
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }

    /// Await an activity result decoded to a typed value.
    pub fn into_activity_typed<Out: serde::de::DeserializeOwned>(self) -> impl Future<Output = Result<Out, String>> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = Result<String, String>;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::Activity(v)) => Poll::Ready(v),
                    Poll::Ready(other) => {
                        panic!("into_activity used on non-activity future: {other:?}")
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        async move {
            let s = Map(self).await?;
            crate::_typed_codec::Json::decode::<Out>(&s)
        }
    }

    /// Converts this unified future into a future that resolves when the
    /// corresponding timer fires.
    pub fn into_timer(self) -> impl Future<Output = ()> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::Timer) => Poll::Ready(()),
                    Poll::Ready(other) => panic!("into_timer used on non-timer future: {other:?}"),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }

    /// Converts this unified future into a future that resolves with the
    /// payload of the correlated external event.
    /// Await an external event as a raw String (back-compat API).
    pub fn into_event(self) -> impl Future<Output = String> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = String;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::External(v)) => Poll::Ready(v),
                    Poll::Ready(other) => {
                        panic!("into_event used on non-external future: {other:?}")
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }

    /// Await an external event decoded to a typed value.
    pub fn into_event_typed<T: serde::de::DeserializeOwned>(self) -> impl Future<Output = T> {
        async move { crate::_typed_codec::Json::decode::<T>(&Self::into_event(self).await).expect("decode") }
    }

    /// Converts this unified future into a future that resolves only for
    /// a sub-orchestration completion or failure.
    /// Await a sub-orchestration result as a raw String (back-compat API).
    pub fn into_sub_orchestration(self) -> impl Future<Output = Result<String, String>> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = Result<String, String>;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::SubOrchestration(v)) => Poll::Ready(v),
                    Poll::Ready(other) => {
                        panic!("into_sub_orchestration used on non-sub-orch future: {other:?}")
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }

    /// Await a sub-orchestration result decoded to a typed value.
    pub fn into_sub_orchestration_typed<Out: serde::de::DeserializeOwned>(
        self,
    ) -> impl Future<Output = Result<Out, String>> {
        async move {
            match Self::into_sub_orchestration(self).await {
                Ok(s) => crate::_typed_codec::Json::decode::<Out>(&s),
                Err(e) => Err(e),
            }
        }
    }
}

impl OrchestrationContext {
    /// Schedule an activity and return a `DurableFuture` correlated to it.
    ///
    /// **Activities should be single-purpose execution units.**
    /// Pull multi-step logic and control flow into orchestrations.
    ///
    /// ⚠️ **IMPORTANT**: You MUST call `.into_activity().await`, not just `.await`!
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
    /// // ✅ CORRECT: Schedule and await activity
    /// let result = ctx.schedule_activity("ProcessData", "input").into_activity().await?;
    ///
    /// // ❌ WRONG: This won't compile!
    /// // let result = ctx.schedule_activity("ProcessData", "input").await;  // Missing .into_activity()!
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Good Activity Examples
    /// - Database queries
    /// - HTTP API calls (can include retries)
    /// - File operations
    /// - Data transformations
    /// - VM provisioning (can poll for readiness internally)
    /// - Any single-purpose work unit
    ///
    /// # What NOT to put in activities
    /// - Multi-step business logic (pull into orchestration)
    /// - Control flow decisions (if/match on business rules)
    /// - Pure delays with no work (use `schedule_timer()` instead)
    /// - Timeouts for orchestration coordination (use `select2` with timers)
    ///
    /// # Note on Sleep/Polling in Activities
    ///
    /// Activities **CAN** sleep or poll as part of their work:
    /// - ✅ Provisioning a resource and polling for readiness
    /// - ✅ Retrying an external API with backoff
    /// - ✅ Waiting for async operation to complete
    /// - ❌ Activity that ONLY sleeps (use orchestration timer instead)
    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
        // event_id will be claimed during first poll
        DurableFuture(Kind::Activity {
            name: name.into(),
            input: input.into(),
            claimed_event_id: Cell::new(None),
            ctx: self.clone(),
        })
    }

    /// Typed helper that serializes input and later decodes output via `into_activity_typed`.
    pub fn schedule_activity_typed<In: serde::Serialize, Out: serde::de::DeserializeOwned>(
        &self,
        name: impl Into<String>,
        input: &In,
    ) -> DurableFuture {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        self.schedule_activity(name, payload)
    }

    /// Schedule a timer for delays, timeouts, and scheduled execution.
    ///
    /// **Use this for any time-based waiting, NOT activities with sleep!**
    ///
    /// ⚠️ **IMPORTANT**: You MUST call `.into_timer().await`, not just `.await`!
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
    /// // ✅ CORRECT: Wait 5 seconds
    /// ctx.schedule_timer(5000).into_timer().await;
    ///
    /// // ❌ WRONG: This won't compile!
    /// // ctx.schedule_timer(5000).await;  // Missing .into_timer()!
    ///
    /// // Timeout pattern
    /// let work = ctx.schedule_activity("LongTask", "input");
    /// let timeout = ctx.schedule_timer(30000); // 30 second timeout
    /// let (winner, _) = ctx.select2(work, timeout).await;
    /// match winner {
    ///     0 => println!("Work completed"),
    ///     1 => println!("Timed out"),
    ///     _ => unreachable!(),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn schedule_timer(&self, delay_ms: u64) -> DurableFuture {
        // No ID allocation here - event_id is discovered during first poll
        DurableFuture(Kind::Timer {
            delay_ms,
            claimed_event_id: Cell::new(None),
            ctx: self.clone(),
        })
    }

    /// Subscribe to an external event by name and return its `DurableFuture`.
    pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture {
        // No ID allocation here - event_id is discovered during first poll
        DurableFuture(Kind::External {
            name: name.into(),
            claimed_event_id: Cell::new(None),
            result: RefCell::new(None),
            ctx: self.clone(),
        })
    }

    /// Typed external wait adapter pairs with `into_event_typed` for decoding.
    pub fn schedule_wait_typed<T: serde::de::DeserializeOwned>(&self, name: impl Into<String>) -> DurableFuture {
        self.schedule_wait(name)
    }

    /// Schedule a sub-orchestration by name with deterministic child instance id derived
    /// from parent context and event_id (determined during first poll).
    pub fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        let input: String = input.into();

        // Instance will be determined during polling based on event_id
        // Use a placeholder for now
        let child_instance = RefCell::new(String::from("sub::pending"));

        DurableFuture(Kind::SubOrch {
            name,
            version: None,
            instance: child_instance,
            input,
            claimed_event_id: Cell::new(None),
            ctx: self.clone(),
        })
    }

    pub fn schedule_sub_orchestration_typed<In: serde::Serialize, Out: serde::de::DeserializeOwned>(
        &self,
        name: impl Into<String>,
        input: &In,
    ) -> DurableFuture {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        self.schedule_sub_orchestration(name, payload)
    }

    /// Versioned sub-orchestration start (string I/O). If `version` is None, registry policy is used.
    pub fn schedule_sub_orchestration_versioned(
        &self,
        name: impl Into<String>,
        version: Option<String>,
        input: impl Into<String>,
    ) -> DurableFuture {
        let child_instance = RefCell::new(String::from("sub::pending"));

        DurableFuture(Kind::SubOrch {
            name: name.into(),
            version,
            instance: child_instance,
            input: input.into(),
            claimed_event_id: Cell::new(None),
            ctx: self.clone(),
        })
    }

    /// Versioned typed sub-orchestration.
    pub fn schedule_sub_orchestration_versioned_typed<In: serde::Serialize, Out: serde::de::DeserializeOwned>(
        &self,
        name: impl Into<String>,
        version: Option<String>,
        input: &In,
    ) -> DurableFuture {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        self.schedule_sub_orchestration_versioned(name, version, payload)
    }

    /// Schedule a detached orchestration with an explicit instance id.
    /// The runtime will prefix this with the parent instance to ensure global uniqueness.
    pub fn schedule_orchestration(
        &self,
        name: impl Into<String>,
        instance: impl Into<String>,
        input: impl Into<String>,
    ) {
        let name: String = name.into();
        let instance: String = instance.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();

        // Assign event_id for the chained orchestration event
        let event_id = inner.next_event_id;
        inner.next_event_id += 1;

        inner.history.push(Event::OrchestrationChained {
            event_id,
            name: name.clone(),
            instance: instance.clone(),
            input: input.clone(),
        });
        inner.record_action(Action::StartOrchestrationDetached {
            scheduling_event_id: event_id,
            name,
            version: None,
            instance,
            input,
        });
    }

    pub fn schedule_orchestration_typed<In: serde::Serialize>(
        &self,
        name: impl Into<String>,
        instance: impl Into<String>,
        input: &In,
    ) {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        self.schedule_orchestration(name, instance, payload)
    }

    /// Versioned detached orchestration start (string I/O). If `version` is None, registry policy is used for the child.
    pub fn schedule_orchestration_versioned(
        &self,
        name: impl Into<String>,
        version: Option<String>,
        instance: impl Into<String>,
        input: impl Into<String>,
    ) {
        let name: String = name.into();
        let instance: String = instance.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();

        let event_id = inner.next_event_id;
        inner.next_event_id += 1;

        inner.history.push(Event::OrchestrationChained {
            event_id,
            name: name.clone(),
            instance: instance.clone(),
            input: input.clone(),
        });

        inner.record_action(Action::StartOrchestrationDetached {
            scheduling_event_id: event_id,
            name,
            version,
            instance,
            input,
        });
    }

    pub fn schedule_orchestration_versioned_typed<In: serde::Serialize>(
        &self,
        name: impl Into<String>,
        version: Option<String>,
        instance: impl Into<String>,
        input: &In,
    ) {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        self.schedule_orchestration_versioned(name, version, instance, payload)
    }
}

// Aggregate future machinery lives in crate::futures

impl OrchestrationContext {
    /// Deterministic select over two futures: returns (winner_index, DurableOutput)
    pub fn select2(&self, a: DurableFuture, b: DurableFuture) -> SelectFuture {
        SelectFuture(AggregateDurableFuture::new_select(self.clone(), vec![a, b]))
    }
    /// Deterministic select over N futures
    pub fn select(&self, futures: Vec<DurableFuture>) -> SelectFuture {
        SelectFuture(AggregateDurableFuture::new_select(self.clone(), futures))
    }
    /// Deterministic join over N futures (history order)
    pub fn join(&self, futures: Vec<DurableFuture>) -> JoinFuture {
        JoinFuture(AggregateDurableFuture::new_join(self.clone(), futures))
    }
}

fn noop_waker() -> Waker {
    unsafe fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    unsafe fn wake(_: *const ()) {}
    unsafe fn wake_by_ref(_: *const ()) {}
    unsafe fn drop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

fn poll_once<F: Future>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    let mut pinned = unsafe { Pin::new_unchecked(fut) };
    pinned.as_mut().poll(&mut cx)
}

/// Poll the orchestrator once with the provided history, producing
/// updated history, requested `Action`s, and an optional output.
pub type TurnResult<O> = (Vec<Event>, Vec<Action>, Option<O>);

/// Execute one orchestration turn with explicit turn_index and execution_id.
/// This is the full-featured run_turn implementation used by the runtime.
pub fn run_turn_with<O, F>(
    history: Vec<Event>,
    turn_index: u64,
    execution_id: u64,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>)
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history, execution_id);
    ctx.set_turn_index(turn_index);
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = orchestrator(ctx.clone());
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, actions, Some(out))
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, actions, None)
        }
    }
}

/// Execute one orchestration turn and also return any nondeterminism flagged by futures.
/// This does not change the deterministic behavior of the orchestrator; it only surfaces
/// `CtxInner.nondeterminism_error` that futures may set during scheduling order checks.
pub fn run_turn_with_status<O, F>(
    history: Vec<Event>,
    turn_index: u64,
    execution_id: u64,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>, Option<String>)
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history, execution_id);
    ctx.set_turn_index(turn_index);
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = orchestrator(ctx.clone());
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let nondet = ctx.inner.lock().unwrap().nondeterminism_error.clone();
            (hist_after, actions, Some(out), nondet)
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let nondet = ctx.inner.lock().unwrap().nondeterminism_error.clone();
            (hist_after, actions, None, nondet)
        }
    }
}

/// Simple run_turn for tests. Uses default execution_id=1 and turn_index=0.
pub fn run_turn<O, F>(history: Vec<Event>, orchestrator: impl Fn(OrchestrationContext) -> F) -> TurnResult<O>
where
    F: Future<Output = O>,
{
    run_turn_with(history, 0, 1, orchestrator)
}
