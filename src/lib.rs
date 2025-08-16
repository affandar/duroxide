//! Minimal deterministic orchestration core inspired by Durable Task.
//!
//! This crate exposes a replay-driven programming model that records
//! append-only `Event`s and replays them to make orchestration logic
//! deterministic. It provides:
//!
//! - Public data model: `Event`, `Action`
//! - Orchestration driver: `run_turn`, `run_turn_with`, and `Executor`
//! - An `OrchestrationContext` with futures to schedule activities,
//!   timers, and external events using correlation IDs
//! - A unified `DurableFuture` that can be composed with `join`/`select`
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// Public orchestration primitives and executor

pub mod logging;
pub mod providers;
pub mod runtime;

use crate::logging::LogLevel;
use serde::{Deserialize, Serialize};

/// Append-only orchestration history entries persisted by a provider and
/// consumed during replay. Variants use stable correlation IDs to pair
/// scheduling operations with their completions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Activity was scheduled with a unique ID and input.
    ActivityScheduled {
        id: u64,
        name: String,
        input: String,
    },
    /// Activity completed successfully with a result.
    ActivityCompleted { id: u64, result: String },
    /// Activity failed with an error string.
    ActivityFailed { id: u64, error: String },

    /// Timer was created and will logically fire at `fire_at_ms`.
    TimerCreated { id: u64, fire_at_ms: u64 },
    /// Timer fired at logical time `fire_at_ms`.
    TimerFired { id: u64, fire_at_ms: u64 },

    /// Subscription to an external event by name was recorded with a unique ID.
    ExternalSubscribed { id: u64, name: String },
    /// An external event with correlation `id` was raised with some data.
    ExternalEvent { id: u64, name: String, data: String },

    /// A structured trace message emitted by the orchestrator.
    TraceEmitted {
        id: u64,
        level: String,
        message: String,
    },
}

/// Declarative decisions produced by an orchestration turn. The host/provider
/// is responsible for materializing these into corresponding `Event`s.
#[derive(Debug, Clone)]
pub enum Action {
    /// Schedule an activity invocation.
    CallActivity {
        id: u64,
        name: String,
        input: String,
    },
    /// Create a timer that will fire after the requested delay.
    CreateTimer { id: u64, delay_ms: u64 },
    /// Subscribe to an external event by name.
    WaitExternal { id: u64, name: String },
    /// Emit a trace entry at the given level with a message.
    EmitTrace {
        id: u64,
        level: String,
        message: String,
    },
}

#[derive(Debug)]
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,

    // Deterministic ids and GUIDs
    guid_counter: u64,
    next_correlation_id: u64,

    // Logging and turn metadata
    turn_index: u64,
    logging_enabled_this_poll: bool,
    // Per-turn buffered logs (messages to flush once per progress turn)
    log_buffer: Vec<(LogLevel, String)>,

    // Reserved for future use: per-turn claimed ids to coordinate multiple futures
    // (prevent re-scheduling the same id). Currently unused.
    #[allow(dead_code)]
    claimed_activity_ids: std::collections::HashSet<u64>,
    #[allow(dead_code)]
    claimed_timer_ids: std::collections::HashSet<u64>,
    #[allow(dead_code)]
    claimed_external_ids: std::collections::HashSet<u64>,
}

