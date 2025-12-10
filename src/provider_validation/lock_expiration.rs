use crate::provider_validation::{Event, EventKind, ExecutionMetadata, start_item};
use crate::provider_validations::ProviderFactory;
use std::sync::Arc;
use std::time::Duration;

fn provider_lock_timeout<F: ProviderFactory>(factory: &F) -> Duration {
    factory.lock_timeout()
}

/// Test 4.1: Lock Expires After Timeout
/// Goal: Verify locks expire and instance becomes available again.
pub async fn test_lock_expires_after_timeout<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: lock expires after timeout");
    let provider = factory.create_provider().await;
    let lock_timeout = provider_lock_timeout(factory);

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().is_none());

    // Wait for lock to expire
    tokio::time::sleep(lock_timeout + Duration::from_millis(100)).await;

    // Instance should be available again
    let item2 = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
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
    let lock_timeout = provider_lock_timeout(factory);

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    let lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().is_none());

    // Abandon the lock
    provider.abandon_orchestration_item(&lock_token, None).await.unwrap();

    // Lock should be released immediately (don't need to wait for expiration)
    let item2 = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    tracing::info!("✓ Test passed: abandon releases lock verified");
}

/// Test 4.3: Lock Renewal on Ack
/// Goal: Verify successful ack releases lock immediately.
pub async fn test_lock_renewal_on_ack<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: lock renewal on ack");
    let provider = factory.create_provider().await;
    let lock_timeout = provider_lock_timeout(factory);

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    let _lock_token = item.lock_token.clone();

    // Verify lock is held
    assert!(provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().is_none());

    // Enqueue another item for the same instance while locked
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();

    // Item should not be available yet (lock still held)
    assert!(provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().is_none());

    // Ack successfully
    provider
        .ack_orchestration_item(
            &_lock_token,
            1,
            vec![Event::with_event_id(
                1,
                "instance-A".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // The new item should be available immediately after ack
    let item2 = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    tracing::info!("✓ Test passed: lock renewal on ack verified");
}

/// Test 4.4: Concurrent Lock Attempts Respect Expiration
/// Goal: Verify multiple dispatchers respect lock expiration times.
pub async fn test_concurrent_lock_attempts_respect_expiration<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing lock expiration: concurrent lock attempts respect expiration");
    let provider = Arc::new(factory.create_provider().await);
    let lock_timeout = provider_lock_timeout(factory);

    // Setup: create and fetch item
    provider
        .enqueue_for_orchestrator(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    let _lock_token = item.lock_token.clone();

    // Spawn multiple concurrent fetchers
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let provider = provider.clone();
            tokio::spawn(async move {
                // Stagger the attempts slightly
                tokio::time::sleep(Duration::from_millis(i * 50)).await;
                provider.fetch_orchestration_item(lock_timeout, None).await.unwrap()
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
    let wait = lock_timeout.checked_sub(Duration::from_millis(200)).unwrap_or_default() + Duration::from_millis(100);
    tokio::time::sleep(wait).await;

    // Now one should succeed
    let item2 = provider.fetch_orchestration_item(lock_timeout, None).await.unwrap().unwrap();
    assert_eq!(item2.instance, "instance-A");
    tracing::info!("✓ Test passed: concurrent lock attempts respect expiration verified");
}

/// Test 4.5: Worker Lock Renewal Success
/// Goal: Verify worker lock can be renewed with valid token
pub async fn test_worker_lock_renewal_success<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing worker lock renewal: renewal succeeds with valid token");
    let provider = factory.create_provider().await;
    let lock_timeout = provider_lock_timeout(factory);

    use crate::providers::WorkItem;

    // Enqueue and fetch work item
    provider
        .enqueue_for_worker(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            name: "TestActivity".to_string(),
            input: "test".to_string(),
        })
        .await
        .unwrap();

    let (_item, token) = provider.fetch_work_item(lock_timeout, None).await.unwrap().unwrap();
    tracing::info!("Fetched work item with lock token: {}", token);

    // Verify item is locked (can't fetch again)
    assert!(provider.fetch_work_item(lock_timeout, None).await.unwrap().is_none());

    // Wait a bit to simulate activity in progress
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Renew lock
    provider.renew_work_item_lock(&token, lock_timeout).await.unwrap();
    tracing::info!("Successfully renewed lock");

    // Item should still be locked
    assert!(provider.fetch_work_item(lock_timeout, None).await.unwrap().is_none());

    tracing::info!("✓ Test passed: worker lock renewal success verified");
}

/// Test 4.6: Worker Lock Renewal Invalid Token
/// Goal: Verify renewal fails with invalid token
pub async fn test_worker_lock_renewal_invalid_token<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing worker lock renewal: renewal fails with invalid token");
    let provider = factory.create_provider().await;

    // Try to renew with invalid token
    let result = provider
        .renew_work_item_lock("invalid-token-123", Duration::from_secs(30))
        .await;
    assert!(result.is_err(), "Should fail with invalid token");
    tracing::info!("✓ Test passed: invalid token rejection verified");
}

/// Test 4.7: Worker Lock Renewal After Expiration
/// Goal: Verify renewal fails after lock expires
pub async fn test_worker_lock_renewal_after_expiration<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing worker lock renewal: renewal fails after expiration");
    let provider = factory.create_provider().await;

    use crate::providers::WorkItem;

    // Enqueue and fetch work item with short timeout
    provider
        .enqueue_for_worker(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            name: "TestActivity".to_string(),
            input: "test".to_string(),
        })
        .await
        .unwrap();

    let short_timeout = Duration::from_secs(1);
    let (_item, token) = provider.fetch_work_item(short_timeout, None).await.unwrap().unwrap();
    tracing::info!("Fetched work item with 1s timeout");

    // Wait for lock to expire
    tokio::time::sleep(factory.lock_timeout() + Duration::from_millis(100)).await;

    // Try to renew expired lock
    let result = provider.renew_work_item_lock(&token, factory.lock_timeout()).await;
    assert!(result.is_err(), "Should fail to renew expired lock");
    tracing::info!("✓ Test passed: expired lock renewal rejection verified");
}

