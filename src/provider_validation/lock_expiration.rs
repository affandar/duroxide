use crate::provider_validation::{Event, ExecutionMetadata, start_item};
use crate::provider_validations::ProviderFactory;
use std::sync::Arc;
use std::time::Duration;

/// Test 4.1: Lock Expires After Timeout
/// Goal: Verify locks expire and instance becomes available again.
pub async fn test_lock_expires_after_timeout<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: lock expires after timeout");
    let provider = factory.create_provider().await;

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item(30).await.unwrap().is_none());

    // Wait for lock to expire
    tokio::time::sleep(Duration::from_millis(factory.lock_timeout_ms() + 100)).await;

    // Instance should be available again
    let item2 = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    assert_ne!(item2.lock_token, lock_token, "Should have new lock token");

    // Original lock token should no longer work
    let result = provider
        .ack_orchestration_item(&lock_token, 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await;
    assert!(result.is_err());
    tracing::info!("✓ Test passed: lock expiration verified");
}

/// Test 4.2: Abandon Releases Lock Immediately
/// Goal: Verify abandon releases lock without waiting for expiration.
pub async fn test_abandon_releases_lock_immediately<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: abandon releases lock immediately");
    let provider = factory.create_provider().await;

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item(30).await.unwrap().is_none());

    // Abandon the lock
    provider.abandon_orchestration_item(&lock_token, None).await.unwrap();

    // Lock should be released immediately (don't need to wait for expiration)
    let item2 = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    tracing::info!("✓ Test passed: abandon releases lock verified");
}

/// Test 4.3: Lock Renewal on Ack
/// Goal: Verify successful ack releases lock immediately.
pub async fn test_lock_renewal_on_ack<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: lock renewal on ack");
    let provider = factory.create_provider().await;

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    let _lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item(30).await.unwrap().is_none());

    // Enqueue another item for the same instance while locked
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();

    // Item should not be available yet (lock still held)
    assert!(provider.fetch_orchestration_item(30).await.unwrap().is_none());

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
    let item2 = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    tracing::info!("✓ Test passed: lock renewal on ack verified");
}

/// Test 4.4: Concurrent Lock Attempts Respect Expiration
/// Goal: Verify multiple dispatchers respect lock expiration times.
pub async fn test_concurrent_lock_attempts_respect_expiration<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: concurrent lock attempts respect expiration");
    let provider = Arc::new(factory.create_provider().await);

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    let _lock_token = item.lock_token.clone();

    // Spawn multiple concurrent fetchers
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let provider = provider.clone();
            tokio::spawn(async move {
                // Stagger the attempts slightly
                tokio::time::sleep(Duration::from_millis(i * 50)).await;
                provider.fetch_orchestration_item(30).await.unwrap()
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
    tokio::time::sleep(Duration::from_millis(factory.lock_timeout_ms() - 200 + 100)).await;

    // Now one should succeed
    let item2 = provider.fetch_orchestration_item(30).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    tracing::info!("✓ Test passed: concurrent lock attempts respect expiration verified");
}
