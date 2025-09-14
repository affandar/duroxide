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
//! - **Orchestrations**: Long-running workflows written as async functions
//! - **Activities**: Stateless functions that perform actual work (NO sleeping/delays!)
//! - **Timers**: Use `ctx.schedule_timer(ms)` for delays, timeouts, and scheduling
//! - **Deterministic Replay**: Orchestrations are replayed from history to ensure consistency
//! - **Durable Futures**: Composable futures for activities, timers, and external events
//!
//! ## ⚠️ Important: Timers vs Activities
//!
//! **For delays and timeouts, always use timers, NOT activities:**
//!
//! ```rust,no_run
//! # use duroxide::OrchestrationContext;
//! # async fn example(ctx: OrchestrationContext) -> Result<(), String> {
//! // ✅ CORRECT: Use timers for delays
//! ctx.schedule_timer(5000).into_timer().await; // Wait 5 seconds
//!
//! // ❌ WRONG: Don't put delays in activities
//! // ctx.schedule_activity("Sleep", "5000").into_activity().await; // DON'T DO THIS
//! # Ok(())
//! # }
//! ```
//!
//! **Activities should be pure business logic without delays:**
//! - Database operations
//! - API calls  
//! - Data transformations
//! - File I/O
//!
//! **Use timers for:**
//! - Delays and waiting periods
//! - Timeouts and deadlines
//! - Scheduled execution
//! - Rate limiting
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
use std::cell::Cell;
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
pub use runtime::{OrchestrationHandler, OrchestrationRegistry, OrchestrationRegistryBuilder, OrchestrationStatus};
// Internal system activity names
pub(crate) const SYSTEM_NOW_ACTIVITY: &str = "__system_now";
pub(crate) const SYSTEM_NEW_GUID_ACTIVITY: &str = "__system_new_guid";
pub(crate) const SYSTEM_TRACE_ACTIVITY: &str = "__system_trace";

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

/// Append-only orchestration history entries persisted by a provider and
/// consumed during replay. Variants use stable correlation IDs to pair
/// scheduling operations with their completions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Event {
    /// Orchestration instance was created and started by name with input.
    /// Version is required; parent linkage is present when this is a child orchestration.
    OrchestrationStarted {
        name: String,
        version: String,
        input: String,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },
    /// Orchestration completed with a final result.
    OrchestrationCompleted { output: String },
    /// Orchestration failed with a final error.
    OrchestrationFailed { error: String },
    /// Activity was scheduled with a unique ID and input.
    ActivityScheduled {
        id: u64,
        name: String,
        input: String,
        execution_id: u64,
    },
    /// Activity completed successfully with a result.
    ActivityCompleted { id: u64, result: String },
    /// Activity failed with an error string.
    ActivityFailed { id: u64, error: String },

    /// Timer was created and will logically fire at `fire_at_ms`.
    TimerCreated {
        id: u64,
        fire_at_ms: u64,
        execution_id: u64,
    },
    /// Timer fired at logical time `fire_at_ms`.
    TimerFired { id: u64, fire_at_ms: u64 },

    /// Subscription to an external event by name was recorded with a unique ID.
    ExternalSubscribed { id: u64, name: String },
    /// An external event with correlation `id` was raised with some data.
    ExternalEvent { id: u64, name: String, data: String },

    /// Fire-and-forget orchestration scheduling (detached).
    OrchestrationChained {
        id: u64,
        name: String,
        instance: String,
        input: String,
    },

    /// Sub-orchestration was scheduled with deterministic child instance id.
    SubOrchestrationScheduled {
        id: u64,
        name: String,
        instance: String,
        input: String,
        execution_id: u64,
    },
    /// Sub-orchestration completed and returned a result to the parent.
    SubOrchestrationCompleted { id: u64, result: String },
    /// Sub-orchestration failed and returned an error to the parent.
    SubOrchestrationFailed { id: u64, error: String },

    /// Orchestration continued as new with fresh input (terminal for this execution).
    OrchestrationContinuedAsNew { input: String },

    /// Cancellation has been requested for the orchestration (terminal will follow deterministically).
    OrchestrationCancelRequested { reason: String },
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
    /// Schedule an activity invocation.
    CallActivity { id: u64, name: String, input: String },
    /// Create a timer that will fire after the requested delay.
    CreateTimer { id: u64, delay_ms: u64 },
    /// Subscribe to an external event by name.
    WaitExternal { id: u64, name: String },
    /// Start a detached orchestration (no result routing back to parent).
    StartOrchestrationDetached {
        id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
    },
    /// Start a sub-orchestration by name and child instance id. Optional version selects target orchestration version.
    StartSubOrchestration {
        id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
    },

    /// Continue the current orchestration as a new execution with new input (terminal for current execution).
    /// Optional version string selects the target orchestration version for the new execution.
    ContinueAsNew { input: String, version: Option<String> },
}

