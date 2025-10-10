//! Diagnostic test to verify orchestration state is being saved correctly

use duroxide::providers::{Provider, sqlite::SqliteProvider};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, OrchestrationStatus};
use duroxide::{Client, Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;

mod common;

/// Test: Verify database state is correct after completion
#[tokio::test]
async fn diagnostic_state_persistence_completed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap());
    
    let activities = ActivityRegistry::builder()
        .register("Task", |_: String| async move { Ok("success".to_string()) })
        .build();
    
    let orch = |ctx: OrchestrationContext, _: String| async move {
        ctx.schedule_activity("Task", "").into_activity().await
    };
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestOrch", orch)
        .build();
    
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    )
    .await;
    
    let client = Client::new(store.clone());
    client.start_orchestration("state-test-1", "TestOrch", "").await.unwrap();
    
    // Wait for completion
    let status = client
        .wait_for_orchestration("state-test-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    
    println!("📊 OrchestrationStatus: {:?}", status);
    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    
    // Check database state directly
    let pool = store.get_pool();
    
    // Check executions table status
    let exec_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = 'state-test-1'"
    )
    .fetch_optional(pool)
    .await
    .unwrap();
    
    println!("📊 Executions.status in DB: {:?}", exec_status);
    
    // Check history
    let history = store.read("state-test-1").await;
    println!("📊 History events count: {}", history.len());
    
    let has_started = history.iter().any(|e| matches!(e, Event::OrchestrationStarted { .. }));
    let has_completed = history.iter().any(|e| matches!(e, Event::OrchestrationCompleted { .. }));
    
    println!("📊 Has OrchestrationStarted: {}", has_started);
    println!("📊 Has OrchestrationCompleted: {}", has_completed);
    
    // These should both be true
    assert!(has_started, "Missing OrchestrationStarted event");
    assert!(has_completed, "Missing OrchestrationCompleted event");
    
    // ✅ FIXED: executions.status should now be properly updated to 'Completed'
    assert_eq!(exec_status, Some("Completed".to_string()), 
        "executions.status should be 'Completed' but is '{:?}'", exec_status);
    
    rt.shutdown().await;
}

/// Test: Verify database state is correct after failure
#[tokio::test]
async fn diagnostic_state_persistence_failed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap());
    
    let activities = ActivityRegistry::builder()
        .register("FailTask", |_: String| async move { Err("error".to_string()) })
        .build();
    
    let orch = |ctx: OrchestrationContext, _: String| async move {
        ctx.schedule_activity("FailTask", "").into_activity().await
    };
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("FailOrch", orch)
        .build();
    
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    )
    .await;
    
    let client = Client::new(store.clone());
    client.start_orchestration("state-test-2", "FailOrch", "").await.unwrap();
    
    // Wait for failure
    let status = client
        .wait_for_orchestration("state-test-2", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    
    println!("📊 OrchestrationStatus: {:?}", status);
    assert!(matches!(status, OrchestrationStatus::Failed { .. }));
    
    // Check database state
    let pool = store.get_pool();
    let exec_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = 'state-test-2'"
    )
    .fetch_optional(pool)
    .await
    .unwrap();
    
    println!("📊 Executions.status in DB: {:?}", exec_status);
    
    // Check history
    let history = store.read("state-test-2").await;
    let has_failed = history.iter().any(|e| matches!(e, Event::OrchestrationFailed { .. }));
    
    println!("📊 Has OrchestrationFailed: {}", has_failed);
    assert!(has_failed, "Missing OrchestrationFailed event");
    
    // ✅ FIXED: executions.status should now be properly updated to 'Failed'
    assert_eq!(exec_status, Some("Failed".to_string()), 
        "executions.status should be 'Failed' but is '{:?}'", exec_status);
    
    rt.shutdown().await;
}

/// Test: Verify executions table has correct execution IDs for ContinueAsNew
#[tokio::test]
async fn diagnostic_state_persistence_continue_as_new() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap());
    
    let activities = ActivityRegistry::builder().build();
    
    let orch = |ctx: OrchestrationContext, input: String| async move {
        let n: i32 = input.parse().unwrap_or(0);
        if n < 1 {
            ctx.continue_as_new((n + 1).to_string());
            Ok("continuing".to_string())
        } else {
            Ok("done".to_string())
        }
    };
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("ContinueOrch", orch)
        .build();
    
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    )
    .await;
    
    let client = Client::new(store.clone());
    client.start_orchestration("state-test-3", "ContinueOrch", "0").await.unwrap();
    
    // Wait for final completion
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    
    // Check executions table
    let pool = store.get_pool();
    let exec_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT execution_id FROM executions WHERE instance_id = 'state-test-3' ORDER BY execution_id"
    )
    .fetch_all(pool)
    .await
    .unwrap();
    
    println!("📊 Execution IDs in DB: {:?}", exec_ids);
    assert_eq!(exec_ids, vec![1, 2], "Should have 2 executions");
    
    // ✅ FIXED: Check that execution statuses are correct
    let exec_statuses: Vec<(i64, String)> = sqlx::query_as(
        "SELECT execution_id, status FROM executions WHERE instance_id = 'state-test-3' ORDER BY execution_id"
    )
    .fetch_all(pool)
    .await
    .unwrap();
    
    println!("📊 Execution statuses: {:?}", exec_statuses);
    assert_eq!(exec_statuses.len(), 2);
    assert_eq!(exec_statuses[0], (1, "ContinuedAsNew".to_string()), "First execution should be ContinuedAsNew");
    assert_eq!(exec_statuses[1], (2, "Completed".to_string()), "Second execution should be Completed");
    
    // Check that provider.read() returns latest execution
    let history = store.read("state-test-3").await;
    let started_events: Vec<_> = history.iter()
        .filter_map(|e| match e {
            Event::OrchestrationStarted { input, .. } => Some(input.clone()),
            _ => None
        })
        .collect();
    
    println!("📊 OrchestrationStarted inputs in latest execution: {:?}", started_events);
    assert_eq!(started_events.len(), 1, "Should only have 1 OrchestrationStarted (from latest exec)");
    assert_eq!(started_events[0], "1", "Latest execution should have input='1'");
    
    rt.shutdown().await;
}

