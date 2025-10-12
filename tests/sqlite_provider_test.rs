use duroxide::providers::{Provider, WorkItem, ExecutionMetadata};
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::Event;

#[tokio::test]
async fn test_sqlite_provider_basic() {
    // Create in-memory SQLite store
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    // Test basic workflow
    let instance = "test-instance-1";
    
    // 1. Enqueue a start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrchestration".to_string(),
        version: Some("1.0.0".to_string()),
        input: r#"{"test": true}"#.to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work.clone(), None)
        .await
        .expect("Failed to enqueue work");
    
    // 2. Fetch orchestration item
    let item = store.fetch_orchestration_item()
        .await
        .expect("Should have work");
    
    assert_eq!(item.instance, instance);
    assert_eq!(item.orchestration_name, "TestOrchestration");
    assert_eq!(item.version, "1.0.0");
    assert_eq!(item.execution_id, 1);
    assert_eq!(item.messages.len(), 1);
    assert_eq!(item.history.len(), 0); // No history yet
    
    // 3. Process and acknowledge with history
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TestOrchestration".to_string(),
            version: "1.0.0".to_string(),
            input: r#"{"test": true}"#.to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
            name: "ProcessData".to_string(),
            input: r#"{"data": "test"}"#.to_string(),
            execution_id: 1,
        },
    ];
    
    let worker_items = vec![
        WorkItem::ActivityExecute {
            instance: instance.to_string(),
            execution_id: 1,
            id: 1,
            name: "ProcessData".to_string(),
            input: r#"{"data": "test"}"#.to_string(),
        },
    ];
    
    store.ack_orchestration_item(
        &item.lock_token,
        history_delta,
        worker_items,
        vec![],  // No timers
        vec![],  // No new orchestrator items
        ExecutionMetadata::default(),
    )
    .await
    .expect("Failed to ack");
    
    // 4. Verify history was saved
    let history = store.read(instance).await;
    assert_eq!(history.len(), 2);
    assert!(matches!(history[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(history[1], Event::ActivityScheduled { .. }));
    
    // 5. Process worker item
    let (work_item, token) = store.dequeue_worker_peek_lock()
        .await
        .expect("Should have worker item");
    
    match work_item {
        WorkItem::ActivityExecute { name, .. } => {
            assert_eq!(name, "ProcessData");
        }
        _ => panic!("Expected ActivityExecute"),
    }
    
    // Ack the worker item
    store.ack_worker(&token)
        .await
        .expect("Failed to ack worker");
    
    // No more worker items
    assert!(store.dequeue_worker_peek_lock().await.is_none());
}

#[tokio::test]
async fn test_sqlite_provider_transactional() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "test-transactional";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TransactionalTest".to_string(),
        version: Some("1.0.0".to_string()),
        input: "{}".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None)
        .await
        .expect("Failed to enqueue");
    
    let item = store.fetch_orchestration_item()
        .await
        .expect("Should have work");
    
    // Simulate orchestration that schedules multiple activities atomically
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TransactionalTest".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
            name: "Activity1".to_string(),
            input: "{}".to_string(),
            execution_id: 1,
        },
        Event::ActivityScheduled {
            event_id: 3,
            name: "Activity2".to_string(),
            input: "{}".to_string(),
            execution_id: 1,
        },
        Event::ActivityScheduled {
            event_id: 4,
            name: "Activity3".to_string(),
            input: "{}".to_string(),
            execution_id: 1,
        },
    ];
    
    let worker_items = vec![
        WorkItem::ActivityExecute {
            instance: instance.to_string(),
            execution_id: 1,
            id: 1,
            name: "Activity1".to_string(),
            input: "{}".to_string(),
        },
        WorkItem::ActivityExecute {
            instance: instance.to_string(),
            execution_id: 1,
            id: 2,
            name: "Activity2".to_string(),
            input: "{}".to_string(),
        },
        WorkItem::ActivityExecute {
            instance: instance.to_string(),
            execution_id: 1,
            id: 3,
            name: "Activity3".to_string(),
            input: "{}".to_string(),
        },
    ];
    
    // All operations should be atomic
    store.ack_orchestration_item(
        &item.lock_token,
        history_delta,
        worker_items,
        vec![],
        vec![],
        ExecutionMetadata::default(),
    )
    .await
    .expect("Failed to ack");
    
    // Verify all history saved
    let history = store.read(instance).await;
    assert_eq!(history.len(), 4); // Start + 3 schedules
    
    // Verify all worker items enqueued
    let mut worker_count = 0;
    while let Some((_, token)) = store.dequeue_worker_peek_lock().await {
        worker_count += 1;
        store.ack_worker(&token).await.expect("Failed to ack");
    }
    assert_eq!(worker_count, 3);
}

