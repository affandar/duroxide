use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// Public orchestration primitives and executor

pub mod runtime;
pub mod providers;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    // Activity lifecycle
    ActivityScheduled { id: u64, name: String, input: String },
    ActivityCompleted { id: u64, result: String },

    // Timer lifecycle
    TimerCreated { id: u64, fire_at_ms: u64 },
    TimerFired { id: u64, fire_at_ms: u64 },

    // External event subscription and raise
    ExternalSubscribed { id: u64, name: String },
    ExternalEvent { id: u64, name: String, data: String },
}

#[derive(Debug, Clone)]
pub enum Action {
    CallActivity { id: u64, name: String, input: String },
    CreateTimer { id: u64, delay_ms: u64 },
    WaitExternal { id: u64, name: String },
}

#[derive(Debug)]
struct CtxInner {
    history: Vec<Event>,
    actions: Vec<Action>,

    // Deterministic ids and GUIDs
    guid_counter: u64,
    next_correlation_id: u64,

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
                | Event::TimerCreated { id, .. }
                | Event::TimerFired { id, .. }
                | Event::ExternalSubscribed { id, .. }
                | Event::ExternalEvent { id, .. } => Some(*id),
            };
            if let Some(id) = id_opt { max_id = max_id.max(id); }
        }
        Self {
            history,
            actions: Vec::new(),
            guid_counter: 0,
            next_correlation_id: max_id.saturating_add(1),
            claimed_activity_ids: Default::default(),
            claimed_timer_ids: Default::default(),
            claimed_external_ids: Default::default(),
        }
    }

    fn record_action(&mut self, a: Action) { self.actions.push(a); }

    fn now_ms(&self) -> u64 {
        // Logical time is last TimerFired.fire_at_ms seen in history
        let mut last = 0u64;
        for ev in &self.history {
            if let Event::TimerFired { fire_at_ms, .. } = ev {
                if *fire_at_ms > last { last = *fire_at_ms; }
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

#[derive(Clone)]
pub struct OrchestrationContext { inner: Arc<Mutex<CtxInner>> }

impl OrchestrationContext {
    pub fn new(history: Vec<Event>) -> Self { Self { inner: Arc::new(Mutex::new(CtxInner::new(history))) } }

    pub fn now_ms(&self) -> u64 { self.inner.lock().unwrap().now_ms() }
    pub fn new_guid(&self) -> String { self.inner.lock().unwrap().new_guid() }

    fn take_actions(&self) -> Vec<Action> { std::mem::take(&mut self.inner.lock().unwrap().actions) }
}

// Unified future/output that allows joining different orchestration primitives

#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(String),
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

pub struct DurableFuture(Kind);

enum Kind {
    Activity { id: u64, name: String, input: String, scheduled: Cell<bool>, ctx: OrchestrationContext },
    Timer { id: u64, delay_ms: u64, scheduled: Cell<bool>, ctx: OrchestrationContext },
    External { id: u64, name: String, scheduled: Cell<bool>, ctx: OrchestrationContext },
}

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };
        match &mut this.0 {
            Kind::Activity { id, name, input, scheduled, ctx } => {
                let mut inner = ctx.inner.lock().unwrap();
                // Is there a completion for this id in history?
                if let Some(result) = inner.history.iter().rev().find_map(|e| match e {
                    Event::ActivityCompleted { id: cid, result } if cid == id => Some(result.clone()),
                    _ => None,
                }) {
                    return Poll::Ready(DurableOutput::Activity(result));
                }
                // If not yet scheduled in history, emit a CallActivity action once
                let already_scheduled = inner.history.iter().any(|e| matches!(e, Event::ActivityScheduled { id: cid, .. } if cid == id));
                if !already_scheduled && !scheduled.replace(true) {
                    // Record schedule locally for this turn so subsequent polls observe it
                    inner.history.push(Event::ActivityScheduled { id: *id, name: name.clone(), input: input.clone() });
                    inner.record_action(Action::CallActivity { id: *id, name: name.clone(), input: input.clone() });
                }
                Poll::Pending
            }
            Kind::Timer { id, delay_ms, scheduled, ctx } => {
                let mut inner = ctx.inner.lock().unwrap();
                if inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::TimerFired { id: cid, .. } if cid == id))
                {
                    return Poll::Ready(DurableOutput::Timer);
                }
                let already_created = inner.history.iter().any(|e| matches!(e, Event::TimerCreated { id: cid, .. } if cid == id));
                if !already_created && !scheduled.replace(true) {
                    let fire_at_ms = inner.now_ms().saturating_add(*delay_ms);
                    inner.history.push(Event::TimerCreated { id: *id, fire_at_ms });
                    inner.record_action(Action::CreateTimer { id: *id, delay_ms: *delay_ms });
                }
                Poll::Pending
            }
            Kind::External { id, name, scheduled, ctx } => {
                let mut inner = ctx.inner.lock().unwrap();
                if let Some(data) = inner.history.iter().rev().find_map(|e| match e {
                    Event::ExternalEvent { id: cid, data, .. } if cid == id => Some(data.clone()),
                    _ => None,
                }) {
                    return Poll::Ready(DurableOutput::External(data));
                }
                let already_subscribed = inner.history.iter().any(|e| matches!(e, Event::ExternalSubscribed { id: cid, .. } if cid == id));
                if !already_subscribed && !scheduled.replace(true) {
                    inner.history.push(Event::ExternalSubscribed { id: *id, name: name.clone() });
                    inner.record_action(Action::WaitExternal { id: *id, name: name.clone() });
                }
                Poll::Pending
            }
        }
    }
}

