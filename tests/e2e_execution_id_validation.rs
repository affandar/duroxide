// Test: Execution ID validation for ContinueAsNew scenarios
// This test verifies that completions from old executions are properly ignored

use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::{QueueKind, WorkItem};
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
mod common;

#[tokio::test]
async fn old_execution_completions_are_ignored() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("LongRunningActivity", |_input: String| async move {
            // Simulate a long-running activity that doesn't complete during test
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok("never_reached".to_string())
        })
        .build();

    // Orchestration that schedules an activity then calls ContinueAsNew
    let orch = |ctx: OrchestrationContext, input: String| async move {
        if input == "first" {
            // First execution: schedule activity then continue as new
            let _activity_future = ctx.schedule_activity("LongRunningActivity", "test");
            // Don't await the activity, just continue as new immediately
            ctx.continue_as_new("second");
            Ok("continued".to_string())
        } else {
            // Second execution: just return success
            Ok("second_execution_complete".to_string())
        }
    };

    let reg = OrchestrationRegistry::builder().register("ContinueAsNewTest", orch).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration with "first" input
    let handle = rt
        .clone()
        .start_orchestration("inst-continue", "ContinueAsNewTest", "first")
        .await
        .unwrap();

    // Wait for the orchestration to complete (should complete via ContinueAsNew)
    let (history, result) = handle.await.expect("orchestration should complete");
    println!("First execution result: {:?}", result);
    
    // The first execution should complete successfully via ContinueAsNew
    assert!(result.is_ok());
    
    // Verify we have a new execution
    let executions = store.list_executions("inst-continue").await;
    println!("Executions: {:?}", executions);
    assert!(executions.len() >= 2, "Should have at least 2 executions after ContinueAsNew");

    // Now simulate a completion from the old execution (execution 1) arriving late
    // This should be ignored since execution 2 is now active
    let _ = store
        .enqueue_work(
            QueueKind::Orchestrator,
            WorkItem::ActivityCompleted {
                instance: "inst-continue".to_string(),
                execution_id: 1, // Old execution ID
                id: 1, // The activity ID from the first execution
                result: "late_completion".to_string(),
            },
        )
        .await;

    // Give some time for the completion to be processed
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The second execution should still be running and not affected by the old completion
    // We can verify this by checking that no new events were added to the current execution
    let current_history = store.read("inst-continue").await;
    println!("Current history length: {}", current_history.len());
    
    // The history should not contain the late completion
    let has_late_completion = current_history.iter().any(|e| match e {
        rust_dtf::Event::ActivityCompleted { result, .. } => result == "late_completion",
        _ => false,
    });
    
    assert!(!has_late_completion, "Late completion from old execution should be ignored");
    
    println!("✓ Old execution completion was properly ignored");
}

#[tokio::test] 
async fn future_execution_completions_are_ignored() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();

    // Simple orchestration that waits for external events
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let _result = ctx.schedule_wait("test_event").into_event().await;
        Ok("completed".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("FutureExecTest", orch).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _handle = rt
        .clone()
        .start_orchestration("inst-future", "FutureExecTest", "")
        .await
        .unwrap();

    // Wait for orchestration to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Inject a completion with a future execution ID (should never happen in practice)
    let _ = store
        .enqueue_work(
            QueueKind::Orchestrator,
            WorkItem::ActivityCompleted {
                instance: "inst-future".to_string(),
                execution_id: 999, // Future execution ID
                id: 1,
                result: "future_completion".to_string(),
            },
        )
        .await;

    // Give some time for processing
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify the completion was ignored
    let history = store.read("inst-future").await;
    let has_future_completion = history.iter().any(|e| match e {
        rust_dtf::Event::ActivityCompleted { result, .. } => result == "future_completion",
        _ => false,
    });
    
    assert!(!has_future_completion, "Future execution completion should be ignored");
    
    println!("✓ Future execution completion was properly ignored");
}
