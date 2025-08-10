use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// Public orchestration primitives and executor

#[derive(Debug, Clone)]
pub enum Event {
    ActivityResult { name: String, input: String, result: String },
    TimerFired { fire_at_ms: u64 },
    ExternalEvent { name: String, data: String },
}

#[derive(Debug, Clone)]
pub enum Action {
    CallActivity { name: String, input: String },
    CreateTimer { delay_ms: u64 },
    WaitExternal { name: String },
}

#[derive(Debug)]
struct CtxInner {
    history: Vec<Event>,
    cursor: usize,
    actions: Vec<Action>,
    logical_time_ms: u64,
    guid_counter: u64,
}

impl CtxInner {
    fn new(history: Vec<Event>) -> Self {
        Self { history, cursor: 0, actions: Vec::new(), logical_time_ms: 0, guid_counter: 0 }
    }

    fn next_event(&self) -> Option<&Event> { self.history.get(self.cursor) }

    fn consume_event(&mut self) -> Option<Event> {
        if let Some(ev) = self.history.get(self.cursor).cloned() {
            self.cursor += 1;
            if let Event::TimerFired { fire_at_ms } = ev { self.logical_time_ms = fire_at_ms; }
            Some(ev)
        } else {
            None
        }
    }

    fn record_action(&mut self, a: Action) { self.actions.push(a); }

    fn now_ms(&self) -> u64 { self.logical_time_ms }
    fn new_guid(&mut self) -> String { self.guid_counter += 1; format!("{:#034x}", self.guid_counter) }
}

#[derive(Clone)]
pub struct OrchestrationContext { inner: Arc<Mutex<CtxInner>> }

impl OrchestrationContext {
    pub fn new(history: Vec<Event>) -> Self { Self { inner: Arc::new(Mutex::new(CtxInner::new(history))) } }

    pub fn now_ms(&self) -> u64 { self.inner.lock().unwrap().now_ms() }
    pub fn new_guid(&self) -> String { self.inner.lock().unwrap().new_guid() }

    pub fn call_activity(&self, name: impl Into<String>, input: impl Into<String>) -> impl Future<Output = String> + '_ {
        struct CallActivity { name: String, input: String, scheduled: Cell<bool>, ctx: OrchestrationContext }
        impl Future for CallActivity {
            type Output = String;
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                let mut ctx = this.ctx.inner.lock().unwrap();
                if let Some(next) = ctx.next_event().cloned() {
                    match next {
                        Event::ActivityResult { name, input, result } => {
                            if name == this.name && input == this.input {
                                ctx.consume_event();
                                return Poll::Ready(result);
                            } else {
                                panic!(
                                    "Replay corruption: expected ActivityResult({}, {}), found {:?}",
                                    this.name, this.input, Event::ActivityResult { name, input, result }
                                );
                            }
                        }
                        other => panic!(
                            "Replay corruption: expected ActivityResult({}, {}), found {other:?}",
                            this.name, this.input
                        ),
                    }
                }
                if !this.scheduled.replace(true) {
                    ctx.record_action(Action::CallActivity { name: this.name.clone(), input: this.input.clone() });
                }
                Poll::Pending
            }
        }
        CallActivity { name: name.into(), input: input.into(), scheduled: Cell::new(false), ctx: self.clone() }
    }

    pub fn timer(&self, delay_ms: u64) -> impl Future<Output = ()> + '_ {
        struct TimerFuture { delay_ms: u64, scheduled: Cell<bool>, ctx: OrchestrationContext }
        impl Future for TimerFuture {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                let mut ctx = this.ctx.inner.lock().unwrap();
                if let Some(next) = ctx.next_event().cloned() {
                    match next {
                        Event::TimerFired { .. } => {
                            ctx.consume_event();
                            return Poll::Ready(());
                        }
                        other => panic!("Replay corruption: expected TimerFired, found {other:?}"),
                    }
                }
                if !this.scheduled.replace(true) {
                    ctx.record_action(Action::CreateTimer { delay_ms: this.delay_ms });
                }
                Poll::Pending
            }
        }
        TimerFuture { delay_ms, scheduled: Cell::new(false), ctx: self.clone() }
    }

    pub fn wait_external(&self, name: impl Into<String>) -> impl Future<Output = String> + '_ {
        struct WaitExternal { name: String, scheduled: Cell<bool>, ctx: OrchestrationContext }
        impl Future for WaitExternal {
            type Output = String;
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                let mut ctx = this.ctx.inner.lock().unwrap();
                if let Some(next) = ctx.next_event().cloned() {
                    match next {
                        Event::ExternalEvent { name, data } => {
                            if name == this.name {
                                ctx.consume_event();
                                return Poll::Ready(data);
                            } else {
                                panic!(
                                    "Replay corruption: expected ExternalEvent({}), found {:?}",
                                    this.name, Event::ExternalEvent { name, data }
                                );
                            }
                        }
                        other => panic!(
                            "Replay corruption: expected ExternalEvent({}), found {other:?}",
                            this.name
                        ),
                    }
                }
                if !this.scheduled.replace(true) {
                    ctx.record_action(Action::WaitExternal { name: this.name.clone() });
                }
                Poll::Pending
            }
        }
        WaitExternal { name: name.into(), scheduled: Cell::new(false), ctx: self.clone() }
    }

    fn take_actions(&self) -> Vec<Action> { std::mem::take(&mut self.inner.lock().unwrap().actions) }
}