#[tokio::test]
async fn test_sqlite_provider_timer_queue() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "test-timer";
    
    // Start orchestration
    store.enqueue_orchestrator_work(WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TimerTest".to_string(),
        version: Some("1.0.0".to_string()),
        input: "{}".to_string(),
        parent_instance: None,
        parent_id: None,
    }, None)
    .await
    .expect("Failed to enqueue");
    
    let item = store.fetch_orchestration_item()
        .await
        .expect("Should have work");
    
    // Schedule a timer
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
        
    let timer_items = vec![
        WorkItem::TimerSchedule {
            instance: instance.to_string(),
            execution_id: 1,
            id: 1,
            fire_at_ms: now_ms + 1000, // 1 second from now
        },
    ];
    
    store.ack_orchestration_item(
        &item.lock_token,
        vec![Event::OrchestrationStarted {
            event_id: 1,
            name: "TimerTest".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        }],
        vec![],
        timer_items,
        vec![],
        ExecutionMetadata::default(),
    )
    .await
    .expect("Failed to ack");
    
    // Timer queue is handled by the runtime, not tested here
    // Just verify the operation completed successfully
}

/// Test: Verify executions.status is updated to 'Completed' on orchestration completion
#[tokio::test]
async fn test_execution_status_completed() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-status-completed-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    
    // Fetch and process
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration completing
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationCompleted {
            event_id: 2,
            output: "success".to_string(),
        },
    ];
    
    let metadata = ExecutionMetadata {
        status: Some("Completed".to_string()),
        output: Some("success".to_string()),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![], vec![], vec![], metadata)
        .await
        .unwrap();
    
    // Query database directly to check execution status
    let pool = store.get_pool();
    let status: String = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "Completed", "Execution status should be Completed");
    
    // Verify completed_at is set
    let completed_at: Option<String> = sqlx::query_scalar(
        "SELECT completed_at FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    
    assert!(completed_at.is_some(), "completed_at should be set");
}

/// Test: Verify executions.status is updated to 'Failed' on orchestration failure
#[tokio::test]
async fn test_execution_status_failed() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-status-failed-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "FailOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    
    // Fetch and process
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration failing
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "FailOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationFailed {
            event_id: 2,
            error: "something went wrong".to_string(),
        },
    ];
    
    let metadata = ExecutionMetadata {
        status: Some("Failed".to_string()),
        output: Some("something went wrong".to_string()),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![], vec![], vec![], metadata)
        .await
        .unwrap();
    
    // Query database directly
    let pool = store.get_pool();
    let status: String = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "Failed", "Execution status should be Failed");
    
    // Verify completed_at is set
    let completed_at: Option<String> = sqlx::query_scalar(
        "SELECT completed_at FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    
    assert!(completed_at.is_some(), "completed_at should be set");
}

