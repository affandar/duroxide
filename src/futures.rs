// Mutex poisoning is a critical error indicating a panic in another thread.
// In this module, all expect() calls are for mutex locks, which should panic on poison.
#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::{Action, Event, EventKind, OrchestrationContext, ReplayMode, SimplifiedResult};

/// Helper function to check if a completion event can be consumed according to FIFO ordering.
///
/// Returns `true` if all completion events in history that occurred before the given
/// `completion_event_id` have already been consumed (or are cancelled select2 losers).
///
/// # Arguments
/// * `history` - The full event history
/// * `consumed_completions` - Set of already consumed completion event IDs
/// * `cancelled_source_ids` - Set of source_event_ids whose completions should be auto-skipped (select2 losers)
/// * `completion_event_id` - The event ID we want to consume
fn can_consume_completion(
    history: &[Event],
    consumed_completions: &std::collections::HashSet<u64>,
    cancelled_source_ids: &std::collections::HashSet<u64>,
    completion_event_id: u64,
) -> bool {
    history.iter().all(|e| {
        match &e.kind {
            // Completions with source_event_id - check if cancelled
            EventKind::ActivityCompleted { .. }
            | EventKind::ActivityFailed { .. }
            | EventKind::TimerFired { .. }
            | EventKind::SubOrchestrationCompleted { .. }
            | EventKind::SubOrchestrationFailed { .. } => {
                // source_event_id is now on the Event struct
                if let Some(source_event_id) = e.source_event_id {
                    // Cancelled completions (select2 losers) don't block - skip them
                    if cancelled_source_ids.contains(&source_event_id) {
                        return true;
                    }
                }
                // Otherwise: if before ours, must be consumed
                e.event_id >= completion_event_id || consumed_completions.contains(&e.event_id)
            }
            // External events don't have source_event_id - can't be cancelled via select2
            EventKind::ExternalEvent { .. } => {
                e.event_id >= completion_event_id || consumed_completions.contains(&e.event_id)
            }
            _ => true, // Non-completions don't affect ordering
        }
    })
}

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
    /// The event ID claimed by this future during scheduling (set on first poll) - Legacy mode
    pub(crate) claimed_event_id: Cell<Option<u64>>,
    /// Token for simplified mode (set when action is emitted)
    pub(crate) simplified_token: Cell<Option<u64>>,
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

// KindTag removed - no longer needed with cursor model

impl Future for DurableFuture {
    type Output = DurableOutput;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
        let this = unsafe { self.get_unchecked_mut() };

