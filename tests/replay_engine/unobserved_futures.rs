//! Unobserved Future Cancellation Tests
//!
//! Tests for cancellation of DurableFuture when dropped without completion.
//!
//! Key design principle: Actions ARE emitted at schedule time (not poll time)
//! because this is a durable orchestration framework. When a future is dropped,
//! the cancellation mechanism kicks in to clean up the in-flight work.
//!
//! Covers three scenarios:
//! 1. Select losers - future loses a race and is dropped
//! 2. Explicitly dropped futures - future dropped before completion
//! 3. Abandoned futures - future polled but dropped before completion

use async_trait::async_trait;
use duroxide::{Either2, Either3, OrchestrationContext, OrchestrationHandler};
use std::sync::Arc;
use std::time::Duration;

use super::helpers::*;

// ============================================================================
// Select Loser Tests
// ============================================================================

/// Handler that does select2 with activity vs timer, timer wins
pub struct Select2TimerWinsHandler {
    activity_name: String,
    activity_input: String,
    timer_duration: Duration,
}

impl Select2TimerWinsHandler {
    pub fn new(name: &str, input: &str, duration: Duration) -> Arc<Self> {
        Arc::new(Self {
            activity_name: name.to_string(),
            activity_input: input.to_string(),
            timer_duration: duration,
        })
    }
}

#[async_trait]
impl OrchestrationHandler for Select2TimerWinsHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let activity = ctx.schedule_activity(&self.activity_name, &self.activity_input);
        let timer = ctx.schedule_timer(self.timer_duration);
        // Timer wins, activity loser should be cancelled
        match ctx.select2(activity, timer).await {
            Either2::First(result) => result.map(|r| format!("activity_won:{r}")),
            Either2::Second(()) => Ok("timer_won".to_string()),
        }
    }
}

/// select2 timer wins - activity should be in cancelled_activity_ids
///
/// Scenario: `ctx.select2(activity, timer)` where timer completes first.
/// The activity future is dropped by select2, triggering cancellation.
/// Note: Activity is scheduled first (emits action), then timer.
#[test]
fn select2_timer_wins_activity_cancelled() {
    // History must match handler's schedule order: activity first, then timer
    let history = vec![
        started_event(1),
        activity_scheduled(2, "LongTask", "input"),  // Activity scheduled first
        timer_created(3, 1000),                      // Timer scheduled second
    ];
    let mut engine = create_engine(history);

    // Timer fires (wins the race)
    engine.prep_completions(vec![timer_fired_msg(3, 1000)]);

    let result = execute(
        &mut engine,
        Select2TimerWinsHandler::new("LongTask", "input", Duration::from_millis(1)),
    );

    assert_completed(&result, "timer_won");

    // The activity should be marked as cancelled
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.contains(&2),
        "Activity schedule_id 2 should be in cancelled list, got {cancelled:?}"
    );
}

/// Handler that does select2 with activity vs timer, activity wins
pub struct Select2ActivityWinsHandler {
    activity_name: String,
    activity_input: String,
    timer_duration: Duration,
}

impl Select2ActivityWinsHandler {
    pub fn new(name: &str, input: &str, duration: Duration) -> Arc<Self> {
        Arc::new(Self {
            activity_name: name.to_string(),
            activity_input: input.to_string(),
            timer_duration: duration,
        })
    }
}

#[async_trait]
impl OrchestrationHandler for Select2ActivityWinsHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let activity = ctx.schedule_activity(&self.activity_name, &self.activity_input);
        let timer = ctx.schedule_timer(self.timer_duration);
        // Activity wins, timer is dropped (but timers don't need explicit cancellation)
        match ctx.select2(activity, timer).await {
            Either2::First(result) => result.map(|r| format!("activity_won:{r}")),
            Either2::Second(()) => Ok("timer_won".to_string()),
        }
    }
}

/// select2 activity wins - timer should NOT be in cancelled list
///
/// Timers are virtual constructs and don't need explicit cancellation.
#[test]
fn select2_activity_wins_timer_not_cancelled() {
    let history = vec![
        started_event(1),
        activity_scheduled(2, "FastTask", "input"),  // Activity scheduled first
        timer_created(3, 5000),                      // Timer scheduled second
    ];
    let mut engine = create_engine(history);

    // Activity completes first
    engine.prep_completions(vec![activity_completed_msg(2, "done")]);

    let result = execute(
        &mut engine,
        Select2ActivityWinsHandler::new("FastTask", "input", Duration::from_millis(5)),
    );

    assert_completed(&result, "activity_won:done");

    // Timer is dropped but timers don't go to cancelled_activity_ids
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.is_empty(),
        "No activities should be cancelled (timer dropped, but timers don't need cancel), got {cancelled:?}"
    );
}

/// select3 with one winner and two losers
pub struct Select3Handler;

