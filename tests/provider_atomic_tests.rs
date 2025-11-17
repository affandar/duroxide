use duroxide::Event;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::{ExecutionMetadata, Provider, WorkItem};
use std::sync::Arc;

mod common;
use common::test_create_execution;

/// Verify provider ignores work items after a terminal event by just acking
#[tokio::test]
async fn test_ignore_work_after_terminal_event() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    let instance = "inst-terminal";

    // Seed terminal history: OrchestrationCompleted (using test helper)
    let _ = test_create_execution(store.as_ref(), instance, "TermOrch", "1.0.0", "seed", None, None)
        .await
        .unwrap();
    // Use event_id=1 (first event)
    store
        .append_with_execution(
            instance,
            1,
            vec![Event::OrchestrationCompleted {
                event_id: 2,
                output: "done".to_string(),
            }],
        )
        .await
        .unwrap();

    // Enqueue arbitrary work item that should be ignored by runtime
    store
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: instance.to_string(),
                name: "Ignored".to_string(),
                data: "x".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch orchestration item - runtime would bail and just ack
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item.instance, instance);
    assert_eq!(item.messages.len(), 1);

    // Simulate runtime acking empty because it's terminal
    Provider::ack_orchestration_item(
        store.as_ref(),
        &item.lock_token,
        1,
        vec![],
        vec![],
        vec![],
        ExecutionMetadata::default(),
    )
    .await
    .unwrap();

    // Queue should now be empty
    assert!(store.fetch_orchestration_item(30).await.unwrap().is_none());

    // History should remain unchanged (no new events)
    let hist = store.read(instance).await.unwrap_or_default();
    assert!(hist.iter().any(|e| matches!(e, Event::OrchestrationCompleted { .. })));
}

