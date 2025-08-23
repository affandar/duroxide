// Test: Various nondeterminism detection scenarios
// This file consolidates all nondeterminism-related tests to verify the robust detection system

use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::{QueueKind, WorkItem};
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::Arc as StdArc;
mod common;

#[tokio::test]
async fn code_swap_triggers_nondeterminism() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register both A1 and B1 activities at all times
    let activity_registry = ActivityRegistry::builder()
        // A1 never completes (simulate long-running or blocked work)
        .register("A1", |_input: String| async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            #[allow(unreachable_code)]
            Ok(String::new())
        })
        // B1 completes quickly
        .register("B1", |input: String| async move { Ok(format!("B1:{{{input}}}")) })
        .build();

    // Code A: schedules activity "A1" then waits for completion
    let orch_a = |ctx: OrchestrationContext, _input: String| async move {
        let res = ctx.schedule_activity("A1", "foo").into_activity().await.unwrap();
        Ok(res)
    };
    // Code B: schedules activity "B1" (different name/id)
    let orch_b = |ctx: OrchestrationContext, _input: String| async move {
        let res = ctx.schedule_activity("B1", "bar").into_activity().await.unwrap();
        Ok(res)
    };

    // Register A, start orchestration
    let reg_a = OrchestrationRegistry::builder().register("SwapTest", orch_a).build();
    let rt_a = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry.clone()), reg_a).await;

    let _h = rt_a
        .clone()
        .start_orchestration("inst-swap", "SwapTest", "")
        .await
        .unwrap();

    // Wait for ActivityScheduled("A1") to appear in history and capture it
    let evt = common::wait_for_history_event(
        store.clone(),
        "inst-swap",
        |hist| {
            hist.iter().find_map(|e| match e {
                Event::ActivityScheduled { name, .. } if name == "A1" => Some(e.clone()),
                _ => None,
            })
        },
        2000,
    )
    .await;
    match evt {
        Some(Event::ActivityScheduled { .. }) => {}
        _ => panic!("timed out waiting for A1 schedule"),
    }

    // Simulate code swap: drop old runtime, create new one with registry B
    drop(rt_a);
    let reg_b = OrchestrationRegistry::builder().register("SwapTest", orch_b).build();
    let rt_b = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg_b).await;

    // Poke the instance so it activates and runs a turn (nondeterminism check occurs before completions)
    // Use a timer that fires immediately to trigger a turn reliably
    let _ = store
        .enqueue_work(
            QueueKind::Timer,
            WorkItem::TimerSchedule {
                instance: "inst-swap".to_string(),
                id: 999, // Use a high ID that won't conflict with orchestration timers
                fire_at_ms: 0, // Fire immediately
            },
        )
        .await;

    // Wait for terminal status using helper
    match rt_b
        .wait_for_orchestration("inst-swap", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { error } => {
            assert!(error.contains("nondeterministic"), "error: {error}")
        }
        other => panic!("expected failure with nondeterminism, got: {other:?}"),
    }
}

#[tokio::test]
async fn completion_kind_mismatch_triggers_nondeterminism() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("TestActivity", |input: String| async move { 
            Ok(format!("result:{}", input)) 
        })
        .build();

    // Orchestration that creates a timer, then waits for it
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Create a timer that fires in 1 second (1000ms)
        let timer_future = ctx.schedule_timer(1000);
        let _result = timer_future.into_timer().await;
        Ok("timer_completed".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("KindMismatchTest", orch).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _h = rt
        .clone()
        .start_orchestration("inst-mismatch", "KindMismatchTest", "")
        .await
        .unwrap();

    // Wait for the timer to be created in history
    let timer_created = common::wait_for_history_event(
        store.clone(),
        "inst-mismatch",
        |hist| {
            hist.iter().find_map(|e| match e {
                Event::TimerCreated { id, .. } => Some(*id),
                _ => None,
            })
        },
        2000,
    )
    .await;

    let timer_id = timer_created.expect("Timer should be created");
    println!("Timer created with ID: {}", timer_id);

    // Inject a completion with the WRONG kind - send ActivityCompleted for a timer ID
    let _ = store
        .enqueue_work(
            QueueKind::Orchestrator,
            WorkItem::ActivityCompleted {
                instance: "inst-mismatch".to_string(),
                id: timer_id, // This is a timer ID, but we're sending ActivityCompleted!
                result: "wrong_kind_result".to_string(),
            },
        )
        .await;

    // The orchestration should fail with nondeterminism error about kind mismatch
    match rt
        .wait_for_orchestration("inst-mismatch", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { error } => {
            println!("Got expected error: {}", error);
            assert!(
                error.contains("nondeterministic") && 
                error.contains("kind mismatch") && 
                error.contains("timer") && 
                error.contains("activity"),
                "Expected nondeterminism error about kind mismatch between timer and activity, got: {error}"
            );
        }
        other => panic!("Expected failure with nondeterminism, got: {other:?}"),
    }
}

#[tokio::test]
async fn unexpected_completion_id_triggers_nondeterminism() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("TestActivity", |input: String| async move { 
            Ok(format!("result:{}", input)) 
        })
        .build();

    // Orchestration that waits for external events (doesn't schedule anything with ID 999)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let _result = ctx.schedule_wait("test_event").into_event().await;
        Ok("external_completed".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("UnexpectedIdTest", orch).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _h = rt
        .clone()
        .start_orchestration("inst-unexpected", "UnexpectedIdTest", "")
        .await
        .unwrap();

    // Wait for the external subscription to be created
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Inject a completion for an ID that was never scheduled (999)
    let _ = store
        .enqueue_work(
            QueueKind::Orchestrator,
            WorkItem::ActivityCompleted {
                instance: "inst-unexpected".to_string(),
                id: 999, // This ID was never scheduled by the orchestration
                result: "unexpected_result".to_string(),
            },
        )
        .await;

    // The orchestration should fail with nondeterminism error about unexpected completion
    match rt
        .wait_for_orchestration("inst-unexpected", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { error } => {
            println!("Got expected error: {}", error);
            assert!(
                error.contains("nondeterministic") && 
                error.contains("no matching schedule") && 
                error.contains("999"),
                "Expected nondeterminism error about unexpected completion ID 999, got: {error}"
            );
        }
        other => panic!("Expected failure with nondeterminism, got: {other:?}"),
    }
}

#[tokio::test]
async fn unexpected_timer_completion_triggers_nondeterminism() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();

    // Simple orchestration that just waits for external events (doesn't create any timers)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Wait for an external event, but don't create any timers
        let _result = ctx.schedule_wait("test").into_event().await;
        Ok("done".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("TimerTest", orch).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _h = rt
        .clone()
        .start_orchestration("inst-timer", "TimerTest", "")
        .await
        .unwrap();

    // Wait for the orchestration to be waiting for external events
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Inject an unexpected timer completion (timer ID 123 was never scheduled)
    let _ = store
        .enqueue_work(
            QueueKind::Orchestrator,
            WorkItem::TimerFired {
                instance: "inst-timer".to_string(),
                id: 123,
                fire_at_ms: 0,
            },
        )
        .await;

    // The orchestration should fail with nondeterminism error
    match rt
        .wait_for_orchestration("inst-timer", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { error } => {
            println!("Got expected error: {}", error);
            assert!(
                error.contains("nondeterministic") && error.contains("timer") && error.contains("123"),
                "Expected nondeterminism error about timer 123, got: {error}"
            );
        }
        other => panic!("Expected failure with nondeterminism, got: {other:?}"),
    }
}
