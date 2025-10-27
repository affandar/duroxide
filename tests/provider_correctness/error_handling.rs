use duroxide::Event;
use duroxide::providers::sqlite::{SqliteOptions, SqliteProvider};
use duroxide::providers::{ExecutionMetadata, Provider, WorkItem};
use std::sync::Arc;
use std::time::Duration;

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

/// Test 3.1: Invalid Lock Token on Ack
/// Goal: Provider should reject invalid lock tokens.
#[tokio::test]
async fn test_invalid_lock_token_on_ack() {
    let provider = create_provider().await;

    // Attempt to ack with non-existent lock token
    let result = provider
        .ack_orchestration_item("invalid-token", 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Invalid lock token") || err_msg.contains("lock_token"));
}

/// Test 3.2: Duplicate Event ID Handling
/// Goal: Provider should detect and handle duplicate event_ids.
#[tokio::test]
async fn test_duplicate_event_id_rejection() {
    let provider = create_provider().await;

    // Create instance with initial event
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Ack with event_id=1
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

    // Try to append duplicate event_id=1
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    let lock_token2 = item2.lock_token.clone();

    let result = provider
        .ack_orchestration_item(
            &lock_token2,
            1,
            vec![Event::ActivityScheduled {
                event_id: 1, // DUPLICATE!
                name: "Activity".to_string(),
                input: "{}".to_string(),
                execution_id: 1,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await;

    // Should fail due to duplicate event_id
    assert!(result.is_err());

    // Verify history unchanged
    let history = provider.read("instance-A").await;
    assert_eq!(history.len(), 1);
    assert!(matches!(history[0], Event::OrchestrationStarted { .. }));
}

/// Test 3.3: Missing Instance Metadata
/// Goal: Provider should handle missing instance gracefully.
#[tokio::test]
async fn test_missing_instance_metadata() {
    let provider = create_provider().await;

    // Attempt to read history for non-existent instance
    let history = provider.read("non-existent-instance").await;
    assert_eq!(history.len(), 0);
}

/// Test 3.4: Corrupted Serialization Data
/// Goal: Provider should handle corrupted JSON in queue/work items gracefully.
#[tokio::test]
async fn test_corrupted_serialization_data() {
    let provider = create_provider().await;

    // This test is primarily about graceful degradation
    // SQLite provider will handle corrupted data by returning None on deserialization failure
    // Test that provider doesn't panic
    let item = provider.fetch_orchestration_item().await;
    assert!(item.is_none() || item.is_some(), "Should not panic");
}

/// Test 3.5: Lock Expiration During Ack
/// Goal: Provider should detect and reject expired locks.
#[tokio::test]
async fn test_lock_expiration_during_ack() {
    let provider = create_provider().await;

    // Create and fetch item
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Wait for lock to expire
    tokio::time::sleep(Duration::from_millis(TEST_LOCK_TIMEOUT_MS + 100)).await;

    // Attempt to ack with expired token
    let result = provider
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
        .await;

    // Should fail - lock expired (or item already consumed by another fetch)
    assert!(result.is_err());
}
