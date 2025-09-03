use rust_dtf::Event;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::{HistoryStore, WorkItem};
use std::sync::Arc;

#[tokio::test]
async fn test_fetch_orchestration_item_new_instance() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Create instance and enqueue start work
    store.create_instance("test-instance").await.unwrap();
    store
        .enqueue_orchestrator_work(WorkItem::StartOrchestration {
            instance: "test-instance".to_string(),
            orchestration: "TestOrch".to_string(),
            input: "test-input".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
        })
        .await
        .unwrap();

    // Fetch orchestration item
    let item = store.fetch_orchestration_item().await.unwrap();

    assert_eq!(item.instance, "test-instance");
    assert_eq!(item.orchestration_name, "TestOrch");
    assert_eq!(item.version, "1.0.0");
    assert_eq!(item.execution_id, 1);
    assert!(item.history.is_empty());
    assert_eq!(item.messages.len(), 1);
    assert!(matches!(
        &item.messages[0],
        WorkItem::StartOrchestration { orchestration, .. } if orchestration == "TestOrch"
    ));
}

#[tokio::test]
async fn test_fetch_orchestration_item_existing_instance() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Create instance with some history
    store.create_instance("test-instance").await.unwrap();
    store
        .append(
            "test-instance",
            vec![
                Event::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "test-input".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
                Event::ActivityScheduled {
                    id: 1,
                    name: "TestActivity".to_string(),
                    input: "activity-input".to_string(),
                    execution_id: 1,
                },
            ],
        )
        .await
        .unwrap();

    // Enqueue completion
    store
        .enqueue_orchestrator_work(WorkItem::ActivityCompleted {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            result: "activity-result".to_string(),
        })
        .await
        .unwrap();

    // Fetch orchestration item
    let item = store.fetch_orchestration_item().await.unwrap();

    assert_eq!(item.instance, "test-instance");
    assert_eq!(item.orchestration_name, "TestOrch");
    assert_eq!(item.version, "1.0.0");
    assert_eq!(item.execution_id, 1);
    assert_eq!(item.history.len(), 2);
    assert_eq!(item.messages.len(), 1);
    assert!(matches!(
        &item.messages[0],
        WorkItem::ActivityCompleted { result, .. } if result == "activity-result"
    ));
}

#[tokio::test]
async fn test_fetch_orchestration_item_no_work() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // No work items
    let item = store.fetch_orchestration_item().await;
    assert!(item.is_none());
}

#[tokio::test]
async fn test_ack_orchestration_item_atomic() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Setup
    store.create_instance("test-instance").await.unwrap();
    store
        .enqueue_orchestrator_work(WorkItem::StartOrchestration {
            instance: "test-instance".to_string(),
            orchestration: "TestOrch".to_string(),
            input: "test-input".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
        })
        .await
        .unwrap();

    // Fetch and get lock token
    let item = store.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Prepare updates
    let history_delta = vec![
        Event::OrchestrationStarted {
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            id: 1,
            name: "TestActivity".to_string(),
            input: "activity-input".to_string(),
            execution_id: 1,
        },
    ];

    let worker_items = vec![WorkItem::ActivityExecute {
        instance: "test-instance".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "activity-input".to_string(),
    }];

    // Ack with updates
    store
        .ack_orchestration_item(&lock_token, history_delta, worker_items, vec![], vec![])
        .await
        .unwrap();

    // Verify history was updated
    let history = store.read("test-instance").await;
    assert_eq!(history.len(), 2);
    assert!(matches!(&history[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(&history[1], Event::ActivityScheduled { .. }));

    // Verify worker item was enqueued
    let (worker_item, _) = store.dequeue_worker_peek_lock().await.unwrap();
    assert!(matches!(worker_item, WorkItem::ActivityExecute { .. }));

    // Verify orchestrator queue is empty (item was acked)
    assert!(store.fetch_orchestration_item().await.is_none());
}

#[tokio::test]
async fn test_ack_orchestration_item_error_handling() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Try to ack with invalid token
    let result = store
        .ack_orchestration_item("invalid-token", vec![], vec![], vec![], vec![])
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Lock token not found"));
}

#[tokio::test]
async fn test_abandon_orchestration_item() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Setup
    store.create_instance("test-instance").await.unwrap();
    store
        .enqueue_orchestrator_work(WorkItem::StartOrchestration {
            instance: "test-instance".to_string(),
            orchestration: "TestOrch".to_string(),
            input: "test-input".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
        })
        .await
        .unwrap();

    // Fetch and get lock token
    let item = store.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Abandon the item
    store.abandon_orchestration_item(&lock_token, None).await.unwrap();

    // Verify item is back in queue
    let item2 = store.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "test-instance");
    assert!(matches!(
        &item2.messages[0],
        WorkItem::StartOrchestration { orchestration, .. } if orchestration == "TestOrch"
    ));
}

#[tokio::test]
async fn test_abandon_orchestration_item_with_delay() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Setup
    store.create_instance("test-instance").await.unwrap();
    store
        .enqueue_orchestrator_work(WorkItem::StartOrchestration {
            instance: "test-instance".to_string(),
            orchestration: "TestOrch".to_string(),
            input: "test-input".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
        })
        .await
        .unwrap();

    // Fetch and get lock token
    let item = store.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Abandon with delay (note: fs provider doesn't support delay yet)
    store.abandon_orchestration_item(&lock_token, Some(5000)).await.unwrap();

    // Item should still be available immediately (no delay support)
    let item2 = store.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "test-instance");
}

#[tokio::test]
async fn test_abandon_orchestration_item_error_handling() {
    let td = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(td.path(), true));

    // Try to abandon with invalid token
    let result = store.abandon_orchestration_item("invalid-token", None).await;

    // fs provider returns Ok even for invalid tokens (best effort)
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_in_memory_provider_atomic_operations() {
    let store: Arc<dyn HistoryStore> = Arc::new(InMemoryHistoryStore::default());

    // Create instance and enqueue work
    store.create_instance("test-instance").await.unwrap();
    store
        .enqueue_orchestrator_work(WorkItem::StartOrchestration {
            instance: "test-instance".to_string(),
            orchestration: "TestOrch".to_string(),
            input: "test-input".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
        })
        .await
        .unwrap();

    // Test fetch
    let item = store.fetch_orchestration_item().await.unwrap();
    assert_eq!(item.instance, "test-instance");
    assert_eq!(item.orchestration_name, "TestOrch");
    let lock_token = item.lock_token.clone();

    // Test ack with updates
    let history_delta = vec![Event::OrchestrationStarted {
        name: "TestOrch".to_string(),
        version: "1.0.0".to_string(),
        input: "test-input".to_string(),
        parent_instance: None,
        parent_id: None,
    }];

    store
        .ack_orchestration_item(&lock_token, history_delta, vec![], vec![], vec![])
        .await
        .unwrap();

    // Verify history
    let history = store.read("test-instance").await;
    assert_eq!(history.len(), 1);
    assert!(matches!(&history[0], Event::OrchestrationStarted { .. }));

    // Test abandon
    store
        .enqueue_orchestrator_work(WorkItem::ActivityCompleted {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            result: "result".to_string(),
        })
        .await
        .unwrap();

    let item2 = store.fetch_orchestration_item().await.unwrap();
    let lock_token2 = item2.lock_token.clone();

    store.abandon_orchestration_item(&lock_token2, None).await.unwrap();

    // Should be available again
    let item3 = store.fetch_orchestration_item().await.unwrap();
    assert_eq!(item3.instance, "test-instance");
}
