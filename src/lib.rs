//! # Duroxide: Durable execution framework in Rust
//!
//! Duroxide is a framework for building reliable, long-running code based workflows that can survive
//! failures and restarts. For a deep dive into how durable execution works, see the
//! [Durable Futures Internals](https://github.com/affandar/duroxide/blob/main/docs/durable-futures-internals.md) documentation.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use duroxide::providers::sqlite::SqliteProvider;
//! use duroxide::runtime::registry::ActivityRegistry;
//! use duroxide::runtime::{self};
//! use duroxide::{ActivityContext, OrchestrationContext, OrchestrationRegistry, Client};
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. Create a storage provider
//! let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await.unwrap());
//!
//! // 2. Register activities (your business logic)
//! let activities = ActivityRegistry::builder()
//!     .register("Greet", |_ctx: ActivityContext, name: String| async move {
//!         Ok(format!("Hello, {}!", name))
//!     })
//!     .build();
//!
//! // 3. Define your orchestration
//! let orchestration = |ctx: OrchestrationContext, name: String| async move {
//!     let greeting = ctx.schedule_activity("Greet", name)
//!         .into_activity().await?;

// Mutex poisoning indicates a panic in another thread - a critical error.
// All expect()/unwrap() calls on mutex locks in this module are intentional:
// poisoned mutexes should panic as they indicate corrupted state.
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
// Arc::clone() vs .clone() is a style preference - we use .clone() for brevity
#![allow(clippy::clone_on_ref_ptr)]
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
//!   - Supports long-running activities via automatic lock renewal (minutes to hours)
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
//! # use std::time::Duration;
//! # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
//! // ✅ CORRECT: Orchestration-level delay using timer
//! ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;  // Wait 5 seconds
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
//!   - Automatic lock renewal for long-running activities (no configuration needed)
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
//! # use std::time::Duration;
//! # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
//! // ✅ CORRECT patterns:
//! let result = ctx.schedule_activity("Task", "input").into_activity().await?;
//! ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;
//! let event = ctx.schedule_wait("Event").into_event().await;
//! let sub_result = ctx.schedule_sub_orchestration("Sub", "input").into_sub_orchestration().await?;
//!
//! // ❌ WRONG - These won't compile:
//! // let result = ctx.schedule_activity("Task", "input").await;  // Missing .into_activity()!
//! // ctx.schedule_timer(Duration::from_secs(5)).await;                            // Missing .into_timer()!
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
//! # use std::time::Duration;
//! async fn approval_example(ctx: OrchestrationContext) -> String {
//!     let timer = ctx.schedule_timer(Duration::from_secs(30)); // 30 second timeout
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
//! ### Delays and Timeouts
//! ```rust,no_run
//! # use duroxide::{OrchestrationContext, DurableOutput};
//! # use std::time::Duration;
//! async fn delay_example(ctx: OrchestrationContext) -> Result<String, String> {
//!     // ✅ CORRECT: Use timer for orchestration-level delays
//!     ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;
//!     
//!     // Process after delay
//!     let result = ctx.schedule_activity("ProcessData", "input")
//!         .into_activity().await?;
//!     Ok(result)
//! }
//!
//! async fn timeout_example(ctx: OrchestrationContext) -> Result<String, String> {
//!     // Race work against timeout
//!     let work = ctx.schedule_activity("SlowOperation", "input");
//!     let timeout = ctx.schedule_timer(Duration::from_secs(5));
//!     
//!     let (winner_index, result) = ctx.select2(work, timeout).await;
//!     match winner_index {
//!         0 => match result {
//!             DurableOutput::Activity(Ok(value)) => Ok(value),
//!             DurableOutput::Activity(Err(e)) => Err(format!("Work failed: {e}")),
//!             _ => unreachable!(),
//!         },
//!         1 => Err("Operation timed out".to_string()),
//!         _ => unreachable!(),
//!     }
//! }
//! ```
//!
//! ### Fan-Out/Fan-In with Error Handling
//! ```rust,no_run
//! # use duroxide::{OrchestrationContext, DurableOutput};
//! async fn fanout_with_errors(ctx: OrchestrationContext, items: Vec<String>) -> Result<Vec<String>, String> {
//!     // Schedule all work in parallel
//!     let futures: Vec<_> = items.iter()
//!         .map(|item| ctx.schedule_activity("ProcessItem", item.clone()))
//!         .collect();
//!     
//!     // Wait for all to complete (deterministic order preserved)
//!     let results = ctx.join(futures).await;
//!     
//!     // Process results with error handling
//!     let mut successes = Vec::new();
//!     for result in results {
//!         match result {
//!             DurableOutput::Activity(Ok(value)) => successes.push(value),
//!             DurableOutput::Activity(Err(e)) => {
//!                 // Log error but continue processing other items
//!                 ctx.trace_error(format!("Item processing failed: {e}"));
//!             }
//!             _ => return Err("Unexpected result type".to_string()),
//!         }
//!     }
//!     
//!     Ok(successes)
//! }
//! ```
//!
//! ### Retry Pattern
//! ```rust,no_run
//! # use duroxide::{OrchestrationContext, RetryPolicy, BackoffStrategy};
//! # use std::time::Duration;
//! async fn retry_example(ctx: OrchestrationContext) -> Result<String, String> {
//!     // Retry with linear backoff: 5 attempts, delay increases linearly (1s, 2s, 3s, 4s)
//!     let result = ctx.schedule_activity_with_retry(
//!         "UnreliableOperation",
//!         "input",
//!         RetryPolicy::new(5)
//!             .with_backoff(BackoffStrategy::Linear {
//!                 base: Duration::from_secs(1),
//!                 max: Duration::from_secs(10),
//!             }),
//!     ).await?;
//!     
//!     Ok(result)
//! }
//! ```
//!
//! ## Examples
//!
//! See the `examples/` directory for complete, runnable examples:
//! - `hello_world.rs` - Basic orchestration setup
//! - `fan_out_fan_in.rs` - Parallel processing pattern with error handling
//! - `timers_and_events.rs` - Human-in-the-loop workflows with timeouts
//! - `delays_and_timeouts.rs` - Correct usage of timers for delays and timeouts
//! - `with_observability.rs` - Using observability features (tracing, metrics)
//! - `metrics_cli.rs` - Querying system metrics via CLI
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
//!
//! ### End-to-End System Architecture
//!
//! ```text
//! +-------------------------------------------------------------------------+
//! |                           Application Layer                             |
//! +-------------------------------------------------------------------------+
//! |                                                                         |
//! |  +--------------+         +------------------------------------+        |
//! |  |    Client    |-------->|  start_orchestration()             |        |
//! |  |              |         |  raise_event()                     |        |
//! |  |              |         |  wait_for_orchestration()          |        |
//! |  +--------------+         +------------------------------------+        |
//! |                                                                         |
//! +-------------------------------------------------------------------------+
//!                                    |
//!                                    v
//! +-------------------------------------------------------------------------+
//! |                            Runtime Layer                                |
//! +-------------------------------------------------------------------------+
//! |                                                                         |
//! |  +-------------------------------------------------------------------+  |
//! |  |                         Runtime                                   |  |
//! |  |  +----------------------+         +----------------------+        |  |
//! |  |  | Orchestration        |         | Work                 |        |  |
//! |  |  | Dispatcher           |         | Dispatcher           |        |  |
//! |  |  | (N concurrent)       |         | (N concurrent)       |        |  |
//! |  |  +----------+-----------+         +----------+-----------+        |  |
//! |  |             |                                |                    |  |
//! |  |             | Processes turns                | Executes activities|  |
//! |  |             |                                |                    |  |
//! |  +-------------+--------------------------------+--------------------+  |
//! |                |                                |                       |
//! |  +-------------v--------------------------------v--------------------+  |
//! |  |  OrchestrationRegistry: maps names -> orchestration handlers     |  |
//! |  +-------------------------------------------------------------------+  |
//! |                                                                         |
//! |  +-------------------------------------------------------------------+  |
//! |  |  ActivityRegistry: maps names -> activity handlers               |  |
//! |  +-------------------------------------------------------------------+  |
//! |                                                                         |
//! +-------------------------------------------------------------------------+
//!                |                                |
//!                | Fetches work items             | Fetches work items
//!                | (peek-lock)                    | (peek-lock)
//!                v                                v
//! +-------------------------------------------------------------------------+
//! |                          Provider Layer                                 |
//! +-------------------------------------------------------------------------+
//! |                                                                         |
//! |  +----------------------------+    +----------------------------+       |
//! |  |  Orchestrator Queue        |    |  Worker Queue              |       |
//! |  |  - StartOrchestration      |    |  - ActivityExecute         |       |
//! |  |  - ActivityCompleted       |    |                            |       |
//! |  |  - ActivityFailed          |    |                            |       |
//! |  |  - TimerFired (delayed)    |    |                            |       |
//! |  |  - ExternalRaised          |    |                            |       |
//! |  |  - ContinueAsNew           |    |                            |       |
//! |  +----------------------------+    +----------------------------+       |
//! |                                                                         |
//! |  +-------------------------------------------------------------------+  |
//! |  |                     Provider (Storage)                            |  |
//! |  |  - History (Events per instance/execution)                        |  |
//! |  |  - Instance metadata                                              |  |
//! |  |  - Execution metadata                                             |  |
//! |  |  - Instance locks (peek-lock semantics)                           |  |
//! |  |  - Queue management (enqueue/dequeue with visibility)             |  |
//! |  +-------------------------------------------------------------------+  |
//! |                                                                         |
//! |  +-------------------------------------------------------------------+  |
//! |  |                Storage Backend (SQLite, etc.)                     |  |
//! |  +-------------------------------------------------------------------+  |
//! |                                                                         |
//! +-------------------------------------------------------------------------+
//!
//! ### Execution Flow
//!
//! 1. **Client** starts orchestration → enqueues `StartOrchestration` to orchestrator queue
//! 2. **OrchestrationDispatcher** fetches work item (peek-lock), loads history from Provider
//! 3. **Runtime** calls user's orchestration function with `OrchestrationContext`
//! 4. **Orchestration** schedules activities/timers → Runtime appends `Event`s to history
//! 5. **Runtime** enqueues `ActivityExecute` to worker queue, `TimerFired` (delayed) to orchestrator queue
//! 6. **WorkDispatcher** fetches activity work item, executes via `ActivityRegistry`
//! 7. **Activity** completes → enqueues `ActivityCompleted`/`ActivityFailed` to orchestrator queue
//! 8. **OrchestrationDispatcher** processes completion → next orchestration turn
//! 9. **Runtime** atomically commits history + queue changes via `ack_orchestration_item()`
//!
//! All operations are deterministic and replayable from history.
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

