use crate::{Action, DurableOutput, Event, OrchestrationContext};
use std::cell::Cell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use super::replay_context_wrapper::ReplayOrchestrationContext;

/// Maximum number of iterations allowed in the replay loop to prevent infinite loops
const MAX_REPLAY_ITERATIONS: usize = 2000;

/// Output from the replay function
#[derive(Debug)]
pub struct ReplayOutput<O> {
    pub decisions: Vec<Action>,
    pub output: Option<O>,
    /// Non-determinism error if detected during replay
    pub non_determinism_error: Option<String>,
}

/// A DurableFuture variant for replay that holds its own completion state
#[derive(Clone)]
pub(crate) struct ReplayDurableFuture {
    #[allow(dead_code)]
    pub(crate) kind: ReplayFutureKind,
    pub(crate) ready: Arc<Mutex<bool>>,
    pub(crate) completion: Arc<Mutex<Option<DurableOutput>>>,
    pub(crate) should_emit_decision: Arc<Mutex<bool>>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) enum ReplayFutureKind {
    Activity {
        id: u64,
        name: String,
        input: String,
        scheduled: Cell<bool>,
    },
    Timer {
        id: u64,
        delay_ms: u64,
        scheduled: Cell<bool>,
    },
    External {
        id: u64,
        name: String,
        scheduled: Cell<bool>,
    },
    SubOrch {
        id: u64,
        name: String,
        version: Option<String>,
        instance: String,
        input: String,
        scheduled: Cell<bool>,
    },
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
    history: Vec<Event>,
    delta_history: Vec<Event>,
) -> Result<ReplayOutput<O>, String>
where
    F: Fn(ReplayOrchestrationContext) -> Fut,
    Fut: Future<Output = O>,
{
    // Validate we have at least OrchestrationStarted
    if history.is_empty() {
        return Err("History must not be empty".to_string());
    }

    let has_start = history.iter().any(|e| matches!(e, Event::OrchestrationStarted { .. }));
    if !has_start {
        return Err("History must contain OrchestrationStarted event".to_string());
    }

    let open_futures = Arc::new(Mutex::new(HashMap::<u64, ReplayDurableFuture>::new()));
    let mut decisions = Vec::new();

    // Phase 1: Process existing history
    let (mut orchestration_output, mut non_determinism_error, pending_future) =
        replay_core(&orchestration_fn, &history, open_futures.clone(), &mut decisions, None)?;

    // Phase 2: Process delta history if orchestration hasn't completed yet and no error
    if orchestration_output.is_none() && non_determinism_error.is_none() && !delta_history.is_empty() && pending_future.is_some() {
        // Create combined history for execution
        let mut combined_history = history;
        combined_history.extend(delta_history);

        let (output, error, _) = replay_core(
            &orchestration_fn,
            &combined_history,
            open_futures.clone(),
            &mut decisions,
            pending_future,
        )?;
        orchestration_output = output;
        non_determinism_error = error;
    }

    Ok(ReplayOutput {
        decisions,
        output: orchestration_output,
        non_determinism_error,
    })
}

