use duroxide::providers::sqlite::{SqliteProvider, SqliteOptions};
use duroxide::providers::{ExecutionMetadata, Provider, WorkItem};
use duroxide::Event;
use std::sync::Arc;
use std::time::Duration;

#[path = "../common/mod.rs"]
mod common;
use common::test_create_execution;

const TEST_LOCK_TIMEOUT_MS: u64 = 1000;

/// Helper to create a provider for testing
async fn create_provider() -> Arc<dyn Provider> {
    let options = SqliteOptions {
        lock_timeout: Duration::from_millis(TEST_LOCK_TIMEOUT_MS),
    };
    Arc::new(SqliteProvider::new_in_memory_with_options(Some(options)).await.unwrap())
}

/// Helper to create a start item for an instance
fn start_item(instance: &str) -> WorkItem {
    WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrch".to_string(),
        input: "{}".to_string(),
        version: Some("1.0.0".to_string()),
        parent_instance: None,
        parent_id: None,
        execution_id: duroxide::INITIAL_EXECUTION_ID,
    }
}

/// Test 2.1: All-or-Nothing Ack
/// Goal: Verify `ack_orchestration_item` commits everything atomically.
#[tokio::test]
async fn test_atomicity_failure_rollback() {
    let provider = create_provider().await;

    // Setup: create instance with some initial state
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Ack with valid data to establish baseline
    provider
        .ack_orchestration_item(
            &lock_token,
            1,
            vec![Event::OrchestrationStarted {
                event_id: 1,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Verify initial state
    let initial_history = provider.read("instance-A").await;
    assert_eq!(initial_history.len(), 1);

    // Now enqueue another work item
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    let lock_token2 = item2.lock_token.clone();

    // Try to ack with duplicate event_id (should fail due to primary key constraint)
    let result = provider
        .ack_orchestration_item(
            &lock_token2,
            1,
            vec![Event::OrchestrationStarted {
                event_id: 1, // DUPLICATE - should fail
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await;

    assert!(result.is_err(), "Should reject duplicate event_id");

    // Verify state unchanged - history should still have only 1 event
    let after_history = provider.read("instance-A").await;
    assert_eq!(after_history.len(), 1, "History should remain unchanged after failed ack");

    // Lock should still be held, preventing another fetch
    assert!(provider.fetch_orchestration_item().await.is_none());
}

/// Test 2.2: Multi-Operation Atomic Ack
/// Goal: Verify complex ack with multiple outputs succeeds atomically.
#[tokio::test]
async fn test_multi_operation_atomic_ack() {
    let provider = create_provider().await;

    // Setup
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Prepare complex ack with multiple operations
    let history_delta = vec![
        Event::OrchestrationStarted {
            event_id: 1,
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        },
        Event::ActivityScheduled {
            event_id: 2,
            name: "Activity1".to_string(),
            input: "input1".to_string(),
            execution_id: 1,
        },
        Event::ActivityScheduled {
            event_id: 3,
            name: "Activity2".to_string(),
            input: "input2".to_string(),
            execution_id: 1,
        },
        Event::ActivityScheduled {
            event_id: 4,
            name: "Activity3".to_string(),
            input: "input3".to_string(),
            execution_id: 1,
        },
        Event::TimerCreated {
            event_id: 5,
            fire_at_ms: 1234567890,
            execution_id: 1,
        },
    ];

    let worker_items = vec![
        WorkItem::ActivityExecute {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 1,
            name: "Activity1".to_string(),
            input: "input1".to_string(),
        },
        WorkItem::ActivityExecute {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 2,
            name: "Activity2".to_string(),
            input: "input2".to_string(),
        },
        WorkItem::ActivityExecute {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 3,
            name: "Activity3".to_string(),
            input: "input3".to_string(),
        },
    ];

    let orchestrator_items = vec![
        WorkItem::TimerFired {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 1,
            fire_at_ms: 1234567890,
        },
        WorkItem::ExternalRaised {
            instance: "instance-A".to_string(),
            name: "TestEvent".to_string(),
            data: "test-data".to_string(),
        },
    ];

    // Ack with all operations
    provider
        .ack_orchestration_item(
            &lock_token,
            1,
            history_delta,
            worker_items,
            orchestrator_items,
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Verify ALL operations succeeded together
    // 1. History should have 5 events
    let history = provider.read("instance-A").await;
    assert_eq!(history.len(), 5, "All 5 history events should be persisted");

    // 2. Worker queue should have 3 items
    for _ in 0..3 {
        assert!(provider.dequeue_worker_peek_lock().await.is_some());
    }
    assert!(provider.dequeue_worker_peek_lock().await.is_none());

    // 3. Orchestrator queue should have 2 items
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item1.messages.len(), 2, "Should have TimerFired and ExternalRaised");
    assert!(matches!(&item1.messages[0], WorkItem::TimerFired { .. }));
    assert!(matches!(&item1.messages[1], WorkItem::ExternalRaised { .. }));
}

/// Test 2.3: Lock Released Only on Successful Ack
/// Goal: Ensure lock is only released after successful commit.
#[tokio::test]
async fn test_lock_released_only_on_successful_ack() {
    let provider = create_provider().await;

    // Setup
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let _lock_token = item.lock_token.clone();

    // Verify lock is held (can't fetch again)
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Attempt ack with invalid lock token (should fail)
    let _result = provider
        .ack_orchestration_item(
            "invalid-lock-token",
            1,
            vec![Event::OrchestrationStarted {
                event_id: 2,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await;

    // Should fail with invalid lock token
    assert!(_result.is_err());

    // Lock should still be held
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Wait for lock expiration
    tokio::time::sleep(Duration::from_millis(TEST_LOCK_TIMEOUT_MS + 100)).await;

    // Now should be able to fetch again
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
}

/// Test 2.4: Concurrent Ack Prevention
/// Goal: Ensure two acks with same lock token don't both succeed.
#[tokio::test]
async fn test_concurrent_ack_prevention() {
    let provider = Arc::new(create_provider().await);

    // Setup
    provider.enqueue_orchestrator_work(start_item("instance-A"), None).await.unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Spawn two concurrent tasks trying to ack with same lock token
    let provider1 = provider.clone();
    let provider2 = provider.clone();
    let token1 = lock_token.clone();
    let token2 = lock_token.clone();

    let handle1 = tokio::spawn(async move {
        provider1
            .ack_orchestration_item(
                &token1,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: 1,
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                }],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await
    });

    let handle2 = tokio::spawn(async move {
        provider2
            .ack_orchestration_item(
                &token2,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: 1,
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                }],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await
    });

    let results = futures::future::join_all(vec![handle1, handle2]).await;
    let results: Vec<_> = results
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // Exactly one should succeed
    let successes: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
    assert_eq!(successes.len(), 1, "Exactly one ack should succeed");

    // The other should fail
    let failures: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
    assert_eq!(failures.len(), 1, "Exactly one ack should fail");
}