#[cfg(feature = "provider-test")]
pub mod provider_validations;

#[cfg(feature = "provider-test")]
pub mod provider_validation;

#[cfg(feature = "provider-test")]
pub mod provider_stress_tests;

#[cfg(feature = "provider-test")]
pub mod provider_stress_test;

// Re-export key runtime types for convenience
pub use client::{Client, ClientError};
pub use runtime::{
    OrchestrationHandler, OrchestrationRegistry, OrchestrationRegistryBuilder, OrchestrationStatus, RuntimeOptions,
};

// Re-export management types for convenience
pub use providers::{
    ExecutionInfo, InstanceInfo, ProviderAdmin, QueueDepths, ScheduledActivityIdentifier, SystemMetrics,
};

// Re-export deletion/pruning types for Client API users
pub use providers::{DeleteInstanceResult, InstanceFilter, InstanceTree, PruneOptions, PruneResult};

// Type aliases for improved readability and maintainability
/// Shared reference to a Provider implementation
pub type ProviderRef = Arc<dyn providers::Provider>;

/// Shared reference to an OrchestrationHandler
pub type OrchestrationHandlerRef = Arc<dyn runtime::OrchestrationHandler>;

// System call operation constants
pub(crate) const SYSCALL_OP_GUID: &str = "guid";
pub(crate) const SYSCALL_OP_UTCNOW_MS: &str = "utcnow_ms";
pub(crate) const SYSCALL_OP_TRACE_PREFIX: &str = "trace:";

use crate::_typed_codec::Codec;
// LogLevel is now defined locally in this file
use serde::{Deserialize, Serialize};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

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

/// Structured error details for orchestration failures.
///
/// Errors are categorized into three types for proper metrics and logging:
/// - **Infrastructure**: Provider failures, data corruption (abort turn, never reach user code)
/// - **Configuration**: Deployment issues like unregistered activities, nondeterminism (abort turn)
/// - **Application**: Business logic failures (flow through normal orchestration code)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ErrorDetails {
    /// Infrastructure failure (provider errors, data corruption).
    /// These errors abort orchestration execution and never reach user code.
    Infrastructure {
        operation: String,
        message: String,
        retryable: bool,
    },

    /// Configuration error (unregistered orchestrations/activities, nondeterminism).
    /// These errors abort orchestration execution and never reach user code.
    Configuration {
        kind: ConfigErrorKind,
        resource: String,
        message: Option<String>,
    },

    /// Application error (business logic failures).
    /// These are the ONLY errors that orchestration code sees.
    Application {
        kind: AppErrorKind,
        message: String,
        retryable: bool,
    },

    /// Poison message error - message exceeded max fetch attempts.
    ///
    /// This indicates a message that repeatedly fails to process.
    /// Could be caused by:
    /// - Malformed message data causing deserialization failures
    /// - Message triggering bugs that crash the worker
    /// - Transient infrastructure issues that became permanent
    /// - Application code bugs triggered by specific input patterns
    Poison {
        /// Number of times the message was fetched
        attempt_count: u32,
        /// Maximum allowed attempts
        max_attempts: u32,
        /// Message type and identity
        message_type: PoisonMessageType,
        /// The poisoned message content (serialized JSON for debugging)
        message: String,
    },
}

/// Poison message type identification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PoisonMessageType {
    /// Orchestration work item batch
    Orchestration { instance: String, execution_id: u64 },
    /// Activity execution
    Activity {
        instance: String,
        execution_id: u64,
        activity_name: String,
        activity_id: u64,
    },
}

/// Configuration error kinds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConfigErrorKind {
    Nondeterminism,
}

/// Application error kinds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppErrorKind {
    ActivityFailed,
    OrchestrationFailed,
    Cancelled { reason: String },
}

impl ErrorDetails {
    /// Get failure category for metrics/logging.
    pub fn category(&self) -> &'static str {
        match self {
            ErrorDetails::Infrastructure { .. } => "infrastructure",
            ErrorDetails::Configuration { .. } => "configuration",
            ErrorDetails::Application { .. } => "application",
            ErrorDetails::Poison { .. } => "poison",
        }
    }

    /// Check if failure is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            ErrorDetails::Infrastructure { retryable, .. } => *retryable,
            ErrorDetails::Application { retryable, .. } => *retryable,
            ErrorDetails::Configuration { .. } => false,
            ErrorDetails::Poison { .. } => false, // Never retryable
        }
    }

    /// Get display message for logging/UI (backward compatible format).
    pub fn display_message(&self) -> String {
        match self {
            ErrorDetails::Infrastructure { operation, message, .. } => {
                format!("infrastructure:{operation}: {message}")
            }
            ErrorDetails::Configuration {
                kind,
                resource,
                message,
            } => match kind {
                ConfigErrorKind::Nondeterminism => message
                    .as_ref()
                    .map(|m| format!("nondeterministic: {m}"))
                    .unwrap_or_else(|| format!("nondeterministic in {resource}")),
            },
            ErrorDetails::Application { kind, message, .. } => match kind {
                AppErrorKind::Cancelled { reason } => format!("canceled: {reason}"),
                _ => message.clone(),
            },
            ErrorDetails::Poison {
                attempt_count,
                max_attempts,
                message_type,
                ..
            } => match message_type {
                PoisonMessageType::Orchestration { instance, .. } => {
                    format!("poison: orchestration {instance} exceeded {attempt_count} attempts (max {max_attempts})")
                }
                PoisonMessageType::Activity {
                    activity_name,
                    activity_id,
                    ..
                } => {
                    format!(
                        "poison: activity {activity_name}#{activity_id} exceeded {attempt_count} attempts (max {max_attempts})"
                    )
                }
            },
        }
    }
}

/// Unified event with common metadata and type-specific payload.
///
/// All events have common fields (event_id, source_event_id, instance_id, etc.)
/// plus type-specific data in the `kind` field.
///
/// Events are append-only history entries persisted by a provider and consumed during replay.
/// The `event_id` is a monotonically increasing position in history.
/// Scheduling and completion events are linked via `source_event_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    /// Sequential position in history (monotonically increasing per execution)
    pub event_id: u64,

    /// For completion events: references the scheduling event this completes.
    /// None for lifecycle events (OrchestrationStarted, etc.) and scheduling events.
    /// Some(id) for completion events (ActivityCompleted, TimerFired, etc.).
    pub source_event_id: Option<u64>,

    /// Instance this event belongs to.
    /// Denormalized from DB key for self-contained events.
    pub instance_id: String,

    /// Execution this event belongs to.
    /// Denormalized from DB key for self-contained events.
    pub execution_id: u64,

    /// Timestamp when event was created (milliseconds since Unix epoch).
    pub timestamp_ms: u64,

    /// Crate semver version that generated this event.
    /// Format: "0.1.0", "0.2.0", etc.
    pub duroxide_version: String,

    /// Event type and associated data.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Event-specific payloads.
