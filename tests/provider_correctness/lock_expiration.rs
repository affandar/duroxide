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

/// Test 4.1: Lock Expires After Timeout
/// Goal: Verify locks expire and instance becomes available again.
#[tokio::test]
async fn test_lock_expires_after_timeout() {
    let provider = create_provider().await;

    // Setup: create and fetch item
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Wait for lock to expire
    tokio::time::sleep(Duration::from_millis(TEST_LOCK_TIMEOUT_MS + 100)).await;

    // Instance should be available again
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
    assert_ne!(item2.lock_token, lock_token, "Should have new lock token");

    // Original lock token should no longer work
    let result = provider
        .ack_orchestration_item(&lock_token, 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await;
    assert!(result.is_err());
}

/// Test 4.2: Abandon Releases Lock Immediately
/// Goal: Verify abandon releases lock without waiting for expiration.
#[tokio::test]
async fn test_abandon_releases_lock_immediately() {
    let provider = create_provider().await;

    // Setup: create and fetch item
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Abandon the lock
    provider.abandon_orchestration_item(&lock_token, None).await.unwrap();

    // Lock should be released immediately (don't need to wait for expiration)
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
}

/// Test 4.3: Lock Renewal on Ack
/// Goal: Verify successful ack releases lock immediately.
#[tokio::test]
async fn test_lock_renewal_on_ack() {
    let provider = create_provider().await;

    // Setup: create and fetch item
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let _lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Enqueue another item for the same instance while locked
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();

    // Item should not be available yet (lock still held)
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Ack successfully
    provider
        .ack_orchestration_item(
            &_lock_token,
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

    // The new item should be available immediately after ack
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
}

/// Test 4.4: Concurrent Lock Attempts Respect Expiration
/// Goal: Verify multiple dispatchers respect lock expiration times.
#[tokio::test]
async fn test_concurrent_lock_attempts_respect_expiration() {
    let provider = Arc::new(create_provider().await);

    // Setup: create and fetch item
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    let _lock_token = item.lock_token.clone();

    // Spawn multiple concurrent fetchers
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let provider = provider.clone();
            tokio::spawn(async move {
                // Stagger the attempts slightly
                tokio::time::sleep(Duration::from_millis(i * 50)).await;
                provider.fetch_orchestration_item().await
            })
        })
        .collect();

    // Wait a bit before expiration
    tokio::time::sleep(Duration::from_millis(200)).await;

    // All should still return None (lock held)
    let results = futures::future::join_all(handles).await;
    for result in results {
        assert!(result.unwrap().is_none());
    }

    // Wait for lock expiration
    tokio::time::sleep(Duration::from_millis(TEST_LOCK_TIMEOUT_MS - 200 + 100)).await;

    // Now one should succeed
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
}
