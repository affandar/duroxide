use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::{Action, Event, EventKind, OrchestrationContext};

#[derive(Debug, Clone)]
pub enum DurableOutput {
    Activity(Result<String, String>),
    Timer,
    External(String),
    SubOrchestration(Result<String, String>),
}

/// A durable future representing an asynchronous operation in an orchestration.
///
/// Common fields `claimed_event_id` and `ctx` are stored directly on the struct,
/// while operation-specific data is stored in the `kind` variant.
pub struct DurableFuture {
    /// The event ID claimed by this future during scheduling (set on first poll)
    pub(crate) claimed_event_id: Cell<Option<u64>>,
    /// Reference to the orchestration context
    pub(crate) ctx: OrchestrationContext,
    /// Operation-specific data
    pub(crate) kind: Kind,
}

pub(crate) enum Kind {
    Activity {
        name: String,
        input: String,
    },
    Timer {
        delay_ms: u64,
    },
    External {
        name: String,
        result: RefCell<Option<String>>, // Cache result once found
    },
    SubOrch {
        name: String,
        version: Option<String>,
        instance: RefCell<String>, // Updated once event_id is known
        input: String,
    },
    System {
        op: String,
        value: RefCell<Option<String>>,
    },
}

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };

        // Common fields: this.claimed_event_id, this.ctx
        match &mut this.kind {
            Kind::Activity { name, input } => {
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // Step 1: Claim scheduling event_id if not already claimed
                if this.claimed_event_id.get().is_none() {
                    let mut found_event_id = None;
                    let start_idx = inner.scheduling_scan_cursor;
                    
                    for (idx, event) in inner.history.iter().enumerate().skip(start_idx) {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::ActivityScheduled { name: n, input: inp } => {
                                if n != name || inp != input {
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is ActivityScheduled('{n}','{inp}') but expected ActivityScheduled('{name}','{input}')"
                                    ));
                                    return Poll::Pending;
                                }
                                found_event_id = Some(eid);
                                inner.scheduling_scan_cursor = idx + 1;
                                break;
                            }
                            EventKind::TimerCreated { .. } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is TimerCreated but expected ActivityScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::ExternalSubscribed { name: en } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{en}') but expected ActivityScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::SubOrchestrationScheduled { name: sn, input: sin, .. } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{sn}','{sin}') but expected ActivityScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let exec_id = inner.execution_id;
                        let inst_id = inner.instance_id.clone();

                        inner.history.push(Event::with_event_id(
                            new_id,
                            inst_id,
                            exec_id,
                            None,
                            EventKind::ActivityScheduled {
                                name: name.clone(),
                                input: input.clone(),
                            },
                        ));

                        inner.record_action(Action::CallActivity {
                            scheduling_event_id: new_id,
                            name: name.clone(),
                            input: input.clone(),
                        });
                        
                        inner.scheduling_scan_cursor = inner.history.len();
                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                let our_event_id = this.claimed_event_id.get().expect("claimed_event_id set");

                // Step 2: Look for completion using index
                if let Some(&completion_idx) = inner.completion_by_source.get(&our_event_id) {
                    let completion_id = inner.history[completion_idx].event_id;
                    
                    // FIFO Check
                    let mut can_consume = true;
                    for &unconsumed_id in inner.unconsumed_completions.range(..completion_id) {
                        let idx = inner.event_id_to_index[&unconsumed_id];
                        let evt = &inner.history[idx];
                        if let Some(sid) = evt.source_event_id {
                            if inner.cancelled_source_ids.contains(&sid) {
                                continue;
                            }
                        }
                        can_consume = false;
                        break;
                    }
                    
                    if can_consume {
                        inner.unconsumed_completions.remove(&completion_id);
                        inner.consumed_completions.insert(completion_id); 
                        
                        // Re-access event after mutation
                        let completion_event = &inner.history[completion_idx];
                        match &completion_event.kind {
                            EventKind::ActivityCompleted { result } => return Poll::Ready(DurableOutput::Activity(Ok(result.clone()))),
                            EventKind::ActivityFailed { details } => {
                                debug_assert!(
                                    matches!(details, crate::ErrorDetails::Application { .. }),
                                    "INVARIANT: Only Application errors should reach orchestration code"
                                );
                                return Poll::Ready(DurableOutput::Activity(Err(details.display_message())));
                            }
                            _ => unreachable!(),
                        }
                    }
                }

                Poll::Pending
            }
            Kind::Timer { delay_ms } => {
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                if this.claimed_event_id.get().is_none() {
                    let mut found_event_id = None;
                    let start_idx = inner.scheduling_scan_cursor;
                    
                    for (idx, event) in inner.history.iter().enumerate().skip(start_idx) {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::TimerCreated { .. } => {
                                found_event_id = Some(eid);
                                inner.scheduling_scan_cursor = idx + 1;
                                break;
                            }
                            EventKind::ActivityScheduled { name: n, input: inp } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{n}','{inp}') but expected TimerCreated"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::ExternalSubscribed { name: en } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{en}') but expected TimerCreated"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::SubOrchestrationScheduled { name: sn, input: sin, .. } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{sn}','{sin}') but expected TimerCreated"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let now = inner.now_ms();
                        let fire_at_ms = now.saturating_add(*delay_ms);
                        let exec_id = inner.execution_id;
                        let inst_id = inner.instance_id.clone();

                        inner.history.push(Event::with_event_id(
                            new_id,
                            inst_id,
                            exec_id,
                            None,
                            EventKind::TimerCreated { fire_at_ms },
                        ));

                        inner.record_action(Action::CreateTimer {
                            scheduling_event_id: new_id,
                            fire_at_ms,
                        });
                        
                        inner.scheduling_scan_cursor = inner.history.len();
                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                let our_event_id = this.claimed_event_id.get().expect("claimed_event_id set");

                if let Some(&completion_idx) = inner.completion_by_source.get(&our_event_id) {
                    let completion_id = inner.history[completion_idx].event_id;
                    
                    let mut can_consume = true;
                    for &unconsumed_id in inner.unconsumed_completions.range(..completion_id) {
                        let idx = inner.event_id_to_index[&unconsumed_id];
                        let evt = &inner.history[idx];
                        if let Some(sid) = evt.source_event_id {
                            if inner.cancelled_source_ids.contains(&sid) {
                                continue;
                            }
                        }
                        can_consume = false;
                        break;
                    }
                    
                    if can_consume {
                        inner.unconsumed_completions.remove(&completion_id);
                        inner.consumed_completions.insert(completion_id);
                        return Poll::Ready(DurableOutput::Timer);
                    }
                }

                Poll::Pending
            }
            Kind::External { name, result } => {
                if let Some(cached) = result.borrow().clone() {
                    return Poll::Ready(DurableOutput::External(cached));
                }

                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                if this.claimed_event_id.get().is_none() {
                    let mut found_event_id = None;
                    let start_idx = inner.scheduling_scan_cursor;
                    
                    for (idx, event) in inner.history.iter().enumerate().skip(start_idx) {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::ExternalSubscribed { name: n } => {
                                if n != name {
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{n}') but expected ExternalSubscribed('{name}')"
                                    ));
                                    return Poll::Pending;
                                }
                                found_event_id = Some(eid);
                                inner.scheduling_scan_cursor = idx + 1;
                                break;
                            }
                            EventKind::ActivityScheduled { name: an, input: ainp } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{an}','{ainp}') but expected ExternalSubscribed('{name}')"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::TimerCreated { .. } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is TimerCreated but expected ExternalSubscribed('{name}')"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::SubOrchestrationScheduled { name: sn, input: sin, .. } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{sn}','{sin}') but expected ExternalSubscribed('{name}')"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let exec_id = inner.execution_id;
                        let inst_id = inner.instance_id.clone();

                        inner.history.push(Event::with_event_id(
                            new_id,
                            inst_id,
                            exec_id,
                            None,
                            EventKind::ExternalSubscribed { name: name.clone() },
                        ));

                        inner.record_action(Action::WaitExternal {
                            scheduling_event_id: new_id,
                            name: name.clone(),
                        });
                        
                        inner.scheduling_scan_cursor = inner.history.len();
                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                // Look for ExternalEvent
                // Optimization: could index external events by name, but linear scan of history is still needed if not indexed.
                // Since we rely on index optimization elsewhere, let's keep linear scan here but optimized by skipping if already consumed.
                // Or better: Iterate unconsumed_completions and check if they match?
                // unconsumed_completions contains event_ids. We can check them.
                
                if !inner.consumed_external_events.contains(name) {
                    let mut found_match = None;
                    // Scan unconsumed completions to find the FIRST matching external event
                    for &eid in &inner.unconsumed_completions {
                        let idx = inner.event_id_to_index[&eid];
                        let event = &inner.history[idx];
                        if let EventKind::ExternalEvent { name: ext_name, data } = &event.kind {
                            if ext_name == name {
                                found_match = Some((eid, data.clone()));
                                break; // Found first match in history order
                            }
                        }
                    }

                    if let Some((event_id, data)) = found_match {
                        // Check FIFO - are there any unconsumed events BEFORE this one?
                        // Since we iterated unconsumed_completions in order, and this is the first match,
                        // we only need to check if there are any *other* unconsumed events before it that are NOT cancelled.
                        
                        let mut can_consume = true;
                        for &unconsumed_id in inner.unconsumed_completions.range(..event_id) {
                            let idx = inner.event_id_to_index[&unconsumed_id];
                            let evt = &inner.history[idx];
                            if let Some(sid) = evt.source_event_id {
                                if inner.cancelled_source_ids.contains(&sid) {
                                    continue;
                                }
                            }
                            // External events themselves cannot be cancelled via select2
                            can_consume = false;
                            break;
                        }

                        if can_consume {
                            inner.unconsumed_completions.remove(&event_id);
                            inner.consumed_completions.insert(event_id);
                            inner.consumed_external_events.insert(name.clone());
                            *result.borrow_mut() = Some(data.clone());
                            return Poll::Ready(DurableOutput::External(data));
                        }
                    }
                }

                Poll::Pending
            }
            Kind::SubOrch { name, version, instance, input } => {
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                if this.claimed_event_id.get().is_none() {
                    let mut found_event_id = None;
                    let start_idx = inner.scheduling_scan_cursor;
                    
                    for (idx, event) in inner.history.iter().enumerate().skip(start_idx) {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::SubOrchestrationScheduled { name: n, input: inp, instance: inst, .. } => {
                                if n != name || inp != input {
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{n}','{inp}') but expected SubOrchestrationScheduled('{name}','{input}')"
                                    ));
                                    return Poll::Pending;
                                }
                                *instance.borrow_mut() = inst.clone();
                                found_event_id = Some(eid);
                                inner.scheduling_scan_cursor = idx + 1;
                                break;
                            }
                            EventKind::ActivityScheduled { name: an, input: ainp } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{an}','{ainp}') but expected SubOrchestrationScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::TimerCreated { .. } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is TimerCreated but expected SubOrchestrationScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            EventKind::ExternalSubscribed { name: en } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{en}') but expected SubOrchestrationScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        let new_id = inner.next_event_id;
                        inner.next_event_id += 1;
                        let exec_id = inner.execution_id;
                        let inst_id = inner.instance_id.clone();
                        let child_instance = format!("sub::{new_id}");
                        *instance.borrow_mut() = child_instance.clone();

                        inner.history.push(Event::with_event_id(
                            new_id,
                            inst_id,
                            exec_id,
                            None,
                            EventKind::SubOrchestrationScheduled {
                                name: name.clone(),
                                instance: child_instance.clone(),
                                input: input.clone(),
                            },
                        ));

                        inner.record_action(Action::StartSubOrchestration {
                            scheduling_event_id: new_id,
                            name: name.clone(),
                            version: version.clone(),
                            instance: child_instance,
                            input: input.clone(),
                        });
                        
                        inner.scheduling_scan_cursor = inner.history.len();
                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                let our_event_id = this.claimed_event_id.get().expect("claimed_event_id set");

                if let Some(&completion_idx) = inner.completion_by_source.get(&our_event_id) {
                    let completion_id = inner.history[completion_idx].event_id;
                    
                    let mut can_consume = true;
                    for &unconsumed_id in inner.unconsumed_completions.range(..completion_id) {
                        let idx = inner.event_id_to_index[&unconsumed_id];
                        let evt = &inner.history[idx];
                        if let Some(sid) = evt.source_event_id {
                            if inner.cancelled_source_ids.contains(&sid) {
                                continue;
                            }
                        }
                        can_consume = false;
                        break;
                    }
                    
                    if can_consume {
                        inner.unconsumed_completions.remove(&completion_id);
                        inner.consumed_completions.insert(completion_id);
                        
                        // Re-access event after mutation
                        let completion_event = &inner.history[completion_idx];
                        match &completion_event.kind {
                            EventKind::SubOrchestrationCompleted { result } => return Poll::Ready(DurableOutput::SubOrchestration(Ok(result.clone()))),
                            EventKind::SubOrchestrationFailed { details } => {
                                debug_assert!(
                                    matches!(details, crate::ErrorDetails::Application { .. }),
                                    "INVARIANT: Only Application errors should reach orchestration code"
                                );
                                return Poll::Ready(DurableOutput::SubOrchestration(Err(details.display_message())));
                            }
                            _ => unreachable!(),
                        }
                    }
                }

                Poll::Pending
            }
            Kind::System { op, value } => {
                if let Some(v) = value.borrow().clone() {
                    return Poll::Ready(DurableOutput::Activity(Ok(v)));
                }

                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                if this.claimed_event_id.get().is_none() {
                    let mut found_event_id = None;
                    // System calls are history events too, should scan from cursor
                    let start_idx = inner.scheduling_scan_cursor;
                    
                    for (idx, event) in inner.history.iter().enumerate().skip(start_idx) {
                        if let EventKind::SystemCall { op: hist_op, value: hist_value } = &event.kind {
                            // Check if this is ours and unclaimed
                            if hist_op == op && !inner.claimed_scheduling_events.contains(&event.event_id) {
                                found_event_id = Some((event.event_id, hist_value.clone()));
                                inner.scheduling_scan_cursor = idx + 1; // Adopted event, advance cursor
                                break;
                            }
                        }
                    }

                    if let Some((found_event_id, found_value)) = found_event_id {
                        inner.claimed_scheduling_events.insert(found_event_id);
                        this.claimed_event_id.set(Some(found_event_id));
                        *value.borrow_mut() = Some(found_value.clone());
                        return Poll::Ready(DurableOutput::Activity(Ok(found_value)));
                    }
                }

                // If not found in history, create new (synchronous execution)
                if this.claimed_event_id.get().is_none() {
                    let computed_value = match op.as_str() {
                        crate::SYSCALL_OP_GUID => generate_guid(),
                        crate::SYSCALL_OP_UTCNOW_MS => inner.now_ms().to_string(),
                        s if s.starts_with(crate::SYSCALL_OP_TRACE_PREFIX) => {
                            // ... log logic ...
                            let parts: Vec<&str> = s.splitn(3, ':').collect();
                            if parts.len() == 3 {
                                let level = parts[1];
                                let message = parts[2];
                                let instance_id = &inner.instance_id;
                                let execution_id = inner.execution_id;
                                let orch_name = inner.orchestration_name.as_deref().unwrap_or("unknown");
                                let orch_version = inner.orchestration_version.as_deref().unwrap_or("unknown");
                                let worker_id = inner.worker_id.as_deref().unwrap_or("unknown");

                                match level {
                                    "ERROR" => tracing::error!(
                                        target: "duroxide::orchestration",
                                        instance_id = %instance_id,
                                        execution_id = %execution_id,
                                        orchestration_name = %orch_name,
                                        orchestration_version = %orch_version,
                                        worker_id = %worker_id,
                                        "{}", message
                                    ),
                                    "WARN" => tracing::warn!(
                                        target: "duroxide::orchestration",
                                        instance_id = %instance_id,
                                        execution_id = %execution_id,
                                        orchestration_name = %orch_name,
                                        orchestration_version = %orch_version,
                                        worker_id = %worker_id,
                                        "{}", message
                                    ),
                                    "DEBUG" => tracing::debug!(
                                        target: "duroxide::orchestration",
                                        instance_id = %instance_id,
                                        execution_id = %execution_id,
                                        orchestration_name = %orch_name,
                                        orchestration_version = %orch_version,
                                        worker_id = %worker_id,
                                        "{}", message
                                    ),
                                    _ => tracing::info!(
                                        target: "duroxide::orchestration",
                                        instance_id = %instance_id,
                                        execution_id = %execution_id,
                                        orchestration_name = %orch_name,
                                        orchestration_version = %orch_version,
                                        worker_id = %worker_id,
                                        "{}", message
                                    ),
                                }
                            }
                            String::new()
                        }
                        _ => {
                            inner.nondeterminism_error = Some(format!("unknown system operation: {op}"));
                            return Poll::Pending;
                        }
                    };

                    let event_id = inner.next_event_id;
                    inner.next_event_id += 1;
                    let exec_id = inner.execution_id;
                    let inst_id = inner.instance_id.clone();

                    inner.history.push(Event::with_event_id(
                        event_id,
                        inst_id,
                        exec_id,
                        None,
                        EventKind::SystemCall {
                            op: op.clone(),
                            value: computed_value.clone(),
                        },
                    ));

                    inner.record_action(Action::SystemCall {
                        scheduling_event_id: event_id,
                        op: op.clone(),
                        value: computed_value.clone(),
                    });
                    
                    inner.scheduling_scan_cursor = inner.history.len();

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                    *value.borrow_mut() = Some(computed_value.clone());

                    return Poll::Ready(DurableOutput::Activity(Ok(computed_value)));
                }

                Poll::Pending
            }
        }
    }
}