///
/// Common fields have been extracted to the Event struct:
/// - event_id: moved to Event.event_id
/// - source_event_id: moved to Event.source_event_id (`Option<u64>`)
/// - execution_id: moved to Event.execution_id (was in 4 variants)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum EventKind {
    /// Orchestration instance was created and started by name with input.
    /// Version is required; parent linkage is present when this is a child orchestration.
    #[serde(rename = "OrchestrationStarted")]
    OrchestrationStarted {
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },

    /// Orchestration completed with a final result.
    #[serde(rename = "OrchestrationCompleted")]
    OrchestrationCompleted { output: String },

    /// Orchestration failed with a final error.
    #[serde(rename = "OrchestrationFailed")]
    OrchestrationFailed { details: ErrorDetails },

    /// Activity was scheduled.
    #[serde(rename = "ActivityScheduled")]
    ActivityScheduled { name: String, input: String },

    /// Activity completed successfully with a result.
    #[serde(rename = "ActivityCompleted")]
    ActivityCompleted { result: String },

    /// Activity failed with error details.
    #[serde(rename = "ActivityFailed")]
    ActivityFailed { details: ErrorDetails },

    /// Timer was created and will logically fire at `fire_at_ms`.
    #[serde(rename = "TimerCreated")]
    TimerCreated { fire_at_ms: u64 },

    /// Timer fired at logical time `fire_at_ms`.
    #[serde(rename = "TimerFired")]
    TimerFired { fire_at_ms: u64 },

    /// Subscription to an external event by name was recorded.
    #[serde(rename = "ExternalSubscribed")]
    ExternalSubscribed { name: String },

    /// An external event was raised. Matched by name (no source_event_id).
    #[serde(rename = "ExternalEvent")]
    ExternalEvent { name: String, data: String },

    /// Fire-and-forget orchestration scheduling (detached).
    #[serde(rename = "OrchestrationChained")]
    OrchestrationChained {
        name: String,
        instance: String,
        input: String,
    },

    /// Sub-orchestration was scheduled with deterministic child instance id.
    #[serde(rename = "SubOrchestrationScheduled")]
    SubOrchestrationScheduled {
        name: String,
        instance: String,
        input: String,
    },

    /// Sub-orchestration completed and returned a result to the parent.
    #[serde(rename = "SubOrchestrationCompleted")]
    SubOrchestrationCompleted { result: String },

    /// Sub-orchestration failed and returned error details to the parent.
    #[serde(rename = "SubOrchestrationFailed")]
    SubOrchestrationFailed { details: ErrorDetails },

    /// Orchestration continued as new with fresh input (terminal for this execution).
    #[serde(rename = "OrchestrationContinuedAsNew")]
    OrchestrationContinuedAsNew { input: String },

    /// Cancellation has been requested for the orchestration (terminal will follow deterministically).
    #[serde(rename = "OrchestrationCancelRequested")]
    OrchestrationCancelRequested { reason: String },

    /// System call executed synchronously during orchestration turn (single event for schedule+completion).
    #[serde(rename = "SystemCall")]
    SystemCall { op: String, value: String },
}

// Event type name for SystemCall (used by providers for persistence)
#[allow(dead_code)] // Used by sqlite provider when sqlite feature is enabled
pub(crate) const EVENT_TYPE_SYSTEM_CALL: &str = "SystemCall";

impl Event {
    /// Create a new event with common fields populated and a specific event_id.
    ///
    /// Use this when you know the event_id upfront (e.g., during replay or when
    /// creating events inline).
    pub fn with_event_id(
        event_id: u64,
        instance_id: impl Into<String>,
        execution_id: u64,
        source_event_id: Option<u64>,
        kind: EventKind,
    ) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        Event {
            event_id,
            source_event_id,
            instance_id: instance_id.into(),
            execution_id,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            duroxide_version: env!("CARGO_PKG_VERSION").to_string(),
            kind,
        }
    }

    /// Create a new event with common fields populated.
    ///
    /// The event_id will be 0 and should be set by the history manager.
    pub fn new(
        instance_id: impl Into<String>,
        execution_id: u64,
        source_event_id: Option<u64>,
        kind: EventKind,
    ) -> Self {
        Self::with_event_id(0, instance_id, execution_id, source_event_id, kind)
    }

    /// Get the event_id (position in history).
    #[inline]
    pub fn event_id(&self) -> u64 {
        self.event_id
    }

    /// Set the event_id (used by runtime when adding events to history).
    #[inline]
    pub(crate) fn set_event_id(&mut self, id: u64) {
        self.event_id = id;
    }

    /// Get the source_event_id if this is a completion event.
    /// Returns None for lifecycle and scheduling events.
    #[inline]
    pub fn source_event_id(&self) -> Option<u64> {
        self.source_event_id
    }

    /// Check if this event is a terminal event (ends the orchestration).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind,
            EventKind::OrchestrationCompleted { .. }
                | EventKind::OrchestrationFailed { .. }
                | EventKind::OrchestrationContinuedAsNew { .. }
        )
    }
}

/// Log levels for orchestration context logging.
#[derive(Debug, Clone)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Backoff strategy for computing delay between retry attempts.
#[derive(Debug, Clone)]
pub enum BackoffStrategy {
    /// No delay between retries.
    None,
    /// Fixed delay between all retries.
    Fixed {
        /// Delay duration between each retry.
        delay: std::time::Duration,
    },
    /// Linear backoff: delay = base * attempt, capped at max.
    Linear {
        /// Base delay multiplied by attempt number.
        base: std::time::Duration,
        /// Maximum delay cap.
        max: std::time::Duration,
    },
    /// Exponential backoff: delay = base * multiplier^(attempt-1), capped at max.
    Exponential {
        /// Initial delay for first retry.
        base: std::time::Duration,
        /// Multiplier applied each attempt.
        multiplier: f64,
        /// Maximum delay cap.
        max: std::time::Duration,
    },
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        BackoffStrategy::Exponential {
            base: std::time::Duration::from_millis(100),
            multiplier: 2.0,
            max: std::time::Duration::from_secs(30),
        }
    }
}

impl BackoffStrategy {
    /// Compute delay for given attempt (1-indexed).
    /// Attempt 1 is after first failure, so delay_for_attempt(1) is the first backoff.
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        match self {
            BackoffStrategy::None => std::time::Duration::ZERO,
            BackoffStrategy::Fixed { delay } => *delay,
            BackoffStrategy::Linear { base, max } => {
                let delay = base.saturating_mul(attempt);
                std::cmp::min(delay, *max)
            }
            BackoffStrategy::Exponential { base, multiplier, max } => {
                // delay = base * multiplier^(attempt-1)
                let factor = multiplier.powi(attempt.saturating_sub(1) as i32);
                let delay_nanos = (base.as_nanos() as f64 * factor) as u128;
                let delay = std::time::Duration::from_nanos(delay_nanos.min(u64::MAX as u128) as u64);
                std::cmp::min(delay, *max)
            }
        }
    }
}

/// Retry policy for activities.
///
/// Configures automatic retry behavior including maximum attempts, backoff strategy,
/// and optional total timeout spanning all attempts.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use duroxide::{RetryPolicy, BackoffStrategy};
///
/// // Simple retry with defaults (3 attempts, exponential backoff)
/// let policy = RetryPolicy::new(3);
///
/// // Custom policy with timeout and fixed backoff
/// let policy = RetryPolicy::new(5)
///     .with_timeout(Duration::from_secs(30))
///     .with_backoff(BackoffStrategy::Fixed {
///         delay: Duration::from_secs(1),
///     });
/// ```
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of attempts (including initial). Must be >= 1.
    pub max_attempts: u32,
    /// Backoff strategy between retries.
    pub backoff: BackoffStrategy,
    /// Per-attempt timeout. If set, each activity attempt is raced against this
    /// timeout. If timeout fires, returns error immediately (no retry).
    /// Retries only occur for activity errors, not timeouts. None = no timeout.
    pub timeout: Option<std::time::Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffStrategy::default(),
            timeout: None,
        }
    }
}

impl RetryPolicy {
    /// Create a new retry policy with specified max attempts and default backoff.
    ///
    /// # Panics
    /// Panics if `max_attempts` is 0.
    pub fn new(max_attempts: u32) -> Self {
        assert!(max_attempts >= 1, "max_attempts must be at least 1");
        Self {
            max_attempts,
            ..Default::default()
        }
    }

    /// Set per-attempt timeout.
    ///
    /// Each activity attempt is raced against this timeout. If the timeout fires
    /// before the activity completes, returns an error immediately (no retry).
    /// Retries only occur for activity errors, not timeouts.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Alias for `with_timeout` for backwards compatibility.
    #[doc(hidden)]
    pub fn with_total_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set backoff strategy.
    pub fn with_backoff(mut self, backoff: BackoffStrategy) -> Self {
        self.backoff = backoff;
        self
    }

    /// Compute delay for given attempt using the configured backoff strategy.
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        self.backoff.delay_for_attempt(attempt)
    }
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
    /// Create a timer that will fire at the specified absolute time.
    /// scheduling_event_id is the event_id of the TimerCreated event.
    /// fire_at_ms is the absolute timestamp (ms since epoch) when the timer should fire.
    CreateTimer { scheduling_event_id: u64, fire_at_ms: u64 },
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

/// Replay mode for orchestration context.
///
/// Controls how `schedule_*()` methods behave and how durable futures resolve:
/// - `Legacy`: Current behavior - mutates history, futures scan history for completions
/// - `Simplified`: New model - emits actions, futures check results map
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ReplayMode {
    /// Legacy mode: schedule_*() mutates history, futures scan history for completions.
    /// This is the current production behavior.
    #[default]
    Legacy,
    /// Simplified mode: schedule_*() emits actions to buffer, futures check results map.
    /// Used by the new commands-vs-history replay model.
    Simplified,
}