#[tokio::test]
async fn test_fetch_orchestration_item_new_instance() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue start work (provider will create instance lazily on fetch)
    store
        .enqueue_for_orchestrator(
            WorkItem::StartOrchestration {
                instance: "test-instance".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Fetch orchestration item
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();

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
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Seed instance history using test helper
    test_create_execution(
        store.as_ref(),
        "test-instance",
        "TestOrch",
        "1.0.0",
        "test-input",
        None,
        None,
    )
    .await
    .unwrap();
    store
        .append_with_execution(
            "test-instance",
            1,
            vec![Event::ActivityScheduled {
                event_id: 2,
                name: "TestActivity".to_string(),
                input: "activity-input".to_string(),
                execution_id: 1,
            }],
        )
        .await
        .unwrap();

    // Enqueue completion
    store
        .enqueue_for_orchestrator(
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "activity-result".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch orchestration item
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();

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
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // No work items
    let item = store.fetch_orchestration_item(30).await.unwrap();
    assert!(item.is_none());
}

#[tokio::test]
async fn test_ack_orchestration_item_atomic() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Setup: enqueue start work; provider will create instance lazily
    store
        .enqueue_for_orchestrator(
            WorkItem::StartOrchestration {
                instance: "test-instance".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Fetch and get lock token
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Prepare updates
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "test-input".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
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
        .ack_orchestration_item(
            &lock_token,
            1,
            history_delta,
            worker_items,
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Verify history was updated
    let history = store.read("test-instance").await.unwrap_or_default();
    assert_eq!(history.len(), 2);
    assert!(matches!(&history[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(&history[1], Event::ActivityScheduled { .. }));

    // Verify worker item was enqueued
    let (worker_item, _) = store.fetch_work_item(30).await.unwrap();
    assert!(matches!(worker_item, WorkItem::ActivityExecute { .. }));

    // Verify orchestrator queue is empty (item was acked)
    assert!(store.fetch_orchestration_item(30).await.unwrap().is_none());
}

#[tokio::test]
async fn test_ack_orchestration_item_error_handling() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Try to ack with invalid token
    let result = store
        .ack_orchestration_item("invalid-token", 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("Invalid lock token"));
}

#[tokio::test]
async fn test_abandon_orchestration_item() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Setup: enqueue start work; provider will create instance lazily
    store
        .enqueue_for_orchestrator(
            WorkItem::StartOrchestration {
                instance: "test-instance".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Fetch and get lock token
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Abandon the item
    store.abandon_orchestration_item(&lock_token, None).await.unwrap();

    // Verify item is back in queue
    let item2 = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item2.instance, "test-instance");
    assert!(matches!(
        &item2.messages[0],
        WorkItem::StartOrchestration { orchestration, .. } if orchestration == "TestOrch"
    ));
}

#[tokio::test]
async fn test_abandon_orchestration_item_with_delay() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Setup: enqueue start work; provider will create instance lazily
    store
        .enqueue_for_orchestrator(
            WorkItem::StartOrchestration {
                instance: "test-instance".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Fetch and get lock token
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Abandon with delay (sqlite supports delayed visibility)
    store.abandon_orchestration_item(&lock_token, Some(500)).await.unwrap();
    // Should not be visible immediately
    assert!(store.fetch_orchestration_item(30).await.unwrap().is_none());
    // After delay, it should be visible
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let item2 = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item2.instance, "test-instance");
}

#[tokio::test]
async fn test_abandon_orchestration_item_error_handling() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Try to abandon with invalid token
    let result = store.abandon_orchestration_item("invalid-token", None).await;

    // sqlite provider returns error for invalid tokens
    assert!(result.is_err());
}

#[tokio::test]
async fn test_in_memory_provider_atomic_operations() {
    let store: Arc<dyn Provider> = Arc::new(SqliteProvider::new_in_memory().await.unwrap());

    // Enqueue work (in-memory will lazily create instance on fetch)
    store
        .enqueue_for_orchestrator(
            WorkItem::StartOrchestration {
                instance: "test-instance".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Test fetch
    let item = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item.instance, "test-instance");
    assert_eq!(item.orchestration_name, "TestOrch");
    let lock_token = item.lock_token.clone();

    // Test ack with updates
    let history_delta = vec![Event::OrchestrationStarted {
        event_id: 1,
        name: "TestOrch".to_string(),
        version: "1.0.0".to_string(),
        input: "test-input".to_string(),
        parent_instance: None,
        parent_id: None,
    }];

    store
        .ack_orchestration_item(
            &lock_token,
            1,
            history_delta,
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Verify history
    let history = store.read("test-instance").await.unwrap_or_default();
    assert_eq!(history.len(), 1);
    assert!(matches!(&history[0], Event::OrchestrationStarted { .. }));

    // Test abandon
    store
        .enqueue_for_orchestrator(
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "result".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    let item2 = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    let lock_token2 = item2.lock_token.clone();

    store.abandon_orchestration_item(&lock_token2, None).await.unwrap();

    // Should be available again
    let item3 = store.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item3.instance, "test-instance");
}

// ===== Batch Fetch Tests =====

#[tokio::test]
async fn test_fetch_orchestration_items_batch_empty() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Fetch from empty queue
    let items = store.fetch_orchestration_items_batch(5, 30).await.unwrap();
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_fetch_orchestration_items_batch_multiple() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 3 orchestration start items
    for i in 1..=3 {
        store
            .enqueue_for_orchestrator(
                WorkItem::StartOrchestration {
                    instance: format!("batch-test-{}", i),
                    orchestration: "BatchOrch".to_string(),
                    input: format!("input-{}", i),
                    version: Some("1.0.0".to_string()),
                    parent_instance: None,
                    parent_id: None,
                    execution_id: duroxide::INITIAL_EXECUTION_ID,
                },
                None,
            )
            .await
            .unwrap();
    }

    // Fetch batch of 5 (should get 3)
    let items = store.fetch_orchestration_items_batch(5, 30).await.unwrap();
    assert_eq!(items.len(), 3);

    // Verify unique instances
    let instances: std::collections::HashSet<String> = items.iter().map(|i| i.instance.clone()).collect();
    assert_eq!(instances.len(), 3);

    // Verify unique lock tokens
    let tokens: std::collections::HashSet<String> = items.iter().map(|i| i.lock_token.clone()).collect();
    assert_eq!(tokens.len(), 3);

    // Verify each item has correct metadata
    for item in &items {
        assert!(item.instance.starts_with("batch-test-"));
        assert_eq!(item.orchestration_name, "BatchOrch");
        assert_eq!(item.version, "1.0.0");
        assert_eq!(item.execution_id, 1);
        assert_eq!(item.messages.len(), 1);
        assert!(item.history.is_empty()); // New instances
    }

    // Ack each item independently
    for item in items {
        store
            .ack_orchestration_item(
                &item.lock_token,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: 1,
                    name: "BatchOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "input".to_string(),
                    parent_instance: None,
                    parent_id: None,
                }],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await
            .unwrap();
    }

    // Queue should be empty now
    let items = store.fetch_orchestration_items_batch(5, 30).await.unwrap();
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_fetch_orchestration_items_batch_respects_limit() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 10 items
    for i in 1..=10 {
        store
            .enqueue_for_orchestrator(
                WorkItem::StartOrchestration {
                    instance: format!("limit-test-{}", i),
                    orchestration: "LimitOrch".to_string(),
                    input: "input".to_string(),
                    version: Some("1.0.0".to_string()),
                    parent_instance: None,
                    parent_id: None,
                    execution_id: duroxide::INITIAL_EXECUTION_ID,
                },
                None,
            )
            .await
            .unwrap();
    }

    // Fetch batch of 3
    let items = store.fetch_orchestration_items_batch(3, 30).await.unwrap();
    assert_eq!(items.len(), 3);

    // Fetch another batch of 5
    let items2 = store.fetch_orchestration_items_batch(5, 30).await.unwrap();
    assert_eq!(items2.len(), 5);

    // Remaining should be 2
    let items3 = store.fetch_orchestration_items_batch(10, 30).await.unwrap();
    assert_eq!(items3.len(), 2);
}

#[tokio::test]
async fn test_fetch_work_items_batch_empty() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Fetch from empty queue
    let items = store.fetch_work_items_batch(5, 30).await;
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_fetch_work_items_batch_multiple() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 4 work items
    for i in 1..=4 {
        store
            .enqueue_for_worker(WorkItem::ActivityExecute {
                instance: format!("work-batch-{}", i),
                execution_id: 1,
                id: i,
                name: "BatchActivity".to_string(),
                input: format!("input-{}", i),
            })
            .await
            .unwrap();
    }

    // Fetch batch of 10 (should get 4)
    let items = store.fetch_work_items_batch(10, 30).await;
    assert_eq!(items.len(), 4);

    // Verify unique lock tokens
    let tokens: std::collections::HashSet<String> = items.iter().map(|(_, token)| token.clone()).collect();
    assert_eq!(tokens.len(), 4);

    // Verify each work item
    for (work_item, token) in &items {
        match work_item {
            WorkItem::ActivityExecute {
                instance,
                execution_id,
                id,
                name,
                ..
            } => {
                assert!(instance.starts_with("work-batch-"));
                assert_eq!(*execution_id, 1);
                assert!(*id >= 1 && *id <= 4);
                assert_eq!(name, "BatchActivity");
                assert!(!token.is_empty());
            }
            _ => panic!("Expected ActivityExecute"),
        }
    }

    // Ack each item independently
    for (work_item, token) in items {
        let instance = match &work_item {
            WorkItem::ActivityExecute { instance, .. } => instance.clone(),
            _ => panic!("Expected ActivityExecute"),
        };
        
        store
            .ack_work_item(
                &token,
                WorkItem::ActivityCompleted {
                    instance,
                    execution_id: 1,
                    id: 1,
                    result: "done".to_string(),
                },
            )
            .await
            .unwrap();
    }

    // Worker queue should be empty now
    let items = store.fetch_work_items_batch(10, 30).await;
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_fetch_work_items_batch_respects_limit() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 7 work items
    for i in 1..=7 {
        store
            .enqueue_for_worker(WorkItem::ActivityExecute {
                instance: "limit-work".to_string(),
                execution_id: 1,
                id: i,
                name: "LimitActivity".to_string(),
                input: format!("input-{}", i),
            })
            .await
            .unwrap();
    }

    // Fetch batch of 2
    let items = store.fetch_work_items_batch(2, 30).await;
    assert_eq!(items.len(), 2);

    // Fetch batch of 3
    let items2 = store.fetch_work_items_batch(3, 30).await;
    assert_eq!(items2.len(), 3);

    // Remaining should be 2
    let items3 = store.fetch_work_items_batch(10, 30).await;
    assert_eq!(items3.len(), 2);
}

#[tokio::test]
async fn test_batch_fetch_independent_locks() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 3 orchestrations
    for i in 1..=3 {
        store
            .enqueue_for_orchestrator(
                WorkItem::StartOrchestration {
                    instance: format!("independent-{}", i),
                    orchestration: "IndOrch".to_string(),
                    input: "input".to_string(),
                    version: Some("1.0.0".to_string()),
                    parent_instance: None,
                    parent_id: None,
                    execution_id: duroxide::INITIAL_EXECUTION_ID,
                },
                None,
            )
            .await
            .unwrap();
    }

    // Fetch all 3
    let items = store.fetch_orchestration_items_batch(3, 30).await.unwrap();
    assert_eq!(items.len(), 3);

    // Abandon item 1
    store.abandon_orchestration_item(&items[0].lock_token, None).await.unwrap();

    // Ack item 2
    store
        .ack_orchestration_item(
            &items[1].lock_token,
            1,
            vec![Event::OrchestrationStarted {
                event_id: 1,
                name: "IndOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "input".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Abandon item 3
    store.abandon_orchestration_item(&items[2].lock_token, None).await.unwrap();

    // Item 1 and 3 should be available again (abandoned), item 2 should be gone (acked)
    let items2 = store.fetch_orchestration_items_batch(5, 30).await.unwrap();
    assert_eq!(items2.len(), 2);

    let instances: std::collections::HashSet<String> = items2.iter().map(|i| i.instance.clone()).collect();
    assert!(instances.contains("independent-1"));
    assert!(!instances.contains("independent-2")); // This one was acked
    assert!(instances.contains("independent-3"));
}

#[tokio::test]
async fn test_batch_fetch_abandoned_items_available_orchestration() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 5 orchestration items
    for i in 1..=5 {
        store
            .enqueue_for_orchestrator(
                WorkItem::StartOrchestration {
                    instance: format!("abandon-batch-{}", i),
                    orchestration: "AbandonOrch".to_string(),
                    input: format!("input-{}", i),
                    version: Some("1.0.0".to_string()),
                    parent_instance: None,
                    parent_id: None,
                    execution_id: duroxide::INITIAL_EXECUTION_ID,
                },
                None,
            )
            .await
            .unwrap();
    }

    // Fetch batch of 3
    let items = store.fetch_orchestration_items_batch(3, 30).await.unwrap();
    assert_eq!(items.len(), 3);

    // Extract instance names for verification
    let fetched_instances: Vec<String> = items.iter().map(|i| i.instance.clone()).collect();

    // Abandon all 3 items
    for item in &items {
        store.abandon_orchestration_item(&item.lock_token, None).await.unwrap();
    }

    // Fetch another batch of 5 - should get all 5 (3 abandoned + 2 that weren't fetched)
    let items2 = store.fetch_orchestration_items_batch(5, 30).await.unwrap();
    assert_eq!(items2.len(), 5);

    // Verify the abandoned items are present
    let refetched_instances: std::collections::HashSet<String> = 
        items2.iter().map(|i| i.instance.clone()).collect();
    
    for instance in &fetched_instances {
        assert!(
            refetched_instances.contains(instance),
            "Abandoned instance {} should be available again",
            instance
        );
    }
}

#[tokio::test]
async fn test_batch_fetch_abandoned_items_available_work() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 6 work items
    for i in 1..=6 {
        store
            .enqueue_for_worker(WorkItem::ActivityExecute {
                instance: format!("work-abandon-{}", i),
                execution_id: 1,
                id: i,
                name: "AbandonActivity".to_string(),
                input: format!("input-{}", i),
            })
            .await
            .unwrap();
    }

    // Fetch batch of 4 with SHORT lock timeout (1 second) to test lock expiration
    let items = store.fetch_work_items_batch(4, 1).await;
    assert_eq!(items.len(), 4);

    // Extract activity IDs for verification
    let fetched_ids: Vec<u64> = items
        .iter()
        .map(|(item, _)| match item {
            WorkItem::ActivityExecute { id, .. } => *id,
            _ => panic!("Expected ActivityExecute"),
        })
        .collect();

    // Wait for locks to expire (abandonment via timeout)
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    // Fetch another batch of 6 - should get all 6 (4 expired locks + 2 that weren't fetched)
    let items2 = store.fetch_work_items_batch(6, 30).await;
    assert_eq!(items2.len(), 6);

    // Verify the "abandoned" (expired lock) items are present
    let refetched_ids: std::collections::HashSet<u64> = items2
        .iter()
        .map(|(item, _)| match item {
            WorkItem::ActivityExecute { id, .. } => *id,
            _ => panic!("Expected ActivityExecute"),
        })
        .collect();
    
    for id in &fetched_ids {
        assert!(
            refetched_ids.contains(id),
            "Work item with expired lock (id {}) should be available again",
            id
        );
    }
}

#[tokio::test]
async fn test_batch_fetch_partial_abandon_orchestration() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 4 orchestration items
    for i in 1..=4 {
        store
            .enqueue_for_orchestrator(
                WorkItem::StartOrchestration {
                    instance: format!("partial-abandon-{}", i),
                    orchestration: "PartialOrch".to_string(),
                    input: format!("input-{}", i),
                    version: Some("1.0.0".to_string()),
                    parent_instance: None,
                    parent_id: None,
                    execution_id: duroxide::INITIAL_EXECUTION_ID,
                },
                None,
            )
            .await
            .unwrap();
    }

    // Fetch batch of 4
    let items = store.fetch_orchestration_items_batch(4, 30).await.unwrap();
    assert_eq!(items.len(), 4);

    // Ack first 2 items
    for item in &items[0..2] {
        store
            .ack_orchestration_item(
                &item.lock_token,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: 1,
                    name: "PartialOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "input".to_string(),
                    parent_instance: None,
                    parent_id: None,
                }],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await
            .unwrap();
    }

    // Abandon last 2 items
    for item in &items[2..4] {
        store.abandon_orchestration_item(&item.lock_token, None).await.unwrap();
    }

    // Fetch again - should only get the 2 abandoned items
    let items2 = store.fetch_orchestration_items_batch(10, 30).await.unwrap();
    assert_eq!(items2.len(), 2);

    // Verify only the abandoned instances are present
    let refetched_instances: std::collections::HashSet<String> = 
        items2.iter().map(|i| i.instance.clone()).collect();
    
    assert!(refetched_instances.contains(&items[2].instance));
    assert!(refetched_instances.contains(&items[3].instance));
    assert!(!refetched_instances.contains(&items[0].instance)); // Acked
    assert!(!refetched_instances.contains(&items[1].instance)); // Acked
}

#[tokio::test]
async fn test_batch_fetch_partial_abandon_work() {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store: Arc<SqliteProvider> = Arc::new(SqliteProvider::new(&db_url, None).await.unwrap());

    // Enqueue 5 work items
    for i in 1..=5 {
        store
            .enqueue_for_worker(WorkItem::ActivityExecute {
                instance: format!("work-partial-{}", i),
                execution_id: 1,
                id: i,
                name: "PartialActivity".to_string(),
                input: format!("input-{}", i),
            })
            .await
            .unwrap();
    }

    // Fetch batch of 5 with SHORT lock timeout for last 2 items test
    let items = store.fetch_work_items_batch(5, 1).await;
    assert_eq!(items.len(), 5);

    // Ack first 3 items
    for (work_item, token) in &items[0..3] {
        let instance = match work_item {
            WorkItem::ActivityExecute { instance, .. } => instance.clone(),
            _ => panic!("Expected ActivityExecute"),
        };
        
        store
            .ack_work_item(
                token,
                WorkItem::ActivityCompleted {
                    instance,
                    execution_id: 1,
                    id: 1,
                    result: "done".to_string(),
                },
            )
            .await
            .unwrap();
    }

    // Let locks expire on last 2 items (implicit abandonment)
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    // Fetch again - should only get the 2 items with expired locks
    let items2 = store.fetch_work_items_batch(10, 30).await;
    assert_eq!(items2.len(), 2);

    // Verify only the abandoned items are present
    let refetched_ids: std::collections::HashSet<u64> = items2
        .iter()
        .map(|(item, _)| match item {
            WorkItem::ActivityExecute { id, .. } => *id,
            _ => panic!("Expected ActivityExecute"),
        })
        .collect();
    
    let abandoned_id_1 = match &items[3].0 {
        WorkItem::ActivityExecute { id, .. } => *id,
        _ => panic!("Expected ActivityExecute"),
    };
    let abandoned_id_2 = match &items[4].0 {
        WorkItem::ActivityExecute { id, .. } => *id,
        _ => panic!("Expected ActivityExecute"),
    };
    
    assert!(refetched_ids.contains(&abandoned_id_1));
    assert!(refetched_ids.contains(&abandoned_id_2));
}