        // Common fields: this.claimed_event_id, this.simplified_token, this.ctx
        match &mut this.kind {
            Kind::Activity { name, input } => {
                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === SIMPLIFIED MODE ===
                if inner.replay_mode == ReplayMode::Simplified {
                    // Token was set at schedule time - just check for result
                    let token = this.simplified_token.get()
                        .expect("simplified_token should be set at schedule time");
                    if let Some(result) = inner.get_result(token) {
                        return match result {
                            SimplifiedResult::ActivityOk(v) => {
                                Poll::Ready(DurableOutput::Activity(Ok(v.clone())))
                            }
                            SimplifiedResult::ActivityErr(e) => {
                                Poll::Ready(DurableOutput::Activity(Err(e.clone())))
                            }
                            _ => Poll::Pending, // Wrong completion type - should not happen
                        };
                    }
                    return Poll::Pending;
                }
                
                // Re-acquire as mutable for legacy mode
                drop(inner);
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === LEGACY MODE ===
                // Step 1: Claim scheduling event_id if not already claimed
                if this.claimed_event_id.get().is_none() {
                    // Find next unclaimed SCHEDULING event in history (global order enforcement)
                    let mut found_event_id = None;
                    for event in &inner.history {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::ActivityScheduled { name: n, input: inp } => {
                                // MUST be our schedule next
                                if n != name || inp != input {
                                    // Record nondeterminism gracefully
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is ActivityScheduled('{n}','{inp}') but expected ActivityScheduled('{name}','{input}')"
                                    ));
                                    return Poll::Pending;
                                }
                                found_event_id = Some(eid);
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
                            EventKind::SubOrchestrationScheduled {
                                name: sn, input: sin, ..
                            } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{sn}','{sin}') but expected ActivityScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new (first execution)
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

                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                // claimed_event_id is guaranteed to be Some after the above block sets it
                let our_event_id = this
                    .claimed_event_id
                    .get()
                    .expect("claimed_event_id should be set after claiming");

                // Step 2: Look for our completion - FIFO enforcement
                // Find our completion in history
                let our_completion = inner.history.iter().find_map(|e| {
                    // source_event_id is on the Event struct
                    if e.source_event_id != Some(our_event_id) {
                        return None;
                    }
                    match &e.kind {
                        EventKind::ActivityCompleted { result } => Some((e.event_id, Ok(result.clone()))),
                        EventKind::ActivityFailed { details } => {
                            debug_assert!(
                                matches!(details, crate::ErrorDetails::Application { .. }),
                                "INVARIANT: Only Application errors should reach orchestration code, got: {details:?}"
                            );
                            Some((e.event_id, Err(details.display_message())))
                        }
                        _ => None,
                    }
                });

                if let Some((completion_event_id, result)) = our_completion {
                    // Check: Are all completion events BEFORE ours consumed?
                    if can_consume_completion(
                        &inner.history,
                        &inner.consumed_completions,
                        &inner.cancelled_source_ids,
                        completion_event_id,
                    ) {
                        inner.consumed_completions.insert(completion_event_id);
                        return Poll::Ready(DurableOutput::Activity(result));
                    }
                }

                Poll::Pending
            }
            Kind::Timer { delay_ms } => {
                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === SIMPLIFIED MODE ===
                if inner.replay_mode == ReplayMode::Simplified {
                    // Token was set at schedule time - just check for result
                    let token = this.simplified_token.get()
                        .expect("simplified_token should be set at schedule time");
                    if let Some(result) = inner.get_result(token) {
                        return match result {
                            SimplifiedResult::TimerFired => Poll::Ready(DurableOutput::Timer),
                            _ => Poll::Pending, // Wrong completion type - should not happen
                        };
                    }
                    return Poll::Pending;
                }
                
                // Re-acquire as mutable for legacy mode
                drop(inner);
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
                // === LEGACY MODE ===
                // Step 1: Claim scheduling event_id
                if this.claimed_event_id.get().is_none() {
                    // Enforce global scheduling order
                    let mut found_event_id = None;
                    for event in &inner.history {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::TimerCreated { .. } => {
                                found_event_id = Some(eid);
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
                            EventKind::SubOrchestrationScheduled {
                                name: sn, input: sin, ..
                            } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{sn}','{sin}') but expected TimerCreated"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new (first execution)
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

                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                // claimed_event_id is guaranteed to be Some after the above block sets it
                let our_event_id = this
                    .claimed_event_id
                    .get()
                    .expect("claimed_event_id should be set after claiming");

                // Step 2: Look for TimerFired - FIFO enforcement
                let our_completion = inner.history.iter().find_map(|e| {
                    if matches!(&e.kind, EventKind::TimerFired { .. }) && e.source_event_id == Some(our_event_id) {
                        return Some(e.event_id);
                    }
                    None
                });

                if let Some(completion_event_id) = our_completion {
                    // Check: Are all completion events BEFORE ours consumed?
                    if can_consume_completion(
                        &inner.history,
                        &inner.consumed_completions,
                        &inner.cancelled_source_ids,
                        completion_event_id,
                    ) {
                        inner.consumed_completions.insert(completion_event_id);
                        return Poll::Ready(DurableOutput::Timer);
                    }
                }

                Poll::Pending
            }
            Kind::External { name, result } => {
                // Check if we already have the result cached (legacy mode only)
                if let Some(cached) = result.borrow().clone() {
                    return Poll::Ready(DurableOutput::External(cached));
                }

                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === SIMPLIFIED MODE ===
                if inner.replay_mode == ReplayMode::Simplified {
                    // Token was set at schedule time - just check for result
                    let token = this.simplified_token.get()
                        .expect("simplified_token should be set at schedule time");
                    
                    // Check for direct result delivery
                    if let Some(res) = inner.get_result(token) {
                        return match res {
                            SimplifiedResult::ExternalEvent(data) => {
                                Poll::Ready(DurableOutput::External(data.clone()))
                            }
                            _ => Poll::Pending,
                        };
                    }

                    // Also check external event by schedule_id (for subscription-based delivery)
                    if let Some(schedule_id) = inner.get_bound_schedule_id(token) {
                        if let Some(data) = inner.get_external_event(schedule_id) {
                            return Poll::Ready(DurableOutput::External(data.clone()));
                        }
                    }
                    return Poll::Pending;
                }
                
                // Re-acquire as mutable for legacy mode
                drop(inner);
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === LEGACY MODE ===
                // Step 1: Claim ExternalSubscribed event_id
                if this.claimed_event_id.get().is_none() {
                    // Enforce global scheduling order
                    let mut found_event_id = None;
                    for event in &inner.history {
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
                            EventKind::SubOrchestrationScheduled {
                                name: sn, input: sin, ..
                            } => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{sn}','{sin}') but expected ExternalSubscribed('{name}')"
                                ));
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }

                    let event_id = found_event_id.unwrap_or_else(|| {
                        // Not in history - create new
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

                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                // claimed_event_id is guaranteed to be Some after the above block sets it
                let _our_event_id = this
                    .claimed_event_id
                    .get()
                    .expect("claimed_event_id should be set after claiming");

                // Step 2: Look for ExternalEvent (special case - search by name)
                // External events can arrive in any order
                if !inner.consumed_external_events.contains(name)
                    && let Some((event_id, data)) = inner.history.iter().find_map(|e| {
                        if let EventKind::ExternalEvent { name: ext_name, data } = &e.kind
                            && ext_name == name
                        {
                            return Some((e.event_id, data.clone()));
                        }
                        None
                    })
                {
                    // Check: Are all completions BEFORE ours consumed?
                    if can_consume_completion(
                        &inner.history,
                        &inner.consumed_completions,
                        &inner.cancelled_source_ids,
                        event_id,
                    ) {
                        inner.consumed_completions.insert(event_id);
                        inner.consumed_external_events.insert(name.clone());
                        *result.borrow_mut() = Some(data.clone());
                        return Poll::Ready(DurableOutput::External(data));
                    }
                }

                Poll::Pending
            }
            Kind::SubOrch {
                name,
                version,
                instance,
                input,
            } => {
                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === SIMPLIFIED MODE ===
                if inner.replay_mode == ReplayMode::Simplified {
                    // Token was set at schedule time - just check for result
                    let token = this.simplified_token.get()
                        .expect("simplified_token should be set at schedule time");
                    if let Some(res) = inner.get_result(token) {
                        return match res {
                            SimplifiedResult::SubOrchOk(v) => {
                                Poll::Ready(DurableOutput::SubOrchestration(Ok(v.clone())))
                            }
                            SimplifiedResult::SubOrchErr(e) => {
                                Poll::Ready(DurableOutput::SubOrchestration(Err(e.clone())))
                            }
                            _ => Poll::Pending,
                        };
                    }
                    return Poll::Pending;
                }
                
                // Re-acquire as mutable for legacy mode
                drop(inner);
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === LEGACY MODE ===
                // Step 1: Claim SubOrchestrationScheduled event_id
                if this.claimed_event_id.get().is_none() {
                    // Enforce global scheduling order
                    let mut found_event_id = None;
                    for event in &inner.history {
                        let eid = event.event_id;
                        if inner.claimed_scheduling_events.contains(&eid) {
                            continue;
                        }
                        match &event.kind {
                            EventKind::SubOrchestrationScheduled {
                                name: n,
                                input: inp,
                                instance: inst,
                            } => {
                                if n != name || inp != input {
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{n}','{inp}') but expected SubOrchestrationScheduled('{name}','{input}')"
                                    ));
                                    return Poll::Pending;
                                }
                                *instance.borrow_mut() = inst.clone();
                                found_event_id = Some(eid);
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
                        // Not in history - create new
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

                        new_id
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    this.claimed_event_id.set(Some(event_id));
                }

                // claimed_event_id is guaranteed to be Some after the above block sets it
                let our_event_id = this
                    .claimed_event_id
                    .get()
                    .expect("claimed_event_id should be set after claiming");

                // Step 2: Look for SubOrch completion - FIFO enforcement
                let our_completion = inner.history.iter().find_map(|e| {
                    // source_event_id is on the Event struct
                    if e.source_event_id != Some(our_event_id) {
                        return None;
                    }
                    match &e.kind {
                        EventKind::SubOrchestrationCompleted { result } => Some((e.event_id, Ok(result.clone()))),
                        EventKind::SubOrchestrationFailed { details } => {
                            debug_assert!(
                                matches!(details, crate::ErrorDetails::Application { .. }),
                                "INVARIANT: Only Application errors should reach orchestration code, got: {details:?}"
                            );
                            Some((e.event_id, Err(details.display_message())))
                        }
                        _ => None,
                    }
                });

                if let Some((completion_event_id, result)) = our_completion {
                    // Check: Are all completions BEFORE ours consumed?
                    if can_consume_completion(
                        &inner.history,
                        &inner.consumed_completions,
                        &inner.cancelled_source_ids,
                        completion_event_id,
                    ) {
                        inner.consumed_completions.insert(completion_event_id);
                        return Poll::Ready(DurableOutput::SubOrchestration(result));
                    }
                }

                Poll::Pending
            }
            Kind::System { op, value } => {
                // Check if we already computed the value (legacy mode cache)
                if let Some(v) = value.borrow().clone() {
                    return Poll::Ready(DurableOutput::Activity(Ok(v)));
                }

                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === SIMPLIFIED MODE ===
                if inner.replay_mode == ReplayMode::Simplified {
                    // Token was set at schedule time - check for result
                    let token = this.simplified_token.get()
                        .expect("simplified_token should be set at schedule time");
                    if let Some(res) = inner.get_result(token) {
                        return match res {
                            SimplifiedResult::SystemCallValue(v) => {
                                Poll::Ready(DurableOutput::Activity(Ok(v.clone())))
                            }
                            _ => Poll::Pending,
                        };
                    }
                    // System calls are synchronous in first execution, so value should be in cache
                    // (set by schedule_system_call), but during replay we wait for result delivery
                    return Poll::Pending;
                }
                
                // Re-acquire as mutable for legacy mode
                drop(inner);
                let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");

                // === LEGACY MODE ===
                // Step 1: Try to adopt from history (replay)
                if this.claimed_event_id.get().is_none() {
                    // Look for matching SystemCall event in history
                    let found = inner.history.iter().find_map(|e| {
                        if let EventKind::SystemCall {
                            op: hist_op,
                            value: hist_value,
                        } = &e.kind
                            && hist_op == op
                            && !inner.claimed_scheduling_events.contains(&e.event_id)
                        {
                            return Some((e.event_id, hist_value.clone()));
                        }
                        None
                    });

                    if let Some((found_event_id, found_value)) = found {
                        // Found our system call in history - adopt it
                        inner.claimed_scheduling_events.insert(found_event_id);
                        this.claimed_event_id.set(Some(found_event_id));
                        *value.borrow_mut() = Some(found_value.clone());
                        return Poll::Ready(DurableOutput::Activity(Ok(found_value)));
                    }
                }

                // Step 2: First execution - compute value synchronously
                if this.claimed_event_id.get().is_none() {
                    let computed_value = match op.as_str() {
                        crate::SYSCALL_OP_GUID => generate_guid(),
                        crate::SYSCALL_OP_UTCNOW_MS => inner.now_ms().to_string(),
                        s if s.starts_with(crate::SYSCALL_OP_TRACE_PREFIX) => {
                            // Parse trace operation: "trace:{level}:{message}"
                            let parts: Vec<&str> = s.splitn(3, ':').collect();
                            if parts.len() == 3 {
                                let level = parts[1];
                                let message = parts[2];
                                // Extract context for structured logging
                                let instance_id = &inner.instance_id;
                                let execution_id = inner.execution_id;
                                let orch_name = &inner.orchestration_name;
                                let orch_version = &inner.orchestration_version;
                                let worker_id = inner.worker_id.as_deref().unwrap_or("unknown");

                                // Log to tracing only on first execution (not during replay)
                                // Include instance context for correlation
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
                            // trace operations don't return values, just empty string
                            String::new()
                        }
                        _ => {
                            inner.nondeterminism_error = Some(format!("unknown system operation: {op}"));
                            return Poll::Pending;
                        }
                    };

                    // Allocate event_id and record event
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

// Compile-time contract: DurableFuture must remain Unpin because poll() relies on freely
// projecting &mut self into its internal Kind. This assertion fails if future changes.
#[allow(dead_code)] // Used in const assertion below
const fn assert_unpin<T: Unpin>() {}
const _: () = {
    assert_unpin::<DurableFuture>();
};

// Helper function to generate deterministic GUIDs
pub(crate) fn generate_guid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // Thread-local counter for uniqueness within the same timestamp
    thread_local! {
        static COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let counter = COUNTER.with(|c| {
        let val = c.get();
        c.set(val.wrapping_add(1));
        val
    });

    // Format as UUID-like string
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (timestamp >> 96) as u32,
        ((timestamp >> 80) & 0xFFFF) as u16,
        (counter & 0xFFFF) as u16,
        ((timestamp >> 64) & 0xFFFF) as u16,
        (timestamp & 0xFFFFFFFFFFFF) as u64
    )
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

    // Note: Unconsumed completion detection removed - the cursor model naturally
    // handles this via strict sequential consumption. Any unconsumed completions
    // will cause a panic when the next future tries to poll.
}

impl Future for AggregateDurableFuture {
    type Output = AggregateOutput;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        // Check if we're in simplified mode
        let is_simplified = {
            let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
            inner.replay_mode == ReplayMode::Simplified
        };

        match this.mode {
            AggregateMode::Select => {
                if is_simplified {
                    // Simplified mode: poll all children, return first ready
                    // Cancellation of losers is handled by the simplified runtime
                    let mut ready_results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
                    for (i, child) in this.children.iter_mut().enumerate() {
                        if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                            ready_results[i] = Some(output);
                        }
                    }

                    // Return first ready (biased toward lower indices)
                    if let Some(winner_idx) = ready_results.iter().position(|r| r.is_some()) {
                        let output = ready_results[winner_idx].take().unwrap();
                        return Poll::Ready(AggregateOutput::Select {
                            winner_index: winner_idx,
                            output,
                        });
                    }
                    return Poll::Pending;
                }
                
                // Legacy mode: Two-phase polling to handle replay correctly:
                // Phase 1: Poll ALL children to ensure they claim their scheduling events.
                //          During replay, the winner might return Ready immediately, but
                //          losers still need to claim their scheduling events to avoid
                //          nondeterminism when subsequent code schedules new operations.
                // Phase 2: Check which child is ready and return the winner.
                // Phase 3: Mark loser source_event_ids as cancelled so their completions
                //          don't block FIFO ordering for subsequent operations.

                // Phase 1: Ensure all children claim their scheduling events
                let mut ready_results: Vec<Option<DurableOutput>> = vec![None; this.children.len()];
                for (i, child) in this.children.iter_mut().enumerate() {
                    if let Poll::Ready(output) = Pin::new(child).poll(cx) {
                        ready_results[i] = Some(output);
                    }
                }

                // Phase 2: Find the first ready child (winner)
                let winner_index = ready_results.iter().position(|r| r.is_some());

                if let Some(winner_idx) = winner_index {
                    // Phase 3: Mark all loser source_event_ids as cancelled
                    {
                        // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                        let mut inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
                        for (i, child) in this.children.iter().enumerate() {
                            if i != winner_idx {
                                // Get the loser's claimed_event_id (source_event_id for its completion)
                                // Now accessed directly from the DurableFuture struct
                                if let Some(source_id) = child.claimed_event_id.get() {
                                    inner.cancelled_source_ids.insert(source_id);

                                    // If the loser is an activity, request provider-side cancellation.
                                    // Timers/external/sub-orchestrations don't have worker-queue entries.
                                    if matches!(&child.kind, Kind::Activity { .. }) {
                                        inner.cancelled_activity_ids.insert(source_id);
                                    }
                                }
                            }
                        }
                    }

                    // Return the winner - safe to unwrap since winner_idx came from position() finding Some
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
                // Check if we're in simplified mode
                let is_simplified = {
                    let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
                    inner.replay_mode == ReplayMode::Simplified
                };
                
                if is_simplified {
                    // Simplified mode: just poll all children and return when all ready
                    // Order doesn't matter here - results are already delivered in history order
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
                            // All ready - return in original order (simplified mode handles history order)
                            let outputs: Vec<DurableOutput> = results.into_iter().map(|r| r.unwrap()).collect();
                            return Poll::Ready(AggregateOutput::Join { outputs });
                        }

                        if !made_progress {
                            return Poll::Pending;
                        }
                    }
                }
                
                // Legacy mode: Fixed-point polling with history-based ordering
                // Fixed-point polling: keep polling children until no new results appear
                // This allows cascading consumption respecting completion FIFO ordering.
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
                        // All outputs ready: return in persisted history order of completions
                        let mut items: Vec<(u64, usize, DurableOutput)> = Vec::with_capacity(results.len());
                        for (i, out_opt) in results.into_iter().enumerate() {
                            // All results are guaranteed to be Some due to the check above
                            let out = out_opt.expect("All results should be Some at this point");
                            // Determine completion event_id for child i
                            let eid = {
                                // Mutex lock should never fail in normal operation - if poisoned, it indicates a serious bug
                                let inner = this.ctx.inner.lock().expect("Mutex should not be poisoned");
                                let child = &this.children[i];
                                // All children must have claimed an event_id before reaching completion
                                let sid = child.claimed_event_id.get().expect("child must claim id");
                                match &child.kind {
                                    Kind::Activity { .. } => inner
                                        .history
                                        .iter()
                                        .find_map(|e| {
                                            if e.source_event_id != Some(sid) {
                                                return None;
                                            }
                                            match &e.kind {
                                                EventKind::ActivityCompleted { .. }
                                                | EventKind::ActivityFailed { .. } => Some(e.event_id),
                                                _ => None,
                                            }
                                        })
                                        .unwrap_or(u64::MAX),
                                    Kind::Timer { .. } => inner
                                        .history
                                        .iter()
                                        .find_map(|e| {
                                            if e.source_event_id != Some(sid) {
                                                return None;
                                            }
                                            match &e.kind {
                                                EventKind::TimerFired { .. } => Some(e.event_id),
                                                _ => None,
                                            }
                                        })
                                        .unwrap_or(u64::MAX),
                                    Kind::External { name, .. } => {
                                        let n = name.clone();
                                        inner
                                            .history
                                            .iter()
                                            .find_map(|e| {
                                                if let EventKind::ExternalEvent { name: en, .. } = &e.kind
                                                    && *en == n
                                                {
                                                    return Some(e.event_id);
                                                }
                                                None
                                            })
                                            .unwrap_or(u64::MAX)
                                    }
                                    Kind::SubOrch { .. } => inner
                                        .history
                                        .iter()
                                        .find_map(|e| {
                                            if e.source_event_id != Some(sid) {
                                                return None;
                                            }
                                            match &e.kind {
                                                EventKind::SubOrchestrationCompleted { .. }
                                                | EventKind::SubOrchestrationFailed { .. } => Some(e.event_id),
                                                _ => None,
                                            }
                                        })
                                        .unwrap_or(u64::MAX),
                                    Kind::System { .. } => {
                                        // For system calls, the event itself is the completion
                                        sid
                                    }
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
                    // Otherwise, loop again: newly consumed completions may unblock others
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