/// Core replay logic that processes a set of events
/// Returns (orchestration_output, non_determinism_error, orchestration_future)
#[allow(dead_code)]
fn replay_core<F, Fut, O>(
    orchestration_fn: &F,
    events: &[Event],
    open_futures: Arc<Mutex<HashMap<u64, ReplayDurableFuture>>>,
    decisions: &mut Vec<Action>,
    existing_future: Option<(Pin<Box<Fut>>, ReplayOrchestrationContext)>,
) -> Result<(Option<O>, Option<String>, Option<(Pin<Box<Fut>>, ReplayOrchestrationContext)>), String>
where
    F: Fn(ReplayOrchestrationContext) -> Fut,
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
    let mut iteration = 0;

    while iteration < MAX_REPLAY_ITERATIONS {
        iteration += 1;
        let mut progress_made = false;

        // Single loop to process all events
        for event in events {
            match event {
                // OrchestrationStarted: Execute the orchestration handler
                Event::OrchestrationStarted { .. } => {
                    if orchestration_future.is_none() {
                        let inner_ctx = OrchestrationContext::new(events.to_vec(), 1);
                        let ctx = ReplayOrchestrationContext::new(inner_ctx, open_futures.clone());
                        orchestration_context = Some(ctx.clone());
                        orchestration_future = Some(Box::pin(orchestration_fn(ctx)));
                    }
                }

                // Scheduled events: If already in open futures, mark as don't emit decision
                Event::ActivityScheduled { id, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        *future.should_emit_decision.lock().unwrap() = false; // Already in history, don't emit
                    }
                }

                Event::TimerCreated { id, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        *future.should_emit_decision.lock().unwrap() = false;
                    }
                }

                Event::ExternalSubscribed { id, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        *future.should_emit_decision.lock().unwrap() = false;
                    }
                }

                Event::SubOrchestrationScheduled { id, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        *future.should_emit_decision.lock().unwrap() = false;
                    }
                }

                // Completion events: Mark futures as ready and store completion data
                Event::ActivityCompleted { id, result } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        let mut ready = future.ready.lock().unwrap();
                        if !*ready {
                            *ready = true;
                            *future.completion.lock().unwrap() = Some(DurableOutput::Activity(Ok(result.clone())));
                            progress_made = true;
                        }
                    }
                }

                Event::ActivityFailed { id, error } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        let mut ready = future.ready.lock().unwrap();
                        if !*ready {
                            *ready = true;
                            *future.completion.lock().unwrap() = Some(DurableOutput::Activity(Err(error.clone())));
                            progress_made = true;
                        }
                    }
                }

                Event::TimerFired { id, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        let mut ready = future.ready.lock().unwrap();
                        if !*ready {
                            *ready = true;
                            *future.completion.lock().unwrap() = Some(DurableOutput::Timer);
                            progress_made = true;
                        }
                    }
                }

                Event::ExternalEvent { id, data, .. } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        let mut ready = future.ready.lock().unwrap();
                        if !*ready {
                            *ready = true;
                            *future.completion.lock().unwrap() = Some(DurableOutput::External(data.clone()));
                            progress_made = true;
                        }
                    }
                }

                Event::SubOrchestrationCompleted { id, result } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        let mut ready = future.ready.lock().unwrap();
                        if !*ready {
                            *ready = true;
                            *future.completion.lock().unwrap() =
                                Some(DurableOutput::SubOrchestration(Ok(result.clone())));
                            progress_made = true;
                        }
                    }
                }

                Event::SubOrchestrationFailed { id, error } => {
                    let futures = open_futures.lock().unwrap();
                    if let Some(future) = futures.get(id) {
                        let mut ready = future.ready.lock().unwrap();
                        if !*ready {
                            *ready = true;
                            *future.completion.lock().unwrap() =
                                Some(DurableOutput::SubOrchestration(Err(error.clone())));
                            progress_made = true;
                        }
                    }
                }

                // Orchestration lifecycle events - these don't affect scheduling/futures
                Event::OrchestrationCompleted { .. } => {
                    // Orchestration completed - this is a terminal event
                    // The orchestration function should have already returned
                }
                
                Event::OrchestrationFailed { .. } => {
                    // Orchestration failed - this is a terminal event
                    // The orchestration function should have already returned with an error
                }
                
                Event::OrchestrationContinuedAsNew { .. } => {
                    // Continue as new - this is a terminal event for this execution
                    // A new execution will be created with the new input
                }
                
                Event::OrchestrationCancelRequested { .. } => {
                    // Cancellation requested - the orchestration should handle this
                    // TODO: We might need to propagate this to the orchestration context
                }
                
                Event::OrchestrationChained { .. } => {
                    // Fire-and-forget orchestration - no future to track
                    // This is just recorded in history but doesn't affect replay
                }
            }
        }

        // At end of loop: if progress was made, poll the orchestration future
        if progress_made || iteration == 1 {
            if let Some(ref mut fut) = orchestration_future {
                if let Some(ref _ctx) = orchestration_context {
                    let waker = create_noop_waker();
                    let mut cx = Context::from_waker(&waker);

                    match fut.as_mut().poll(&mut cx) {
                        Poll::Ready(output) => {
                            orchestration_output = Some(output);
                        }
                        Poll::Pending => {
                            // Continue processing
                        }
                    }

                }
            }

            // Collect actions from context (only those that should be emitted)
            if let Some(ref _ctx) = orchestration_context {
                for action in _ctx.take_actions() {
                        // Check if this action should be emitted based on open_futures
                        let should_emit = match &action {
                            Action::CallActivity { id, .. } |
                            Action::CreateTimer { id, .. } |
                            Action::WaitExternal { id, .. } |
                            Action::StartSubOrchestration { id, .. } => {
                                let futures = open_futures.lock().unwrap();
                                futures
                                    .get(id)
                                    .map_or(true, |f| *f.should_emit_decision.lock().unwrap())
                            }
                            _ => true,
                        };

                    if should_emit && !is_duplicate_decision(&action, decisions) {
                        decisions.push(action);
                    }
                }
            }
        }

        // Check for non-determinism if orchestration completed
        if orchestration_output.is_some() && non_determinism_error.is_none() {
            // Re-process events to check for non-determinism
            for event in events {
                match event {
                    Event::ActivityScheduled { id, name, .. } => {
                        let futures = open_futures.lock().unwrap();
                        if !futures.contains_key(id) {
                            non_determinism_error = Some(format!(
                                "Non-determinism detected: ActivityScheduled event (id={}, name='{}') found in history, but orchestration did not call schedule_activity()",
                                id, name
                            ));
                            break;
                        }
                    }
                    Event::TimerCreated { id, .. } => {
                        let futures = open_futures.lock().unwrap();
                        if !futures.contains_key(id) {
                            non_determinism_error = Some(format!(
                                "Non-determinism detected: TimerCreated event (id={}) found in history, but orchestration did not call schedule_timer()",
                                id
                            ));
                            break;
                        }
                    }
                    Event::ExternalSubscribed { id, name, .. } => {
                        let futures = open_futures.lock().unwrap();
                        if !futures.contains_key(id) {
                            non_determinism_error = Some(format!(
                                "Non-determinism detected: ExternalSubscribed event (id={}, name='{}') found in history, but orchestration did not call schedule_wait()",
                                id, name
                            ));
                            break;
                        }
                    }
                    Event::SubOrchestrationScheduled { id, name, .. } => {
                        let futures = open_futures.lock().unwrap();
                        if !futures.contains_key(id) {
                            non_determinism_error = Some(format!(
                                "Non-determinism detected: SubOrchestrationScheduled event (id={}, name='{}') found in history, but orchestration did not call schedule_sub_orchestration()",
                                id, name
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

        // Stop if orchestration completed or error occurred
        if orchestration_output.is_some() || non_determinism_error.is_some() {
            break;
        }

        // Continue if no progress made and no decisions added after first iteration
        if iteration > 1 && !progress_made {
            break;
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

// Helper to check if decision already exists
#[allow(dead_code)]
fn is_duplicate_decision(action: &Action, existing: &[Action]) -> bool {
    existing.iter().any(|a| match (a, action) {
        (Action::CallActivity { id: id1, .. }, Action::CallActivity { id: id2, .. }) => id1 == id2,
        (Action::CreateTimer { id: id1, .. }, Action::CreateTimer { id: id2, .. }) => id1 == id2,
        (Action::WaitExternal { id: id1, .. }, Action::WaitExternal { id: id2, .. }) => id1 == id2,
        (Action::StartSubOrchestration { id: id1, .. }, Action::StartSubOrchestration { id: id2, .. }) => id1 == id2,
        _ => false,
    })
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

    #[tokio::test]
    async fn test_replay_simple_activity() {
        // History with activity scheduled and completed
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                id: 1,
                result: "done".to_string(),
            },
        ];

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
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

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
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

        // Completion comes in delta
        let delta_history = vec![Event::ActivityCompleted {
            id: 1,
            result: "done from delta".to_string(),
        }];

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
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::TimerCreated {
                id: 1,
                fire_at_ms: 1000,
                execution_id: 1,
            },
            Event::TimerFired {
                id: 1,
                fire_at_ms: 1000,
            },
        ];

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
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

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
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

        let delta_history = vec![Event::ActivityCompleted {
            id: 1,
            result: "activity result".to_string(),
        }];

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
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];

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

        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                id: 1,
                result: "result from history".to_string(),
            },
        ];

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

        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
            Event::ActivityCompleted {
                id: 1,
                result: "test result".to_string(),
            },
            Event::TimerCreated {
                id: 2,
                fire_at_ms: 1000,
                execution_id: 1,
            },
            Event::TimerFired {
                id: 2,
                fire_at_ms: 1000,
            },
        ];

        // Create open_futures to inspect it after replay
        let open_futures = Arc::new(Mutex::new(HashMap::<u64, ReplayDurableFuture>::new()));

        // Manually run the replay_core to get access to open_futures
        let mut decisions = Vec::new();
        let result = replay_core(
            &|ctx: ReplayOrchestrationContext| async move {
                let activity_result = ctx.schedule_activity("my_activity", "{}").into_activity().await?;
                ctx.schedule_timer(1000).into_timer().await;
                Ok::<String, String>(format!("Activity: {}", activity_result))
            },
            &history,
            open_futures.clone(),
            &mut decisions,
            None,
        );

        assert!(result.is_ok());
        let (output, error, _) = result.unwrap();
        if let Some(err) = &error {
            println!("Got error: {}", err);
        }
        assert!(error.is_none());
        assert_eq!(output, Some(Ok("Activity: test result".to_string())));

        // Verify the open_futures table has the correct completion data
        let futures = open_futures.lock().unwrap();

        // Check activity completion
        let activity_future = futures.get(&1).unwrap();
        assert!(*activity_future.ready.lock().unwrap());
        assert_eq!(
            *activity_future.completion.lock().unwrap(),
            Some(DurableOutput::Activity(Ok("test result".to_string())))
        );

        // Check timer completion
        let timer_future = futures.get(&2).unwrap();
        assert!(*timer_future.ready.lock().unwrap());
        assert_eq!(*timer_future.completion.lock().unwrap(), Some(DurableOutput::Timer));
    }
    
    #[tokio::test]
    async fn test_future_reuse_between_phases() {
        // Test that the orchestration future is reused between phase 1 and phase 2
        use std::sync::atomic::{AtomicUsize, Ordering};
        
        // Counter to track how many times the orchestration function is called
        static ORCH_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
        
        let history = vec![
            Event::OrchestrationStarted {
                name: "test".to_string(),
                version: "1.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
            Event::ActivityScheduled {
                id: 1,
                name: "my_activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            },
        ];
        
        let delta_history = vec![
            Event::ActivityCompleted {
                id: 1,
                result: "done".to_string(),
            },
        ];
        
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
        assert_eq!(ORCH_CALL_COUNT.load(Ordering::SeqCst), 1, 
            "Orchestration function should only be called once, not recreated in phase 2");
    }
}
