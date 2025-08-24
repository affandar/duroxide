use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::{Action, Event, OrchestrationContext};

#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(Result<String, String>),
    Timer,
    External(String),
    SubOrchestration(Result<String, String>),
}

pub struct DurableFuture(pub(crate) Kind);

pub(crate) enum Kind {
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
    SubOrch {
        id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
        scheduled: Cell<bool>,
        ctx: OrchestrationContext,
    },
}

// Internal tag to classify DurableFuture kinds for history indexing
#[derive(Clone, Copy)]
pub(crate) enum KindTag {
    Activity,
    Timer,
    External,
    SubOrch,
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
                if let Some(outcome) = inner.history.iter().rev().find_map(|e| match e {
                    Event::ActivityCompleted { id: cid, result } if cid == id => Some(Ok(result.clone())),
                    Event::ActivityFailed { id: cid, error } if cid == id => Some(Err(error.clone())),
                    _ => None,
                }) {
                    return Poll::Ready(DurableOutput::Activity(outcome));
                }
                let already_scheduled = inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::ActivityScheduled { id: cid, .. } if cid == id));
                if !already_scheduled && !scheduled.replace(true) {
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
                    inner.history.push(Event::TimerCreated { id: *id, fire_at_ms });
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
            Kind::SubOrch {
                id,
                name,
                version,
                instance,
                input,
                scheduled,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                if let Some(outcome) = inner.history.iter().rev().find_map(|e| match e {
                    Event::SubOrchestrationCompleted { id: cid, result } if cid == id => Some(Ok(result.clone())),
                    Event::SubOrchestrationFailed { id: cid, error } if cid == id => Some(Err(error.clone())),
                    _ => None,
                }) {
                    return Poll::Ready(DurableOutput::SubOrchestration(outcome));
                }
                let already_scheduled = inner
                    .history
                    .iter()
                    .any(|e| matches!(e, Event::SubOrchestrationScheduled { id: cid, .. } if cid == id));
                if !already_scheduled && !scheduled.replace(true) {
                    inner.history.push(Event::SubOrchestrationScheduled {
                        id: *id,
                        name: name.clone(),
                        instance: instance.clone(),
                        input: input.clone(),
                    });
                    inner.record_action(Action::StartSubOrchestration {
                        id: *id,
                        name: name.clone(),
                        version: version.clone(),
                        instance: instance.clone(),
                        input: input.clone(),
                    });
                }
                Poll::Pending
            }
        }
    }
}

// Aggregate future machinery
enum AggregateMode {
    Select,
    Join,
}

pub enum AggregateOutput {
    Select { winner_index: usize, output: DurableOutput },
    Join { outputs: Vec<DurableOutput> },
}

pub struct AggregateDurableFuture {
    ctx: OrchestrationContext,
    children: Vec<DurableFuture>,
    meta: Vec<(u64, KindTag)>,
    mode: AggregateMode,
}

impl AggregateDurableFuture {
    pub(crate) fn new_select(ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        let meta = children
            .iter()
            .map(|f| match &f.0 {
                Kind::Activity { id, .. } => (*id, KindTag::Activity),
                Kind::Timer { id, .. } => (*id, KindTag::Timer),
                Kind::External { id, .. } => (*id, KindTag::External),
                Kind::SubOrch { id, .. } => (*id, KindTag::SubOrch),
            })
            .collect();
        Self {
            ctx,
            children,
            meta,
            mode: AggregateMode::Select,
        }
    }
    pub(crate) fn new_join(ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        let meta = children
            .iter()
            .map(|f| match &f.0 {
                Kind::Activity { id, .. } => (*id, KindTag::Activity),
                Kind::Timer { id, .. } => (*id, KindTag::Timer),
                Kind::External { id, .. } => (*id, KindTag::External),
                Kind::SubOrch { id, .. } => (*id, KindTag::SubOrch),
            })
            .collect();
        Self {
            ctx,
            children,
            meta,
            mode: AggregateMode::Join,
        }
    }
}

impl Future for AggregateDurableFuture {
    type Output = AggregateOutput;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        for f in &mut this.children {
            let _ = Pin::new(f).poll(cx);
        }
        let hist = this.ctx.inner.lock().unwrap().history.clone();
        let mut present: Vec<(usize, usize)> = Vec::new();
        for (i, (cid, tag)) in this.meta.iter().enumerate() {
            if let Some(idx) = OrchestrationContext::find_history_index(&hist, *cid, *tag) {
                present.push((i, idx));
            }
        }
        match this.mode {
            AggregateMode::Select => {
                if let Some((win, _)) = present.into_iter().min_by_key(|(_, idx)| *idx) {
                    let (cid, tag) = this.meta[win];
                    let out = OrchestrationContext::synth_output_from_history(&hist, cid, tag);
                    return Poll::Ready(AggregateOutput::Select {
                        winner_index: win,
                        output: out,
                    });
                }
            }
            AggregateMode::Join => {
                if present.len() == this.meta.len() {
                    let mut outs: Vec<(usize, DurableOutput)> = Vec::with_capacity(this.meta.len());
                    for (i, idx) in present {
                        let (cid, tag) = this.meta[i];
                        let out = OrchestrationContext::synth_output_from_history(&hist, cid, tag);
                        outs.push((idx, out));
                    }
                    outs.sort_by_key(|(idx, _)| *idx);
                    let res = outs.into_iter().map(|(_, o)| o).collect();
                    return Poll::Ready(AggregateOutput::Join { outputs: res });
                }
            }
        }
        Poll::Pending
    }
}

pub struct SelectFuture(pub(crate) AggregateDurableFuture);
impl Future for SelectFuture {
    type Output = (usize, DurableOutput);
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        match inner.poll(cx) {
            Poll::Ready(AggregateOutput::Select { winner_index, output }) => Poll::Ready((winner_index, output)),
            Poll::Ready(_) => unreachable!(),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct JoinFuture(pub(crate) AggregateDurableFuture);
impl Future for JoinFuture {
    type Output = Vec<DurableOutput>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        match inner.poll(cx) {
            Poll::Ready(AggregateOutput::Join { outputs }) => Poll::Ready(outputs),
            Poll::Ready(_) => unreachable!(),
            Poll::Pending => Poll::Pending,
        }
    }
}
