//! Fresh Execution Tests
//!
//! Tests where the orchestration starts from OrchestrationStarted with no prior schedule events.

use super::helpers::*;

/// Handler returns Ok immediately - no work scheduled.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     Ok("success".to_string())
/// }
/// ```
#[test]
fn immediate_return_ok() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, ImmediateHandler::ok("success"));
    
    assert_completed(&result, "success");
    assert!(engine.pending_actions().is_empty(), "No pending actions expected");
    assert!(engine.history_delta().is_empty(), "No history delta expected");
}

/// Handler returns Err immediately - orchestration fails.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     Err("failure".to_string())
/// }
/// ```
#[test]
fn immediate_return_err() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, ImmediateHandler::err("failure"));
    
    assert_failed(&result);
    assert!(engine.pending_actions().is_empty(), "No pending actions expected");
}

/// Handler schedules an activity and awaits - should yield Continue with pending action.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let result = ctx.schedule_activity("Greet", "Alice").await?;
///     Ok(result)
/// }
/// ```
#[test]
fn schedule_activity_pending() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, SingleActivityHandler::new("Greet", "Alice"));
    
    assert_continue(&result);
    assert_eq!(engine.pending_actions().len(), 1, "One pending action expected");
    assert!(has_activity_action(&engine, "Greet"), "Activity action expected");
    assert_eq!(engine.history_delta().len(), 1, "One history delta expected");
    assert!(has_activity_scheduled_delta(&engine, "Greet"), "ActivityScheduled event expected");
}

/// Handler schedules a timer and awaits - should yield Continue with pending action.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     ctx.schedule_timer(Duration::from_secs(60)).await;
///     Ok("timer_done".to_string())
/// }
/// ```
#[test]
fn schedule_timer_pending() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, SingleTimerHandler::new(std::time::Duration::from_secs(60)));
    
    assert_continue(&result);
    assert_eq!(engine.pending_actions().len(), 1, "One pending action expected");
    assert!(has_timer_action(&engine), "Timer action expected");
    assert_eq!(engine.history_delta().len(), 1, "One history delta expected");
    assert!(has_timer_created_delta(&engine), "TimerCreated event expected");
}

/// Handler waits for external event - should yield Continue with pending action.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let data = ctx.schedule_wait("Approval").await;
///     Ok(data)
/// }
/// ```
#[test]
fn schedule_external_pending() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, WaitExternalHandler::new("Approval"));
    
    assert_continue(&result);
    assert_eq!(engine.pending_actions().len(), 1, "One pending action expected");
    assert!(has_external_action(&engine, "Approval"), "External wait action expected");
    assert_eq!(engine.history_delta().len(), 1, "One history delta expected");
    assert!(has_external_subscribed_delta(&engine, "Approval"), "ExternalSubscribed event expected");
}

/// Handler schedules a sub-orchestration - should yield Continue with pending action.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let result = ctx.schedule_sub_orchestration("ChildOrch", "child-input").await?;
///     Ok(result)
/// }
/// ```
#[test]
fn schedule_sub_orch_pending() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, SubOrchHandler::new("ChildOrch", "child-input"));
    
    assert_continue(&result);
    assert_eq!(engine.pending_actions().len(), 1, "One pending action expected");
    assert!(has_sub_orch_action(&engine, "ChildOrch"), "Sub-orchestration action expected");
}

/// Handler calls continue_as_new - should yield ContinueAsNew.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     ctx.continue_as_new("new-input").await;
///     Ok("unreachable".to_string())
/// }
/// ```
#[test]
fn continue_as_new() {
    let mut engine = create_engine(vec![started_event(1)]);  // OrchestrationStarted
    let result = execute(&mut engine, ContinueAsNewHandler::new("new-input"));
    
    assert_continue_as_new(&result, "new-input");
}

/// Handler schedules multiple activities but returns immediately (doesn't await).
/// Should complete with all activities as pending actions.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let _ = ctx.schedule_activity("A", "a");  // fire-and-forget
///     let _ = ctx.schedule_activity("B", "b");  // fire-and-forget  
///     let _ = ctx.schedule_activity("C", "c");  // fire-and-forget
///     Ok("done".to_string())  // return immediately, activities run in background
/// }
/// ```
#[test]
fn multiple_schedules_no_await() {
    let mut engine = create_engine(vec![started_event(1)]);
    let handler = MultiScheduleNoAwaitHandler::new(vec![
        ("A", "a"),
        ("B", "b"),
        ("C", "c"),
    ]);
    let result = execute(&mut engine, handler);
    
    assert_completed(&result, "done");
    assert_eq!(engine.pending_actions().len(), 3, "Three pending actions expected");
    assert_eq!(engine.history_delta().len(), 3, "Three history delta events expected");
}
