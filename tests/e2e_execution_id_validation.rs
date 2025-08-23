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
        .register("TestActivity", |_input: String| async move {
            // Activity that completes quickly
            Ok("activity_result".to_string())
        })
        .build();

    // Orchestration that waits for external events (stays active)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Wait for an external event to keep the orchestration active
        let _result = ctx.schedule_wait("continue_signal").into_event().await;
        Ok("orchestration_complete".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("ExecutionIdTest", orch).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _handle = rt
        .clone()
        .start_orchestration("inst-exec-test", "ExecutionIdTest", "")
        .await
        .unwrap();

    // Wait for orchestration to start and be waiting for external event
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Inject a completion with OLD execution ID (execution_id=0, but current should be 1)
    let _ = store
        .enqueue_work(
            QueueKind::Orchestrator,
            WorkItem::ActivityCompleted {
                instance: "inst-exec-test".to_string(),
                execution_id: 0, // Old execution ID (current is 1)
                id: 999, // Some activity ID
                result: "old_execution_result".to_string(),
            },
        )
        .await;

    // Give time for the completion to be processed (and ignored)
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify the completion was ignored - check that it's not in history
    let history = store.read("inst-exec-test").await;
    let has_old_completion = history.iter().any(|e| match e {
        rust_dtf::Event::ActivityCompleted { result, .. } => result == "old_execution_result",
        _ => false,
    });
    
    assert!(!has_old_completion, "Old execution completion should be ignored due to execution ID mismatch");
    
    println!("✓ Old execution completion was properly ignored due to execution ID validation");
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