/// Result delivered to a durable future in simplified replay mode.
#[derive(Debug, Clone)]
pub(crate) enum SimplifiedResult {
    /// Activity completed successfully
    ActivityOk(String),
    /// Activity failed with error
    ActivityErr(String),
    /// Timer fired
    TimerFired,
    /// Sub-orchestration completed successfully
    SubOrchOk(String),
    /// Sub-orchestration failed with error
    SubOrchErr(String),
    /// External event received
    ExternalEvent(String),
    /// System call value
    SystemCallValue(String),
}

#[derive(Debug)]
struct CtxInner {
    // === Replay Mode ===
    /// Current replay mode (Legacy or Simplified)
    replay_mode: ReplayMode,
    
    /// Whether we're currently replaying history (true) or processing new events (false).
    /// In simplified mode: true while processing baseline_history events, false after.
    /// Users can check this via `ctx.is_replaying()` to skip side effects during replay.
    is_replaying: bool,

    // === Simplified Mode State ===
    /// Token counter for simplified mode (each schedule_*() call gets a unique token)
    simplified_next_token: u64,
    /// Emitted actions in simplified mode (token -> Action kind info)
    /// Token is used to correlate with schedule events during replay
    simplified_emitted: Vec<(u64, Action)>,
    /// Results map: token -> completion result (populated by replay engine)
    simplified_results: std::collections::HashMap<u64, SimplifiedResult>,
    /// Token -> schedule_id binding (set when replay engine matches action to history)
    simplified_bindings: std::collections::HashMap<u64, u64>,
    /// External subscriptions: schedule_id -> (name, subscription_index)
    simplified_external_subscriptions: std::collections::HashMap<u64, (String, usize)>,
    /// External arrivals: name -> list of payloads in arrival order
    simplified_external_arrivals: std::collections::HashMap<String, Vec<String>>,
    /// Next subscription index per external event name
    simplified_external_next_index: std::collections::HashMap<String, usize>,

    // === Legacy Mode State ===
    history: Vec<Event>,
    actions: Vec<Action>,

    // Event ID generation
    next_event_id: u64,

    // Track claimed scheduling events (to prevent collision)
    claimed_scheduling_events: std::collections::HashSet<u64>,

    // Track consumed completions by event_id (FIFO enforcement)
    consumed_completions: std::collections::HashSet<u64>,

    // Track cancelled source_event_ids (select2 losers) - their completions are auto-skipped in FIFO
    cancelled_source_ids: std::collections::HashSet<u64>,

    // Track cancelled activity scheduling ids (select/select2 losers).
    // These are ActivityScheduled event_ids for losers that should be cancelled via the provider.
    cancelled_activity_ids: std::collections::HashSet<u64>,

    // Track consumed external events (by name) since they're searched, not cursor-based
    consumed_external_events: std::collections::HashSet<String>,

    // Execution metadata
    execution_id: u64,
    instance_id: String,
    orchestration_name: String,
    orchestration_version: String,
    worker_id: Option<String>,
    logging_enabled_this_poll: bool,
    // When set, indicates a nondeterminism condition detected by futures during polling
    nondeterminism_error: Option<String>,
}

impl CtxInner {
    fn new(
        history: Vec<Event>,
        execution_id: u64,
        instance_id: String,
        orchestration_name: String,
        orchestration_version: String,
        worker_id: Option<String>,
    ) -> Self {
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
            // Replay mode - default to Legacy for backward compatibility
            replay_mode: ReplayMode::Legacy,
            
            // Start in replaying state - will be set to false when we move past baseline history
            is_replaying: true,

            // Simplified mode state (unused in Legacy mode)
            simplified_next_token: 0,
            simplified_emitted: Vec::new(),
            simplified_results: Default::default(),
            simplified_bindings: Default::default(),
            simplified_external_subscriptions: Default::default(),
            simplified_external_arrivals: Default::default(),
            simplified_external_next_index: Default::default(),

            // Legacy mode state
            history,
            actions: Vec::new(),
            next_event_id,
            claimed_scheduling_events: Default::default(),
            consumed_completions: Default::default(),
            cancelled_source_ids: Default::default(),
            cancelled_activity_ids: Default::default(),
            consumed_external_events: Default::default(),
            execution_id,
            instance_id,
            orchestration_name,
            orchestration_version,
            worker_id,
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

    // === Simplified Mode Helpers ===

    /// Emit an action in simplified mode and return a token for correlation.
    fn emit_action(&mut self, action: Action) -> u64 {
        debug_assert_eq!(
            self.replay_mode,
            ReplayMode::Simplified,
            "emit_action should only be called in Simplified mode"
        );
        self.simplified_next_token += 1;
        let token = self.simplified_next_token;
        self.simplified_emitted.push((token, action));
        token
    }

    /// Drain all emitted actions (called by replay engine after polling).
    fn drain_emitted_actions(&mut self) -> Vec<(u64, Action)> {
        std::mem::take(&mut self.simplified_emitted)
    }

    /// Bind a token to a schedule_id (called by replay engine when matching action to history).
    fn bind_token(&mut self, token: u64, schedule_id: u64) {
        self.simplified_bindings.insert(token, schedule_id);
    }

    /// Get the schedule_id bound to a token (returns None if not yet bound).
    fn get_bound_schedule_id(&self, token: u64) -> Option<u64> {
        self.simplified_bindings.get(&token).copied()
    }

    /// Deliver a completion result for a schedule_id.
    fn deliver_result(&mut self, schedule_id: u64, result: SimplifiedResult) {
        // Find the token that was bound to this schedule_id
        for (&token, &sid) in &self.simplified_bindings {
            if sid == schedule_id {
                self.simplified_results.insert(token, result);
                return;
            }
        }
        // affandar : cr : lets think through this a bit more deeply. when / why would no binding be found??

        // If no binding found, store by schedule_id directly (will be looked up by schedule_id)
        // This handles the case where results arrive before binding
    }

    /// Check if a result is available for a token.
    fn get_result(&self, token: u64) -> Option<&SimplifiedResult> {
        self.simplified_results.get(&token)
    }

    /// Bind an external subscription to a deterministic index.
    fn bind_external_subscription(&mut self, schedule_id: u64, name: &str) {
        let idx = self
            .simplified_external_next_index
            .entry(name.to_string())
            .or_insert(0);
        let subscription_index = *idx;
        *idx += 1;
        self.simplified_external_subscriptions
            .insert(schedule_id, (name.to_string(), subscription_index));
    }

    /// Deliver an external event (appends to arrival list for the name).
    fn deliver_external_event(&mut self, name: String, data: String) {
        self.simplified_external_arrivals
            .entry(name)
            .or_default()
            .push(data);
    }

    /// Get external event data for a subscription (by schedule_id).
    fn get_external_event(&self, schedule_id: u64) -> Option<&String> {
        let (name, subscription_index) = self.simplified_external_subscriptions.get(&schedule_id)?;
        let arrivals = self.simplified_external_arrivals.get(name)?;
        arrivals.get(*subscription_index)
    }

    // Note: deterministic GUID generation was removed from public API.
}

/// User-facing orchestration context for scheduling and replay-safe helpers.
/// Context provided to activities for logging and metadata access.
///
/// Unlike [`OrchestrationContext`], activities are leaf nodes that cannot schedule new work,
/// but they often need to emit structured logs and inspect orchestration metadata. The
/// `ActivityContext` exposes the parent orchestration information and trace helpers that log
/// with full correlation fields.
///
/// # Examples
///
/// ```rust,no_run
/// # use duroxide::ActivityContext;
/// # use duroxide::runtime::registry::ActivityRegistry;
/// let activities = ActivityRegistry::builder()
///     .register("ProvisionVM", |ctx: ActivityContext, config: String| async move {
///         ctx.trace_info(format!("Provisioning VM with config: {}", config));
///         
///         // Do actual work (can use sleep, HTTP, etc.)
///         let vm_id = provision_vm_internal(config).await?;
///         
///         ctx.trace_info(format!("VM provisioned: {}", vm_id));
///         Ok(vm_id)
///     })
///     .build();
/// # async fn provision_vm_internal(config: String) -> Result<String, String> { Ok("vm-123".to_string()) }
/// ```
///
/// # Metadata Access
///
/// Activity context provides access to orchestration correlation metadata:
/// - `instance_id()` - Orchestration instance identifier
/// - `execution_id()` - Execution number (for ContinueAsNew scenarios)
/// - `orchestration_name()` - Parent orchestration name
/// - `orchestration_version()` - Parent orchestration version
/// - `activity_name()` - Current activity name
///
/// # Cancellation Support
///
/// Activities can respond to cancellation when their parent orchestration reaches a terminal state:
/// - `is_cancelled()` - Check if cancellation has been requested
/// - `cancelled()` - Future that completes when cancellation is requested (for use with `tokio::select!`)
/// - `cancellation_token()` - Get a clone of the token for spawned tasks
///
/// # Determinism
///
/// Activity trace helpers (`trace_info`, `trace_warn`, etc.) do **not** participate in
/// deterministic replay. They emit logs directly using [`tracing`] and should only be used for
/// diagnostic purposes.
#[derive(Clone)]
pub struct ActivityContext {
    instance_id: String,
    execution_id: u64,
    orchestration_name: String,
    orchestration_version: String,
    activity_name: String,
    activity_id: u64,
    worker_id: String,
    /// Cancellation token for cooperative cancellation.
    /// Triggered when the parent orchestration reaches a terminal state.
    cancellation_token: tokio_util::sync::CancellationToken,
    /// Provider store for accessing the Client API.
    store: std::sync::Arc<dyn crate::providers::Provider>,
}

impl ActivityContext {
    /// Create a new activity context. This constructor is intended for internal runtime use.
    ///
    /// Creates context with a new (non-cancelled) cancellation token.
    /// Note: The runtime now uses `new_with_cancellation` to provide a shared token.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        instance_id: String,
        execution_id: u64,
        orchestration_name: String,
        orchestration_version: String,
        activity_name: String,
        activity_id: u64,
        worker_id: String,
        store: std::sync::Arc<dyn crate::providers::Provider>,
    ) -> Self {
        Self {
            instance_id,
            execution_id,
            orchestration_name,
            orchestration_version,
            activity_name,
            activity_id,
            worker_id,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            store,
        }
    }

