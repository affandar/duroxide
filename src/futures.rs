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

impl DurableFuture {
    /// Try to poll using completion map (deterministic execution)
    fn try_completion_map_poll(&self) -> Option<Poll<DurableOutput>> {
        // Access the shared thread-local completion map accessor
        crate::runtime::completion_aware_futures::with_completion_map_accessor(|accessor_opt| {
            if let Some(accessor) = accessor_opt {
                // Extract completion info from this future
                let (kind, correlation_id) = match &self.0 {
                    Kind::Activity { id, .. } => (crate::runtime::completion_map::CompletionKind::Activity, *id),
                    Kind::Timer { id, .. } => (crate::runtime::completion_map::CompletionKind::Timer, *id),
                    Kind::External { id, .. } => (crate::runtime::completion_map::CompletionKind::External, *id),
                    Kind::SubOrch { id, .. } => (crate::runtime::completion_map::CompletionKind::SubOrchestration, *id),
                };

                // Check completion map for deterministic ordering

                // Check if this completion is ready in the ordered map
                if accessor.is_completion_ready(kind, correlation_id) {
                    if let Some(completion_value) = accessor.get_completion(kind, correlation_id) {
                        // Found ready completion - consume it and add to history

                        // IMPORTANT: Add corresponding history event when consuming completion
                        // This ensures history consistency
                        let history_event = match &completion_value {
                            crate::runtime::completion_map::CompletionValue::ActivityResult(result) => match result {
                                Ok(result) => Some(crate::Event::ActivityCompleted {
                                    id: correlation_id,
                                    result: result.clone(),
                                }),
                                Err(error) => Some(crate::Event::ActivityFailed {
                                    id: correlation_id,
                                    error: error.clone(),
                                }),
                            },
                            crate::runtime::completion_map::CompletionValue::TimerFired { fire_at_ms } => {
                                Some(crate::Event::TimerFired {
                                    id: correlation_id,
                                    fire_at_ms: *fire_at_ms,
                                })
                            }
                            crate::runtime::completion_map::CompletionValue::ExternalData { data, name } => {
                                Some(crate::Event::ExternalEvent {
                                    id: correlation_id,
                                    name: name.clone(),
                                    data: data.clone(),
                                })
                            }
                            crate::runtime::completion_map::CompletionValue::SubOrchResult(result) => match result {
                                Ok(result) => Some(crate::Event::SubOrchestrationCompleted {
                                    id: correlation_id,
                                    result: result.clone(),
                                }),
                                Err(error) => Some(crate::Event::SubOrchestrationFailed {
                                    id: correlation_id,
                                    error: error.clone(),
                                }),
                            },
                            crate::runtime::completion_map::CompletionValue::CancelReason(_) => {
                                // Cancel is handled differently, fall back to normal polling
                                return None;
                            }
                        };

                        // Add the history event to the context (we need access to ctx for this)
                        // For now, let's see if we can access the context through the future
                        if let Some(event) = history_event {
                            // We need to add this event to the orchestration context's history
                            // The context is available in the future's Kind enum
                            let ctx = match &self.0 {
                                Kind::Activity { ctx, .. } => Some(ctx),
                                Kind::Timer { ctx, .. } => Some(ctx),
                                Kind::External { ctx, .. } => Some(ctx),
                                Kind::SubOrch { ctx, .. } => Some(ctx),
                            };

                            if let Some(context) = ctx {
                                // Add completion event to history for consistency
                                context.inner.lock().unwrap().history.push(event);
                            }
                        }

                        let output = match completion_value {
                            crate::runtime::completion_map::CompletionValue::ActivityResult(result) => {
                                DurableOutput::Activity(result)
                            }
                            crate::runtime::completion_map::CompletionValue::TimerFired { .. } => DurableOutput::Timer,
                            crate::runtime::completion_map::CompletionValue::ExternalData { data, .. } => {
                                DurableOutput::External(data)
                            }
                            crate::runtime::completion_map::CompletionValue::SubOrchResult(result) => {
                                DurableOutput::SubOrchestration(result)
                            }
                            crate::runtime::completion_map::CompletionValue::CancelReason(_) => {
                                // Cancel is handled differently, fall back to normal polling
                                return None;
                            }
                        };
                        return Some(Poll::Ready(output));
                    }
                }

                // Completion not ready yet - fall back to original logic for scheduling

                // IMPORTANT: We still need to handle scheduling even when using completion map!
                // The completion map only handles *completed* operations, but we still need to
                // schedule operations that haven't been scheduled yet.
                // Fall back to original logic for scheduling side effects, but return early if already scheduled.
                None // This will cause fall-through to original logic which handles scheduling
            } else {
                // No completion map accessor available, fall back to original logic
                None
            }
        })
    }
}

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
#[derive(Clone, Copy, Debug)]
pub(crate) enum KindTag {
    Activity,
    Timer,
    External,
    SubOrch,
}

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // First try completion map approach if available
        if let Some(result) = self.as_ref().try_completion_map_poll() {
            return result;
        }

        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };

        // Fall back to history-based approach
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
                    let exec_id = inner.execution_id;
                    inner.history.push(Event::ActivityScheduled {
                        id: *id,
                        name: name.clone(),
                        input: input.clone(),
                        execution_id: exec_id,
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
                    let exec_id = inner.execution_id;
                    inner.history.push(Event::TimerCreated { id: *id, fire_at_ms, execution_id: exec_id });
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
                    let exec_id = inner.execution_id;
                    inner.history.push(Event::SubOrchestrationScheduled {
                        id: *id,
                        name: name.clone(),
                        instance: instance.clone(),
                        input: input.clone(),
                        execution_id: exec_id,
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

    /// Check if there are any available completions that weren't consumed by our children
    fn check_for_unconsumed_completions(&self) -> Option<Vec<(crate::runtime::completion_map::CompletionKind, u64)>> {
        // Access the completion map via the thread-local accessor
        let unconsumed = crate::runtime::completion_aware_futures::with_completion_map_accessor(|accessor_opt| {
            let mut unconsumed = Vec::new();

            if let Some(accessor) = accessor_opt {
                // Check each of our children's expected completions
                for (cid, tag) in &self.meta {
                    let kind = match tag {
                        KindTag::Activity => crate::runtime::completion_map::CompletionKind::Activity,
                        KindTag::Timer => crate::runtime::completion_map::CompletionKind::Timer,
                        KindTag::External => crate::runtime::completion_map::CompletionKind::External,
                        KindTag::SubOrch => crate::runtime::completion_map::CompletionKind::SubOrchestration,
                    };

                    // Check if this completion is available but not consumed
                    if accessor.is_completion_ready(kind, *cid) {
                        unconsumed.push((kind, *cid));
                    }
                }
            }

            unconsumed
        });

        if unconsumed.is_empty() { None } else { Some(unconsumed) }
    }
}

impl Future for AggregateDurableFuture {
    type Output = AggregateOutput;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        // Continue polling children until no more progress is made
        // Track which children became ready in previous rounds
        let mut children_ready_status = vec![false; this.children.len()];
        let mut round = 0;
        const MAX_POLLING_ROUNDS: usize = 10; // Safety limit

        loop {
            let mut any_new_ready = false;
            round += 1;

            if round > MAX_POLLING_ROUNDS {
                // Emergency brake to prevent infinite loops
                break;
            }

            for (i, f) in this.children.iter_mut().enumerate() {
                let poll_result = Pin::new(f).poll(cx);
                let is_ready = poll_result.is_ready();

                // Only count as "new progress" if this child wasn't ready before
                if is_ready && !children_ready_status[i] {
                    any_new_ready = true;
                    children_ready_status[i] = true;
                }
            }

            // If no child became newly ready this round, break out of polling loop
            if !any_new_ready {
                break;
            }
        }

        // Check for unconsumed completions (non-deterministic execution error)
        if let Some(unconsumed) = this.check_for_unconsumed_completions() {
            panic!("non-deterministic execution: unconsumed completions: {:?}", unconsumed);
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
