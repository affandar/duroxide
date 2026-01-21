//! Test utility: run_turn for simplified mode
//!
//! This module provides a `run_turn` function that allows testing orchestration logic
//! in isolation without needing a full runtime. It creates an OrchestrationContext,
//! populates it with history, and runs the orchestration future.

use duroxide::{Action, Event, EventKind, OrchestrationContext, SimplifiedResult};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// Result of running one turn of an orchestration.
#[derive(Debug)]
pub enum TurnResult<O> {
    /// Orchestration completed with output
    Completed(O),
    /// Orchestration is pending (waiting for more events)
    Pending { actions: Vec<Action> },
    /// Orchestration failed with error
    Failed(String),
}

/// Run one turn of an orchestration against history.
///
/// This is a test utility that runs an orchestration closure against a provided history,
/// returning the output if the orchestration completes, or the pending actions if it doesn't.
///
/// # Arguments
/// * `history` - The event history to replay
/// * `orchestrator` - The orchestration closure to run
///
/// # Returns
/// A tuple of (final_history_events, emitted_actions, optional_output)
pub fn run_turn<O, F>(
    history: Vec<Event>,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>)
where
    F: Future<Output = O>,
{
    run_turn_with(
        history,
        1, // execution_id
        "test-instance".to_string(),
        "TestOrch".to_string(),
        "1.0.0".to_string(),
        orchestrator,
    )
}

/// Run one turn with custom execution parameters.
pub fn run_turn_with<O, F>(
    history: Vec<Event>,
    execution_id: u64,
    instance_id: String,
    orch_name: String,
    orch_version: String,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>)
where
    F: Future<Output = O>,
{
    let (history, actions, output, _status) = run_turn_with_status(
        history,
        execution_id,
        instance_id,
        orch_name,
        orch_version,
        "test-worker".to_string(),
        orchestrator,
    );
    (history, actions, output)
}

/// Run one turn with full parameters including worker_id.
pub fn run_turn_with_status<O, F>(
    mut history: Vec<Event>,
    execution_id: u64,
    instance_id: String,
    orch_name: String,
    orch_version: String,
    worker_id: String,
    orchestrator: impl Fn(OrchestrationContext) -> F,
) -> (Vec<Event>, Vec<Action>, Option<O>, Option<String>)
where
    F: Future<Output = O>,
{
    // If history is empty, this is a fresh start - generate OrchestrationStarted
    if history.is_empty() {
        history.push(Event::with_event_id(
            1,
            instance_id.clone(),
            execution_id,
            None,
            EventKind::OrchestrationStarted {
                name: orch_name.clone(),
                version: orch_version.clone(),
                input: String::new(),
                parent_instance: None,
                parent_id: None,
            },
        ));
    }

    // Validate first event is OrchestrationStarted
    let (_input, _parent_instance, _parent_id) = match &history[0].kind {
        EventKind::OrchestrationStarted { input, parent_instance, parent_id, .. } => {
            (input.clone(), parent_instance.clone(), parent_id.clone())
        }
        _ => {
            return (
                history,
                vec![],
                None,
                Some("first event must be OrchestrationStarted".to_string()),
            );
        }
    };

    // Check for terminal history
    if history.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::OrchestrationCompleted { .. }
                | EventKind::OrchestrationFailed { .. }
                | EventKind::OrchestrationContinuedAsNew { .. }
        )
    }) {
        return (history, vec![], None, Some("terminal history".to_string()));
    }

    // Create context in simplified mode
    let ctx = OrchestrationContext::new(
        Vec::new(), // Empty - simplified mode doesn't use legacy history field
        execution_id,
        instance_id.clone(),
        orch_name.clone(),
        orch_version.clone(),
        Some(worker_id),
    );

    // Set to replaying mode
    ctx.set_is_replaying(true);

    // Create the orchestration future
    let mut fut = Box::pin(orchestrator(ctx.clone()));

    // Track open schedules and their kinds
    let mut open_schedules: HashSet<u64> = HashSet::new();
    let mut schedule_kinds: HashMap<u64, ActionKind> = HashMap::new();
    let mut emitted_actions: VecDeque<(u64, Action)> = VecDeque::new();

    let mut must_poll = true;
    let mut output_opt: Option<O> = None;

    // Process history events
    for event in history.iter().skip(1) {
        // Skip OrchestrationStarted
        // Poll if needed to get any new emitted actions
        if must_poll {
            must_poll = false;
            match poll_once(fut.as_mut()) {
                Poll::Ready(output) => {
                    output_opt = Some(output);
                    break;
                }
                Poll::Pending => {
                    // Collect newly emitted actions
                    for (token, action) in ctx.get_emitted_actions() {
                        if !emitted_actions.iter().any(|(t, _)| *t == token) {
                            emitted_actions.push_back((token, action));
                        }
                    }
                }
            }
        }

        // Process event
        match &event.kind {
            EventKind::ActivityScheduled { name, input } => {
                if let Some(result) = match_and_bind_schedule(
                    &ctx,
                    &mut emitted_actions,
                    &mut open_schedules,
                    &mut schedule_kinds,
                    event,
                    ActionKind::Activity {
                        name: name.clone(),
                        input: input.clone(),
                    },
                ) {
                    return (
                        history,
                        vec![],
                        None,
                        Some(format!("nondeterminism: {result}")),
                    );
                }
            }

            EventKind::TimerCreated { fire_at_ms } => {
                if let Some(result) = match_and_bind_schedule(
                    &ctx,
                    &mut emitted_actions,
                    &mut open_schedules,
                    &mut schedule_kinds,
                    event,
                    ActionKind::Timer {
                        fire_at_ms: *fire_at_ms,
                    },
                ) {
                    return (
                        history,
                        vec![],
                        None,
                        Some(format!("nondeterminism: {result}")),
                    );
                }
            }

            EventKind::ExternalSubscribed { name } => {
                if let Some(result) = match_and_bind_schedule(
                    &ctx,
                    &mut emitted_actions,
                    &mut open_schedules,
                    &mut schedule_kinds,
                    event,
                    ActionKind::External { name: name.clone() },
                ) {
                    return (
                        history,
                        vec![],
                        None,
                        Some(format!("nondeterminism: {result}")),
                    );
                }
                // Bind external subscription
                ctx.bind_external_subscription(event.event_id(), name);
            }

            EventKind::SubOrchestrationScheduled { name, instance, input: inp, .. } => {
                if let Some(result) = match_and_bind_schedule(
                    &ctx,
                    &mut emitted_actions,
                    &mut open_schedules,
                    &mut schedule_kinds,
                    event,
                    ActionKind::SubOrch {
                        name: name.clone(),
                        instance: instance.clone(),
                        input: inp.clone(),
                    },
                ) {
                    return (
                        history,
                        vec![],
                        None,
                        Some(format!("nondeterminism: {result}")),
                    );
                }
            }

            EventKind::ActivityCompleted { result } => {
                if let Some(source_id) = event.source_event_id {
                    if open_schedules.contains(&source_id) {
                        ctx.deliver_result(
                            source_id,
                            SimplifiedResult::ActivityOk(result.clone()),
                        );
                        must_poll = true;
                    }
                }
            }

            EventKind::ActivityFailed { details } => {
                if let Some(source_id) = event.source_event_id {
                    if open_schedules.contains(&source_id) {
                        ctx.deliver_result(
                            source_id,
                            SimplifiedResult::ActivityErr(details.display_message()),
                        );
                        must_poll = true;
                    }
                }
            }

            EventKind::TimerFired { .. } => {
                if let Some(source_id) = event.source_event_id {
                    if open_schedules.contains(&source_id) {
                        ctx.deliver_result(source_id, SimplifiedResult::TimerFired);
                        must_poll = true;
                    }
                }
            }

            EventKind::SubOrchestrationCompleted { result } => {
                if let Some(source_id) = event.source_event_id {
                    if open_schedules.contains(&source_id) {
                        ctx.deliver_result(
                            source_id,
                            SimplifiedResult::SubOrchOk(result.clone()),
                        );
                        must_poll = true;
                    }
                }
            }

            EventKind::SubOrchestrationFailed { details } => {
                if let Some(source_id) = event.source_event_id {
                    if open_schedules.contains(&source_id) {
                        ctx.deliver_result(
                            source_id,
                            SimplifiedResult::SubOrchErr(details.display_message()),
                        );
                        must_poll = true;
                    }
                }
            }

            EventKind::ExternalEvent { name, data } => {
                ctx.deliver_external_event(name.clone(), data.clone());
                must_poll = true;
            }

            _ => {}
        }
    }

    // Final poll after processing all history
    ctx.set_is_replaying(false);

    if output_opt.is_none() {
        match poll_once(fut.as_mut()) {
            Poll::Ready(output) => {
                output_opt = Some(output);
            }
            Poll::Pending => {
                // Collect any remaining emitted actions
                for (token, action) in ctx.get_emitted_actions() {
                    if !emitted_actions.iter().any(|(t, _)| *t == token) {
                        emitted_actions.push_back((token, action));
                    }
                }
            }
        }
    }

    // Collect final actions
    let actions: Vec<Action> = emitted_actions.into_iter().map(|(_, a)| a).collect();

    (history, actions, output_opt, None)
}

