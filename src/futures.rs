use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::{Action, Event, OrchestrationContext};

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
        match e {
            // Completions with source_event_id - check if cancelled
            Event::ActivityCompleted {
                event_id,
                source_event_id,
                ..
            }
            | Event::ActivityFailed {
                event_id,
                source_event_id,
                ..
            }
            | Event::TimerFired {
                event_id,
                source_event_id,
                ..
            }
            | Event::SubOrchestrationCompleted {
                event_id,
                source_event_id,
                ..
            }
            | Event::SubOrchestrationFailed {
                event_id,
                source_event_id,
                ..
            } => {
                // Cancelled completions (select2 losers) don't block - skip them
                if cancelled_source_ids.contains(source_event_id) {
                    return true;
                }
                // Otherwise: if before ours, must be consumed
                *event_id >= completion_event_id || consumed_completions.contains(event_id)
            }
            // External events don't have source_event_id - can't be cancelled via select2
            Event::ExternalEvent { event_id, .. } => {
                *event_id >= completion_event_id || consumed_completions.contains(event_id)
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

pub struct DurableFuture(pub(crate) Kind);

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
        result: RefCell<Option<String>>, // Cache result once found
        ctx: OrchestrationContext,
    },
    SubOrch {
        name: String,
        version: Option<String>,
        instance: RefCell<String>, // Updated once event_id is known
        input: String,
        claimed_event_id: Cell<Option<u64>>,
        ctx: OrchestrationContext,
    },
    System {
        op: String,
        claimed_event_id: Cell<Option<u64>>,
        value: RefCell<Option<String>>,
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
                    // Find next unclaimed SCHEDULING event in history (global order enforcement)
                    let mut found_event_id = None;
                    for event in &inner.history {
                        match event {
                            Event::ActivityScheduled {
                                event_id,
                                name: n,
                                input: inp,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
                                // MUST be our schedule next
                                if n != name || inp != input {
                                    // Record nondeterminism gracefully
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is ActivityScheduled('{n}','{inp}') but expected ActivityScheduled('{name}','{input}')"
                                    ));
                                    return Poll::Pending;
                                }
                                found_event_id = Some(*event_id);
                                break;
                            }
                            Event::TimerCreated { event_id, .. }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is TimerCreated but expected ActivityScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            Event::ExternalSubscribed { event_id, name: en }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{en}') but expected ActivityScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            Event::SubOrchestrationScheduled {
                                event_id,
                                name: sn,
                                input: sin,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
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

                // Step 2: Look for our completion - FIFO enforcement
                // Find our completion in history
                let our_completion = inner.history.iter().find_map(|e| match e {
                    Event::ActivityCompleted {
                        event_id,
                        source_event_id,
                        result,
                        ..
                    } if *source_event_id == our_event_id => Some((*event_id, Ok(result.clone()))),
                    Event::ActivityFailed {
                        event_id,
                        source_event_id,
                        details,
                        ..
                    } if *source_event_id == our_event_id => {
                        debug_assert!(
                            matches!(details, crate::ErrorDetails::Application { .. }),
                            "INVARIANT: Only Application errors should reach orchestration code, got: {details:?}"
                        );
                        Some((*event_id, Err(details.display_message())))
                    }
                    _ => None,
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
            Kind::Timer {
                delay_ms,
                claimed_event_id,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();

                // Step 1: Claim scheduling event_id
                if claimed_event_id.get().is_none() {
                    // Enforce global scheduling order
                    let mut found_event_id = None;
                    for event in &inner.history {
                        match event {
                            Event::TimerCreated { event_id, .. }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                found_event_id = Some(*event_id);
                                break;
                            }
                            Event::ActivityScheduled {
                                event_id,
                                name: n,
                                input: inp,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{n}','{inp}') but expected TimerCreated"
                                ));
                                return Poll::Pending;
                            }
                            Event::ExternalSubscribed { event_id, name: en }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{en}') but expected TimerCreated"
                                ));
                                return Poll::Pending;
                            }
                            Event::SubOrchestrationScheduled {
                                event_id,
                                name: sn,
                                input: sin,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
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

                // Step 2: Look for TimerFired - FIFO enforcement
                let our_completion = inner.history.iter().find_map(|e| {
                    if let Event::TimerFired {
                        event_id,
                        source_event_id,
                        ..
                    } = e
                        && *source_event_id == our_event_id
                    {
                        return Some(*event_id);
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
                    // Enforce global scheduling order
                    let mut found_event_id = None;
                    for event in &inner.history {
                        match event {
                            Event::ExternalSubscribed { event_id, name: n }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                if n != name {
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is ExternalSubscribed('{n}') but expected ExternalSubscribed('{name}')"
                                    ));
                                    return Poll::Pending;
                                }
                                found_event_id = Some(*event_id);
                                break;
                            }
                            Event::ActivityScheduled {
                                event_id,
                                name: an,
                                input: ainp,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{an}','{ainp}') but expected ExternalSubscribed('{name}')"
                                ));
                                return Poll::Pending;
                            }
                            Event::TimerCreated { event_id, .. }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is TimerCreated but expected ExternalSubscribed('{name}')"
                                ));
                                return Poll::Pending;
                            }
                            Event::SubOrchestrationScheduled {
                                event_id,
                                name: sn,
                                input: sin,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
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
                // External events can arrive in any order
                if !inner.consumed_external_events.contains(name)
                    && let Some((event_id, data)) = inner.history.iter().find_map(|e| {
                        if let Event::ExternalEvent {
                            event_id,
                            name: ext_name,
                            data,
                            ..
                        } = e
                            && ext_name == name
                        {
                            return Some((*event_id, data.clone()));
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
                claimed_event_id,
                ctx,
            } => {
                let mut inner = ctx.inner.lock().unwrap();

                // Step 1: Claim SubOrchestrationScheduled event_id
                if claimed_event_id.get().is_none() {
                    // Enforce global scheduling order
                    let mut found_event_id = None;
                    for event in &inner.history {
                        match event {
                            Event::SubOrchestrationScheduled {
                                event_id,
                                name: n,
                                input: inp,
                                instance: inst,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
                                if n != name || inp != input {
                                    inner.nondeterminism_error = Some(format!(
                                        "nondeterministic: schedule order mismatch: next is SubOrchestrationScheduled('{n}','{inp}') but expected SubOrchestrationScheduled('{name}','{input}')"
                                    ));
                                    return Poll::Pending;
                                }
                                *instance.borrow_mut() = inst.clone();
                                found_event_id = Some(*event_id);
                                break;
                            }
                            Event::ActivityScheduled {
                                event_id,
                                name: an,
                                input: ainp,
                                ..
                            } if !inner.claimed_scheduling_events.contains(event_id) => {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is ActivityScheduled('{an}','{ainp}') but expected SubOrchestrationScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            Event::TimerCreated { event_id, .. }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
                                inner.nondeterminism_error = Some(format!(
                                    "nondeterministic: schedule order mismatch: next is TimerCreated but expected SubOrchestrationScheduled('{name}','{input}')"
                                ));
                                return Poll::Pending;
                            }
                            Event::ExternalSubscribed { event_id, name: en }
                                if !inner.claimed_scheduling_events.contains(event_id) =>
                            {
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
                        let child_instance = format!("sub::{new_id}");
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

                // Step 2: Look for SubOrch completion - FIFO enforcement
                let our_completion = inner.history.iter().find_map(|e| match e {
                    Event::SubOrchestrationCompleted {
                        event_id,
                        source_event_id,
                        result,
                        ..
                    } if *source_event_id == our_event_id => Some((*event_id, Ok(result.clone()))),
                    Event::SubOrchestrationFailed {
                        event_id,
                        source_event_id,
                        details,
                        ..
                    } if *source_event_id == our_event_id => {
                        debug_assert!(
                            matches!(details, crate::ErrorDetails::Application { .. }),
                            "INVARIANT: Only Application errors should reach orchestration code, got: {details:?}"
                        );
                        Some((*event_id, Err(details.display_message())))
                    }
                    _ => None,
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
            Kind::System {
                op,
                claimed_event_id,
                value,
                ctx,
            } => {
                // Check if we already computed the value
                if let Some(v) = value.borrow().clone() {
                    return Poll::Ready(DurableOutput::Activity(Ok(v)));
                }

                let mut inner = ctx.inner.lock().unwrap();

                // Step 1: Try to adopt from history (replay)
                if claimed_event_id.get().is_none() {
                    // Look for matching SystemCall event in history
                    let found = inner.history.iter().find_map(|e| {
                        if let Event::SystemCall {
                            event_id,
                            op: hist_op,
                            value: hist_value,
                            ..
                        } = e
                            && hist_op == op
                            && !inner.claimed_scheduling_events.contains(event_id)
                        {
                            return Some((*event_id, hist_value.clone()));
                        }
                        None
                    });

                    if let Some((found_event_id, found_value)) = found {
                        // Found our system call in history - adopt it
                        inner.claimed_scheduling_events.insert(found_event_id);
                        claimed_event_id.set(Some(found_event_id));
                        *value.borrow_mut() = Some(found_value.clone());
                        return Poll::Ready(DurableOutput::Activity(Ok(found_value)));
                    }
                }

                // Step 2: First execution - compute value synchronously
                if claimed_event_id.get().is_none() {
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
                                let orch_name = inner.orchestration_name.as_deref().unwrap_or("unknown");
                                let orch_version = inner.orchestration_version.as_deref().unwrap_or("unknown");
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

                    inner.history.push(Event::SystemCall {
                        event_id,
                        op: op.clone(),
                        value: computed_value.clone(),
                        execution_id: exec_id,
                    });

                    inner.record_action(Action::SystemCall {
                        scheduling_event_id: event_id,
                        op: op.clone(),
                        value: computed_value.clone(),
                    });

                    inner.claimed_scheduling_events.insert(event_id);
                    claimed_event_id.set(Some(event_id));
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
const fn assert_unpin<T: Unpin>() {}
const _: () = {
    assert_unpin::<DurableFuture>();
};

// Helper function to generate deterministic GUIDs
fn generate_guid() -> String {
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

        match this.mode {
            AggregateMode::Select => {
                // Two-phase polling to handle replay correctly:
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
                        let mut inner = this.ctx.inner.lock().unwrap();
                        for (i, child) in this.children.iter().enumerate() {
                            if i != winner_idx {
                                // Get the loser's claimed_event_id (source_event_id for its completion)
                                let loser_source_id = match &child.0 {
                                    Kind::Activity { claimed_event_id, .. } => claimed_event_id.get(),
                                    Kind::Timer { claimed_event_id, .. } => claimed_event_id.get(),
                                    Kind::External { claimed_event_id, .. } => claimed_event_id.get(),
                                    Kind::SubOrch { claimed_event_id, .. } => claimed_event_id.get(),
                                    Kind::System { claimed_event_id, .. } => claimed_event_id.get(),
                                };
                                if let Some(source_id) = loser_source_id {
                                    inner.cancelled_source_ids.insert(source_id);
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
                            let out = out_opt.unwrap();
                            // Determine completion event_id for child i
                            let eid = {
                                let inner = this.ctx.inner.lock().unwrap();
                                match &this.children[i].0 {
                                    Kind::Activity { claimed_event_id, .. } => {
                                        let sid = claimed_event_id.get().expect("activity must claim id");
                                        inner
                                            .history
                                            .iter()
                                            .find_map(|e| match e {
                                                Event::ActivityCompleted {
                                                    event_id,
                                                    source_event_id,
                                                    ..
                                                } if *source_event_id == sid => Some(*event_id),
                                                Event::ActivityFailed {
                                                    event_id,
                                                    source_event_id,
                                                    ..
                                                } if *source_event_id == sid => Some(*event_id),
                                                _ => None,
                                            })
                                            .unwrap_or(u64::MAX)
                                    }
                                    Kind::Timer { claimed_event_id, .. } => {
                                        let sid = claimed_event_id.get().expect("timer must claim id");
                                        inner
                                            .history
                                            .iter()
                                            .find_map(|e| match e {
                                                Event::TimerFired {
                                                    event_id,
                                                    source_event_id,
                                                    ..
                                                } if *source_event_id == sid => Some(*event_id),
                                                _ => None,
                                            })
                                            .unwrap_or(u64::MAX)
                                    }
                                    Kind::External { name, .. } => {
                                        let n = name.clone();
                                        inner
                                            .history
                                            .iter()
                                            .find_map(|e| match e {
                                                Event::ExternalEvent { event_id, name: en, .. } if *en == n => {
                                                    Some(*event_id)
                                                }
                                                _ => None,
                                            })
                                            .unwrap_or(u64::MAX)
                                    }
                                    Kind::SubOrch { claimed_event_id, .. } => {
                                        let sid = claimed_event_id.get().expect("suborch must claim id");
                                        inner
                                            .history
                                            .iter()
                                            .find_map(|e| match e {
                                                Event::SubOrchestrationCompleted {
                                                    event_id,
                                                    source_event_id,
                                                    ..
                                                } if *source_event_id == sid => Some(*event_id),
                                                Event::SubOrchestrationFailed {
                                                    event_id,
                                                    source_event_id,
                                                    ..
                                                } if *source_event_id == sid => Some(*event_id),
                                                _ => None,
                                            })
                                            .unwrap_or(u64::MAX)
                                    }
                                    Kind::System { claimed_event_id, .. } => {
                                        // For system calls, the event itself is the completion
                                        claimed_event_id.get().expect("system call must claim id")
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