    /// Create a new activity context with a specific cancellation token.
    ///
    /// This constructor is intended for internal runtime use when the worker
    /// dispatcher needs to provide a cancellation token that can be triggered
    /// during activity execution.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_cancellation(
        instance_id: String,
        execution_id: u64,
        orchestration_name: String,
        orchestration_version: String,
        activity_name: String,
        activity_id: u64,
        worker_id: String,
        cancellation_token: tokio_util::sync::CancellationToken,
        store: std::sync::Arc<dyn crate::providers::Provider>,
    ) -> Self {
        Self {
            instance_id,
            execution_id,
            orchestration_name,
            orchestration_version,
            activity_name,
            activity_id,
            worker_id,
            cancellation_token,
            store,
        }
    }

    /// Returns the orchestration instance identifier.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Returns the execution id within the orchestration instance.
    pub fn execution_id(&self) -> u64 {
        self.execution_id
    }

    /// Returns the parent orchestration name.
    pub fn orchestration_name(&self) -> &str {
        &self.orchestration_name
    }

    /// Returns the parent orchestration version.
    pub fn orchestration_version(&self) -> &str {
        &self.orchestration_version
    }

    /// Returns the activity name being executed.
    pub fn activity_name(&self) -> &str {
        &self.activity_name
    }

    /// Returns the worker dispatcher ID processing this activity.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Emit an INFO level trace entry associated with this activity.
    pub fn trace_info(&self, message: impl Into<String>) {
        tracing::info!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            worker_id = %self.worker_id,
            "{}",
            message.into()
        );
    }

    /// Emit a WARN level trace entry associated with this activity.
    pub fn trace_warn(&self, message: impl Into<String>) {
        tracing::warn!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            worker_id = %self.worker_id,
            "{}",
            message.into()
        );
    }

    /// Emit an ERROR level trace entry associated with this activity.
    pub fn trace_error(&self, message: impl Into<String>) {
        tracing::error!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            worker_id = %self.worker_id,
            "{}",
            message.into()
        );
    }

    /// Emit a DEBUG level trace entry associated with this activity.
    pub fn trace_debug(&self, message: impl Into<String>) {
        tracing::debug!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            worker_id = %self.worker_id,
            "{}",
            message.into()
        );
    }

    // ===== Cancellation Support =====

    /// Check if cancellation has been requested.
    ///
    /// Returns `true` if the parent orchestration has completed, failed,
    /// or been cancelled. Activities can use this for cooperative cancellation.
    ///
    /// # Example
    ///
    /// ```ignore
    /// for item in items {
    ///     if ctx.is_cancelled() {
    ///         return Err("Activity cancelled".into());
    ///     }
    ///     process(item).await;
    /// }
    /// ```
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Returns a future that completes when cancellation is requested.
    ///
    /// Use with `tokio::select!` for interruptible activities. This allows
    /// activities to respond promptly to cancellation without polling.
    ///
    /// # Example
    ///
    /// ```ignore
    /// tokio::select! {
    ///     result = do_work() => return result,
    ///     _ = ctx.cancelled() => return Err("Cancelled".into()),
    /// }
    /// ```
    pub async fn cancelled(&self) {
        self.cancellation_token.cancelled().await
    }

    /// Get a clone of the cancellation token for use in spawned tasks.
    ///
    /// If your activity spawns child tasks with `tokio::spawn()`, you should
    /// pass them this token so they can also respond to cancellation.
    ///
    /// **Important:** If you spawn additional tasks/threads and do not pass them
    /// the cancellation token, they may outlive the activity's cancellation/abort.
    /// This is user error - the runtime provides the signal but cannot guarantee
    /// termination of arbitrary spawned work.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let token = ctx.cancellation_token();
    /// let handle = tokio::spawn(async move {
    ///     loop {
    ///         tokio::select! {
    ///             _ = do_work() => {}
    ///             _ = token.cancelled() => break,
    ///         }
    ///     }
    /// });
    /// ```
    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation_token.clone()
    }

    /// Get a Client for management operations.
    ///
    /// This allows activities to perform management operations such as
    /// pruning old executions, deleting instances, or querying instance status.
    ///
    /// # Example: Self-Pruning Eternal Orchestration
    ///
    /// ```ignore
    /// // Activity that prunes old executions
    /// async fn prune_self(ctx: ActivityContext, _input: String) -> Result<String, String> {
    ///     let client = ctx.get_client();
    ///     let instance_id = ctx.instance_id();
    ///
    ///     let result = client.prune_executions(instance_id, PruneOptions {
    ///         keep_last: Some(1), // Keep only current execution
    ///         ..Default::default()
    ///     }).await.map_err(|e| e.to_string())?;
    ///
    ///     Ok(format!("Pruned {} executions", result.executions_deleted))
    /// }
    /// ```
    pub fn get_client(&self) -> crate::Client {
        crate::Client::new(self.store.clone())
    }
}

impl std::fmt::Debug for ActivityContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityContext")
            .field("instance_id", &self.instance_id)
            .field("execution_id", &self.execution_id)
            .field("orchestration_name", &self.orchestration_name)
            .field("orchestration_version", &self.orchestration_version)
            .field("activity_name", &self.activity_name)
            .field("activity_id", &self.activity_id)
            .field("worker_id", &self.worker_id)
            .field("cancellation_token", &self.cancellation_token)
            .field("store", &"<Provider>")
            .finish()
    }
}

#[derive(Clone)]
pub struct OrchestrationContext {
    inner: Arc<Mutex<CtxInner>>,
}

/// A future that never resolves, used by `continue_as_new()` to prevent further execution.
///
/// This future always returns `Poll::Pending`, ensuring that code after `await ctx.continue_as_new()`
/// is unreachable. The runtime extracts actions before checking the future's state, so the
/// `ContinueAsNew` action is properly recorded and processed.
struct ContinueAsNewFuture;

impl Future for ContinueAsNewFuture {
    type Output = Result<String, String>; // Matches orchestration return type, but never resolves

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Always pending - never resolves, making code after await unreachable
        // The runtime checks pending_actions before using the output, so this value is never used
        Poll::Pending
    }
}

