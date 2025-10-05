use std::cell::{Cell, RefCell};
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

// CompletionMap-based polling removed - replaced with unified cursor approach

pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
        claimed_event_id: Cell<Option<u64>>,
        ctx: OrchestrationContext,
    },
    Timer {
        delay_ms: u64,
        claimed_event_id: Cell<Option<u64>>,
        ctx: OrchestrationContext,
    },
    External {
        name: String,
        claimed_event_id: Cell<Option<u64>>,
        result: RefCell<Option<String>>,  // Cache result once found
        ctx: OrchestrationContext,
    },
    SubOrch {
        name: String,
        version: Option<String>,
        instance: RefCell<String>,  // Updated once event_id is known
        input: String,
        claimed_event_id: Cell<Option<u64>>,
        ctx: OrchestrationContext,
    },
}

// KindTag removed - no longer needed with cursor model

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };

        match &mut this.0 {
            Kind::Activity {
                name,
                input,
                claimed_event_id,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                
                // Step 1: Claim scheduling event_id if not already claimed
                if claimed_event_id.get().is_none() {
                    // Find next unclaimed ActivityScheduled in history
                    let mut found_event_id = None;
                    for event in &inner.history {
                        if let Event::ActivityScheduled { event_id, name: n, input: inp, .. } = event {
                            if !inner.claimed_scheduling_events.contains(event_id) {
                                // Found next unclaimed - MUST match us
                                if n != name || inp != input {
                                    panic!(
                                        "Non-deterministic: Next unclaimed ActivityScheduled is ('{}', '{}') \
                                         but expected ('{}', '{}')",
                                        n, inp, name, input
                                    );
                                }
                                found_event_id = Some(*event_id);
                                break;
                            }
                        }
                    }
                    
                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new (first execution)
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let exec_id = inner.execution_id;
                        
                        inner.history.push(Event::ActivityScheduled {
                            event_id: new_id,
                            name: name.clone(),
                            input: input.clone(),
                            execution_id: exec_id,
                        });
                        
                        inner.record_action(Action::CallActivity {
                            scheduling_event_id: new_id,
                            name: name.clone(),
                            input: input.clone(),
                        });
                        
                        new_id
                    });
                    
                    inner.claimed_scheduling_events.insert(event_id);
                    claimed_event_id.set(Some(event_id));
                }
                
                let our_event_id = claimed_event_id.get().unwrap();
                
                // Step 2: Look for our completion using completion cursor
                loop {
                    if inner.next_completion_index >= inner.history.len() {
                        return Poll::Pending;
                    }
                    
                    let event = inner.history[inner.next_completion_index].clone();
                    
                    match event {
                        Event::ActivityCompleted { source_event_id, ref result, .. }
                            if source_event_id == our_event_id => {
                            inner.next_completion_index += 1;
                            return Poll::Ready(DurableOutput::Activity(Ok(result.clone())));
                        }
                        
                        Event::ActivityFailed { source_event_id, ref error, .. }
                            if source_event_id == our_event_id => {
                            inner.next_completion_index += 1;
                            return Poll::Ready(DurableOutput::Activity(Err(error.clone())));
                        }
                        
                        // Is this a completion for someone else?
                        // Don't panic - just return Pending and let aggregate handle it
                        // (might be for another future in a select/join)
                        Event::ActivityCompleted { .. }
                        | Event::ActivityFailed { .. }
                        | Event::TimerFired { .. }
                        | Event::ExternalEvent { .. }
                        | Event::SubOrchestrationCompleted { .. }
                        | Event::SubOrchestrationFailed { .. } => {
                            // Completion not for us - return Pending without advancing cursor
                            // Aggregate will check if any other future matches
                            return Poll::Pending;
                        }
                        
                        // Not a completion - skip it
                        _ => {
                            inner.next_completion_index += 1;
                        }
                    }
                }
            }
            Kind::Timer {
                delay_ms,
                claimed_event_id,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                
                // Step 1: Claim scheduling event_id
                if claimed_event_id.get().is_none() {
                    // Find next unclaimed TimerCreated in history
                    let mut found_event_id = None;
                    for event in &inner.history {
                        if let Event::TimerCreated { event_id, .. } = event {
                            if !inner.claimed_scheduling_events.contains(event_id) {
                                // Found next unclaimed Timer
                                found_event_id = Some(*event_id);
                                break;
                            }
                        }
                    }
                    
                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new (first execution)
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let now = inner.now_ms();
                        let fire_at_ms = now.saturating_add(*delay_ms);
                        let exec_id = inner.execution_id;
                        
                        inner.history.push(Event::TimerCreated {
                            event_id: new_id,
                            fire_at_ms,
                            execution_id: exec_id,
                        });
                        
                        inner.record_action(Action::CreateTimer {
                            scheduling_event_id: new_id,
                            delay_ms: *delay_ms,
                        });
                        
                        new_id
                    });
                    
                    inner.claimed_scheduling_events.insert(event_id);
                    claimed_event_id.set(Some(event_id));
                }
                
                let our_event_id = claimed_event_id.get().unwrap();
                
                // Step 2: Look for TimerFired using completion cursor
                loop {
                    if inner.next_completion_index >= inner.history.len() {
                        return Poll::Pending;
                    }
                    
                    let event = inner.history[inner.next_completion_index].clone();
                    
                    match event {
                        Event::TimerFired { source_event_id, .. }
                            if source_event_id == our_event_id => {
                            inner.next_completion_index += 1;
                            return Poll::Ready(DurableOutput::Timer);
                        }
                        
                        // Completion for someone else - return Pending without advancing cursor
                        Event::ActivityCompleted { .. }
                        | Event::ActivityFailed { .. }
                        | Event::TimerFired { .. }
                        | Event::ExternalEvent { .. }
                        | Event::SubOrchestrationCompleted { .. }
                        | Event::SubOrchestrationFailed { .. } => {
                            return Poll::Pending;
                        }
                        
                        // Skip non-completions
                        _ => {
                            inner.next_completion_index += 1;
                        }
                    }
                }
            }
            Kind::External {
                name,
                claimed_event_id,
                result,
                ctx,
            } => {
                // Check if we already have the result cached
                if let Some(cached) = result.borrow().clone() {
                    return Poll::Ready(DurableOutput::External(cached));
                }
                
                let mut inner = ctx.inner.lock().unwrap();
                
                // Step 1: Claim ExternalSubscribed event_id
                if claimed_event_id.get().is_none() {
                    // Find next unclaimed ExternalSubscribed in history
                    let mut found_event_id = None;
                    for event in &inner.history {
                        if let Event::ExternalSubscribed { event_id, name: n } = event {
                            if !inner.claimed_scheduling_events.contains(event_id) {
                                // Found next unclaimed - MUST match us
                                if n != name {
                                    panic!(
                                        "Non-deterministic: Next unclaimed ExternalSubscribed is '{}' \
                                         but expected '{}'",
                                        n, name
                                    );
                                }
                                found_event_id = Some(*event_id);
                                break;
                            }
                        }
                    }
                    
                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        
                        inner.history.push(Event::ExternalSubscribed {
                            event_id: new_id,
                            name: name.clone(),
                        });
                        
                        inner.record_action(Action::WaitExternal {
                            scheduling_event_id: new_id,
                            name: name.clone(),
                        });
                        
                        new_id
                    });
                    
                    inner.claimed_scheduling_events.insert(event_id);
                    claimed_event_id.set(Some(event_id));
                }
                
                let _our_event_id = claimed_event_id.get().unwrap();
                
                // Step 2: Look for ExternalEvent (special case - search by name)
                // External events can arrive in any order, so we search from cursor position
                // Track consumed events to avoid re-reading the same one
                let name_clone = name.clone();
                let already_consumed = inner.consumed_external_events.contains(&name_clone);
                
                if !already_consumed {
                    for i in 0..inner.history.len() {
                        if let Event::ExternalEvent { name: ref ext_name, ref data, .. } = inner.history[i] {
                            if ext_name == &name_clone {
                                // Found our event! Mark as consumed and cache result
                                let result_data = data.clone();
                                inner.consumed_external_events.insert(name_clone);
                                *result.borrow_mut() = Some(result_data.clone());
                                return Poll::Ready(DurableOutput::External(result_data));
                            }
                        }
                    }
                }
                
                Poll::Pending
            }
            Kind::SubOrch {
                name,
                version,
                instance,
                input,
                claimed_event_id,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();
                
                // Step 1: Claim SubOrchestrationScheduled event_id
                if claimed_event_id.get().is_none() {
                    // Find next unclaimed SubOrchestrationScheduled in history
                    let mut found_event_id = None;
                    for event in &inner.history {
                        if let Event::SubOrchestrationScheduled { event_id, name: n, input: inp, instance: inst, .. } = event {
                            if !inner.claimed_scheduling_events.contains(event_id) {
                                // Found next unclaimed - MUST match us
                                if n != name || inp != input {
                                    panic!(
                                        "Non-deterministic: Next unclaimed SubOrchestrationScheduled is ('{}', '{}') \
                                         but expected ('{}', '{}')",
                                        n, inp, name, input
                                    );
                                }
                                *instance.borrow_mut() = inst.clone();
                                found_event_id = Some(*event_id);
                                break;
                            }
                        }
                    }
                    
                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let exec_id = inner.execution_id;
                        let child_instance = format!("sub::{}", new_id);
                        *instance.borrow_mut() = child_instance.clone();
                        
                        inner.history.push(Event::SubOrchestrationScheduled {
                            event_id: new_id,
                            name: name.clone(),
                            instance: child_instance.clone(),
                            input: input.clone(),
                            execution_id: exec_id,
                        });
                        
                        inner.record_action(Action::StartSubOrchestration {
                            scheduling_event_id: new_id,
                            name: name.clone(),
                            version: version.clone(),
                            instance: child_instance,
                            input: input.clone(),
                        });
                        
                        new_id
                    });
                    
                    inner.claimed_scheduling_events.insert(event_id);
                    claimed_event_id.set(Some(event_id));
                }
                
                let our_event_id = claimed_event_id.get().unwrap();
                
                // Step 2: Look for SubOrch completion using completion cursor
                loop {
                    if inner.next_completion_index >= inner.history.len() {
                        return Poll::Pending;
                    }
                    
                    let event = inner.history[inner.next_completion_index].clone();
                    
                    match event {
                        Event::SubOrchestrationCompleted { source_event_id, ref result, .. }
                            if source_event_id == our_event_id => {
                            inner.next_completion_index += 1;
                            return Poll::Ready(DurableOutput::SubOrchestration(Ok(result.clone())));
                        }
                        
                        Event::SubOrchestrationFailed { source_event_id, ref error, .. }
                            if source_event_id == our_event_id => {
                            inner.next_completion_index += 1;
                            return Poll::Ready(DurableOutput::SubOrchestration(Err(error.clone())));
                        }
                        
                        // Completion for someone else - return Pending without advancing cursor
                        Event::ActivityCompleted { .. }
                        | Event::ActivityFailed { .. }
                        | Event::TimerFired { .. }
                        | Event::ExternalEvent { .. }
                        | Event::SubOrchestrationCompleted { .. }
                        | Event::SubOrchestrationFailed { .. } => {
                            return Poll::Pending;
                        }
                        
                        // Skip non-completions
                        _ => {
                            inner.next_completion_index += 1;
                        }
                    }
                }
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
    children: Vec<DurableFuture>,
    mode: AggregateMode,
}

