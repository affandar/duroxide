//! Orchestration Stall Tests
//!
//! MIGRATION NOTE: These tests use run_turn() which is the legacy in-memory replay API.
//! They test edge cases in the legacy cursor-based replay logic which has been replaced
//! by the simplified event-processing replay engine.
//!
//! All tests in this file are marked as #[ignore] because:
//! 1. They depend on run_turn() which uses legacy replay mode
//! 2. The behaviors they test are handled differently in simplified mode
//! 3. Equivalent coverage exists in the runtime e2e tests
//!
//! Original purpose:
//! These tests verify that orchestrations with valid histories always terminate properly
//! and never hang due to unconsumed completion events.
//!
//! **Bug Context:**
//! When `select2(activity, timer)` returns with activity winning, the timer's eventual
//! `TimerFired` completion was never consumed. These "stale" completions blocked later
//! completions due to FIFO ordering enforcement in `can_consume_completion()`.
//!
//! **Fix (legacy mode):**
//! Loser `source_event_id`s are marked as "cancelled" when select2 returns. Their
//! completions are automatically skipped in FIFO ordering checks.

#![allow(dead_code)] // Tests are ignored but code should still compile

use duroxide::{Event, EventKind, OrchestrationContext, run_turn};
use std::time::Duration;

/// Core repro: Two select2s with activity winners create stale timers that
/// should NOT block a subsequent timer in a third select2.
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn select2_loser_timer_does_not_block_later_timer() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // Step 1: First select2 - activity races with timeout timer
        let activity1 = ctx.schedule_activity("FastActivity", "input1");
        let timeout1 = ctx.schedule_timer(Duration::from_secs(15));
        let (winner1, _) = ctx.select2(activity1, timeout1).await;
        assert_eq!(winner1, 0, "activity should win the first race");

        // Step 2: Second select2 - another activity with timeout
        let activity2 = ctx.schedule_activity("FastActivity", "input2");
        let timeout2 = ctx.schedule_timer(Duration::from_secs(30));
        let (winner2, _) = ctx.select2(activity2, timeout2).await;
        assert_eq!(winner2, 0, "activity should win the second race");

        // Step 3: Wait with select2(timer, external)
        // Stale TimerFired events from steps 1 and 2 should NOT block this
        let sleep_timer = ctx.schedule_timer(Duration::from_secs(30));
        let deletion_signal = ctx.schedule_wait("InstanceDeleted");
        let (winner3, _) = ctx.select2(sleep_timer, deletion_signal).await;

        if winner3 == 0 { "timer_fired" } else { "signal_received" }
    };

    // History with stale timers from select2 losers
    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "test".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // First select2: activity scheduled (event 2)
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "FastActivity".to_string(),
                input: "input1".to_string(),
            },
        ),
        // First select2: timeout timer created (event 3)
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 15000 }),
        // First activity completes (event 4) - WINS the race
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "result1".to_string(),
            },
        ),
        // Second select2: activity scheduled (event 5)
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "FastActivity".to_string(),
                input: "input2".to_string(),
            },
        ),
        // Second select2: timeout timer created (event 6)
        Event::with_event_id(6, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        // Second activity completes (event 7) - WINS the race
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "result2".to_string(),
            },
        ),
        // Third select2: sleep timer created (event 8)
        Event::with_event_id(8, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        // External wait subscription (event 9)
        Event::with_event_id(
            9,
            inst.clone(),
            1,
            None,
            EventKind::ExternalSubscribed {
                name: "InstanceDeleted".to_string(),
            },
        ),
        // First stale timer fires (event 10) - from first select2, LOSER
        Event::with_event_id(
            10,
            inst.clone(),
            1,
            Some(3),
            EventKind::TimerFired { fire_at_ms: 15000 },
        ),
        // Second stale timer fires (event 11) - from second select2, LOSER
        Event::with_event_id(
            11,
            inst.clone(),
            1,
            Some(6),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        // Third timer fires (event 12) - from third select2, should be consumed
        Event::with_event_id(
            12,
            inst.clone(),
            1,
            Some(8),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
    ];

    let (final_history, actions, output) = run_turn(history, orchestrator);

    // Debug output
    eprintln!("\n=== select2_loser_timer_does_not_block_later_timer ===");
    eprintln!("Final History: {} events", final_history.len());
    eprintln!("Actions: {}", actions.len());
    eprintln!("Output: {output:?}");

    assert!(
        output.is_some(),
        "Orchestration should complete! Stale loser timers should not block."
    );
    assert_eq!(output.unwrap(), "timer_fired");
}

/// Single stale loser - minimal repro case
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn single_stale_loser_handled() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // select2 where activity wins
        let activity = ctx.schedule_activity("Fast", "input");
        let timeout = ctx.schedule_timer(Duration::from_secs(10));
        let (winner, _) = ctx.select2(activity, timeout).await;
        assert_eq!(winner, 0);

        // Await another timer - should not be blocked by stale timeout
        ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;

        "completed"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Fast".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 10000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "done".to_string(),
            },
        ),
        Event::with_event_id(5, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 5000 }),
        // Stale timer from select2 loser
        Event::with_event_id(6, inst.clone(), 1, Some(3), EventKind::TimerFired { fire_at_ms: 10000 }),
        // Timer we want to consume
        Event::with_event_id(7, inst.clone(), 1, Some(5), EventKind::TimerFired { fire_at_ms: 5000 }),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Should complete, stale timer should not block");
    assert_eq!(output.unwrap(), "completed");
}