/// Test: Verify executions.status is updated to 'ContinuedAsNew' and new execution is created
#[tokio::test]
async fn test_execution_status_continued_as_new() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-status-can-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "ContinueOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "0".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    
    // Fetch and process first execution
    let item = store.fetch_orchestration_item().await.unwrap();
    assert_eq!(item.execution_id, 1);
    
    // Simulate orchestration continuing as new
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "ContinueOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "0".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationContinuedAsNew {
            event_id: 2,
            input: "1".to_string(),
        },
    ];
    
    // ContinueAsNew work item
    let continue_work = WorkItem::ContinueAsNew {
        instance: instance.to_string(),
        orchestration: "ContinueOrch".to_string(),
        input: "1".to_string(),
        version: Some("1.0.0".to_string()),
    };
    
    let metadata = ExecutionMetadata {
        status: Some("ContinuedAsNew".to_string()),
        output: Some("1".to_string()),
    };
    
    store.ack_orchestration_item(
        &item.lock_token, 
        history_delta, 
        vec![], 
        vec![], 
        vec![continue_work],
        metadata,
    )
    .await
    .unwrap();
    
    // Query database directly
    let pool = store.get_pool();
    
    // Check first execution status
    let status1: String = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution 1");
    
    assert_eq!(status1, "ContinuedAsNew", "Execution 1 status should be ContinuedAsNew");
    
    // With the new approach, execution 2 is NOT created yet in ack_orchestration_item
    // It will be created when fetch_orchestration_item processes the WorkItem::ContinueAsNew
    let exec2_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM executions WHERE instance_id = ? AND execution_id = 2"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    
    assert_eq!(exec2_count, 0, "Execution 2 should not exist yet (created when ContinueAsNew work item is fetched)");
    
    // Verify completed_at is set for first execution
    let completed_at1: Option<String> = sqlx::query_scalar(
        "SELECT completed_at FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    
    assert!(completed_at1.is_some(), "completed_at should be set for first execution");
    
    // Now fetch the ContinueAsNew work item - this will create execution 2
    let item2 = store.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.execution_id, 2);
    
    // Verify execution 2 was created with Running status
    let status2: String = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = ? AND execution_id = 2"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution 2 after fetch");
    
    assert_eq!(status2, "Running", "Execution 2 status should be Running");
    
    let history_delta2 = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "ContinueOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "1".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationCompleted {
            event_id: 2,
            output: "done".to_string(),
        },
    ];
    
    let metadata2 = ExecutionMetadata {
        status: Some("Completed".to_string()),
        output: Some("done".to_string()),
    };
    
    store.ack_orchestration_item(&item2.lock_token, history_delta2, vec![], vec![], vec![], metadata2)
        .await
        .unwrap();
    
    // Check second execution is completed
    let status2_final: String = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = ? AND execution_id = 2"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution 2");
    
    assert_eq!(status2_final, "Completed", "Execution 2 status should be Completed");
}

/// Test: Verify execution status remains Running when no terminal event occurs
#[tokio::test]
async fn test_execution_status_running() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-status-running-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "RunningOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    
    // Fetch and process
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration processing but not completing (just scheduling activities)
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "RunningOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
            name: "SomeActivity".to_string(),
            input: "input".to_string(),
            execution_id: 1,
        },
    ];
    
    let activity_work = WorkItem::ActivityExecute {
        instance: instance.to_string(),
        execution_id: 1,
        id: 2,
        name: "SomeActivity".to_string(),
        input: "input".to_string(),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![activity_work], vec![], vec![], ExecutionMetadata::default())
        .await
        .unwrap();
    
    // Query database directly
    let pool = store.get_pool();
    let status: String = sqlx::query_scalar(
        "SELECT status FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "Running", "Execution status should remain Running");
    
    // Verify completed_at is NOT set
    let completed_at: Option<String> = sqlx::query_scalar(
        "SELECT completed_at FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .unwrap();
    
    assert!(completed_at.is_none(), "completed_at should NOT be set for running execution");
}

/// Test: Verify output column captures orchestration result on completion
#[tokio::test]
async fn test_execution_output_captured_on_completion() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-output-completed-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "OutputOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test-input".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration completing with specific output
    let expected_output = r#"{"result": "success", "count": 42}"#;
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "OutputOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationCompleted {
            event_id: 2,
            output: expected_output.to_string(),
        },
    ];
    
    let metadata = ExecutionMetadata {
        status: Some("Completed".to_string()),
        output: Some(expected_output.to_string()),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![], vec![], vec![], metadata)
        .await
        .unwrap();
    
    // Query database to verify output is stored
    let pool = store.get_pool();
    let (status, output): (String, Option<String>) = sqlx::query_as(
        "SELECT status, output FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "Completed");
    assert_eq!(output, Some(expected_output.to_string()), "Output should be captured");
}

/// Test: Verify output column captures error on failure
#[tokio::test]
async fn test_execution_output_captured_on_failure() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-output-failed-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "FailOutputOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test-input".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration failing with specific error
    let expected_error = "Database connection failed: timeout after 30s";
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "FailOutputOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationFailed {
            event_id: 2,
            error: expected_error.to_string(),
        },
    ];
    
    let metadata = ExecutionMetadata {
        status: Some("Failed".to_string()),
        output: Some(expected_error.to_string()),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![], vec![], vec![], metadata)
        .await
        .unwrap();
    
    // Query database to verify error is stored in output column
    let pool = store.get_pool();
    let (status, output): (String, Option<String>) = sqlx::query_as(
        "SELECT status, output FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "Failed");
    assert_eq!(output, Some(expected_error.to_string()), "Error should be captured in output column");
}