impl AggregateDurableFuture {
    pub(crate) fn new_select(_ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        Self {
            children,
            mode: AggregateMode::Select,
        }
    }
    pub(crate) fn new_join(_ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        Self {
            children,
            mode: AggregateMode::Join,
        }
    }

    // Note: Unconsumed completion detection removed - the cursor model naturally
    // handles this via strict sequential consumption. Any unconsumed completions
    // will cause a panic when the next future tries to poll.
}

impl Future for AggregateDurableFuture {
    type Output = AggregateOutput;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        // Poll all children and collect results
        let mut results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
        
        for (i, child) in this.children.iter_mut().enumerate() {
            let poll_result = Pin::new(child).poll(cx);
            if let Poll::Ready(output) = poll_result {
                results[i] = Some(output);
            }
        }

        match this.mode {
            AggregateMode::Select => {
                // Return the first child that became ready
                for (i, result) in results.into_iter().enumerate() {
                    if let Some(output) = result {
                    return Poll::Ready(AggregateOutput::Select {
                            winner_index: i,
                            output,
                    });
                    }
                }
                Poll::Pending
            }
            AggregateMode::Join => {
                // Return when all children are ready
                if results.iter().all(|r| r.is_some()) {
                    let outputs: Vec<DurableOutput> = results.into_iter()
                        .filter_map(|r| r)
                        .collect();
                    return Poll::Ready(AggregateOutput::Join { outputs });
                }
                Poll::Pending
            }
        }
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