/// Timer wins select2, stale activity should not block later timer
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn select2_loser_activity_does_not_block_later_timer() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // select2 where timer wins (activity is slower)
        let activity = ctx.schedule_activity("SlowActivity", "input");
        let timeout = ctx.schedule_timer(Duration::from_secs(1));
        let (winner, _) = ctx.select2(activity, timeout).await;
        assert_eq!(winner, 1, "timer should win");

        // Await another timer - should not be blocked by stale activity
        ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;

        "completed"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "SlowActivity".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 1000 }),
        // Timer fires first - WINS
        Event::with_event_id(4, inst.clone(), 1, Some(3), EventKind::TimerFired { fire_at_ms: 1000 }),
        // Second timer scheduled
        Event::with_event_id(5, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 5000 }),
        // Stale activity completes (loser)
        Event::with_event_id(
            6,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "slow_result".to_string(),
            },
        ),
        // Second timer fires - should be consumable
        Event::with_event_id(7, inst.clone(), 1, Some(5), EventKind::TimerFired { fire_at_ms: 5000 }),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Should complete, stale activity should not block");
    assert_eq!(output.unwrap(), "completed");
}

/// Real-world pattern: schedule_activity_with_retry internally uses select2
/// Multiple retry calls should not accumulate blocking stale timers
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn multiple_retry_pattern_then_timer_completes() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // Simulate 3x schedule_activity_with_retry (each uses select2 internally)
        for i in 0..3 {
            let activity = ctx.schedule_activity("Task", format!("input{i}"));
            let timeout = ctx.schedule_timer(Duration::from_secs(30));
            let (winner, _) = ctx.select2(activity, timeout).await;
            assert_eq!(winner, 0, "activity should win retry {i}");
        }

        // Final await - should not be blocked by 3 stale timeout timers
        ctx.schedule_timer(Duration::from_secs(10)).into_timer().await;

        "all_retries_done"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // Retry 1
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input0".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // Retry 2
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input1".to_string(),
            },
        ),
        Event::with_event_id(6, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // Retry 3
        Event::with_event_id(
            8,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input2".to_string(),
            },
        ),
        Event::with_event_id(9, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            10,
            inst.clone(),
            1,
            Some(8),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // Final timer
        Event::with_event_id(11, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 10000 }),
        // Stale timers from all 3 retries
        Event::with_event_id(
            12,
            inst.clone(),
            1,
            Some(3),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        Event::with_event_id(
            13,
            inst.clone(),
            1,
            Some(6),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        Event::with_event_id(
            14,
            inst.clone(),
            1,
            Some(9),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        // Final timer fires
        Event::with_event_id(
            15,
            inst.clone(),
            1,
            Some(11),
            EventKind::TimerFired { fire_at_ms: 10000 },
        ),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Should complete despite 3 stale timers");
    assert_eq!(output.unwrap(), "all_retries_done");
}

/// Instance actor pattern (toygres): retry → retry → select2(sleep, signal)
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn instance_actor_pattern_completes() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // First retry with timeout (activity wins)
        let a1 = ctx.schedule_activity("GetConnection", "input");
        let t1 = ctx.schedule_timer(Duration::from_secs(15));
        ctx.select2(a1, t1).await;

        // Second retry with timeout (activity wins)
        let a2 = ctx.schedule_activity("TestConnection", "input");
        let t2 = ctx.schedule_timer(Duration::from_secs(30));
        ctx.select2(a2, t2).await;

        // Sleep OR deletion signal
        let sleep = ctx.schedule_timer(Duration::from_secs(30));
        let signal = ctx.schedule_wait("InstanceDeleted");
        let (winner, _) = ctx.select2(sleep, signal).await;

        if winner == 0 { "sleep_done" } else { "deleted" }
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "InstanceActor".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // First retry
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "GetConnection".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 15000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "conn".to_string(),
            },
        ),
        // Second retry
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "TestConnection".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(6, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // Sleep/signal select2
        Event::with_event_id(8, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            9,
            inst.clone(),
            1,
            None,
            EventKind::ExternalSubscribed {
                name: "InstanceDeleted".to_string(),
            },
        ),
        // Stale timers from retries
        Event::with_event_id(
            10,
            inst.clone(),
            1,
            Some(3),
            EventKind::TimerFired { fire_at_ms: 15000 },
        ),
        Event::with_event_id(
            11,
            inst.clone(),
            1,
            Some(6),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        // Sleep timer fires
        Event::with_event_id(
            12,
            inst.clone(),
            1,
            Some(8),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Instance actor pattern should complete");
    assert_eq!(output.unwrap(), "sleep_done");
}

/// Both select2 children have completions, first argument wins
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn both_ready_first_argument_wins() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let activity = ctx.schedule_activity("Fast", "input");
        let timer = ctx.schedule_timer(Duration::from_secs(1));
        let (winner, _) = ctx.select2(activity, timer).await;

        // Activity was passed first, so it wins
        if winner == 0 { "activity_won" } else { "timer_won" }
    };

    // Both complete, but activity is first in select2 order
    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Fast".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 1000 }),
        // Both complete - activity has lower event_id, consumable first
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "done".to_string(),
            },
        ),
        Event::with_event_id(5, inst.clone(), 1, Some(3), EventKind::TimerFired { fire_at_ms: 1000 }),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some());
    assert_eq!(output.unwrap(), "activity_won");
}