// Helper types for action matching
#[derive(Debug, Clone, PartialEq)]
enum ActionKind {
    Activity { name: String, input: String },
    Timer { fire_at_ms: u64 },
    External { name: String },
    SubOrch { name: String, instance: String, input: String },
}

fn match_and_bind_schedule(
    ctx: &OrchestrationContext,
    emitted_actions: &mut VecDeque<(u64, Action)>,
    open_schedules: &mut HashSet<u64>,
    schedule_kinds: &mut HashMap<u64, ActionKind>,
    event: &Event,
    expected_kind: ActionKind,
) -> Option<String> {
    // Get the next emitted action
    let (token, action) = match emitted_actions.pop_front() {
        Some(a) => a,
        None => {
            return Some(format!(
                "no emitted action to match schedule event {:?}",
                event.kind
            ));
        }
    };

    // Validate action matches expected kind
    let action_matches = match (&expected_kind, &action) {
        (
            ActionKind::Activity { name, input },
            Action::CallActivity {
                name: n, input: i, ..
            },
        ) => name == n && input == i,
        (ActionKind::Timer { fire_at_ms }, Action::CreateTimer { fire_at_ms: f, .. }) => {
            fire_at_ms == f
        }
        (ActionKind::External { name }, Action::WaitExternal { name: n, .. }) => name == n,
        (
            ActionKind::SubOrch { name, instance, input },
            Action::StartSubOrchestration {
                name: n,
                instance: inst,
                input: i,
                ..
            },
        ) => name == n && instance == inst && input == i,
        _ => false,
    };

    if !action_matches {
        return Some(format!(
            "action mismatch: expected {:?}, got {:?}",
            expected_kind, action
        ));
    }

    // Bind token to schedule_id
    let schedule_id = event.event_id();
    ctx.bind_token(token, schedule_id);
    open_schedules.insert(schedule_id);
    schedule_kinds.insert(schedule_id, expected_kind);

    None
}

fn noop_waker() -> Waker {
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn wake(_: *const ()) {}
    fn wake_by_ref(_: *const ()) {}
    fn drop(_: *const ()) {}

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    fut.poll(&mut cx)
}