#[async_trait]
impl OrchestrationHandler for Select3Handler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let a1 = ctx.schedule_activity("Task1", "input1");
        let a2 = ctx.schedule_activity("Task2", "input2");
        let timer = ctx.schedule_timer(Duration::from_millis(1));
        // Timer wins, both activities dropped
        match ctx.select3(a1, a2, timer).await {
            Either3::First(r) => r,
            Either3::Second(r) => r,
            Either3::Third(()) => Ok("timer_won".to_string()),
        }
    }
}

/// select3 timer wins - both activities should be cancelled
#[test]
fn select3_two_activities_cancelled() {
    let history = vec![
        started_event(1),
        activity_scheduled(2, "Task1", "input1"),
        activity_scheduled(3, "Task2", "input2"),
        timer_created(4, 1000),
    ];
    let mut engine = create_engine(history);

    // Timer fires first
    engine.prep_completions(vec![timer_fired_msg(4, 1000)]);

    let result = execute(&mut engine, Arc::new(Select3Handler));

    assert_completed(&result, "timer_won");

    // Both activities should be cancelled
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.contains(&2) && cancelled.contains(&3),
        "Both activities should be cancelled, got {cancelled:?}"
    );
}

// ============================================================================
// Completed Future Tests
// ============================================================================

/// Completed future should NOT be in cancelled list
#[test]
fn completed_future_not_in_cancelled_list() {
    let history = vec![
        started_event(1),
        activity_scheduled(2, "Task", "input"),
        activity_completed(3, 2, "result"),
    ];
    let mut engine = create_engine(history);

    let result = execute(&mut engine, SingleActivityHandler::new("Task", "input"));

    assert_completed(&result, "result");

    // Activity completed normally, should NOT be cancelled
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.is_empty(),
        "Completed activity should NOT be in cancelled list, got {cancelled:?}"
    );
}

// ============================================================================
// Explicit Drop Tests (After Schedule)
// ============================================================================

/// Handler that schedules an activity then immediately drops the future
pub struct DropActivityAfterScheduleHandler {
    activity_name: String,
    activity_input: String,
}

impl DropActivityAfterScheduleHandler {
    pub fn new(name: &str, input: &str) -> Arc<Self> {
        Arc::new(Self {
            activity_name: name.to_string(),
            activity_input: input.to_string(),
        })
    }
}

#[async_trait]
impl OrchestrationHandler for DropActivityAfterScheduleHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        // Schedule activity (action is emitted)
        let fut = ctx.schedule_activity(&self.activity_name, &self.activity_input);
        // Explicitly drop before awaiting completion
        drop(fut);
        // Complete the orchestration via a different path
        Ok("completed_after_dropping_activity".to_string())
    }
}

/// Explicitly dropped activity - should be in cancelled list
///
/// Actions ARE emitted at schedule time, so the activity will be in history.
/// Dropping the future should add it to cancelled_activity_ids.
#[test]
fn explicit_drop_activity_gets_cancelled() {
    // Fresh execution - activity will be scheduled then dropped
    let history = vec![started_event(1)];
    let mut engine = create_engine(history);

    let result = execute(
        &mut engine,
        DropActivityAfterScheduleHandler::new("DroppedTask", "input"),
    );


    assert_completed(&result, "completed_after_dropping_activity");

    // Activity was scheduled (check history delta)
    assert!(
        has_activity_scheduled_delta(&engine, "DroppedTask"),
        "Activity should be in history delta (emitted at schedule time)"
    );

    // Activity should be in cancelled list
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.contains(&2), // event_id 2 for the scheduled activity
        "Dropped activity should be in cancelled list, got {cancelled:?}"
    );
}

/// Handler that explicitly drops a timer
pub struct ExplicitDropTimerHandler;

#[async_trait]
impl OrchestrationHandler for ExplicitDropTimerHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let timer = ctx.schedule_timer(Duration::from_secs(60));
        drop(timer); // Explicitly drop timer
        Ok("timer_dropped".to_string())
    }
}

/// Explicit drop of timer - no cancellation needed (timers are virtual)
#[test]
fn explicit_drop_timer_no_cancel_needed() {
    let history = vec![started_event(1)];
    let mut engine = create_engine(history);

    let result = execute(&mut engine, Arc::new(ExplicitDropTimerHandler));

    assert_completed(&result, "timer_dropped");

    // Timer should not appear in cancelled_activity_ids (timers are virtual)
    let cancelled = engine.cancelled_activity_ids();
    assert!(cancelled.is_empty(), "No activities cancelled for timer drop");
}

/// Handler that explicitly drops an external wait
pub struct ExplicitDropExternalWaitHandler {
    event_name: String,
}

impl ExplicitDropExternalWaitHandler {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            event_name: name.to_string(),
        })
    }
}