// =============================================================================
// Continue-As-New Boundary Tests
// =============================================================================

/// After continue_as_new, execution 2 starts with fresh state.
/// Stale completions from execution 1 are filtered by execution_id mismatch,
/// NOT by cancelled_source_ids (which resets with new execution).
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn new_execution_starts_clean() {
    use duroxide::run_turn_with;

    // This orchestrator is for execution 2 (after CAN)
    let orchestrator = |ctx: OrchestrationContext| async move {
        // In execution 2, we just do a simple select2
        let activity = ctx.schedule_activity("Task", "input");
        let timeout = ctx.schedule_timer(Duration::from_secs(10));
        let (winner, _) = ctx.select2(activity, timeout).await;

        if winner == 0 { "activity_won" } else { "timeout" }
    };

    // History for execution 2 - note execution_id: 2 on scheduling events
    // The history does NOT include stale completions from execution 1 because
    // each execution has its own history. This test verifies the orchestrator
    // works correctly with a fresh execution.
    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            2,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "continued".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        Event::with_event_id(
            2,
            inst.clone(),
            2,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 2, None, EventKind::TimerCreated { fire_at_ms: 10000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            2,
            Some(2),
            EventKind::ActivityCompleted {
                result: "done".to_string(),
            },
        ),
    ];

    // Run as execution 2
    let (_history, _actions, output) = run_turn_with(
        history,
        2, // execution_id = 2
        "test-instance".to_string(),
        "Test".to_string(),
        "1.0.0".to_string(),
        orchestrator,
    );

    assert!(output.is_some(), "Execution 2 should complete cleanly");
    assert_eq!(output.unwrap(), "activity_won");
}

