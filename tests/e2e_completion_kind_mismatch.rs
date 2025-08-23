// Test: Completion kind mismatch should trigger nondeterminism error
// This test verifies that when a completion arrives with a different kind than what was scheduled,
// it's detected as nondeterministic (e.g., ActivityCompleted for a timer ID)

use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::{QueueKind, WorkItem};
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::Arc as StdArc;
mod common;

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