#[derive(Debug)]
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,

    // Reserved for future deterministic GUIDs if reintroduced
    next_correlation_id: u64,

    // Execution metadata
    execution_id: u64,

    // Logging and turn metadata
    turn_index: u64,
    logging_enabled_this_poll: bool,
    // Per-turn buffered logs (messages to flush once per progress turn)
    log_buffer: Vec<(LogLevel, String)>,

    // Per-turn claimed correlation IDs to prevent reusing IDs across multiple
    // futures scheduled within the same poll turn. This preserves deterministic
    // replay semantics when multiple timers/external waits/activities are
    // scheduled sequentially in one turn.
    claimed_activity_ids: std::collections::HashSet<u64>,
    claimed_timer_ids: std::collections::HashSet<u64>,
    claimed_external_ids: std::collections::HashSet<u64>,
}

impl CtxInner {
    fn new(history: Vec<Event>, execution_id: u64) -> Self {
        // In the new monotonic system, correlation IDs should start from 1
        // regardless of history length, since OrchestrationStarted doesn't use correlation IDs
        let next_id = 1u64;

        Self {
            history,
            actions: Vec::new(),
            // guid_counter removed
            next_correlation_id: next_id,
            execution_id,
            turn_index: 0,
            logging_enabled_this_poll: false,
            log_buffer: Vec::new(),
            claimed_activity_ids: Default::default(),
            claimed_timer_ids: Default::default(),
            claimed_external_ids: Default::default(),
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

    fn next_id(&mut self) -> u64 {
        let id = self.next_correlation_id;
        self.next_correlation_id += 1;
        id
    }
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
    // Removed: use system_now_ms().await for wall-clock time
    /// Returns a deterministic GUID string, incremented per instance.
    // Removed: use system_new_guid().await for GUIDs

    pub(crate) fn take_actions(&self) -> Vec<Action> {
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
    /// Drain the buffered log messages accumulated during the last turn.
    pub fn take_log_buffer(&self) -> Vec<(LogLevel, String)> {
        std::mem::take(&mut self.inner.lock().unwrap().log_buffer)
    }
    /// Buffer a structured log message for the current turn.
    pub fn push_log(&self, level: LogLevel, msg: String) {
        self.inner.lock().unwrap().log_buffer.push((level, msg));
    }

    /// Emit a structured trace entry using the system trace activity.
    /// This is fire-and-forget - the trace is recorded in history but doesn't need to be awaited.
    pub fn trace(&self, level: impl Into<String>, message: impl Into<String>) {
        let payload = format!("{}:{}", level.into(), message.into());
        let mut fut = self.schedule_activity(SYSTEM_TRACE_ACTIVITY, payload);
        // Fire and forget - poll once to schedule the activity
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

    /// Generate a new deterministic GUID.
    /// This is syntactic sugar for scheduling the system GUID activity.
    /// Returns a DurableFuture that must be awaited to get the GUID value.
    pub fn new_guid(&self) -> DurableFuture {
        self.schedule_activity(SYSTEM_NEW_GUID_ACTIVITY, "")
    }

    /// Get the current UTC time in milliseconds since epoch.
    /// This is syntactic sugar for scheduling the system time activity.
    /// Returns a DurableFuture that must be awaited to get the timestamp value.
    pub fn utcnow_ms(&self) -> DurableFuture {
        self.schedule_activity(SYSTEM_NOW_ACTIVITY, "")
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
use crate::futures::KindTag;

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
    /// **Activities should be pure business logic without delays or sleeps!**
    /// For time-based waiting, use `schedule_timer()` instead.
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
    /// - HTTP API calls
    /// - File operations
    /// - Data transformations
    ///
    /// # What NOT to put in activities
    /// - `tokio::time::sleep()` or similar delays
    /// - Long polling or waiting
    /// - Timeouts (use `select2` with timers instead)
    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();
        // Try to adopt an existing scheduled activity id that matches and isn't claimed yet
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                Event::ActivityScheduled {
                    id,
                    name: n,
                    input: inp,
                    execution_id: _,
                } if n == &name && inp == &input && !inner.claimed_activity_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_activity_ids.insert(adopted_id);
        drop(inner);
        DurableFuture(Kind::Activity {
            id: adopted_id,
            name,
            input,
            scheduled: Cell::new(false),
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
        let mut inner = self.inner.lock().unwrap();
        // Adopt first unclaimed TimerCreated id if any, else allocate
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                Event::TimerCreated { id, .. } if !inner.claimed_timer_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_timer_ids.insert(adopted_id);
        drop(inner);
        DurableFuture(Kind::Timer {
            id: adopted_id,
            delay_ms,
            scheduled: Cell::new(false),
            ctx: self.clone(),
        })
    }

    /// Subscribe to an external event by name and return its `DurableFuture`.
    pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        let mut inner = self.inner.lock().unwrap();
        // Adopt existing subscription id for this name if present and unclaimed, else allocate
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                Event::ExternalSubscribed { id, name: n } if n == &name && !inner.claimed_external_ids.contains(id) => {
                    Some(*id)
                }
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_external_ids.insert(adopted_id);
        drop(inner);
        DurableFuture(Kind::External {
            id: adopted_id,
            name,
            scheduled: Cell::new(false),
            ctx: self.clone(),
        })
    }

    /// Typed external wait adapter pairs with `into_event_typed` for decoding.
    pub fn schedule_wait_typed<T: serde::de::DeserializeOwned>(&self, name: impl Into<String>) -> DurableFuture {
        self.schedule_wait(name)
    }

    /// Schedule a sub-orchestration by name with deterministic child instance id derived
    /// from parent context and correlation id.
    pub fn schedule_sub_orchestration(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();
        // Adopt existing record or allocate new id
        let adopted = inner.history.iter().find_map(|e| match e {
            Event::SubOrchestrationScheduled {
                id,
                name: n,
                input: inp,
                instance: inst,
                execution_id: _,
            } if n == &name && inp == &input => Some((*id, inst.clone())),
            _ => None,
        });
        let (id, instance) = if let Some((id, inst)) = adopted {
            (id, inst)
        } else {
            (inner.next_id(), String::new())
        };
        // Use a portable placeholder that the runtime can disambiguate by prefixing parent instance
        let child_instance = if instance.is_empty() {
            format!("sub::{id}")
        } else {
            instance
        };
        drop(inner);
        DurableFuture(Kind::SubOrch {
            id,
            name,
            version: None,
            instance: child_instance,
            input,
            scheduled: Cell::new(false),
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
        let name: String = name.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();
        let adopted = inner.history.iter().find_map(|e| match e {
            Event::SubOrchestrationScheduled {
                id,
                name: n,
                input: inp,
                instance: inst,
                execution_id: _,
            } if n == &name && inp == &input => Some((*id, inst.clone())),
            _ => None,
        });
        let (id, instance) = if let Some((id, inst)) = adopted {
            (id, inst)
        } else {
            (inner.next_id(), String::new())
        };
        let child_instance = if instance.is_empty() {
            format!("sub::{id}")
        } else {
            instance
        };
        drop(inner);
        DurableFuture(Kind::SubOrch {
            id,
            name,
            version,
            instance: child_instance,
            input,
            scheduled: Cell::new(false),
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
        let adopted = inner.history.iter().find_map(|e| match e {
            Event::OrchestrationChained {
                id,
                name: n,
                instance: inst,
                input: inp,
            } if n == &name && inp == &input && inst == &instance => Some(*id),
            _ => None,
        });
        let id = adopted.unwrap_or_else(|| inner.next_id());
        inner.history.push(Event::OrchestrationChained {
            id,
            name: name.clone(),
            instance: instance.clone(),
            input: input.clone(),
        });
        inner.record_action(Action::StartOrchestrationDetached {
            id,
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
        let adopted = inner.history.iter().find_map(|e| match e {
            Event::OrchestrationChained {
                id,
                name: n,
                instance: inst,
                input: inp,
            } if n == &name && inp == &input && inst == &instance => Some(*id),
            _ => None,
        });
        let id = adopted.unwrap_or_else(|| inner.next_id());
        inner.history.push(Event::OrchestrationChained {
            id,
            name: name.clone(),
            instance: instance.clone(),
            input: input.clone(),
        });
        let version_for_note = version.clone();
        inner.record_action(Action::StartOrchestrationDetached {
            id,
            name,
            version,
            instance,
            input,
        });
        drop(inner);
        if let Some(ver) = version_for_note {
            // best-effort: stash as a side-effect by pinning child on the host when dispatched (handled in runtime)
            let _ = ver; // signaling only; runtime will resolve by policy unless explicitly set elsewhere
        }
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

    // removed: schedule_orchestration(name, input) without instance id (must pass instance id)
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

    fn find_history_index(hist: &Vec<Event>, id: u64, kind: KindTag) -> Option<usize> {
        for (idx, e) in hist.iter().enumerate() {
            match (kind, e) {
                (KindTag::Activity, Event::ActivityCompleted { id: cid, .. }) if *cid == id => return Some(idx),
                (KindTag::Activity, Event::ActivityFailed { id: cid, .. }) if *cid == id => return Some(idx),
                (KindTag::Timer, Event::TimerFired { id: cid, .. }) if *cid == id => return Some(idx),
                (KindTag::External, Event::ExternalEvent { id: cid, .. }) if *cid == id => return Some(idx),
                (KindTag::SubOrch, Event::SubOrchestrationCompleted { id: cid, .. }) if *cid == id => return Some(idx),
                (KindTag::SubOrch, Event::SubOrchestrationFailed { id: cid, .. }) if *cid == id => return Some(idx),
                _ => {}
            }
        }
        None
    }

    fn synth_output_from_history(hist: &Vec<Event>, id: u64, kind: KindTag) -> DurableOutput {
        for e in hist.iter().rev() {
            match (kind, e) {
                (KindTag::Activity, Event::ActivityCompleted { id: cid, result }) if *cid == id => {
                    return DurableOutput::Activity(Ok(result.clone()));
                }
                (KindTag::Activity, Event::ActivityFailed { id: cid, error }) if *cid == id => {
                    return DurableOutput::Activity(Err(error.clone()));
                }
                (KindTag::Timer, Event::TimerFired { id: cid, .. }) if *cid == id => return DurableOutput::Timer,
                (KindTag::External, Event::ExternalEvent { id: cid, data, .. }) if *cid == id => {
                    return DurableOutput::External(data.clone());
                }
                (KindTag::SubOrch, Event::SubOrchestrationCompleted { id: cid, result }) if *cid == id => {
                    return DurableOutput::SubOrchestration(Ok(result.clone()));
                }
                (KindTag::SubOrch, Event::SubOrchestrationFailed { id: cid, error }) if *cid == id => {
                    return DurableOutput::SubOrchestration(Err(error.clone()));
                }
                _ => {}
            }
        }
        DurableOutput::Timer
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
/// updated history, requested `Action`s, buffered logs, and an optional output.
/// Tuple returned by `run_turn` and `run_turn_with` containing the updated
/// history, actions to execute, per-turn logs, and an optional output.
pub type TurnResult<O> = (Vec<Event>, Vec<Action>, Vec<(LogLevel, String)>, Option<O>);

pub fn run_turn<O, F>(history: Vec<Event>, orchestrator: impl Fn(OrchestrationContext) -> F) -> TurnResult<O>
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history, 1); // Default execution_id for legacy compatibility
    let mut fut = orchestrator(ctx.clone());
    // Reset logging flag at start of poll; it will be flipped to true when a decision is recorded
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let logs = ctx.take_log_buffer();
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, actions, logs, Some(out))
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let logs = ctx.take_log_buffer();
            (hist_after, actions, logs, None)
        }
    }
}

/// Same as `run_turn` but annotates the context with a caller-supplied
/// turn index for diagnostics and logging.
pub fn run_turn_with<O, F>(
    history: Vec<Event>,
    turn_index: u64,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> TurnResult<O>
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history, 1); // Default execution_id for legacy compatibility
    ctx.set_turn_index(turn_index);
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = orchestrator(ctx.clone());
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let logs = ctx.take_log_buffer();
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, actions, logs, Some(out))
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let logs = ctx.take_log_buffer();
            (hist_after, actions, logs, None)
        }
    }
}

/// Snapshot of IDs claimed by the orchestrator during a single poll turn.
#[derive(Debug, Clone, Default)]
pub struct ClaimedIdsSnapshot {
    pub activities: std::collections::HashSet<u64>,
    pub timers: std::collections::HashSet<u64>,
    pub externals: std::collections::HashSet<u64>,
    pub sub_orchestrations: std::collections::HashSet<u64>,
}

impl OrchestrationContext {
    /// Internal: export a snapshot of correlation IDs that were claimed during this poll.
    pub(crate) fn claimed_ids_snapshot(&self) -> ClaimedIdsSnapshot {
        let inner = self.inner.lock().unwrap();
        ClaimedIdsSnapshot {
            activities: inner
                .history
                .iter()
                .filter_map(|e| match e {
                    Event::ActivityScheduled { id, .. } => Some(*id),
                    _ => None,
                })
                .collect(),
            timers: inner
                .history
                .iter()
                .filter_map(|e| match e {
                    Event::TimerCreated { id, .. } => Some(*id),
                    _ => None,
                })
                .collect(),
            externals: inner
                .history
                .iter()
                .filter_map(|e| match e {
                    Event::ExternalSubscribed { id, .. } => Some(*id),
                    _ => None,
                })
                .collect(),
            sub_orchestrations: inner
                .history
                .iter()
                .filter_map(|e| match e {
                    Event::SubOrchestrationScheduled { id, .. } => Some(*id),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// Same as `run_turn_with` but also returns which correlation IDs were claimed during the poll.
pub fn run_turn_with_claims<O, F>(
    history: Vec<Event>,
    turn_index: u64,
    execution_id: u64,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (
    Vec<Event>,
    Vec<Action>,
    Vec<(LogLevel, String)>,
    Option<O>,
    ClaimedIdsSnapshot,
)
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history, execution_id);
    ctx.set_turn_index(turn_index);
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = orchestrator(ctx.clone());
    let res = match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let logs = ctx.take_log_buffer();
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let claims = ctx.claimed_ids_snapshot();
            (hist_after, actions, logs, Some(out), claims)
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let logs = ctx.take_log_buffer();
            let claims = ctx.claimed_ids_snapshot();
            (hist_after, actions, logs, None, claims)
        }
    };
    res
}

/// Helper for single-threaded, host-driven execution in tests and samples.
pub struct Executor;

impl Executor {
    /// Drives an orchestrator by alternately replaying one turn and invoking
    /// the provided `execute_actions` to materialize requested actions into
    /// history, until the orchestrator completes.
    pub fn drive_to_completion<O, F, X>(
        mut history: Vec<Event>,
        orchestrator: impl Fn(OrchestrationContext) -> F,
        mut execute_actions: X,
    ) -> (Vec<Event>, O)
    where
        F: Future<Output = O>,
        X: FnMut(Vec<Action>, &mut Vec<Event>),
    {
        loop {
            let (hist_after_replay, actions, _logs, output) = run_turn(history, &orchestrator);
            history = hist_after_replay;
            if let Some(out) = output {
                return (history, out);
            }
            execute_actions(actions, &mut history);
        }
    }
}
