//! Nondeterminism Detection Tests
//!
//! Tests verifying the engine detects replay mismatches.

use super::helpers::*;

/// History has activity, handler schedules timer - should fail with nondeterminism.
///
/// Original orchestration code (that created history):
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let result = ctx.schedule_activity("Task", "input").await?;
///     Ok(result)
/// }
/// ```
///
/// Changed orchestration code (causes mismatch):
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     ctx.schedule_timer(Duration::from_secs(60)).await;  // Different!
///     Ok("done".to_string())
/// }
/// ```
#[test]
fn schedule_mismatch_activity_vs_timer() {
    let history = vec![
        started_event(1),                        // OrchestrationStarted
        activity_scheduled(2, "Task", "input"), // Original code scheduled activity
    ];
    let mut engine = create_engine(history);
    
    // Handler schedules timer instead of activity
    let result = execute(&mut engine, SingleTimerHandler::new(std::time::Duration::from_secs(60)));
    
    assert_nondeterminism(&result);
}

/// History has timer, handler schedules activity - should fail with nondeterminism.
///
/// Original orchestration scheduled a timer, but code was changed to schedule an activity.
#[test]
fn schedule_mismatch_timer_vs_activity() {
    let history = vec![
        started_event(1),        // OrchestrationStarted
        timer_created(2, 1000),  // Original code scheduled timer
    ];
    let mut engine = create_engine(history);
    
    // Handler schedules activity instead of timer
    let result = execute(&mut engine, SingleActivityHandler::new("Task", "input"));
    
    assert_nondeterminism(&result);
}

/// History has activity with name "A", handler schedules activity "B" - should fail.
///
/// Original orchestration called schedule_activity("ActivityA", ...) but code now
/// calls schedule_activity("ActivityB", ...) - activity name changed.
#[test]
fn schedule_mismatch_wrong_activity_name() {
    let history = vec![
        started_event(1),                          // OrchestrationStarted
        activity_scheduled(2, "ActivityA", "input"), // Original name: "ActivityA"
    ];
    let mut engine = create_engine(history);
    
    // Handler schedules activity with different name
    let result = execute(&mut engine, SingleActivityHandler::new("ActivityB", "input"));
    
    assert_nondeterminism(&result);
}

/// History has activity with input "x", handler schedules with input "y" - should fail.
///
/// Activity input changed between runs - this is nondeterministic.
#[test]
fn schedule_mismatch_wrong_input() {
    let history = vec![
        started_event(1),                          // OrchestrationStarted
        activity_scheduled(2, "Task", "input-x"), // Original input: "input-x"
    ];
    let mut engine = create_engine(history);
    
    // Handler schedules activity with different input
    let result = execute(&mut engine, SingleActivityHandler::new("Task", "input-y"));
    
    assert_nondeterminism(&result);
}

/// History has external subscription "X", handler waits for "Y" - should fail.
///
/// Original code waited for "EventX", but code now waits for "EventY".
#[test]
fn schedule_mismatch_wrong_external_name() {
    let history = vec![
        started_event(1),                // OrchestrationStarted
        external_subscribed(2, "EventX"), // Original: schedule_wait("EventX")
    ];
    let mut engine = create_engine(history);
    
    // Handler waits for different event
    let result = execute(&mut engine, WaitExternalHandler::new("EventY"));
    
    assert_nondeterminism(&result);
}

/// History has schedule, handler returns immediately.
/// This is legitimate because user code can schedule work without awaiting it.
///
/// Orchestration code that produces this history:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let _ = ctx.schedule_activity("Task", "input");  // fire-and-forget, no .await
///     Ok("done".to_string())
/// }
/// ```
#[test]
fn history_schedule_no_emitted_action() {
    // History from a previous run where activity was scheduled but not awaited
    let history = vec![
        started_event(1),              // OrchestrationStarted
        activity_scheduled(2, "Task", "input"),  // from schedule_activity() call
    ];
    let mut engine = create_engine(history);
    
    // Handler returns immediately without scheduling anything
    // This simulates a replay where the original code scheduled but didn't await
    let result = execute(&mut engine, ImmediateHandler::ok("done"));
    
    // Completes successfully - unconsumed schedule events are valid
    // (the original orchestration may have scheduled without awaiting)
    assert_completed(&result, "done");
}

/// Completion message for non-existent schedule - should fail with nondeterminism.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     Ok("done".to_string())  // No activities scheduled
/// }
/// ```
///
/// But a completion arrives for activity id=999 which was never scheduled.
#[test]
fn completion_without_schedule() {
    let history = vec![started_event(1)];  // OrchestrationStarted - nothing scheduled
    let mut engine = create_engine(history);
    
    // Send completion for activity that was never scheduled
    engine.prep_completions(vec![activity_completed_msg(999, "result")]);
    
    // prep_completions should set abort_error
    let result = execute(&mut engine, ImmediateHandler::ok("done"));
    
    assert_nondeterminism(&result);
}

/// Completion kind mismatch - activity completion for timer schedule.
///
/// Orchestration scheduled a timer, but an ActivityCompleted message arrives
/// for that same id. This indicates data corruption or bug.
#[test]
fn completion_kind_mismatch() {
    let history = vec![
        started_event(1),        // OrchestrationStarted
        timer_created(2, 1000),  // Scheduled a timer, not an activity
    ];
    let mut engine = create_engine(history);
    
    // Send activity completion for timer schedule
    engine.prep_completions(vec![activity_completed_msg(2, "result")]);
    
    let result = execute(&mut engine, SingleTimerHandler::new(std::time::Duration::from_secs(60)));
    
    // Should fail - completion kind doesn't match schedule kind
    assert_nondeterminism(&result);
}