/// Stale completion arrives in same execution (before CAN boundary).
/// This is handled by cancelled_source_ids mechanism.
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn stale_completion_same_execution_handled() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // First select2 - activity wins, timer becomes stale
        let a1 = ctx.schedule_activity("Fast", "input");
        let t1 = ctx.schedule_timer(Duration::from_secs(30));
        let (w1, _) = ctx.select2(a1, t1).await;
        assert_eq!(w1, 0, "activity should win");

        // Do some work before CAN - the stale timer fires during this
        let a2 = ctx.schedule_activity("MoreWork", "data");
        a2.into_activity().await.unwrap();

        // This would normally call ctx.continue_as_new("next"), but for this test
        // we just complete. The key is that stale TimerFired from t1 doesn't block a2.
        "completed_before_can"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // First select2
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Fast".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "fast_done".to_string(),
            },
        ),
        // Second activity
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "MoreWork".to_string(),
                input: "data".to_string(),
            },
        ),
        // Stale timer fires (from select2 loser) - this should NOT block
        Event::with_event_id(6, inst.clone(), 1, Some(3), EventKind::TimerFired { fire_at_ms: 30000 }),
        // Second activity completes
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "more_done".to_string(),
            },
        ),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Stale timer should not block subsequent activity");
    assert_eq!(output.unwrap(), "completed_before_can");
}

// =============================================================================
// Composition Tests - Sequential Selects
// =============================================================================

/// Sequential select2s: first select2 loser shouldn't block second select2
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn sequential_select2s_loser_from_first_does_not_block_second() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // First select2: activity wins
        let a1 = ctx.schedule_activity("Fast1", "input");
        let t1 = ctx.schedule_timer(Duration::from_secs(100));
        let (w1, _) = ctx.select2(a1, t1).await;
        assert_eq!(w1, 0, "activity should win first select2");

        // Second select2: timer wins
        let a2 = ctx.schedule_activity("Slow2", "input");
        let t2 = ctx.schedule_timer(Duration::from_secs(1));
        let (w2, _) = ctx.select2(a2, t2).await;

        if w2 == 0 {
            "second_activity_won"
        } else {
            "second_timer_won"
        }
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // First select2
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Fast1".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 100000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "done1".to_string(),
            },
        ),
        // Second select2
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Slow2".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(6, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 1000 }),
        // Stale timer from first select2
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(3),
            EventKind::TimerFired { fire_at_ms: 100000 },
        ),
        // Second timer wins
        Event::with_event_id(8, inst.clone(), 1, Some(6), EventKind::TimerFired { fire_at_ms: 1000 }),
        // Stale activity from second select2
        Event::with_event_id(
            9,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "done2".to_string(),
            },
        ),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Sequential select2s should complete");
    assert_eq!(output.unwrap(), "second_timer_won");
}

/// Join followed by select2 - join completes, then select2 races
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn join_then_select2_completes() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // First: join two activities
        let a1 = ctx.schedule_activity("Task1", "input1");
        let a2 = ctx.schedule_activity("Task2", "input2");
        let _results = ctx.join(vec![a1, a2]).await;

        // Then: select2 with activity winning
        let a3 = ctx.schedule_activity("Task3", "input3");
        let timeout = ctx.schedule_timer(Duration::from_secs(30));
        let (winner, _) = ctx.select2(a3, timeout).await;

        if winner == 0 { "activity_won" } else { "timeout" }
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // Join activities
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task1".to_string(),
                input: "input1".to_string(),
            },
        ),
        Event::with_event_id(
            3,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task2".to_string(),
                input: "input2".to_string(),
            },
        ),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "r1".to_string(),
            },
        ),
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            Some(3),
            EventKind::ActivityCompleted {
                result: "r2".to_string(),
            },
        ),
        // Select2
        Event::with_event_id(
            6,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task3".to_string(),
                input: "input3".to_string(),
            },
        ),
        Event::with_event_id(7, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            8,
            inst.clone(),
            1,
            Some(6),
            EventKind::ActivityCompleted {
                result: "r3".to_string(),
            },
        ),
        // Stale timeout
        Event::with_event_id(9, inst.clone(), 1, Some(7), EventKind::TimerFired { fire_at_ms: 30000 }),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "join then select2 should complete");
    assert_eq!(output.unwrap(), "activity_won");
}

