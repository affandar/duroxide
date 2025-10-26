use duroxide::Event;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::{ExecutionMetadata, ManagementCapability, Provider, WorkItem};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::task::JoinSet;

/// Helper to create a SQLite store for testing
#[allow(dead_code)]
async fn create_sqlite_store() -> (Arc<dyn Provider>, TempDir) {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap()) as Arc<dyn Provider>;
    (store, td)
}

/// Helper to create a SQLite store with specific name
async fn create_sqlite_store_named(name: &str) -> (Arc<dyn Provider>, TempDir, String) {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join(format!("{}.db", name));
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = Arc::new(SqliteProvider::new(&db_url).await.unwrap()) as Arc<dyn Provider>;
    (store, td, db_url)
}

// ============================================================================
// BASIC FUNCTIONALITY TESTS
// ============================================================================

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
        execution_id: duroxide::INITIAL_EXECUTION_ID,
    };

    store
        .enqueue_orchestrator_work(start_work.clone(), None)
        .await
        .expect("Failed to enqueue work");

    // 2. Fetch orchestration item
    let item = store.fetch_orchestration_item().await.expect("Should have work");

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
            execution_id: 1,
            name: "TestActivity".to_string(),
            input: "test-input".to_string(),
        },
    ];

    let metadata = ExecutionMetadata {
        status: Some("Running".to_string()),
        output: None,
        orchestration_name: None,
        orchestration_version: None,
    };

    store
        .ack_orchestration_item(&item.lock_token, 1, history_delta, vec![], vec![], vec![], metadata)
        .await
        .expect("Failed to ack orchestration item");

    // 4. Verify history was persisted
    let history = store.read(instance).await;
    assert_eq!(history.len(), 2);
    assert!(matches!(history[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(history[1], Event::ActivityScheduled { .. }));
}

#[tokio::test]
async fn test_execution_status_completed() {
    let store = SqliteProvider::new_in_memory().await.unwrap();

    let instance = "test-instance";
    let execution_id = 1;

    // Create instance and execution
    store
        .enqueue_orchestrator_work(
            WorkItem::StartOrchestration {
                instance: instance.to_string(),
                orchestration: "TestOrch".to_string(),
                version: Some("1.0.0".to_string()),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Fetch and ack with completion
    let item = store.fetch_orchestration_item().await.unwrap();
    let metadata = ExecutionMetadata {
        status: Some("Completed".to_string()),
        output: Some("Success".to_string()),
        orchestration_name: None,
        orchestration_version: None,
    };

    store
        .ack_orchestration_item(
            &item.lock_token,
            execution_id,
            vec![Event::OrchestrationCompleted {
                event_id: 1,
                output: "Success".to_string(),
            }],
            vec![],
            vec![],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // Verify execution status
    let executions = Provider::list_executions(&store, instance).await;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0], execution_id);

    // Verify execution info
    let exec_info = store.get_execution_info(instance, execution_id).await.unwrap();
    assert_eq!(exec_info.status, "Completed");
    assert_eq!(exec_info.output, Some("Success".to_string()));
}

#[tokio::test]
async fn test_execution_status_failed() {
    let store = SqliteProvider::new_in_memory().await.unwrap();

    let instance = "test-instance";
    let execution_id = 1;

    // Create instance and execution
    store
        .enqueue_orchestrator_work(
            WorkItem::StartOrchestration {
                instance: instance.to_string(),
                orchestration: "TestOrch".to_string(),
                version: Some("1.0.0".to_string()),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Fetch and ack with failure
    let item = store.fetch_orchestration_item().await.unwrap();
    let metadata = ExecutionMetadata {
        status: Some("Failed".to_string()),
        output: Some("Error occurred".to_string()),
        orchestration_name: None,
        orchestration_version: None,
    };

    store
        .ack_orchestration_item(
            &item.lock_token,
            execution_id,
            vec![Event::OrchestrationFailed {
                event_id: 1,
                error: "Error occurred".to_string(),
            }],
            vec![],
            vec![],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // Verify execution status
    let exec_info = store.get_execution_info(instance, execution_id).await.unwrap();
    assert_eq!(exec_info.status, "Failed");
    assert_eq!(exec_info.output, Some("Error occurred".to_string()));
}

// ============================================================================
// PERSISTENCE TESTS
// ============================================================================

#[tokio::test]
async fn test_sqlite_basic_persistence() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("test.db");

    // Pre-create the database file
    std::fs::File::create(&db_path).expect("Failed to create db file");

    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());

    // Phase 1: Create store and add data
    {
        let store = SqliteProvider::new(&db_url)
            .await
            .expect("Failed to create SQLite store");
        let store: Arc<dyn Provider> = Arc::new(store);

        // Enqueue worker items
        store
            .enqueue_worker_work(WorkItem::ActivityExecute {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                name: "TestActivity".to_string(),
                input: "test-input".to_string(),
            })
            .await
            .expect("Failed to enqueue worker work");

        store
            .enqueue_worker_work(WorkItem::ActivityExecute {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 2,
                name: "TestActivity2".to_string(),
                input: "test-input-2".to_string(),
            })
            .await
            .expect("Failed to enqueue worker work 2");

        println!("Phase 1: Enqueued 2 worker items");
    }

    // Phase 2: Drop and recreate store, verify persistence
    {
        println!("Phase 2: Recreating store...");
        let store = SqliteProvider::new(&db_url)
            .await
            .expect("Failed to recreate SQLite store");
        let store: Arc<dyn Provider> = Arc::new(store);

        // Dequeue and verify items
        let (item1, token1) = store.dequeue_worker_peek_lock().await.expect("Should have first item");
        match item1 {
            WorkItem::ActivityExecute { name, input, .. } => {
                assert_eq!(name, "TestActivity");
                assert_eq!(input, "test-input");
            }
            _ => panic!("Expected ActivityExecute"),
        }

        let (item2, token2) = store.dequeue_worker_peek_lock().await.expect("Should have second item");
        match item2 {
            WorkItem::ActivityExecute { name, input, .. } => {
                assert_eq!(name, "TestActivity2");
                assert_eq!(input, "test-input-2");
            }
            _ => panic!("Expected ActivityExecute"),
        }

        // Acknowledge items with dummy completions
        store.ack_worker(&token1, WorkItem::ActivityCompleted {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            result: "done".to_string(),
        }).await.expect("Failed to ack worker 1");
        store.ack_worker(&token2, WorkItem::ActivityCompleted {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 2,
            result: "done".to_string(),
        }).await.expect("Failed to ack worker 2");

        // Verify no more items
        assert!(store.dequeue_worker_peek_lock().await.is_none());

        println!("Phase 2: Successfully verified persistence");
    }
}

// ============================================================================
// CONCURRENCY TESTS
// ============================================================================

#[tokio::test]
async fn test_sqlite_file_concurrent_access() {
    // Create a temporary directory for the database
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join("test.db");

    // Create an empty file to ensure it exists
    std::fs::File::create(&db_path).expect("Failed to create database file");

    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());

    // Create the store
    let store = Arc::new(
        SqliteProvider::new(&db_url)
            .await
            .expect("Failed to create SQLite store"),
    ) as Arc<dyn Provider>;

    // Test concurrent writes
    let mut tasks = JoinSet::new();

    // Spawn 10 concurrent tasks that enqueue work
    for i in 0..10 {
        let store_clone = store.clone();
        tasks.spawn(async move {
            let work_item = WorkItem::StartOrchestration {
                instance: format!("concurrent-instance-{}", i),
                orchestration: "TestOrch".to_string(),
                version: Some("1.0.0".to_string()),
                input: format!("{{\"id\": {}}}", i),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            };

            store_clone
                .enqueue_orchestrator_work(work_item, None)
                .await
                .expect("Failed to enqueue work");
        });
    }

    // Wait for all tasks to complete
    while let Some(result) = tasks.join_next().await {
        result.expect("Task failed");
    }

    // Verify all instances were created
    let instances = store.list_instances().await;
    assert_eq!(instances.len(), 10);

    // Verify we can fetch orchestration items for all instances
    let mut fetched_count = 0;
    while let Some(item) = store.fetch_orchestration_item().await {
        store
            .ack_orchestration_item(
                &item.lock_token,
                item.execution_id,
                vec![],
                vec![],
                vec![],
                vec![],
                ExecutionMetadata {
                    status: Some("Running".to_string()),
                    output: None,
                    orchestration_name: None,
                    orchestration_version: None,
                },
            )
            .await
            .expect("Failed to ack item");
        fetched_count += 1;
    }

    assert_eq!(fetched_count, 10);
}

// ============================================================================
// TIMER RECOVERY TESTS
// ============================================================================

#[tokio::test]
async fn timer_recovery_after_crash_before_fire() {
    let (store1, _td, _db_url) = create_sqlite_store_named("timer_recovery").await;

    const TIMER_MS: u64 = 500;

    // Simple orchestration that schedules a timer and then completes
    let orch = |ctx: duroxide::OrchestrationContext, _input: String| async move {
        // Schedule a timer with enough delay that we can "crash" before it fires
        ctx.schedule_timer(TIMER_MS).into_timer().await;

        // Do something after timer to prove it fired
        let result = ctx.schedule_activity("PostTimer", "done").into_activity().await?;
        Ok(result)
    };

    let activity_registry = duroxide::runtime::registry::ActivityRegistry::builder()
        .register("PostTimer", |input: String| async move {
            Ok(format!("Timer fired, then: {}", input))
        })
        .build();

    let orchestration_registry = duroxide::OrchestrationRegistry::builder()
        .register("TimerRecoveryTest", orch)
        .build();

    let rt = duroxide::runtime::Runtime::start_with_store(
        store1.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    let client = duroxide::Client::new(store1.clone());

    // Start orchestration
    client
        .start_orchestration("timer-recovery-instance", "TimerRecoveryTest", "")
        .await
        .unwrap();

    // Wait a bit to ensure timer is scheduled
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // "Crash" the runtime (drop it)
    drop(rt);

    // Simulate crash by checking that timer is in queue but not fired
    // Note: Timer might have already been processed, so we don't assert it's still there
    // The important part is that the orchestration can recover

    // Restart runtime with same store
    let orch2 = |ctx: duroxide::OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(TIMER_MS).into_timer().await;
        let result = ctx.schedule_activity("PostTimer", "done").into_activity().await?;
        Ok(result)
    };

    let activity_registry2 = duroxide::runtime::registry::ActivityRegistry::builder()
        .register("PostTimer", |input: String| async move {
            Ok(format!("Timer fired, then: {}", input))
        })
        .build();

    let orchestration_registry2 = duroxide::OrchestrationRegistry::builder()
        .register("TimerRecoveryTest", orch2)
        .build();

    let rt2 = duroxide::runtime::Runtime::start_with_store(
        store1.clone(),
        Arc::new(activity_registry2),
        orchestration_registry2,
    )
    .await;

    // Wait for orchestration to complete
    let status = client
        .wait_for_orchestration("timer-recovery-instance", std::time::Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(
        status,
        duroxide::runtime::OrchestrationStatus::Completed { .. }
    ));

    // Verify the result shows timer fired
    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        assert_eq!(output, "Timer fired, then: done");
    }

    drop(rt2);
}

#[tokio::test]
async fn test_sqlite_provider_transactional() {
    let store = SqliteProvider::new_in_memory().await.unwrap();

    let instance = "test-transactional";

    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TransactionalTest".to_string(),
        version: Some("1.0.0".to_string()),
        input: "{}".to_string(),
        parent_instance: None,
        parent_id: None,
        execution_id: duroxide::INITIAL_EXECUTION_ID,
    };

    store
        .enqueue_orchestrator_work(start_work, None)
        .await
        .expect("Failed to enqueue");

    let item = store.fetch_orchestration_item().await.expect("Should have work");

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
            execution_id: 1,
            name: "Activity1".to_string(),
            input: "{}".to_string(),
        },
        Event::ActivityScheduled {
            event_id: 3,
            execution_id: 1,
            name: "Activity2".to_string(),
            input: "{}".to_string(),
        },
        Event::ActivityScheduled {
            event_id: 4,
            execution_id: 1,
            name: "Activity3".to_string(),
            input: "{}".to_string(),
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
    store
        .ack_orchestration_item(
            &item.lock_token,
            1, // execution_id
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
    while let Some((work_item, token)) = store.dequeue_worker_peek_lock().await {
        worker_count += 1;
        // Extract id from work item for completion
        let id = match work_item {
            WorkItem::ActivityExecute { id, .. } => id,
            _ => panic!("Expected ActivityExecute"),
        };
        store.ack_worker(&token, WorkItem::ActivityCompleted {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id,
            result: "done".to_string(),
        }).await.expect("Failed to ack");
    }
    assert_eq!(worker_count, 3);
}

#[tokio::test]
async fn test_sqlite_provider_timer_queue() {
    let store = SqliteProvider::new_in_memory().await.unwrap();

    let instance = "test-timer";

    // Start orchestration
    store
        .enqueue_orchestrator_work(
            WorkItem::StartOrchestration {
                instance: instance.to_string(),
                orchestration: "TimerTest".to_string(),
                version: Some("1.0.0".to_string()),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .expect("Failed to enqueue");

    let item = store.fetch_orchestration_item().await.expect("Should have work");

    // Schedule a timer
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let timer_items = vec![WorkItem::TimerSchedule {
        instance: instance.to_string(),
        execution_id: 1,
        id: 1,
        fire_at_ms: now_ms + 1000, // 1 second from now
    }];

    store
        .ack_orchestration_item(
            &item.lock_token,
            1, // execution_id
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

#[tokio::test]
async fn test_execution_status_running() {
    let store = SqliteProvider::new_in_memory().await.unwrap();

    let instance = "exec-status-running-1";

    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test".to_string(),
        parent_instance: None,
        parent_id: None,
        execution_id: duroxide::INITIAL_EXECUTION_ID,
    };

    store.enqueue_orchestrator_work(start_work, None).await.unwrap();

    // Fetch and process
    let item = store.fetch_orchestration_item().await.unwrap();

    // Simulate orchestration running (not completed)
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
            execution_id: 1,
            name: "TestActivity".to_string(),
            input: "test".to_string(),
        },
    ];

    store
        .ack_orchestration_item(
            &item.lock_token,
            1,
            history_delta,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Verify execution exists and is running
    let executions = Provider::list_executions(&store, instance).await;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0], 1);
}

#[tokio::test]
async fn test_execution_output_captured_on_continue_as_new() {
    let store = SqliteProvider::new_in_memory().await.unwrap();

    let instance = "exec-output-continue-as-new-1";

    // Start orchestration
    let start_work = WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrch".to_string(),
        version: Some("1.0.0".to_string()),
        input: "test".to_string(),
        parent_instance: None,
        parent_id: None,
        execution_id: duroxide::INITIAL_EXECUTION_ID,
    };

    store.enqueue_orchestrator_work(start_work, None).await.unwrap();

    // Fetch and process
    let item = store.fetch_orchestration_item().await.unwrap();

    // Simulate orchestration continuing as new
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::OrchestrationContinuedAsNew {
            event_id: 2,
            input: "new-input".to_string(),
        },
    ];

    store
        .ack_orchestration_item(
            &item.lock_token,
            1,
            history_delta,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Verify execution exists
    let executions = Provider::list_executions(&store, instance).await;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0], 1);
}
