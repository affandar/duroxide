//! Sequential Progress Tests
//!
//! Tests verifying multi-step orchestration replay.

use super::helpers::*;
use std::time::Duration;

/// Two activities: first one done, second should be scheduled.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let r1 = ctx.schedule_activity("A1", "input1").await?;  // Completed
///     let r2 = ctx.schedule_activity("A2", "input2").await?;  // Scheduled now
///     Ok(format!("{},{}", r1, r2))
/// }
/// ```
#[test]
fn two_activities_first_done() {
    let history = vec![
        started_event(1),                        // OrchestrationStarted
        activity_scheduled(2, "A1", "input1"),  // 1st schedule_activity()
        activity_completed(3, 2, "result1"),    // 1st activity done
    ];
    let mut engine = create_engine(history);
    let handler = TwoActivitiesHandler::new(("A1", "input1"), ("A2", "input2"));
    let result = execute(&mut engine, handler);
    
    assert_continue(&result);
    assert_eq!(engine.pending_actions().len(), 1, "Second activity should be pending");
    assert!(has_activity_action(&engine, "A2"), "A2 should be in pending actions");
}

/// Two activities: both done, should complete.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let r1 = ctx.schedule_activity("A1", "input1").await?;
///     let r2 = ctx.schedule_activity("A2", "input2").await?;
///     Ok(format!("{},{}", r1, r2))
/// }
/// ```
#[test]
fn two_activities_both_done() {
    // History from a completed multi-step execution:
    // Turn 1: Started, scheduled A1, yielded
    // Turn 2: A1 completed, scheduled A2, yielded  
    // Turn 3: A2 completed, orchestration returned
    let history = vec![
        started_event(1),                        // OrchestrationStarted
        activity_scheduled(2, "A1", "input1"),  // 1st schedule_activity()
        activity_completed(3, 2, "result1"),    // 1st activity done
        activity_scheduled(4, "A2", "input2"),  // 2nd schedule_activity()
        activity_completed(5, 4, "result2"),    // 2nd activity done
    ];
    let mut engine = create_engine(history);
    let handler = TwoActivitiesHandler::new(("A1", "input1"), ("A2", "input2"));
    let result = execute(&mut engine, handler);
    
    assert_completed(&result, "result1,result2");
}

/// Activity completed, then timer - should schedule timer.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let r = ctx.schedule_activity("Task", "input").await?;  // Completed
///     ctx.schedule_timer(Duration::from_secs(60)).await;       // Scheduled now
///     Ok(r)
/// }
/// ```
#[test]
fn activity_then_timer() {
    let history = vec![
        started_event(1),                     // OrchestrationStarted
        activity_scheduled(2, "Task", "input"), // schedule_activity()
        activity_completed(3, 2, "done"),    // activity done
    ];
    let mut engine = create_engine(history);
    let handler = ActivityThenTimerHandler::new("Task", "input", Duration::from_secs(60));
    let result = execute(&mut engine, handler);
    
    assert_continue(&result);
    assert!(has_timer_action(&engine), "Timer should be pending");
}

/// Timer fired, then activity - should schedule activity.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     ctx.schedule_timer(Duration::from_secs(60)).await;  // Fired
///     let r = ctx.schedule_activity("Task", "input").await?;  // Scheduled now
///     Ok(r)
/// }
/// ```
#[test]
fn timer_then_activity() {
    use duroxide::{OrchestrationContext, OrchestrationHandler};
    use async_trait::async_trait;
    use std::sync::Arc;
    
    struct TimerThenActivityHandler;
    
    #[async_trait]
    impl OrchestrationHandler for TimerThenActivityHandler {
        async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
            ctx.schedule_timer(Duration::from_secs(60)).await;
            let r = ctx.schedule_activity("Task", "input").await?;
            Ok(r)
        }
    }
    
    let history = vec![
        started_event(1),
        timer_created(2, 1000),
        timer_fired(3, 2, 1000),
    ];
    let mut engine = create_engine(history);
    let result = execute(&mut engine, Arc::new(TimerThenActivityHandler));
    
    assert_continue(&result);
    assert!(has_activity_action(&engine, "Task"), "Activity should be pending");
}

/// Many sequential activities - all complete.
///
/// Orchestration code:
/// ```ignore
/// async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
///     let mut results = Vec::new();
///     for i in 0..10 {
///         let r = ctx.schedule_activity(&format!("Act{i}"), &format!("input{i}")).await?;
///         results.push(r);
///     }
///     Ok(results.join(","))
/// }
/// ```
#[test]
fn many_sequential_activities() {
    use duroxide::{OrchestrationContext, OrchestrationHandler};
    use async_trait::async_trait;
    use std::sync::Arc;
    
    struct TenActivitiesHandler;
    
    #[async_trait]
    impl OrchestrationHandler for TenActivitiesHandler {
        async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
            let mut results = Vec::new();
            for i in 0..10 {
                let r = ctx.schedule_activity(&format!("Act{i}"), &format!("input{i}")).await?;
                results.push(r);
            }
            Ok(results.join(","))
        }
    }
    
    // Build history with 10 activities scheduled and completed
    let mut history = vec![started_event(1)];
    let mut event_id = 2u64;
    for i in 0..10 {
        let sched_id = event_id;
        history.push(activity_scheduled(event_id, &format!("Act{i}"), &format!("input{i}")));
        event_id += 1;
        history.push(activity_completed(event_id, sched_id, &format!("result{i}")));
        event_id += 1;
    }
    
    let mut engine = create_engine(history);
    let result = execute(&mut engine, Arc::new(TenActivitiesHandler));
    
    let expected: Vec<String> = (0..10).map(|i| format!("result{i}")).collect();
    assert_completed(&result, &expected.join(","));
}