/// select2 followed by join - select2 loser shouldn't block join
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn select2_then_join_completes() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // First: select2 with activity winning
        let a1 = ctx.schedule_activity("Fast", "input");
        let timeout = ctx.schedule_timer(Duration::from_secs(30));
        let (winner, _) = ctx.select2(a1, timeout).await;
        assert_eq!(winner, 0, "activity should win");

        // Then: join two activities
        let a2 = ctx.schedule_activity("Task2", "input2");
        let a3 = ctx.schedule_activity("Task3", "input3");
        let _results = ctx.join(vec![a2, a3]).await;

        "join_completed"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // Select2
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Fast".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "done".to_string(),
            },
        ),
        // Join activities
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task2".to_string(),
                input: "input2".to_string(),
            },
        ),
        Event::with_event_id(
            6,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task3".to_string(),
                input: "input3".to_string(),
            },
        ),
        // Stale timer from select2 (should not block join)
        Event::with_event_id(7, inst.clone(), 1, Some(3), EventKind::TimerFired { fire_at_ms: 30000 }),
        // Join completions
        Event::with_event_id(
            8,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "r2".to_string(),
            },
        ),
        Event::with_event_id(
            9,
            inst.clone(),
            1,
            Some(6),
            EventKind::ActivityCompleted {
                result: "r3".to_string(),
            },
        ),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(
        output.is_some(),
        "select2 then join should complete, stale timer shouldn't block"
    );
    assert_eq!(output.unwrap(), "join_completed");
}

/// Mixed completion types: activity loser, timer loser, then final timer
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn mixed_loser_types_do_not_block() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // First select2: timer wins, activity is loser
        let a1 = ctx.schedule_activity("SlowActivity", "input");
        let t1 = ctx.schedule_timer(Duration::from_secs(1));
        let (w1, _) = ctx.select2(a1, t1).await;
        assert_eq!(w1, 1, "timer should win");

        // Second select2: activity wins, timer is loser
        let a2 = ctx.schedule_activity("FastActivity", "input");
        let t2 = ctx.schedule_timer(Duration::from_secs(100));
        let (w2, _) = ctx.select2(a2, t2).await;
        assert_eq!(w2, 0, "activity should win");

        // Final timer - should not be blocked by stale activity or timer
        ctx.schedule_timer(Duration::from_secs(5)).into_timer().await;

        "completed"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // First select2: timer wins
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "SlowActivity".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 1000 }),
        Event::with_event_id(4, inst.clone(), 1, Some(3), EventKind::TimerFired { fire_at_ms: 1000 }),
        // Second select2: activity wins
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "FastActivity".to_string(),
                input: "input".to_string(),
            },
        ),
        Event::with_event_id(6, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 100000 }),
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "fast".to_string(),
            },
        ),
        // Final timer
        Event::with_event_id(8, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 5000 }),
        // Stale activity (from first select2 loser)
        Event::with_event_id(
            9,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "slow".to_string(),
            },
        ),
        // Stale timer (from second select2 loser)
        Event::with_event_id(
            10,
            inst.clone(),
            1,
            Some(6),
            EventKind::TimerFired { fire_at_ms: 100000 },
        ),
        // Final timer fires
        Event::with_event_id(11, inst.clone(), 1, Some(8), EventKind::TimerFired { fire_at_ms: 5000 }),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Mixed loser types should not block");
    assert_eq!(output.unwrap(), "completed");
}

