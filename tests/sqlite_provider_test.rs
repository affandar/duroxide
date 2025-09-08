use duroxide::providers::{Provider, WorkItem};
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
            name: "TestOrchestration".to_string(),
            version: "1.0.0".to_string(),
            input: r#"{"test": true}"#.to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            id: 1,
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
            name: "TransactionalTest".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            id: 1,
            name: "Activity1".to_string(),
            input: "{}".to_string(),
            execution_id: 1,
        },
        Event::ActivityScheduled {
            id: 2,
            name: "Activity2".to_string(),
            input: "{}".to_string(),
            execution_id: 1,
        },
        Event::ActivityScheduled {
            id: 3,
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
            name: "TimerTest".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        }],
        vec![],
        timer_items,
        vec![],
    )
    .await
    .expect("Failed to ack");
    
    // Timer queue is handled by the runtime, not tested here
    // Just verify the operation completed successfully
}