#[allow(dead_code)]
const fn assert_unpin<T: Unpin>() {}
const _: () = {
    assert_unpin::<DurableFuture>();
};

fn generate_guid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    thread_local! {
        static COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let counter = COUNTER.with(|c| {
        let val = c.get();
        c.set(val.wrapping_add(1));
        val
    });

    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (timestamp >> 96) as u32,
        ((timestamp >> 80) & 0xFFFF) as u16,
        (counter & 0xFFFF) as u16,
        ((timestamp >> 64) & 0xFFFF) as u16,
        (timestamp & 0xFFFFFFFFFFFF) as u64
    )
}

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
    mode: AggregateMode,
}

impl AggregateDurableFuture {
    pub(crate) fn new_select(ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        Self {
            ctx,
            children,
            mode: AggregateMode::Select,
        }
    }
    pub(crate) fn new_join(ctx: OrchestrationContext, children: Vec<DurableFuture>) -> Self {
        Self {
            ctx,
            children,
            mode: AggregateMode::Join,
        }
    }
}

impl Future for AggregateDurableFuture {
    type Output = AggregateOutput;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        match this.mode {
            AggregateMode::Select => {
                let mut ready_results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
                for (i, child) in this.children.iter_mut().enumerate() {
                    if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                        ready_results[i] = Some(output);
                    }
                }

                let winner_index = ready_results.iter().position(|r| r.is_some());

                if let Some(winner_idx) = winner_index {
                    {
                        let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
                        for (i, child) in this.children.iter().enumerate() {
                            if i != winner_idx {
                                if let Some(source_id) = child.claimed_event_id.get() {
                                    inner.cancelled_source_ids.insert(source_id);
                                    if matches!(&child.kind, Kind::Activity { .. }) {
                                        inner.cancelled_activity_ids.insert(source_id);
                                    }
                                }
                            }
                        }
                    }

                    let output = ready_results[winner_idx]
                        .take()
                        .expect("winner_idx points to Ready result");
                    return Poll::Ready(AggregateOutput::Select {
                        winner_index: winner_idx,
                        output,
                    });
                }

                Poll::Pending
            }
            AggregateMode::Join => {
                let mut results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
                loop {
                    let mut made_progress = false;
                    for (i, child) in this.children.iter_mut().enumerate() {
                        if results[i].is_some() {
                            continue;
                        }
                        if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                            results[i] = Some(output);
                            made_progress = true;
                        }
                    }

                    if results.iter().all(|r| r.is_some()) {
                        let mut items: Vec<(u64, usize, DurableOutput)> = Vec::with_capacity(results.len());
                        for (i, out_opt) in results.into_iter().enumerate() {
                            let out = out_opt.expect("All results should be Some at this point");
                            let eid = {
                                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
                                let child = &this.children[i];
                                let sid = child.claimed_event_id.get().expect("child must claim id");
                                
                                match &child.kind {
                                    Kind::Activity { .. } 
                                    | Kind::Timer { .. }
                                    | Kind::SubOrch { .. } => {
                                        // Use index for O(1) lookup
                                        if let Some(&idx) = inner.completion_by_source.get(&sid) {
                                            inner.history[idx].event_id
                                        } else {
                                            u64::MAX // Should be unreachable if completed
                                        }
                                    }
                                    Kind::External { name, .. } => {
                                        // External events don't have source_event_id in history
                                        // But we consumed it, so it should be in consumed_completions
                                        // We can't easily find WHICH one without re-searching?
                                        // Actually, if it's ready, we found it in poll() and it's in history.
                                        // But DurableOutput doesn't carry event_id.
                                        // We need event_id to sort.
                                        // Re-find it?
                                        // Or we could have stored it in poll() but DurableFuture doesn't store completion id.
                                        
                                        // Fallback to searching unconsumed_completions is wrong because we just consumed it!
                                        // It is now in consumed_completions.
                                        // We search history for matching ExternalEvent that is in consumed_completions?
                                        // But there could be multiple.
                                        
                                        // OPTIMIZATION: DurableFuture could store completion_event_id once resolved.
                                        // But DurableFuture structure is fixed.
                                        // Let's use linear search for External events for now (they are rare).
                                        // Or rely on `consumed_completions`?
                                        
                                        // Existing code did a linear search finding FIRST match.
                                        let n = name.clone();
                                        inner.history.iter().find_map(|e| {
                                            if let EventKind::ExternalEvent { name: en, .. } = &e.kind
                                                && *en == n
                                                // && inner.consumed_completions.contains(&e.event_id) // We know we consumed it
                                                // Issue: how do we know WHICH one corresponds to THIS child?
                                                // Join consumes them in order.
                                                // The children are polled in order.
                                                // If we have 2 children waiting for same event name, they consume 2 events.
                                                // But here we just want to sort them.
                                                // It's ambiguous which child took which event if they are identical.
                                                // But sorting by event_id should be stable.
                                            {
                                                return Some(e.event_id);
                                            }
                                            None
                                        }).unwrap_or(u64::MAX)
                                    }
                                    Kind::System { .. } => sid,
                                }
                            };
                            items.push((eid, i, out));
                        }
                        items.sort_by_key(|(eid, _i, _)| *eid);
                        let outputs: Vec<DurableOutput> = items.into_iter().map(|(_, _, o)| o).collect();
                        return Poll::Ready(AggregateOutput::Join { outputs });
                    }

                    if !made_progress {
                        return Poll::Pending;
                    }
                }
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