/// Sequential select2s followed by timer - verifies cancelled_source_ids accumulates correctly
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn sequential_select2s_accumulate_cancelled_sources() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        // 5 sequential select2s, each activity wins
        for i in 0..5 {
            let activity = ctx.schedule_activity("Task", format!("input{i}"));
            let timeout = ctx.schedule_timer(Duration::from_secs(30));
            let (winner, _) = ctx.select2(activity, timeout).await;
            assert_eq!(winner, 0, "activity {i} should win");
        }

        // Final timer - should complete despite 5 stale timers
        ctx.schedule_timer(Duration::from_secs(1)).into_timer().await;

        "all_done"
    };

    let inst = "test-inst".to_string();
    let history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // select2 #0
        Event::with_event_id(
            2,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input0".to_string(),
            },
        ),
        Event::with_event_id(3, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            4,
            inst.clone(),
            1,
            Some(2),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // select2 #1
        Event::with_event_id(
            5,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input1".to_string(),
            },
        ),
        Event::with_event_id(6, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            7,
            inst.clone(),
            1,
            Some(5),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // select2 #2
        Event::with_event_id(
            8,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input2".to_string(),
            },
        ),
        Event::with_event_id(9, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            10,
            inst.clone(),
            1,
            Some(8),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // select2 #3
        Event::with_event_id(
            11,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input3".to_string(),
            },
        ),
        Event::with_event_id(12, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            13,
            inst.clone(),
            1,
            Some(11),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // select2 #4
        Event::with_event_id(
            14,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input4".to_string(),
            },
        ),
        Event::with_event_id(15, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 30000 }),
        Event::with_event_id(
            16,
            inst.clone(),
            1,
            Some(14),
            EventKind::ActivityCompleted {
                result: "ok".to_string(),
            },
        ),
        // Final timer
        Event::with_event_id(17, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 1000 }),
        // All 5 stale timers fire
        Event::with_event_id(
            18,
            inst.clone(),
            1,
            Some(3),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        Event::with_event_id(
            19,
            inst.clone(),
            1,
            Some(6),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        Event::with_event_id(
            20,
            inst.clone(),
            1,
            Some(9),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        Event::with_event_id(
            21,
            inst.clone(),
            1,
            Some(12),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        Event::with_event_id(
            22,
            inst.clone(),
            1,
            Some(15),
            EventKind::TimerFired { fire_at_ms: 30000 },
        ),
        // Final timer fires
        Event::with_event_id(
            23,
            inst.clone(),
            1,
            Some(17),
            EventKind::TimerFired { fire_at_ms: 1000 },
        ),
    ];

    let (_history, _actions, output) = run_turn(history, orchestrator);

    assert!(output.is_some(), "Should complete with 5 accumulated stale timers");
    assert_eq!(output.unwrap(), "all_done");
}

/// Verify that schedule order mismatch triggers nondeterminism error
#[test]
#[ignore = "Legacy mode only: uses run_turn() which requires cursor-based replay"]
fn schedule_order_mismatch_triggers_nondeterminism() {
    use duroxide::run_turn_with_status;
    // Orchestration schedules ACTIVITY first, then TIMER
    let orchestrator = |ctx: OrchestrationContext| async move {
        let activity = ctx.schedule_activity("Task", "input");
        let timer = ctx.schedule_timer(Duration::from_secs(10));
        ctx.select2(activity, timer).await;
        "done"
    };

    // But history shows TIMER was scheduled first (wrong order!)
    let inst = "test-inst".to_string();
    let mismatched_history = vec![
        Event::with_event_id(
            1,
            inst.clone(),
            1,
            None,
            EventKind::OrchestrationStarted {
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                input: "".to_string(),
                parent_instance: None,
                parent_id: None,
            },
        ),
        // History: TIMER first (event_id: 2)
        Event::with_event_id(2, inst.clone(), 1, None, EventKind::TimerCreated { fire_at_ms: 10000 }),
        // History: ACTIVITY second (event_id: 3)
        Event::with_event_id(
            3,
            inst.clone(),
            1,
            None,
            EventKind::ActivityScheduled {
                name: "Task".to_string(),
                input: "input".to_string(),
            },
        ),
    ];

    let (_history, _actions, _output, nondeterminism_error) = run_turn_with_status(
        mismatched_history,
        1,
        "test-instance".to_string(),
        "Test".to_string(),
        "1.0.0".to_string(),
        "worker-1".to_string(),
        orchestrator,
    );

    // Should detect nondeterminism: code schedules Activity first, but history has Timer first
    assert!(
        nondeterminism_error.is_some(),
        "Expected nondeterminism error when schedule order doesn't match history"
    );

    let err = nondeterminism_error.unwrap();
    assert!(
        err.contains("TimerCreated") && err.contains("ActivityScheduled"),
        "Error should mention the mismatch: got '{err}'"
    );

    eprintln!("\n✅ Nondeterminism detected: {err}");
}
