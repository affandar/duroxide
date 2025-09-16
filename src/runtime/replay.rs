use crate::{Action, DurableOutput, Event, OrchestrationContext};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use super::event_ids::ReplayHistoryEvent;
use super::replay_context_wrapper::ReplayOrchestrationContext;

// No replay iteration loop is used anymore

type OpenFutures = Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>;

/// Output from the replay function
#[derive(Debug)]
pub struct ReplayOutput<O> {
    pub decisions: Vec<Action>,
    pub output: Option<O>,
    /// Non-determinism error if detected during replay
    pub non_determinism_error: Option<String>,
}

// Turn-based API
pub struct ReplayTurnHandle<Fut>
where
    Fut: Future + Send + 'static,
{
    pub(crate) orchestration_future: Pin<Box<Fut>>,
    pub(crate) orchestration_context: ReplayOrchestrationContext,
    pub(crate) open_futures: OpenFutures,
}

pub struct ReplayTurnResult<O, Fut>
where
    Fut: Future + Send + 'static,
{
    pub decisions: Vec<Action>,
    pub output: Option<O>,
    pub non_determinism_error: Option<String>,
    pub next_handle: Option<ReplayTurnHandle<Fut>>,
}

// Helper: set a future ready with provided DurableOutput if present
fn set_future_ready(open_futures: &OpenFutures, id: u64, output: DurableOutput) {
    let futures = open_futures.lock().unwrap();
    if let Some(future) = futures.get(&id) {
        let mut ready = future.ready.lock().unwrap();
        if !*ready {
            *ready = true;
            *future.completion.lock().unwrap() = Some(output);
        }
    }
}

// Helper: should we emit this decision (i.e., not already in history)?
fn should_emit_decision(action: &Action, open_futures: &OpenFutures) -> bool {
    match action {
        Action::CallActivity { id, .. }
        | Action::CreateTimer { id, .. }
        | Action::WaitExternal { id, .. }
        | Action::StartSubOrchestration { id, .. } => {
            open_futures
                .lock()
                .unwrap()
                .get(id)
                .map_or(true, |f| *f.should_emit_decision.lock().unwrap())
        }
        // Non-schedule decisions: always emit if produced (dup suppression handled by ReplayOrchestrationContext)
        Action::ContinueAsNew { .. } | Action::StartOrchestrationDetached { .. } => true,
    }
}

// Helper: poll orchestration once and collect actions
fn poll_and_collect<Fut, O>(
    orchestration_future: &mut Option<Pin<Box<Fut>>>,
    orchestration_context: &Option<ReplayOrchestrationContext>,
    open_futures: &OpenFutures,
    decisions: &mut Vec<Action>,
) -> Option<O>
where
    Fut: Future<Output = O>,
{
    if let Some(fut) = orchestration_future.as_mut() {
        if let Some(ctx) = orchestration_context.as_ref() {
            let waker = create_noop_waker();
            let mut cx = Context::from_waker(&waker);
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(output) => return Some(output),
                Poll::Pending => {}
            }
            for action in ctx.take_actions() {
                if should_emit_decision(&action, open_futures) {
                    decisions.push(action);
                }
            }
        }
    }
    None
}

/// A DurableFuture variant for replay that holds its own completion state
#[derive(Clone)]
pub(crate) struct ReplayDurableFuture {
    pub(crate) ready: Arc<Mutex<bool>>,
    pub(crate) completion: Arc<Mutex<Option<DurableOutput>>>,
    pub(crate) should_emit_decision: Arc<Mutex<bool>>,
}

impl Future for ReplayDurableFuture {
    type Output = DurableOutput;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Check if we're ready
        if *self.ready.lock().unwrap() {
            // Return the completion data (clone it to allow multiple polls if needed)
            if let Some(completion) = self.completion.lock().unwrap().clone() {
                return Poll::Ready(completion);
            }
        }

        Poll::Pending
    }
}