impl OrchestrationContext {
    /// Construct a new context from an existing history vector.
    ///
    /// # Parameters
    ///
    /// * `orchestration_name` - The name of the orchestration being executed.
    /// * `orchestration_version` - The semantic version string of the orchestration.
    /// * `worker_id` - Optional dispatcher worker ID for logging correlation.
    ///   - `Some(id)`: Used by runtime dispatchers to include worker_id in traces
    ///   - `None`: Used by standalone/test execution without runtime context
    pub fn new(
        history: Vec<Event>,
        execution_id: u64,
        instance_id: String,
        orchestration_name: String,
        orchestration_version: String,
        worker_id: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CtxInner::new(
                history,
                execution_id,
                instance_id,
                orchestration_name,
                orchestration_version,
                worker_id,
            ))),
        }
    }

    /// Set the replay mode for this context.
    /// This is used by the replay engine to switch between legacy and simplified modes.
    pub(crate) fn set_replay_mode(&self, mode: ReplayMode) {
        self.inner.lock().unwrap().replay_mode = mode;
    }

    /// Get the current replay mode.
    #[allow(dead_code)]
    pub(crate) fn replay_mode(&self) -> ReplayMode {
        self.inner.lock().unwrap().replay_mode
    }

    /// Check if the orchestration is currently replaying history.
    ///
    /// Returns `true` when processing events from persisted history (replay),
    /// and `false` when executing new logic beyond the stored history.
    ///
    /// This is useful for skipping side effects during replay, such as:
    /// - Logging/tracing that should only happen on first execution
    /// - Metrics that shouldn't be double-counted
    /// - External notifications that shouldn't be re-sent
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// if !ctx.is_replaying() {
    ///     // Only log on first execution, not during replay
    ///     println!("Starting workflow for the first time");
    /// }
    /// # }
    /// ```
    pub fn is_replaying(&self) -> bool {
        self.inner.lock().unwrap().is_replaying
    }

    /// Set the replaying state (used by replay engine).
    pub(crate) fn set_is_replaying(&self, is_replaying: bool) {
        self.inner.lock().unwrap().is_replaying = is_replaying;
    }

    // =========================================================================
    // Simplified Mode Tracing (Replay-Guarded)
    // =========================================================================
    //
    // These trace methods are only available in simplified replay mode.
    // They use `is_replaying()` as a guard instead of system calls, which means:
    // - No history events are created for traces
    // - Traces only emit on first execution, not during replay
    // - Much simpler and more efficient than system-call-based tracing

    /// Emit a trace entry with the specified level (simplified mode only).
    ///
    /// This method guards the trace with `is_replaying()` - traces are only
    /// emitted on first execution, not during replay. No system call or
    /// history event is created.
    ///
    /// # Panics
    ///
    /// Panics if called in legacy replay mode. Use `trace()` for legacy mode.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// ctx.trace_simplified("INFO", "Processing order");
    /// # }
    /// ```
    pub fn trace_simplified(&self, level: impl Into<String>, message: impl Into<String>) {
        let inner = self.inner.lock().unwrap();
        assert_eq!(
            inner.replay_mode,
            ReplayMode::Simplified,
            "trace_simplified is only available in simplified replay mode"
        );
        
        // Only trace if not replaying
        if !inner.is_replaying {
            let level_str = level.into();
            let msg = message.into();
            
            match level_str.to_uppercase().as_str() {
                "INFO" => tracing::info!(
                    target: "duroxide::orchestration",
                    instance_id = %inner.instance_id,
                    execution_id = %inner.execution_id,
                    orchestration_name = %inner.orchestration_name,
                    orchestration_version = %inner.orchestration_version,
                    "{}",
                    msg
                ),
                "WARN" => tracing::warn!(
                    target: "duroxide::orchestration",
                    instance_id = %inner.instance_id,
                    execution_id = %inner.execution_id,
                    orchestration_name = %inner.orchestration_name,
                    orchestration_version = %inner.orchestration_version,
                    "{}",
                    msg
                ),
                "ERROR" => tracing::error!(
                    target: "duroxide::orchestration",
                    instance_id = %inner.instance_id,
                    execution_id = %inner.execution_id,
                    orchestration_name = %inner.orchestration_name,
                    orchestration_version = %inner.orchestration_version,
                    "{}",
                    msg
                ),
                "DEBUG" => tracing::debug!(
                    target: "duroxide::orchestration",
                    instance_id = %inner.instance_id,
                    execution_id = %inner.execution_id,
                    orchestration_name = %inner.orchestration_name,
                    orchestration_version = %inner.orchestration_version,
                    "{}",
                    msg
                ),
                _ => tracing::trace!(
                    target: "duroxide::orchestration",
                    instance_id = %inner.instance_id,
                    execution_id = %inner.execution_id,
                    orchestration_name = %inner.orchestration_name,
                    orchestration_version = %inner.orchestration_version,
                    level = %level_str,
                    "{}",
                    msg
                ),
            }
        }
    }

    /// Convenience wrapper for INFO level tracing (simplified mode only).
    ///
    /// See [`trace_simplified`](Self::trace_simplified) for details.
    pub fn trace_info_simplified(&self, message: impl Into<String>) {
        self.trace_simplified("INFO", message);
    }

    /// Convenience wrapper for WARN level tracing (simplified mode only).
    ///
    /// See [`trace_simplified`](Self::trace_simplified) for details.
    pub fn trace_warn_simplified(&self, message: impl Into<String>) {
        self.trace_simplified("WARN", message);
    }

    /// Convenience wrapper for ERROR level tracing (simplified mode only).
    ///
    /// See [`trace_simplified`](Self::trace_simplified) for details.
    pub fn trace_error_simplified(&self, message: impl Into<String>) {
        self.trace_simplified("ERROR", message);
    }

    /// Convenience wrapper for DEBUG level tracing (simplified mode only).
    ///
    /// See [`trace_simplified`](Self::trace_simplified) for details.
    pub fn trace_debug_simplified(&self, message: impl Into<String>) {
        self.trace_simplified("DEBUG", message);
    }

    /// Drain emitted actions (simplified mode only).
    /// Returns a list of (token, Action) pairs.
    pub(crate) fn drain_emitted_actions(&self) -> Vec<(u64, Action)> {
        self.inner.lock().unwrap().drain_emitted_actions()
    }

    /// Bind a token to a schedule_id (simplified mode only).
    pub(crate) fn bind_token(&self, token: u64, schedule_id: u64) {
        self.inner.lock().unwrap().bind_token(token, schedule_id);
    }

    /// Deliver a result for a token (simplified mode only).
    pub(crate) fn deliver_result(&self, schedule_id: u64, result: SimplifiedResult) {
        self.inner.lock().unwrap().deliver_result(schedule_id, result);
    }

    /// Returns the orchestration instance identifier.
    ///
    /// This is the unique identifier for this orchestration instance, typically
    /// provided when starting the orchestration.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// let id = ctx.instance_id();
    /// ctx.trace_info(format!("Processing instance: {}", id));
    /// # }
    /// ```
    pub fn instance_id(&self) -> String {
        self.inner.lock().unwrap().instance_id.clone()
    }

    /// Returns the current execution ID within this orchestration instance.
    ///
    /// The execution ID increments each time `continue_as_new()` is called.
    /// Execution 1 is the initial execution.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// let exec_id = ctx.execution_id();
    /// ctx.trace_info(format!("Execution #{}", exec_id));
    /// # }
    /// ```
    pub fn execution_id(&self) -> u64 {
        self.inner.lock().unwrap().execution_id
    }

    /// Returns the orchestration name.
    ///
    /// This is the name registered with the orchestration registry.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// let name = ctx.orchestration_name();
    /// ctx.trace_info(format!("Running orchestration: {}", name));
    /// # }
    /// ```
    pub fn orchestration_name(&self) -> String {
        self.inner.lock().unwrap().orchestration_name.clone()
    }

    /// Returns the orchestration version.
    ///
    /// This is the semantic version string associated with the orchestration.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// let version = ctx.orchestration_version();
    /// ctx.trace_info(format!("Version: {}", version));
    /// # }
    /// ```
    pub fn orchestration_version(&self) -> String {
        self.inner.lock().unwrap().orchestration_version.clone()
    }

    /// Returns the current logical time in milliseconds based on the last
    /// `TimerFired` event in history.
    fn take_actions(&self) -> Vec<Action> {
        std::mem::take(&mut self.inner.lock().unwrap().actions)
    }

    pub(crate) fn take_cancelled_activity_ids(&self) -> Vec<u64> {
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        inner.cancelled_activity_ids.drain().collect()
    }

    // Replay-safe logging control
    /// Indicates whether logging is enabled for the current poll. This is
    /// flipped on when a decision is recorded to minimize log noise.
    pub fn is_logging_enabled(&self) -> bool {
        self.inner.lock().unwrap().logging_enabled_this_poll
    }
    // log_buffer removed - not used

    /// Emit a structured trace entry with automatic context correlation.
    ///
    /// Creates a system call event for deterministic replay and logs to tracing.
    /// The log entry automatically includes correlation fields:
    /// - `instance_id` - The orchestration instance identifier
    /// - `execution_id` - The current execution number
    /// - `orchestration_name` - Name of the orchestration
    /// - `orchestration_version` - Semantic version
    ///
    /// # Determinism
    ///
    /// This method is replay-safe: logs are only emitted on first execution,
    /// not during replay. A `SystemCall` event is created in history to ensure
    /// deterministic replay behavior.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// ctx.trace("INFO", "Processing started");
    /// ctx.trace("WARN", format!("Retry attempt: {}", 3));
    /// ctx.trace("ERROR", "Payment validation failed");
    /// # }
    /// ```
    ///
    /// # Output
    ///
    /// ```text
    /// 2024-10-30T10:15:23.456Z INFO duroxide::orchestration [order-123] Processing started
    /// ```
    ///
    /// All logs include instance_id, execution_id, orchestration_name for correlation.
    pub fn trace(&self, level: impl Into<String>, message: impl Into<String>) {
        let level_str = level.into();
        let msg = message.into();

        // Schedule and poll system call synchronously for deterministic replay
        // Format: "trace:{level}:{message}"
        // Note: Actual logging happens inside the System future during first execution only
        let op = format!("{SYSCALL_OP_TRACE_PREFIX}{level_str}:{msg}");
        let mut fut = Box::pin(self.schedule_system_call(&op));
        // Poll immediately to record the event synchronously
        let _ = poll_once(fut.as_mut());
    }

    /// Convenience wrapper for INFO level tracing.
    ///
    /// Logs with INFO level and includes instance context automatically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// ctx.trace_info("Order validation successful");
    /// ctx.trace_info(format!("Processing {} items", 42));
    /// # }
    /// ```
    pub fn trace_info(&self, message: impl Into<String>) {
        self.trace("INFO", message.into())
    }

    /// Convenience wrapper for WARN level tracing.
    ///
    /// Logs with WARN level and includes instance context automatically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// ctx.trace_warn("Retrying failed operation");
    /// ctx.trace_warn(format!("Attempt {}/5", 3));
    /// # }
    /// ```
    pub fn trace_warn(&self, message: impl Into<String>) {
        self.trace("WARN", message.into())
    }
    /// Convenience wrapper for ERROR level tracing.
    ///
    /// Logs with ERROR level and includes instance context automatically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// ctx.trace_error("Payment processing failed");
    /// ctx.trace_error(format!("Critical error: {}", "timeout"));
    /// # }
    /// ```
    pub fn trace_error(&self, message: impl Into<String>) {
        self.trace("ERROR", message.into())
    }

    /// Convenience wrapper for DEBUG level tracing.
    ///
    /// Logs with DEBUG level and includes instance context automatically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) {
    /// ctx.trace_debug("Detailed state information");
    /// ctx.trace_debug(format!("Variable value: {:?}", 42));
    /// # }
    /// ```
    pub fn trace_debug(&self, message: impl Into<String>) {
        self.trace("DEBUG", message.into())
    }

    /// Schedule a system call operation (internal helper).
    pub(crate) fn schedule_system_call(&self, op: &str) -> DurableFuture {
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        
        // In simplified mode, emit action at schedule time (not poll time)
        let simplified_token = if inner.replay_mode == ReplayMode::Simplified {
            // For system calls, compute value immediately (they're synchronous)
            let computed_value = match op {
                SYSCALL_OP_GUID => generate_guid(),
                SYSCALL_OP_UTCNOW_MS => inner.now_ms().to_string(),
                _ => String::new(),
            };
            let token = inner.emit_action(Action::SystemCall {
                scheduling_event_id: 0, // Will be assigned by replay engine
                op: op.to_string(),
                value: computed_value.clone(),
            });
            // For system calls, deliver result immediately (they're synchronous)
            // The token is used as the key - we don't wait for event loop binding
            inner.simplified_results.insert(token, SimplifiedResult::SystemCallValue(computed_value));
            Some(token)
        } else {
            None
        };
        drop(inner);
        
        DurableFuture {
            claimed_event_id: Cell::new(None),
            simplified_token: Cell::new(simplified_token),
            ctx: self.clone(),
            kind: Kind::System {
                op: op.to_string(),
                value: RefCell::new(None),
            },
        }
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

    /// Get the current UTC time.
    /// Returns a future that resolves to a SystemTime.
    ///
    /// # Errors
    ///
    /// Returns an error if the system call fails or if the time value cannot be parsed.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # use std::time::{SystemTime, Duration};
    /// # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
    /// let now = ctx.utcnow().await?;
    /// let deadline = now + Duration::from_secs(3600); // 1 hour from now
    /// # Ok(())
    /// # }
    /// ```
    pub fn utcnow(&self) -> impl Future<Output = Result<SystemTime, String>> {
        let fut = self.schedule_system_call(SYSCALL_OP_UTCNOW_MS).into_activity();
        async move {
            let s = fut.await?;
            let ms = s.parse::<u64>().map_err(|e| e.to_string())?;
            Ok(UNIX_EPOCH + StdDuration::from_millis(ms))
        }
    }

    /// Get the current UTC time as a DurableFuture.
    /// This variant returns a DurableFuture that can be used with join/select.
    ///
    /// **Note:** When awaited, this returns a String representation of milliseconds.
    /// For direct use, prefer `utcnow()` which returns `SystemTime`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::{OrchestrationContext, DurableOutput};
    /// # use std::time::{SystemTime, Duration, UNIX_EPOCH};
    /// # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
    /// let time_future = ctx.utcnow_future();
    /// let activity_future = ctx.schedule_activity("Task", "input");
    ///
    /// let results = ctx.join(vec![time_future, activity_future]).await;
    /// for result in results {
    ///     match result {
    ///         DurableOutput::Activity(Ok(s)) => {
    ///             // Parse timestamp string to SystemTime
    ///             let ms: u64 = s.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    ///             let time = UNIX_EPOCH + Duration::from_millis(ms);
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn utcnow_future(&self) -> DurableFuture {
        self.schedule_system_call(SYSCALL_OP_UTCNOW_MS)
    }

    /// Continue the current execution as a new execution with fresh input.
    ///
    /// This terminates the current execution and starts a new execution with the provided input.
    /// Returns a future that never resolves, ensuring code after `await` is unreachable.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use duroxide::OrchestrationContext;
    /// # async fn example(ctx: OrchestrationContext) -> Result<String, String> {
    /// let n: u32 = 0;
    /// if n < 2 {
    ///     return ctx.continue_as_new("next_input").await; // Execution terminates here
    ///     // This code is unreachable - compiler will warn
    /// }
    /// Ok("completed".to_string())
    /// # }
    /// ```
    pub fn continue_as_new(&self, input: impl Into<String>) -> impl Future<Output = Result<String, String>> {
        let mut inner = self.inner.lock().unwrap();
        let input: String = input.into();
        let action = Action::ContinueAsNew {
            input,
            version: None,
        };
        // In simplified mode, emit action; in legacy mode, record action
        if inner.replay_mode == ReplayMode::Simplified {
            inner.emit_action(action);
        } else {
            inner.record_action(action);
        }
        ContinueAsNewFuture
    }

    pub fn continue_as_new_typed<In: serde::Serialize>(
        &self,
        input: &In,
    ) -> impl Future<Output = Result<String, String>> {
        // Serialization should never fail for valid input types - if it does, it's a programming error
        let payload =
            crate::_typed_codec::Json::encode(input).expect("Serialization should never fail for valid input");
        self.continue_as_new(payload)
    }

    /// ContinueAsNew to a specific target version (string is parsed as semver later).
    pub fn continue_as_new_versioned(
        &self,
        version: impl Into<String>,
        input: impl Into<String>,
    ) -> impl Future<Output = Result<String, String>> {
        let mut inner = self.inner.lock().unwrap();
        let action = Action::ContinueAsNew {
            input: input.into(),
            version: Some(version.into()),
        };
        // In simplified mode, emit action; in legacy mode, record action
        if inner.replay_mode == ReplayMode::Simplified {
            inner.emit_action(action);
        } else {
            inner.record_action(action);
        }
        ContinueAsNewFuture
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
use crate::futures::generate_guid;

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
    ///
    /// # Errors
    ///
    /// Returns an error if the activity fails or if the result cannot be deserialized to the target type.
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
    pub async fn into_event_typed<T: serde::de::DeserializeOwned>(self) -> T {
        // Deserialization should never fail if the type matches the stored data - if it does, it's a programming error
        crate::_typed_codec::Json::decode::<T>(&Self::into_event(self).await)
            .expect("Deserialization should never fail for matching types")
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
    ///
    /// # Errors
    ///
    /// Returns an error if the sub-orchestration fails or if the result cannot be deserialized to the target type.
    pub async fn into_sub_orchestration_typed<Out: serde::de::DeserializeOwned>(self) -> Result<Out, String> {
        match Self::into_sub_orchestration(self).await {
            Ok(s) => crate::_typed_codec::Json::decode::<Out>(&s),
            Err(e) => Err(e),
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
        let name: String = name.into();
        let input: String = input.into();
        
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        
        // In simplified mode, emit action at schedule time (not poll time)
        let simplified_token = if inner.replay_mode == ReplayMode::Simplified {
            Some(inner.emit_action(Action::CallActivity {
                scheduling_event_id: 0, // Will be assigned by replay engine
                name: name.clone(),
                input: input.clone(),
            }))
        } else {
            None
        };
        drop(inner);
        
        DurableFuture {
            claimed_event_id: Cell::new(None),
            simplified_token: Cell::new(simplified_token),
            ctx: self.clone(),
            kind: Kind::Activity { name, input },
        }
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

    /// Schedule activity with automatic retry on failure.
    ///
    /// **Retry behavior:**
    /// - Retries on activity **errors** up to `policy.max_attempts`
    /// - **Timeouts are NOT retried** - if any attempt times out, returns error immediately
    /// - Only application errors trigger retry logic
    ///
    /// **Timeout behavior (if `policy.total_timeout` is set):**
    /// - Each activity attempt is raced against the timeout
    /// - If the timeout fires before the activity completes → returns timeout error (no retry)
    /// - If the activity fails with an error before timeout → retry according to policy
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::{OrchestrationContext, RetryPolicy, BackoffStrategy};
    /// # use std::time::Duration;
    /// # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
    /// // Simple retry with defaults (no timeout)
    /// let result = ctx.schedule_activity_with_retry(
    ///     "CallAPI",
    ///     "request",
    ///     RetryPolicy::new(3),
    /// ).await?;
    ///
    /// // Retry with per-attempt timeout and custom backoff
    /// let result = ctx.schedule_activity_with_retry(
    ///     "CallAPI",
    ///     "request",
    ///     RetryPolicy::new(5)
    ///         .with_timeout(Duration::from_secs(30)) // 30s per attempt
    ///         .with_backoff(BackoffStrategy::Fixed { delay: Duration::from_secs(1) }),
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if all retry attempts fail or if a timeout occurs (timeouts are not retried).
    pub async fn schedule_activity_with_retry(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        policy: RetryPolicy,
    ) -> Result<String, String> {
        let name = name.into();
        let input = input.into();
        let mut last_error = String::new();

        for attempt in 1..=policy.max_attempts {
            // Each attempt: optionally race against per-attempt timeout
            let activity_result = if let Some(timeout) = policy.timeout {
                // Race activity vs per-attempt timeout
                let deadline = self.schedule_timer(timeout);
                let activity = self.schedule_activity(&name, &input);
                let (winner, output) = self.select2(activity, deadline).await;

                match winner {
                    0 => match output {
                        DurableOutput::Activity(result) => result,
                        _ => unreachable!(),
                    },
                    1 => {
                        // Timeout fired - exit immediately, no retry for timeouts
                        return Err("timeout: activity timed out".to_string());
                    }
                    _ => unreachable!(),
                }
            } else {
                // No timeout - just await the activity
                self.schedule_activity(&name, &input).into_activity().await
            };

            match activity_result {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Activity failed with error - apply retry policy
                    last_error = e.clone();
                    if attempt < policy.max_attempts {
                        self.trace(
                            "warn",
                            format!(
                                "Activity '{}' attempt {}/{} failed: {}. Retrying...",
                                name, attempt, policy.max_attempts, e
                            ),
                        );
                        let delay = policy.delay_for_attempt(attempt);
                        if !delay.is_zero() {
                            self.schedule_timer(delay).into_timer().await;
                        }
                    }
                }
            }
        }
        Err(last_error)
    }

    /// Typed variant of `schedule_activity_with_retry`.
    ///
    /// Serializes input once and deserializes the successful result.
    ///
    /// # Errors
    ///
    /// Returns an error if all retry attempts fail, if a timeout occurs, if input serialization fails, or if result deserialization fails.
    pub async fn schedule_activity_with_retry_typed<In: serde::Serialize, Out: serde::de::DeserializeOwned>(
        &self,
        name: impl Into<String>,
        input: &In,
        policy: RetryPolicy,
    ) -> Result<Out, String> {
        let payload = crate::_typed_codec::Json::encode(input).expect("encode");
        let result = self.schedule_activity_with_retry(name, payload, policy).await?;
        crate::_typed_codec::Json::decode::<Out>(&result)
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
    /// # use std::time::Duration;
    /// # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
    /// // ✅ CORRECT: Wait 5 seconds
    /// ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;
    ///
    /// // ❌ WRONG: This won't compile!
    /// // ctx.schedule_timer(Duration::from_secs(5)).await;  // Missing .into_timer()!
    ///
    /// // Timeout pattern
    /// let work = ctx.schedule_activity("LongTask", "input");
    /// let timeout = ctx.schedule_timer(Duration::from_secs(30)); // 30 second timeout
    /// let (winner, _) = ctx.select2(work, timeout).await;
    /// match winner {
    ///     0 => println!("Work completed"),
    ///     1 => println!("Timed out"),
    ///     _ => unreachable!(),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn schedule_timer(&self, delay: std::time::Duration) -> DurableFuture {
        let delay_ms = delay.as_millis() as u64;
        
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        
        // In simplified mode, emit action at schedule time (not poll time)
        let simplified_token = if inner.replay_mode == ReplayMode::Simplified {
            let now = inner.now_ms();
            let fire_at_ms = now.saturating_add(delay_ms);
            Some(inner.emit_action(Action::CreateTimer {
                scheduling_event_id: 0, // Will be assigned by replay engine
                fire_at_ms,
            }))
        } else {
            None
        };
        drop(inner);
        
        DurableFuture {
            claimed_event_id: Cell::new(None),
            simplified_token: Cell::new(simplified_token),
            ctx: self.clone(),
            kind: Kind::Timer { delay_ms },
        }
    }

    /// Subscribe to an external event by name and return its `DurableFuture`.
    pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        
        // In simplified mode, emit action at schedule time (not poll time)
        let simplified_token = if inner.replay_mode == ReplayMode::Simplified {
            Some(inner.emit_action(Action::WaitExternal {
                scheduling_event_id: 0, // Will be assigned by replay engine
                name: name.clone(),
            }))
        } else {
            None
        };
        drop(inner);
        
        DurableFuture {
            claimed_event_id: Cell::new(None),
            simplified_token: Cell::new(simplified_token),
            ctx: self.clone(),
            kind: Kind::External {
                name,
                result: RefCell::new(None),
            },
        }
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
        
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        
        // In simplified mode, emit action at schedule time (not poll time)
        let simplified_token = if inner.replay_mode == ReplayMode::Simplified {
            let placeholder_instance = format!("sub::pending_{}", inner.simplified_next_token + 1);
            Some(inner.emit_action(Action::StartSubOrchestration {
                scheduling_event_id: 0, // Will be assigned by replay engine
                name: name.clone(),
                version: None,
                instance: placeholder_instance,
                input: input.clone(),
            }))
        } else {
            None
        };
        drop(inner);

        DurableFuture {
            claimed_event_id: Cell::new(None),
            simplified_token: Cell::new(simplified_token),
            ctx: self.clone(),
            kind: Kind::SubOrch {
                name,
                version: None,
                instance: child_instance,
                input,
            },
        }
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
        let name: String = name.into();
        let input: String = input.into();
        let child_instance = RefCell::new(String::from("sub::pending"));
        
        let mut inner = self.inner.lock().expect("Mutex should not be poisoned");
        
        // In simplified mode, emit action at schedule time (not poll time)
        let simplified_token = if inner.replay_mode == ReplayMode::Simplified {
            let placeholder_instance = format!("sub::pending_{}", inner.simplified_next_token + 1);
            Some(inner.emit_action(Action::StartSubOrchestration {
                scheduling_event_id: 0, // Will be assigned by replay engine
                name: name.clone(),
                version: version.clone(),
                instance: placeholder_instance,
                input: input.clone(),
            }))
        } else {
            None
        };
        drop(inner);

        DurableFuture {
            claimed_event_id: Cell::new(None),
            simplified_token: Cell::new(simplified_token),
            ctx: self.clone(),
            kind: Kind::SubOrch {
                name,
                version,
                instance: child_instance,
                input,
            },
        }
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
        let exec_id = inner.execution_id;
        let inst_id = inner.instance_id.clone();

        inner.history.push(Event::with_event_id(
            event_id,
            inst_id,
            exec_id,
            None,
            EventKind::OrchestrationChained {
                name: name.clone(),
                instance: instance.clone(),
                input: input.clone(),
            },
        ));
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
        let exec_id = inner.execution_id;
        let inst_id = inner.instance_id.clone();

        inner.history.push(Event::with_event_id(
            event_id,
            inst_id,
            exec_id,
            None,
            EventKind::OrchestrationChained {
                name: name.clone(),
                instance: instance.clone(),
                input: input.clone(),
            },
        ));

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

fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    fut.as_mut().poll(&mut cx)
}

/// Poll the orchestrator once with the provided history, producing
/// updated history, requested `Action`s, and an optional output.
pub type TurnResult<O> = (Vec<Event>, Vec<Action>, Option<O>);

/// Execute one orchestration turn with explicit execution_id.
/// This is the full-featured run_turn implementation used by the runtime.
pub fn run_turn_with<O, F>(
    history: Vec<Event>,
    execution_id: u64,
    instance_id: String,
    orchestration_name: String,
    orchestration_version: String,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>)
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(
        history,
        execution_id,
        instance_id,
        orchestration_name,
        orchestration_version,
        None, // No worker_id in standalone execution
    );
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = Box::pin(orchestrator(ctx.clone()));
    match poll_once(fut.as_mut()) {
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
    execution_id: u64,
    instance_id: String,
    orchestration_name: String,
    orchestration_version: String,
    worker_id: String,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>, Option<String>)
where
    F: Future<Output = O>,
{
    let (history, actions, output, nondet, _cancelled_activity_ids) = run_turn_with_status_and_cancellations(
        history,
        execution_id,
        instance_id,
        orchestration_name,
        orchestration_version,
        worker_id,
        orchestrator,
    );

    (history, actions, output, nondet)
}

/// Execute one orchestration turn and also return:
/// - any nondeterminism flagged by futures, and
/// - any activity scheduling IDs that were cancelled during this turn (e.g., select/select2 losers).
///
/// This is used by the runtime to cancel "select losers" promptly via provider lock stealing,
/// while keeping orchestration code deterministic.
#[allow(clippy::type_complexity)]
pub fn run_turn_with_status_and_cancellations<O, F>(
    history: Vec<Event>,
    execution_id: u64,
    instance_id: String,
    orchestration_name: String,
    orchestration_version: String,
    worker_id: String,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>, Option<String>, Vec<u64>)
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(
        history,
        execution_id,
        instance_id,
        orchestration_name,
        orchestration_version,
        Some(worker_id),
    );
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = Box::pin(orchestrator(ctx.clone()));
    match poll_once(fut.as_mut()) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let actions = ctx.take_actions();
            let cancelled_activity_ids = ctx.take_cancelled_activity_ids();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let nondet = ctx.inner.lock().unwrap().nondeterminism_error.clone();
            (hist_after, actions, Some(out), nondet, cancelled_activity_ids)
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let cancelled_activity_ids = ctx.take_cancelled_activity_ids();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let nondet = ctx.inner.lock().unwrap().nondeterminism_error.clone();
            (hist_after, actions, None, nondet, cancelled_activity_ids)
        }
    }
}

/// Simple run_turn for tests. Uses default execution_id=1 and placeholder instance metadata.
pub fn run_turn<O, F>(history: Vec<Event>, orchestrator: impl Fn(OrchestrationContext) -> F) -> TurnResult<O>
where
    F: Future<Output = O>,
{
    run_turn_with(
        history,
        1,
        "test-instance".to_string(),
        "TestOrch".to_string(),
        "1.0.0".to_string(),
        orchestrator,
    )
}