/// Test: Verify output column captures next input on ContinueAsNew
#[tokio::test]
async fn test_execution_output_captured_on_continue_as_new() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-output-can-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "CANOutputOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "cursor:page1".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration continuing with next input
    let next_input = "cursor:page2";
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "CANOutputOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "cursor:page1".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationContinuedAsNew {
            event_id: 2,
            input: next_input.to_string(),
        },
    ];
    
    let continue_work = WorkItem::ContinueAsNew {
        instance: instance.to_string(),
        orchestration: "CANOutputOrch".to_string(),
        input: next_input.to_string(),
        version: Some("1.0.0".to_string()),
    };
    
    let metadata = ExecutionMetadata {
        status: Some("ContinuedAsNew".to_string()),
        output: Some(next_input.to_string()),
    };
    
    store.ack_orchestration_item(
        &item.lock_token,
        history_delta,
        vec![],
        vec![],
        vec![continue_work],
        metadata,
    )
    .await
    .unwrap();
    
    // Query database to verify next input is stored in output column
    let pool = store.get_pool();
    let (status, output): (String, Option<String>) = sqlx::query_as(
        "SELECT status, output FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "ContinuedAsNew");
    assert_eq!(output, Some(next_input.to_string()), "Next input should be captured in output column");
}

/// Test: Verify output column remains NULL for running executions
#[tokio::test]
async fn test_execution_output_null_when_running() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-output-null-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "RunningOutputOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test-input".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Simulate orchestration still running (no terminal event)
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "RunningOutputOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
            name: "SomeActivity".to_string(),
            input: "activity-input".to_string(),
            execution_id: 1,
        },
    ];
    
    let activity_work = WorkItem::ActivityExecute {
        instance: instance.to_string(),
        execution_id: 1,
        id: 2,
        name: "SomeActivity".to_string(),
        input: "activity-input".to_string(),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![activity_work], vec![], vec![], ExecutionMetadata::default())
        .await
        .unwrap();
    
    // Query database to verify output is NULL for running execution
    let pool = store.get_pool();
    let (status, output): (String, Option<String>) = sqlx::query_as(
        "SELECT status, output FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(status, "Running");
    assert_eq!(output, None, "Output should be NULL for running execution");
}

/// Test: Verify complex JSON output is captured correctly
#[tokio::test]
async fn test_execution_output_complex_json() {
    let store = SqliteProvider::new("sqlite::memory:")
        .await
        .expect("Failed to create SQLite store");
    
    let instance = "exec-output-json-1";
    
    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "JsonOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "{}".to_string(),
        parent_instance: None,
        parent_id: None,
    };
    
    store.enqueue_orchestrator_work(start_work, None).await.unwrap();
    let item = store.fetch_orchestration_item().await.unwrap();
    
    // Complex JSON output
    let complex_output = r#"{"users":[{"id":1,"name":"Alice"},{"id":2,"name":"Bob"}],"total":2,"metadata":{"processed_at":"2024-01-01T00:00:00Z","version":"1.0"}}"#;
    
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "JsonOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationCompleted {
            event_id: 2,
            output: complex_output.to_string(),
        },
    ];
    
    let metadata = ExecutionMetadata {
        status: Some("Completed".to_string()),
        output: Some(complex_output.to_string()),
    };
    
    store.ack_orchestration_item(&item.lock_token, history_delta, vec![], vec![], vec![], metadata)
        .await
        .unwrap();
    
    // Query database and verify complex JSON is preserved
    let pool = store.get_pool();
    let output: Option<String> = sqlx::query_scalar(
        "SELECT output FROM executions WHERE instance_id = ? AND execution_id = 1"
    )
    .bind(instance)
    .fetch_one(pool)
    .await
    .expect("Should find execution");
    
    assert_eq!(output, Some(complex_output.to_string()), "Complex JSON output should be preserved exactly");
    
    // Verify it's valid JSON
    let parsed: serde_json::Value = serde_json::from_str(output.as_ref().unwrap())
        .expect("Output should be valid JSON");
    assert_eq!(parsed["total"], 2);
    assert_eq!(parsed["users"][0]["name"], "Alice");
}