impl ReplayDurableFuture {
    /// Await an activity result as a raw String
    #[allow(dead_code)]
    pub fn into_activity(self) -> impl Future<Output = Result<String, String>> {
        struct Map(ReplayDurableFuture);
        impl Future for Map {
            type Output = Result<String, String>;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                match Pin::new(&mut self.0).poll(cx) {
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

    /// Await a timer
    #[allow(dead_code)]
    pub fn into_timer(self) -> impl Future<Output = ()> {
        struct Map(ReplayDurableFuture);
        impl Future for Map {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                match Pin::new(&mut self.0).poll(cx) {
                    Poll::Ready(DurableOutput::Timer) => Poll::Ready(()),
                    Poll::Ready(other) => {
                        panic!("into_timer used on non-timer future: {other:?}")
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
        Map(self)
    }

    /// Await an external event as a String
    #[allow(dead_code)]
    pub fn into_event(self) -> impl Future<Output = String> {
        struct Map(ReplayDurableFuture);
        impl Future for Map {
            type Output = String;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                match Pin::new(&mut self.0).poll(cx) {
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

/// Single stateless replay function
#[allow(dead_code)]
pub(crate) fn replay_orchestration<F, Fut, O>(
    orchestration_fn: F,
    history: Vec<ReplayHistoryEvent>,
    delta_history: Vec<ReplayHistoryEvent>,
) -> Result<ReplayOutput<O>, String>
where
    F: Fn(ReplayOrchestrationContext) -> Fut,
    Fut: Future<Output = O> + Send + 'static,
    O: Send + 'static,
{
    // Preserve legacy error behavior
    if history.is_empty() && delta_history.is_empty() {
        return Err("History must not be empty".to_string());
    }
    // Phase 1: baseline history (may be empty); on first turn we need the orch fn
    let turn1 = replay_turn(Some(&orchestration_fn), history, None)?;

    let mut decisions = turn1.decisions;
    let mut output = turn1.output;
    let mut non_det = turn1.non_determinism_error;
    let handle = turn1.next_handle;

    // Phase 2: delta history with the returned handle
    if output.is_none() && non_det.is_none() && !delta_history.is_empty() && handle.is_some() {
        let turn2 = replay_turn::<F, Fut, O>(None, delta_history, handle)?;
        decisions.extend(turn2.decisions);
        output = output.or(turn2.output);
        non_det = non_det.or(turn2.non_determinism_error);
    }

    Ok(ReplayOutput { decisions, output, non_determinism_error: non_det })
}

// Single turn entrypoint: first turn uses None handle and must include OrchestrationStarted in events
#[allow(dead_code)]
pub(crate) fn replay_turn<F, Fut, O>(
    orchestration_fn: Option<&F>,
    events_this_turn: Vec<ReplayHistoryEvent>,
    handle: Option<ReplayTurnHandle<Fut>>,
) -> Result<ReplayTurnResult<O, Fut>, String>
where
    F: Fn(ReplayOrchestrationContext) -> Fut,
    Fut: Future<Output = O> + Send + 'static,
    O: Send + 'static,
{
    // Prepare or reuse future/context
    let (orchestration_future, orchestration_context, open_futures) = match handle {
        Some(h) => (h.orchestration_future, h.orchestration_context, h.open_futures),
        None => {
            let has_start = events_this_turn
                .iter()
                .any(|he| matches!(he.event, Event::OrchestrationStarted { .. }));
            if !has_start {
                return Err("First turn must contain OrchestrationStarted".to_string());
            }
            let open_futures = Arc::new(Mutex::new(HashMap::<u64, ReplayDurableFuture>::new()));
            let exec_id = events_this_turn
                .iter()
                .find_map(|he| match &he.event {
                    Event::OrchestrationStarted { execution_id, .. } => Some(*execution_id),
                    _ => None,
                })
                .unwrap_or(1);
            let inner_ctx = OrchestrationContext::new(
                events_this_turn.iter().map(|he| he.event.clone()).collect(),
                exec_id,
            );
            let ctx = ReplayOrchestrationContext::new(inner_ctx, open_futures.clone(), events_this_turn.clone());
            let orch_fn = orchestration_fn.ok_or("orchestration_fn is required on first turn")?;
            let fut: Pin<Box<Fut>> = Box::pin(orch_fn(ctx.clone()));
            (fut, ctx, open_futures)
        }
    };

    // Reuse the core single-pass processor on this turn's events
    let mut decisions = Vec::new();
    let (output, non_determinism_error, pending) = replay_core_with_future(
        &events_this_turn,
        open_futures.clone(),
        &mut decisions,
        Some((orchestration_future, orchestration_context.clone())),
    )?;

    let next_handle = if let Some((f, _ctx)) = pending {
        Some(ReplayTurnHandle {
            orchestration_future: f,
            orchestration_context,
            open_futures,
        })
    } else {
        None
    };

    Ok(ReplayTurnResult {
        decisions,
        output,
        non_determinism_error,
        next_handle,
    })
}

/// Core replay logic that processes a set of events with an existing future
/// Returns (orchestration_output, non_determinism_error, orchestration_future)
#[allow(dead_code)]
fn replay_core_with_future<Fut, O>(
    events: &[ReplayHistoryEvent],
    open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    decisions: &mut Vec<Action>,
    existing_future: Option<(Pin<Box<Fut>>, ReplayOrchestrationContext)>,
) -> Result<
    (
        Option<O>,
        Option<String>,
        Option<(Pin<Box<Fut>>, ReplayOrchestrationContext)>,
    ),
    String,
>
where
    Fut: Future<Output = O>,
{
    let mut orchestration_future: Option<Pin<Box<Fut>>> = None;
    let mut orchestration_context: Option<ReplayOrchestrationContext> = None;

    // Use existing future if provided
    if let Some((fut, ctx)) = existing_future {
        orchestration_future = Some(fut);
        orchestration_context = Some(ctx);
    }
    let mut orchestration_output = None;
    let mut non_determinism_error: Option<String> = None;
    // Single pass over all events
    let mut _iter: usize = 0;
    for wrapped_event in events {
        _iter += 1;
        let event = &wrapped_event.event;
        match event {
            // OrchestrationStarted: nothing special here; unified polling happens after event
            Event::OrchestrationStarted { .. } => {}

            // Scheduled events: verify queued schedule order matches history and mark should-not-emit
            Event::ActivityScheduled { .. }
            | Event::TimerCreated { .. }
            | Event::ExternalSubscribed { .. }
            | Event::SubOrchestrationScheduled { .. } => {
                if let Some(ctx) = orchestration_context.as_ref() {
                    if let Err(msg) = ctx.verify_next_schedule_against(wrapped_event) {
                        non_determinism_error = Some(msg);
                    }
                }
                let schedule_eid = wrapped_event.event_id;
                let futures = open_futures.lock().unwrap();
                if let Some(future) = futures.get(&schedule_eid) {
                    *future.should_emit_decision.lock().unwrap() = false;
                }
            }

            // Completion events: set ready by scheduled_event_id as before
            Event::ActivityCompleted { result, .. } => {
                if let Some(sid) = wrapped_event.scheduled_event_id {
                    set_future_ready(&open_futures, sid, DurableOutput::Activity(Ok(result.clone())));
                }
            }

            Event::ActivityFailed { error, .. } => {
                if let Some(sid) = wrapped_event.scheduled_event_id {
                    set_future_ready(&open_futures, sid, DurableOutput::Activity(Err(error.clone())));
                }
            }

            Event::TimerFired { .. } => {
                if let Some(sid) = wrapped_event.scheduled_event_id {
                    set_future_ready(&open_futures, sid, DurableOutput::Timer);
                }
            }

            Event::ExternalEvent { data, .. } => {
                if let Some(sid) = wrapped_event.scheduled_event_id {
                    set_future_ready(&open_futures, sid, DurableOutput::External(data.clone()));
                }
            }

            Event::SubOrchestrationCompleted { result, .. } => {
                if let Some(sid) = wrapped_event.scheduled_event_id {
                    set_future_ready(
                        &open_futures,
                        sid,
                        DurableOutput::SubOrchestration(Ok(result.clone())),
                    );
                }
            }

            Event::SubOrchestrationFailed { error, .. } => {
                if let Some(sid) = wrapped_event.scheduled_event_id {
                    set_future_ready(
                        &open_futures,
                        sid,
                        DurableOutput::SubOrchestration(Err(error.clone())),
                    );
                }
            }

            // Orchestration lifecycle events - these don't affect scheduling/futures
            Event::OrchestrationCompleted { .. } => {}

            Event::OrchestrationFailed { .. } => {}

            Event::OrchestrationContinuedAsNew { .. } => {}

            Event::OrchestrationCancelRequested { .. } => {}

            Event::OrchestrationChained { .. } => {}
        }
        // After each event, poll orchestration once to allow it to advance
        if orchestration_output.is_none() {
            if let Some(out) = poll_and_collect(&mut orchestration_future, &orchestration_context, &open_futures, decisions) {
                orchestration_output = Some(out);
            }
        }
        // Do not advance schedule cursor on non-schedule events; adoption happens in schedule_next_or_new
    }

    // Check for non-determinism if orchestration completed
    if orchestration_output.is_some() && non_determinism_error.is_none() {
        // Re-process events to check for non-determinism
        for wrapped_event in events {
            let event = &wrapped_event.event;
            match event {
                Event::ActivityScheduled { name, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if !futures.contains_key(&wrapped_event.event_id) {
                        non_determinism_error = Some(format!(
                            "Non-determinism detected: ActivityScheduled event (id={}, name='{}') found in history, but orchestration did not call schedule_activity()",
                            wrapped_event.event_id, name
                        ));
                        break;
                    }
                }
                Event::TimerCreated { .. } => {
                    let futures = open_futures.lock().unwrap();
                    if !futures.contains_key(&wrapped_event.event_id) {
                        non_determinism_error = Some(format!(
                            "Non-determinism detected: TimerCreated event (id={}) found in history, but orchestration did not call schedule_timer()",
                            wrapped_event.event_id
                        ));
                        break;
                    }
                }
                Event::ExternalSubscribed { name, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if !futures.contains_key(&wrapped_event.event_id) {
                        non_determinism_error = Some(format!(
                            "Non-determinism detected: ExternalSubscribed event (id={}, name='{}') found in history, but orchestration did not call schedule_wait()",
                            wrapped_event.event_id, name
                        ));
                        break;
                    }
                }
                Event::SubOrchestrationScheduled { name, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if !futures.contains_key(&wrapped_event.event_id) {
                        non_determinism_error = Some(format!(
                            "Non-determinism detected: SubOrchestrationScheduled event (id={}, name='{}') found in history, but orchestration did not call schedule_sub_orchestration()",
                            wrapped_event.event_id, name
                        ));
                        break;
                    }
                }
                Event::OrchestrationChained { .. } => {
                    // For chained orchestrations, we need to check if it was scheduled
                    // This is a fire-and-forget operation, so no future tracking needed
                    // TODO: We might need to track these separately since they don't have futures
                }
                _ => {}
            }
        }
    }

    // Return the future if it's still pending
    let pending_future = if orchestration_output.is_none() && orchestration_future.is_some() {
        orchestration_future.zip(orchestration_context)
    } else {
        None
    };

    Ok((orchestration_output, non_determinism_error, pending_future))
}

// Create a no-op waker for polling
#[allow(dead_code)]
fn create_noop_waker() -> Waker {
    use std::task::{RawWaker, RawWakerVTable};

    fn noop_clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &NOOP_WAKER_VTABLE)
    }

    fn noop(_: *const ()) {}

    const NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(noop_clone, noop, noop, noop);

    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &NOOP_WAKER_VTABLE)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::event_ids::assign_event_ids_for_delta;
    use serde_json;
    use std::fs;
    use std::path::Path;

    /// Helper function to load test history from JSON file
    fn load_test_history(filename: &str) -> Vec<ReplayHistoryEvent> {
        let test_data_dir = Path::new("src/runtime/test_data");
        let file_path = test_data_dir.join(filename);
        let json_content = fs::read_to_string(&file_path)
            .unwrap_or_else(|_| panic!("Failed to read test data file: {}", file_path.display()));
        serde_json::from_str(&json_content)
            .unwrap_or_else(|_| panic!("Failed to parse JSON from file: {}", file_path.display()))
    }


    #[tokio::test]
    async fn test_replay_simple_activity() {
        // History with activity scheduled and completed
        let history = load_test_history("simple_activity.json");

        let delta_history = vec![];

        // Simple orchestration that calls one activity
        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>(result)
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.decisions.len(), 0); // No new decisions needed
        assert!(output.output.is_some()); // Orchestration completed
        assert_eq!(output.output.unwrap(), Ok::<String, String>("done".to_string()));
    }

    #[tokio::test]
    async fn test_replay_missing_activity_completion() {
        // History with activity scheduled but not completed
        let history = load_test_history("missing_activity_completion.json");

        let delta_history = vec![];

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let _result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>("should not complete".to_string())
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.decisions.len(), 0); // No new decisions since activity is already scheduled
        assert!(output.output.is_none()); // Orchestration not completed
    }

    #[tokio::test]
    async fn test_replay_with_delta_completion() {
        // History with activity scheduled
        let history = load_test_history("replay_with_delta_completion.json");

        // Completion comes in delta
        let base_events = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: 1,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];
        let delta_history = assign_event_ids_for_delta(
            &base_events,
            &vec![Event::ActivityCompleted {
                id: 1,
                result: "done from delta".to_string(),
            }],
        );

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>(result)
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.decisions.len(), 0);
        assert!(output.output.is_some());
        assert_eq!(
            output.output.unwrap(),
            Ok::<String, String>("done from delta".to_string())
        );
    }

    #[tokio::test]
    async fn test_replay_timer() {
        let history = load_test_history("timer.json");

        let delta_history = vec![];

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                ctx.schedule_timer(1000).into_timer().await;
                Ok::<String, String>("timer done".to_string())
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.decisions.len(), 0);
        assert!(output.output.is_some());
        assert_eq!(output.output.unwrap(), Ok::<String, String>("timer done".to_string()));
    }

    #[tokio::test]
    async fn test_replay_no_history() {
        let history = vec![];
        let delta_history = vec![];

        let result = replay_orchestration(
            |_ctx: ReplayOrchestrationContext| async move { Ok::<String, String>("test".to_string()) },
            history,
            delta_history,
        );

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "History must not be empty");
    }

    #[tokio::test]
    async fn test_scheduled_events_dont_emit_decisions() {
        // History with activity already scheduled but not completed
        let history = load_test_history("scheduled_events_dont_emit_decisions.json");

        let delta_history = vec![];

        // Orchestration that schedules the same activity
        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let _result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>("done".to_string())
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        // Should have NO decisions because the activity is already scheduled in history
        assert_eq!(
            output.decisions.len(),
            0,
            "Expected no decisions for already scheduled activity"
        );
        assert!(output.output.is_none()); // Orchestration not completed since activity hasn't returned
    }

    #[tokio::test]
    async fn test_scheduled_event_proper_tracking() {
        // Test that scheduled events in history properly track the future
        // so when completion comes in delta history, it works correctly
        let history = load_test_history("scheduled_event_proper_tracking.json");

        let base_events = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: 1,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

        let delta_history = assign_event_ids_for_delta(
            &base_events,
            &vec![Event::ActivityCompleted {
                id: 1,
                result: "activity result".to_string(),
            }],
        );

        // Orchestration that schedules the same activity
        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>(format!("Got: {}", result))
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        // Should have NO decisions because the activity was already scheduled
        assert_eq!(
            output.decisions.len(),
            0,
            "Expected no decisions for already scheduled activity"
        );
        // Orchestration should complete with delta history
        assert!(output.output.is_some());
        assert_eq!(
            output.output.unwrap(),
            Ok::<String, String>("Got: activity result".to_string())
        );
    }

    #[tokio::test]
    async fn test_nondeterminism_detection() {
        // History shows activity was scheduled
        let history = load_test_history("nondeterminism_detection.json");

        let delta_history = vec![];

        // But orchestration doesn't schedule anything (non-deterministic!)
        let result = replay_orchestration(
            |_ctx: ReplayOrchestrationContext| async move {
                // Oops! Forgot to schedule the activity
                Ok::<String, String>("done".to_string())
            },
            history,
            delta_history,
        );

        println!("Result: {:?}", result);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.non_determinism_error.is_some());
        let err = output.non_determinism_error.unwrap();
        assert!(err.contains("Non-determinism detected"));
        assert!(err.contains("ActivityScheduled"));
        assert!(err.contains("did not call schedule_activity"));
    }