impl CtxInner {
    fn new(history: Vec<Event>) -> Self {
        // Compute next correlation id based on max id found in history
        let mut max_id = 0u64;
        for ev in &history {
            let id_opt = match ev {
                Event::ActivityScheduled { id, .. }
                | Event::ActivityCompleted { id, .. }
                | Event::ActivityFailed { id, .. }
                | Event::TimerCreated { id, .. }
                | Event::TimerFired { id, .. }
                | Event::ExternalSubscribed { id, .. }
                | Event::ExternalEvent { id, .. }
                | Event::TraceEmitted { id, .. } => Some(*id),
            };
            if let Some(id) = id_opt {
                max_id = max_id.max(id);
            }
        }
        Self {
            history,
            actions: Vec::new(),
            guid_counter: 0,
            next_correlation_id: max_id.saturating_add(1),
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
        // Logical time is last TimerFired.fire_at_ms seen in history
        let mut last = 0u64;
        for ev in &self.history {
            if let Event::TimerFired { fire_at_ms, .. } = ev {
                if *fire_at_ms > last {
                    last = *fire_at_ms;
                }
            }
        }
        last
    }

    fn new_guid(&mut self) -> String {
        self.guid_counter += 1;
        format!("{:#034x}", self.guid_counter)
    }

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
    pub fn new(history: Vec<Event>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CtxInner::new(history))),
        }
    }

    /// Returns the current logical time in milliseconds based on the last
    /// `TimerFired` event in history.
    pub fn now_ms(&self) -> u64 {
        self.inner.lock().unwrap().now_ms()
    }
    /// Returns a deterministic GUID string, incremented per instance.
    pub fn new_guid(&self) -> String {
        self.inner.lock().unwrap().new_guid()
    }

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
    #[allow(dead_code)]
    pub(crate) fn set_logging_enabled(&self, enabled: bool) {
        self.inner.lock().unwrap().logging_enabled_this_poll = enabled;
    }
    /// Drain the buffered log messages accumulated during the last turn.
    pub fn take_log_buffer(&self) -> Vec<(LogLevel, String)> {
        std::mem::take(&mut self.inner.lock().unwrap().log_buffer)
    }
    /// Buffer a structured log message for the current turn.
    pub fn push_log(&self, level: LogLevel, msg: String) {
        self.inner.lock().unwrap().log_buffer.push((level, msg));
    }

    // System trace helper: thin wrapper over schedule_activity + immediate first poll to record scheduling
    /// Emit a structured trace entry using the system trace activity.
    pub fn trace(&self, level: impl Into<String>, message: impl Into<String>) {
        let payload = format!("{}:{}", level.into(), message.into());
        let mut fut = self.schedule_activity("__system_trace", payload);
        let _ = poll_once(&mut fut);
    }

    // Typed wrappers for common levels
    /// Convenience wrapper for INFO level tracing.
    pub fn trace_info(&self, message: impl Into<String>) {
        self.trace("INFO", message.into());
    }
    /// Convenience wrapper for WARN level tracing.
    pub fn trace_warn(&self, message: impl Into<String>) {
        self.trace("WARN", message.into());
    }
    /// Convenience wrapper for ERROR level tracing.
    pub fn trace_error(&self, message: impl Into<String>) {
        self.trace("ERROR", message.into());
    }
    /// Convenience wrapper for DEBUG level tracing.
    pub fn trace_debug(&self, message: impl Into<String>) {
        self.trace("DEBUG", message.into());
    }

    /// Return current wall-clock time from a system activity in milliseconds since epoch.
    pub async fn system_now_ms(&self) -> u128 {
        let v = self
            .schedule_activity("__system_now", "".to_string())
            .into_activity()
            .await
            .unwrap_or_else(|e| panic!("system_now failed: {e}"));
        v.parse::<u128>().unwrap_or(0)
    }

    /// Return a new pseudo-GUID string from a system activity. Intended for
    /// integration paths; for deterministic GUIDs prefer `new_guid()`.
    pub async fn system_new_guid(&self) -> String {
        self.schedule_activity("__system_new_guid", "".to_string())
            .into_activity()
            .await
            .unwrap_or_else(|e| panic!("system_new_guid failed: {e}"))
    }
}

// Unified future/output that allows joining different orchestration primitives

/// Output of a `DurableFuture` when awaited via unified composition.
#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(Result<String, String>),
    Timer,
    External(String),
}

// NOTE: Current replay model strictly consumes the next history event for each await.
// This breaks down in races (e.g., select(timer, external)) where the host may append
// multiple completions in one turn, and the "loser" event can end up ahead of the next
// awaited operation, causing a replay mismatch. We will refactor to correlate by stable
// IDs and buffer completions so futures resolve by correlation rather than head-of-queue
// order, matching Durable Task semantics where multiple results can be present out of
// arrival order without corrupting replay.

/// A unified future for activities, timers, and external events that carries a
/// correlation ID. Useful for composing with `futures::select`/`join`.
pub struct DurableFuture(Kind);