impl DurableFuture {
    pub fn into_activity(self) -> impl Future<Output = String> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = String;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::Activity(v)) => Poll::Ready(v),
                    Poll::Ready(other) => panic!("into_activity used on non-activity future: {other:?}"),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }

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

    pub fn into_event(self) -> impl Future<Output = String> {
        struct Map(DurableFuture);
        impl Future for Map {
            type Output = String;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
                match this.poll(cx) {
                    Poll::Ready(DurableOutput::External(v)) => Poll::Ready(v),
                    Poll::Ready(other) => panic!("into_event used on non-external future: {other:?}"),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }
}

impl OrchestrationContext {
    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        let input: String = input.into();
        let mut inner = self.inner.lock().unwrap();
        // Try to adopt an existing scheduled activity id that matches and isn't claimed yet
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                Event::ActivityScheduled { id, name: n, input: inp } if n == &name && inp == &input && !inner.claimed_activity_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_activity_ids.insert(adopted_id);
        drop(inner);
        DurableFuture(Kind::Activity { id: adopted_id, name, input, scheduled: Cell::new(false), ctx: self.clone() })
    }

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
        DurableFuture(Kind::Timer { id: adopted_id, delay_ms, scheduled: Cell::new(false), ctx: self.clone() })
    }

    pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture {
        let name: String = name.into();
        let mut inner = self.inner.lock().unwrap();
        // Adopt existing subscription id for this name if present and unclaimed, else allocate
        let adopted_id = inner
            .history
            .iter()
            .find_map(|e| match e {
                Event::ExternalSubscribed { id, name: n } if n == &name && !inner.claimed_external_ids.contains(id) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| inner.next_id());
        inner.claimed_external_ids.insert(adopted_id);
        drop(inner);
        DurableFuture(Kind::External { id: adopted_id, name, scheduled: Cell::new(false), ctx: self.clone() })
    }
}

fn noop_waker() -> Waker {
    unsafe fn clone(_: *const ()) -> RawWaker { RawWaker::new(std::ptr::null(), &VTABLE) }
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

pub fn run_turn<O, F>(history: Vec<Event>, orchestrator: impl Fn(OrchestrationContext) -> F) -> (Vec<Event>, Vec<Action>, Option<O>)
where
    F: Future<Output = O>,
{
    let ctx = OrchestrationContext::new(history);
    let mut fut = orchestrator(ctx.clone());
    match poll_once(&mut fut) {
        Poll::Ready(out) => {
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, Vec::new(), Some(out))
        }
        Poll::Pending => {
            let actions = ctx.take_actions();
            let hist_after = ctx.inner.lock().unwrap().history.clone();
            (hist_after, actions, None)
        }
    }
}

pub struct Executor;

impl Executor {
    pub fn drive_to_completion<O, F, X>(mut history: Vec<Event>, orchestrator: impl Fn(OrchestrationContext) -> F, mut execute_actions: X) -> (Vec<Event>, O)
    where
        F: Future<Output = O>,
        X: FnMut(Vec<Action>, &mut Vec<Event>),
    {
        loop {
            let (hist_after_replay, actions, output) = run_turn(history, &orchestrator);
            history = hist_after_replay;
            if let Some(out) = output {
                return (history, out);
            }
            execute_actions(actions, &mut history);
        }
    }
}