    #[tokio::test]
    async fn test_replay_durable_future_polls_from_open_futures() {
        // This test verifies that ReplayDurableFuture polls from the open_futures table
        // rather than from OrchestrationContext's internal state

        let history = load_test_history("replay_durable_future_polls_from_open_futures.json");

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                // The ReplayDurableFuture should poll from open_futures table
                // which gets marked ready when we process ActivityCompleted event
                let result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>(format!("Completed with: {}", result))
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.decisions.len(), 0); // No new decisions - activity was already scheduled
        assert_eq!(
            output.output,
            Some(Ok("Completed with: result from history".to_string()))
        );
        assert!(output.non_determinism_error.is_none());
    }

    #[tokio::test]
    async fn test_open_futures_stores_completion_data() {
        // Test that completion data is properly stored in OpenFuture when events are processed

        let history = load_test_history("open_futures_stores_completion_data.json");

        // Use the public API
        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let activity_result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                ctx.schedule_timer(1000).into_timer().await;
                Ok::<String, String>(format!("Activity: {}", activity_result))
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.non_determinism_error.is_none());
        assert_eq!(output.output, Some(Ok("Activity: test result".to_string())));

        // Note: We can't inspect open_futures internals anymore as replay_core is private
        // The test now validates the public API behavior
    }

    #[tokio::test]
    async fn test_monotonic_id_generation() {
        // Test that the replay methods generate monotonically increasing IDs
        let history = load_test_history("monotonic_id_generation.json");

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                // Schedule multiple activities - IDs should be generated monotonically
                // The replay engine will handle generating the actions/decisions
                let _act1 = ctx.schedule_activity("activity1", "input1");
                let _act2 = ctx.schedule_activity("activity2", "input2");
                let _timer = ctx.schedule_timer(1000);

                // In the new replay engine, actions are not recorded during scheduling
                // They are generated by the replay engine when processing history events

                Ok::<(), String>(())
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.non_determinism_error.is_none());
        assert_eq!(output.output, Some(Ok(())));
    }

    #[tokio::test]
    async fn test_future_reuse_between_phases() {
        // Test that the orchestration future is reused between phase 1 and phase 2
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Counter to track how many times the orchestration function is called
        static ORCH_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

        let history = load_test_history("future_reuse_between_phases.json");

        let base_events = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: 1,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

        let delta_history = assign_event_ids_for_delta(
            &base_events,
            &vec![Event::ActivityCompleted {
                id: 1,
                result: "done".to_string(),
            }],
        );

        ORCH_CALL_COUNT.store(0, Ordering::SeqCst);

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                // Increment the call count
                ORCH_CALL_COUNT.fetch_add(1, Ordering::SeqCst);

                let result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                Ok::<String, String>(format!("Result: {}", result))
            },
            history,
            delta_history,
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.output, Some(Ok("Result: done".to_string())));
        assert!(output.non_determinism_error.is_none());

        // The orchestration function should only be called once!
        assert_eq!(
            ORCH_CALL_COUNT.load(Ordering::SeqCst),
            1,
            "Orchestration function should only be called once, not recreated in phase 2"
        );
    }

    #[tokio::test]
    async fn replay_e2e_like_hello_world() {
        // HelloWorld orchestration: two activities, return second result
        let history = load_test_history("hello_world.json");

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let _ = ctx.schedule_activity("Hello", "Rust").into_activity().await?;
                let res1 = ctx.schedule_activity("Hello", "World").into_activity().await?;
                Ok::<String, String>(res1)
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.output, Some(Ok("Hello, World!".to_string())));
        assert!(output.non_determinism_error.is_none());
    }

    #[tokio::test]
    async fn replay_e2e_like_control_flow_yes_branch() {
        // ControlFlow orchestration: GetFlag -> yes => SayYes
        let history = load_test_history("control_flow_yes_branch.json");

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let flag = ctx.schedule_activity("GetFlag", "").into_activity().await?;
                if flag == "yes" {
                    Ok::<String, String>(ctx.schedule_activity("SayYes", "").into_activity().await?)
                } else {
                    Ok::<String, String>(ctx.schedule_activity("SayNo", "").into_activity().await?)
                }
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.output, Some(Ok("picked_yes".to_string())));
        assert!(output.non_determinism_error.is_none());
    }

    #[tokio::test]
    async fn replay_e2e_like_loop_three_iterations() {
        // Loop orchestration: Append 3 times accumulating value
        let history = load_test_history("loop_three_iterations.json");

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                let mut acc = String::from("start");
                for _ in 0..3 {
                    acc = ctx.schedule_activity("Append", acc).into_activity().await?;
                }
                Ok::<String, String>(acc)
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.output, Some(Ok("startxxx".to_string())));
        assert!(output.non_determinism_error.is_none());
    }

    #[tokio::test]
    async fn replay_e2e_like_error_handling() {
        // ErrorHandling orchestration: Fragile(bad) => Err, then Recover => recovered
        let history = load_test_history("error_handling.json");

        let result = replay_orchestration(
            |ctx: ReplayOrchestrationContext| async move {
                match ctx.schedule_activity("Fragile", "bad").into_activity().await {
                    Ok(v) => Ok::<String, String>(v),
                    Err(_e) => Ok::<String, String>(ctx.schedule_activity("Recover", "").into_activity().await?),
                }
            },
            history,
            vec![],
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.output, Some(Ok("recovered".to_string())));
        assert!(output.non_determinism_error.is_none());
    }
}