/// Test 4.8: Worker Lock Renewal Extends Timeout
/// Goal: Verify renewal properly extends lock timeout
pub async fn test_worker_lock_renewal_extends_timeout<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing worker lock renewal: renewal extends timeout");
    let provider = Arc::new(factory.create_provider().await);

    use crate::providers::WorkItem;

    // Enqueue and fetch work item with short timeout
    provider
        .enqueue_for_worker(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            name: "TestActivity".to_string(),
            input: "test".to_string(),
        })
        .await
        .unwrap();

    let short_timeout = Duration::from_secs(1);
    let (_item, token) = provider.fetch_work_item(short_timeout, None).await.unwrap().unwrap();
    tracing::info!("Fetched work item with 1s timeout");

    // Wait 800ms (before expiration)
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Renew lock for another 1 second
    provider.renew_work_item_lock(&token, short_timeout).await.unwrap();
    tracing::info!("Renewed lock at 800ms mark");

    // Wait another 800ms (total 1.6s from original fetch)
    // Without renewal, lock would have expired at 1s
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Item should still be locked (because we renewed at 800ms for another 1s = expires at 1.8s)
    let result = provider.fetch_work_item(short_timeout, None).await.unwrap();
    assert!(result.is_none(), "Item should still be locked after renewal");

    tracing::info!("✓ Test passed: lock timeout extension verified");
}

/// Test 4.9: Worker Lock Renewal After Ack Fails
/// Goal: Verify renewal fails after item has been acked
pub async fn test_worker_lock_renewal_after_ack<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing worker lock renewal: renewal fails after ack");
    let provider = factory.create_provider().await;

    use crate::providers::WorkItem;

    // Enqueue and fetch work item
    provider
        .enqueue_for_worker(WorkItem::ActivityExecute {
            instance: "test-instance".to_string(),
            execution_id: 1,
            id: 1,
            name: "TestActivity".to_string(),
            input: "test".to_string(),
        })
        .await
        .unwrap();

    let lock_timeout = factory.lock_timeout();
    let (_item, token) = provider.fetch_work_item(lock_timeout, None).await.unwrap().unwrap();
    tracing::info!("Fetched work item");

    // Ack the work item
    provider
        .ack_work_item(
            &token,
            WorkItem::ActivityCompleted {
                instance: "test-instance".to_string(),
                execution_id: 1,
                id: 1,
                result: "done".to_string(),
            },
        )
        .await
        .unwrap();
    tracing::info!("Acked work item");

    // Try to renew after ack
    let result = provider.renew_work_item_lock(&token, lock_timeout).await;
    assert!(result.is_err(), "Should fail to renew after ack");
    tracing::info!("✓ Test passed: renewal after ack rejection verified");
}