#[async_trait]
impl OrchestrationHandler for ExplicitDropExternalWaitHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let wait = ctx.schedule_wait(&self.event_name);
        drop(wait); // Explicitly drop wait
        Ok("wait_dropped".to_string())
    }
}

/// Explicit drop of external wait - should be cleaned up
#[test]
fn explicit_drop_external_wait() {
    let history = vec![started_event(1)];
    let mut engine = create_engine(history);

    let result = execute(
        &mut engine,
        ExplicitDropExternalWaitHandler::new("MyEvent"),
    );

    assert_completed(&result, "wait_dropped");

    // External wait should not appear in activity cancel list (it's not an activity)
    let cancelled = engine.cancelled_activity_ids();
    assert!(cancelled.is_empty(), "External waits don't go to activity cancel list");
}

// ============================================================================
// Multiple Dropped Futures Tests
// ============================================================================

/// Handler that creates multiple futures and drops all of them
pub struct MultipleDroppedActivitiesHandler;

#[async_trait]
impl OrchestrationHandler for MultipleDroppedActivitiesHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        // Create several futures (actions emitted at schedule time)
        let _a = ctx.schedule_activity("Task1", "input1");
        let _b = ctx.schedule_activity("Task2", "input2");
        let _c = ctx.schedule_timer(Duration::from_secs(10));
        // All dropped here without await
        Ok("all_dropped".to_string())
    }
}

/// Multiple dropped activities - all activities should be cancelled (not timer)
#[test]
fn multiple_dropped_activities_all_cancelled() {
    let history = vec![started_event(1)];
    let mut engine = create_engine(history);

    let result = execute(&mut engine, Arc::new(MultipleDroppedActivitiesHandler));

    assert_completed(&result, "all_dropped");

    // Both activities should be in cancelled list (but not the timer)
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.contains(&2) && cancelled.contains(&3),
        "Both activities should be cancelled, got {cancelled:?}"
    );
    // Timer (event_id 4) should NOT be in cancelled list
    assert!(
        !cancelled.contains(&4),
        "Timer should NOT be in cancelled list"
    );
}

// ============================================================================
// Sub-Orchestration Cancellation Tests
// ============================================================================

/// Handler that drops a sub-orchestration future after scheduling
pub struct DropSubOrchAfterScheduleHandler {
    sub_name: String,
    sub_input: String,
}

impl DropSubOrchAfterScheduleHandler {
    pub fn new(name: &str, input: &str) -> Arc<Self> {
        Arc::new(Self {
            sub_name: name.to_string(),
            sub_input: input.to_string(),
        })
    }
}

#[async_trait]
impl OrchestrationHandler for DropSubOrchAfterScheduleHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let sub_orch = ctx.schedule_sub_orchestration(&self.sub_name, &self.sub_input);
        drop(sub_orch); // Drop after schedule
        Ok("sub_orch_dropped".to_string())
    }
}

/// Drop sub-orchestration after schedule - child should be in cancelled list
#[test]
fn sub_orch_drop_after_schedule_cancelled() {
    let history = vec![started_event(1)];
    let mut engine = create_engine(history);

    let result = execute(
        &mut engine,
        DropSubOrchAfterScheduleHandler::new("ChildOrch", "child_input"),
    );

    assert_completed(&result, "sub_orch_dropped");

    // Sub-orchestration should be in cancelled list
    let cancelled = engine.cancelled_sub_orchestration_ids();
    assert!(
        !cancelled.is_empty(),
        "Child sub-orchestration should be in cancelled list, got {cancelled:?}"
    );
}

/// Handler that polls a sub-orchestration then drops it via select2
pub struct SubOrchSelectLoserHandler {
    sub_name: String,
    sub_input: String,
    timer_duration: Duration,
}

impl SubOrchSelectLoserHandler {
    pub fn new(name: &str, input: &str, duration: Duration) -> Arc<Self> {
        Arc::new(Self {
            sub_name: name.to_string(),
            sub_input: input.to_string(),
            timer_duration: duration,
        })
    }
}

#[async_trait]
impl OrchestrationHandler for SubOrchSelectLoserHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let sub_orch = ctx.schedule_sub_orchestration(&self.sub_name, &self.sub_input);
        let timer = ctx.schedule_timer(self.timer_duration);
        // Timer wins, sub-orchestration is dropped
        match ctx.select2(sub_orch, timer).await {
            Either2::First(result) => result,
            Either2::Second(()) => Ok("timer_won_sub_orch_cancelled".to_string()),
        }
    }
}