// Unified future/output that allows joining different orchestration primitives

#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(String),
    Timer,
    External(String),
}

pub struct DurableFuture(Kind);

enum Kind {
    Activity { name: String, input: String, scheduled: Cell<bool>, ctx: OrchestrationContext },
    Timer { delay_ms: u64, scheduled: Cell<bool>, ctx: OrchestrationContext },
    External { name: String, scheduled: Cell<bool>, ctx: OrchestrationContext },
}

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };
        match &mut this.0 {
            Kind::Activity { name, input, scheduled, ctx } => {
                let mut inner = ctx.inner.lock().unwrap();
                if let Some(next) = inner.next_event().cloned() {
                    match next {
                        Event::ActivityResult { name: n, input: inp, result } => {
                            if &n == name && &inp == input {
                                inner.consume_event();
                                return Poll::Ready(DurableOutput::Activity(result));
                            } else {
                                panic!(
                                    "Replay corruption: expected ActivityResult({}, {}), found {:?}",
                                    name, input, Event::ActivityResult { name: n, input: inp, result }
                                );
                            }
                        }
                        other => panic!(
                            "Replay corruption: expected ActivityResult({name}, {input}), found {other:?}",
                        ),
                    }
                }
                if !scheduled.replace(true) {
                    inner.record_action(Action::CallActivity { name: name.clone(), input: input.clone() });
                }
                Poll::Pending
            }
            Kind::Timer { delay_ms, scheduled, ctx } => {
                let mut inner = ctx.inner.lock().unwrap();
                if let Some(next) = inner.next_event().cloned() {
                    match next {
                        Event::TimerFired { .. } => {
                            inner.consume_event();
                            return Poll::Ready(DurableOutput::Timer);
                        }
                        other => panic!("Replay corruption: expected TimerFired, found {other:?}"),
                    }
                }
                if !scheduled.replace(true) {
                    inner.record_action(Action::CreateTimer { delay_ms: *delay_ms });
                }
                Poll::Pending
            }
            Kind::External { name, scheduled, ctx } => {
                let mut inner = ctx.inner.lock().unwrap();
                if let Some(next) = inner.next_event().cloned() {
                    match next {
                        Event::ExternalEvent { name: n, data } => {
                            if &n == name {
                                inner.consume_event();
                                return Poll::Ready(DurableOutput::External(data));
                            } else {
                                panic!(
                                    "Replay corruption: expected ExternalEvent({}), found {:?}",
                                    name, Event::ExternalEvent { name: n, data }
                                );
                            }
                        }
                        other => panic!(
                            "Replay corruption: expected ExternalEvent({name}), found {other:?}",
                        ),
                    }
                }
                if !scheduled.replace(true) {
                    inner.record_action(Action::WaitExternal { name: name.clone() });
                }
                Poll::Pending
            }
        }
    }
}

impl OrchestrationContext {
    pub fn schedule_activity(&self, name: impl Into<String>, input: impl Into<String>) -> DurableFuture {
        DurableFuture(Kind::Activity { name: name.into(), input: input.into(), scheduled: Cell::new(false), ctx: self.clone() })
    }

    pub fn schedule_timer(&self, delay_ms: u64) -> DurableFuture {
        DurableFuture(Kind::Timer { delay_ms, scheduled: Cell::new(false), ctx: self.clone() })
    }

    pub fn schedule_wait(&self, name: impl Into<String>) -> DurableFuture {
        DurableFuture(Kind::External { name: name.into(), scheduled: Cell::new(false), ctx: self.clone() })
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
    let ctx = OrchestrationContext::new(history.clone());
    let mut fut = orchestrator(ctx.clone());
    match poll_once(&mut fut) {
        Poll::Ready(out) => (history, Vec::new(), Some(out)),
        Poll::Pending => {
            let actions = ctx.take_actions();
            if !actions.is_empty() {
                (history, actions, None)
            } else {
                (history, Vec::new(), None)
            }
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
            let (hist_after_replay, actions, output) = run_turn(history.clone(), &orchestrator);
            if let Some(out) = output {
                return (history, out);
            }
            execute_actions(actions, &mut history);
            // history = execute_actions(hist_after_replay, actions); // host mutates in place
            let _ = hist_after_replay; // not used; we mutate history directly
        }
    }
}