enum Kind {
    Activity {
        id: u64,
        name: String,
        input: String,
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    Timer {
        id: u64,
        delay_ms: u64,
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
    External {
        id: u64,
        name: String,
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
}

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };
        match &mut this.0 {
            Kind::Activity {
                id,
                name,
                input,
                scheduled,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                // Is there a completion for this id in history?
                if let Some(outcome) = inner.history.iter().rev().find_map(|e| match e {
                    Event::ActivityCompleted { id: cid, result } if cid == id => {
                        Some(Ok(result.clone()))
                    }
                    Event::ActivityFailed { id: cid, error } if cid == id => {
                        Some(Err(error.clone()))
                    }
                    _ => None,
                }) {
                    return Poll::Ready(DurableOutput::Activity(outcome));
                }
                // If not yet scheduled in history, emit a CallActivity action once
                let already_scheduled = inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::ActivityScheduled { id: cid, .. } if cid == id));
                if !already_scheduled && !scheduled.replace(true) {
                    // Record schedule locally for this turn so subsequent polls observe it
                    inner.history.push(Event::ActivityScheduled {
                        id: *id,
                        name: name.clone(),
                        input: input.clone(),
                    });
                    inner.record_action(Action::CallActivity {
                        id: *id,
                        name: name.clone(),
                        input: input.clone(),
                    });
                }
                Poll::Pending
            }
            Kind::Timer {
                id,
                delay_ms,
                scheduled,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                if inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::TimerFired { id: cid, .. } if cid == id))
                {
                    return Poll::Ready(DurableOutput::Timer);
                }
                let already_created = inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::TimerCreated { id: cid, .. } if cid == id));
                if !already_created && !scheduled.replace(true) {
                    let fire_at_ms = inner.now_ms().saturating_add(*delay_ms);
                    inner.history.push(Event::TimerCreated {
                        id: *id,
                        fire_at_ms,
                    });
                    inner.record_action(Action::CreateTimer {
                        id: *id,
                        delay_ms: *delay_ms,
                    });
                }
                Poll::Pending
            }
            Kind::External {
                id,
                name,
                scheduled,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                if let Some(data) = inner.history.iter().rev().find_map(|e| match e {
                    Event::ExternalEvent { id: cid, data, .. } if cid == id => Some(data.clone()),
                    _ => None,
                }) {
                    return Poll::Ready(DurableOutput::External(data));
                }
                let already_subscribed = inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::ExternalSubscribed { id: cid, .. } if cid == id));
                if !already_subscribed && !scheduled.replace(true) {
                    inner.history.push(Event::ExternalSubscribed {
                        id: *id,
                        name: name.clone(),
                    });
                    inner.record_action(Action::WaitExternal {
                        id: *id,
                        name: name.clone(),
                    });
                }
                Poll::Pending
            }
        }
    }
}

impl DurableFuture {
    /// Converts this unified future into a future that resolves only for
    /// an activity completion or failure.
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
}

impl OrchestrationContext {
    /// Schedule an activity and return a `DurableFuture` correlated to it.
    pub fn schedule_activity(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
    ) -> DurableFuture {
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
                } if n == &name && inp == &input && !inner.claimed_activity_ids.contains(id) => {
                    Some(*id)
                }
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

    /// Schedule a timer and return a `DurableFuture` correlated to it.
    pub fn schedule_timer(&self, delay_ms: u64) -> DurableFuture {
        let mut inner = self.inner.lock().unwrap();
        // Adopt first unclaimed TimerCreated id if any, else allocate
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                Event::TimerCreated { id, .. } if !inner.claimed_timer_ids.contains(id) => {
                    Some(*id)
                }
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
                Event::ExternalSubscribed { id, name: n }
                    if n == &name && !inner.claimed_external_ids.contains(id) =>
                {
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

pub fn run_turn<O, F>(
    history: Vec<Event>,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> TurnResult<O>
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history);
    let mut fut = orchestrator(ctx.clone());
    // Reset logging flag at start of poll; it will be flipped to true when a decision is recorded
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let logs = ctx.take_log_buffer();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, Vec::new(), logs, Some(out))
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
    let ctx = OrchestrationContext::new(history);
    ctx.set_turn_index(turn_index);
    ctx.inner.lock().unwrap().logging_enabled_this_poll = false;
    let mut fut = orchestrator(ctx.clone());
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            ctx.inner.lock().unwrap().logging_enabled_this_poll = true;
            let logs = ctx.take_log_buffer();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, Vec::new(), logs, Some(out))
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            let logs = ctx.take_log_buffer();
            (hist_after, actions, logs, None)
        }
    }
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