/// Sub-orchestration select loser - child should be in cancelled_sub_orchestration_ids
#[test]
fn sub_orch_select_loser_child_cancelled() {
    // History must match handler's schedule order: sub_orch first, timer second
    let history = vec![
        started_event(1),
        sub_orch_scheduled(2, "ChildOrch", "sub::2", "child_input"),
        timer_created(3, 1000),
    ];
    let mut engine = create_engine(history);

    // Timer fires first
    engine.prep_completions(vec![timer_fired_msg(3, 1000)]);

    let result = execute(
        &mut engine,
        SubOrchSelectLoserHandler::new("ChildOrch", "child_input", Duration::from_millis(1)),
    );

    assert_completed(&result, "timer_won_sub_orch_cancelled");

    // Sub-orchestration should be in cancelled list
    let cancelled = engine.cancelled_sub_orchestration_ids();
    assert!(
        cancelled.iter().any(|id| id == "sub::2"),
        "Child sub-orchestration should be cancelled, got {cancelled:?}"
    );
}

// ============================================================================
// Replay Determinism Tests
// ============================================================================

/// Replay with select loser should produce same cancellation
#[test]
fn replay_select_loser_same_cancellation() {
    // Replay scenario: history has completed turn with timer winning
    let history = vec![
        started_event(1),
        activity_scheduled(2, "Task", "input"),
        timer_created(3, 1000),
        timer_fired(4, 3, 1000),  // Timer completed in history
    ];
    let mut engine = create_engine(history);

    let result = execute(
        &mut engine,
        Select2TimerWinsHandler::new("Task", "input", Duration::from_millis(1)),
    );

    assert_completed(&result, "timer_won");

    // During replay, same cancellation should occur
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.contains(&2),
        "Replay should still cancel the activity"
    );
}

/// Completed sub-orchestration should NOT be in cancelled list
#[test]
fn completed_sub_orch_not_cancelled() {
    let history = vec![
        started_event(1),
        sub_orch_scheduled(2, "ChildOrch", "sub::2", "input"),
        sub_orch_completed(3, 2, "child_result"),
    ];
    let mut engine = create_engine(history);

    let result = execute(&mut engine, SubOrchHandler::new("ChildOrch", "input"));

    assert_completed(&result, "child_result");

    // Completed sub-orch should NOT be in cancelled list
    let cancelled = engine.cancelled_sub_orchestration_ids();
    assert!(
        cancelled.is_empty(),
        "Completed sub-orchestration should NOT be cancelled, got {cancelled:?}"
    );
}

// ============================================================================
// Dehydration Tests - Suspended orchestrations should NOT trigger cancellation
// ============================================================================

/// Handler that awaits an activity (will block/dehydrate if no completion)
#[allow(dead_code)]
pub struct AwaitActivityHandler {
    activity_name: String,
    activity_input: String,
}

#[allow(dead_code)]
impl AwaitActivityHandler {
    pub fn new(name: &str, input: &str) -> Arc<Self> {
        Arc::new(Self {
            activity_name: name.to_string(),
            activity_input: input.to_string(),
        })
    }
}

#[async_trait]
impl OrchestrationHandler for AwaitActivityHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        let result = ctx.schedule_activity(&self.activity_name, &self.activity_input).await?;
        Ok(format!("got:{result}"))
    }
}

/// Handler that drops an activity then awaits a timer (completes within turn)
pub struct DropActivityThenAwaitTimerHandler;

#[async_trait]
impl OrchestrationHandler for DropActivityThenAwaitTimerHandler {
    async fn invoke(&self, ctx: OrchestrationContext, _input: String) -> Result<String, String> {
        // Schedule and immediately drop the activity
        let activity = ctx.schedule_activity("DroppedTask", "input");
        drop(activity);
        
        // Now await a timer that completes - orchestration finishes this turn
        ctx.schedule_timer(Duration::from_millis(100)).await;
        Ok("done_after_drop".to_string())
    }
}

/// Explicit drop mid-execution SHOULD trigger cancellation.
///
/// This proves the cancellation mechanism works: when a future is dropped
/// while the orchestration is still running (before returning), the cancellation
/// is collected. Even if the orchestration then dehydrates waiting for something
/// else, the earlier explicit drop is still captured.
#[test]
fn explicit_drop_then_await_captures_cancellation() {
    // History: activity scheduled, timer scheduled, but timer NOT fired yet
    // Orchestration will dehydrate waiting for timer
    let history = vec![
        started_event(1),
        activity_scheduled(2, "DroppedTask", "input"),
        timer_created(3, 100),
        // No timer_fired - orchestration will dehydrate
    ];
    let mut engine = create_engine(history);

    let result = execute(&mut engine, Arc::new(DropActivityThenAwaitTimerHandler));

    // Orchestration dehydrates waiting for timer
    assert_continue(&result);

    // The explicitly dropped activity SHOULD still be in cancelled list
    // even though orchestration didn't complete this turn
    let cancelled = engine.cancelled_activity_ids();
    assert!(
        cancelled.contains(&2),
        "Explicitly dropped activity should be cancelled even when orchestration dehydrates, got {cancelled:?}"
    );
}