use duroxide::providers::sqlite::{SqliteOptions, SqliteProvider};
use duroxide::providers::{ExecutionMetadata, Provider, WorkItem};
use std::sync::Arc;
use std::time::Duration;

#[path = "../common/mod.rs"]
mod common;

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

// Default SQLite lock timeout is 30 seconds (30000ms)
// Tests use 1 second for faster execution
const TEST_LOCK_TIMEOUT_MS: u64 = 1000;

/// Test 1.1: Exclusive Instance Lock Acquisition
/// Goal: Verify only one dispatcher can process an instance at a time.
#[tokio::test]
async fn test_exclusive_instance_lock() {
    let provider = create_provider().await;

    // Enqueue work for instance "A"
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();

    // Fetch orchestration item (acquires lock)
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    let lock_token1 = item1.lock_token.clone();

    // Second fetch should fail (instance locked)
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Wait for lock to expire
    tokio::time::sleep(Duration::from_millis(TEST_LOCK_TIMEOUT_MS + 100)).await;

    // Now should be able to fetch again
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_ne!(item2.lock_token, lock_token1);
}

/// Test 1.2: Lock Token Uniqueness
/// Goal: Ensure each fetch generates a unique lock token.
#[tokio::test]
async fn test_lock_token_uniqueness() {
    let provider = create_provider().await;

    // Enqueue work for multiple instances
    for i in 0..5 {
        provider
            .enqueue_orchestrator_work(start_item(&format!("inst-{}", i)), None)
            .await
            .unwrap();
    }

    // Fetch multiple orchestration items
    let mut tokens = Vec::new();
    for _ in 0..5 {
        let item = provider.fetch_orchestration_item().await.unwrap();
        tokens.push(item.lock_token);
    }

    // Verify all lock tokens are unique
    let unique_tokens: std::collections::HashSet<_> = tokens.iter().collect();
    assert_eq!(unique_tokens.len(), 5, "All lock tokens should be unique");
}

/// Test 1.3: Invalid Lock Token Rejection
/// Goal: Verify ack/abandon reject invalid lock tokens.
#[tokio::test]
async fn test_invalid_lock_token_rejection() {
    let provider = create_provider().await;

    // Enqueue and fetch an item
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let _item = provider.fetch_orchestration_item().await.unwrap();

    // Try to ack with invalid token
    let result = provider
        .ack_orchestration_item("invalid-token", 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await;
    assert!(result.is_err(), "Should reject invalid lock token");

    // Try to abandon with invalid token
    let result = provider.abandon_orchestration_item("invalid-token", None).await;
    assert!(result.is_err(), "Should reject invalid lock token for abandon");

    // Original item should still be locked
    assert!(provider.fetch_orchestration_item().await.is_none());
}

/// Test 1.4: Concurrent Fetch Attempts
/// Goal: Test provider under concurrent access from multiple dispatchers.
#[tokio::test]
async fn test_concurrent_instance_fetching() {
    let provider = Arc::new(create_provider().await);

    // Seed 10 instances
    for i in 0..10 {
        provider
            .enqueue_orchestrator_work(start_item(&format!("inst-{}", i)), None)
            .await
            .unwrap();
    }

    // Fetch concurrently with small delay to reduce contention
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let p = provider.clone();
            tokio::spawn(async move {
                // Add small random delay to reduce contention
                tokio::time::sleep(Duration::from_millis(i * 10)).await;
                p.fetch_orchestration_item().await
            })
        })
        .collect();

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // Verify no duplicates
    let instances: std::collections::HashSet<_> = results
        .iter()
        .filter_map(|r| r.as_ref())
        .map(|item| item.instance.clone())
        .collect();

    assert_eq!(instances.len(), 10, "Each instance should be fetched exactly once");
}

/// Test 1.5: Message Arrival During Lock (Critical)
/// Goal: Verify completions arriving during a lock cannot be fetched by other dispatchers.
#[tokio::test]
async fn test_completions_arriving_during_lock_blocked() {
    let provider = Arc::new(create_provider().await);

    // Step 1: Create instance with initial work
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();

    // Step 2: Fetch and acquire lock
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item1.instance, "instance-A");
    let _lock_token = item1.lock_token.clone();

    // Step 3: While locked, completions arrive
    for i in 1..=3 {
        provider
            .enqueue_orchestrator_work(
                WorkItem::ActivityCompleted {
                    instance: "instance-A".to_string(),
                    execution_id: 1,
                    id: i,
                    result: format!("result-{}", i),
                },
                None,
            )
            .await
            .unwrap();
    }

    // Step 4: Another dispatcher tries to fetch "instance-A"
    let item2 = provider.fetch_orchestration_item().await;
    assert!(item2.is_none(), "Instance still locked, no fetch possible");

    // Step 5: Wait for lock expiration
    tokio::time::sleep(Duration::from_millis(TEST_LOCK_TIMEOUT_MS + 100)).await;

    // Step 6: Now completions should be fetchable
    let item3 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item3.instance, "instance-A");
    // Should have StartOrchestration + 3 ActivityCompleted messages = 4 total
    assert_eq!(
        item3.messages.len(),
        4,
        "Should have StartOrchestration + 3 completions"
    );

    // Verify they're the messages including completions that arrived during lock
    let activity_completions: Vec<_> = item3
        .messages
        .iter()
        .filter_map(|msg| match msg {
            WorkItem::ActivityCompleted { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(
        activity_completions.len(),
        3,
        "Should have 3 ActivityCompleted messages"
    );
    assert_eq!(activity_completions, vec![1, 2, 3]);
}

/// Test 1.6: Cross-Instance Lock Isolation
/// Goal: Verify locks on one instance don't block other instances.
#[tokio::test]
async fn test_cross_instance_lock_isolation() {
    let provider = Arc::new(create_provider().await);

    // Enqueue work for two different instances
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    provider
        .enqueue_orchestrator_work(start_item("instance-B"), None)
        .await
        .unwrap();

    // Lock instance A
    let item_a = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item_a.instance, "instance-A");

    // Should still be able to fetch instance B (different instance, not blocked)
    let item_b = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item_b.instance, "instance-B");

    // Ack B to release its lock, then enqueue another completion for B
    provider
        .ack_orchestration_item(
            &item_b.lock_token,
            1,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // While A is still locked, completion arrives for B
    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-B".to_string(),
                execution_id: 1,
                id: 1,
                result: "done".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Should be able to fetch B again (B is not locked)
    let item_b2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item_b2.instance, "instance-B");

    // Key assertion: instance-level locks don't block other instances
    assert_ne!(item_a.instance, item_b.instance);
}

/// Test 1.7: Completing Messages During Lock (Message Tagging)
/// Goal: Verify only messages present at fetch time are deleted on ack.
#[tokio::test]
async fn test_message_tagging_during_lock() {
    let provider = Arc::new(create_provider().await);

    // Create instance first by enqueuing StartOrchestration
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();

    // Fetch and ack to create the instance properly
    let initial_item = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &initial_item.lock_token,
            1,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Enqueue initial messages
    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 1,
                result: "msg1".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 2,
                result: "msg2".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch (marks messages with lock_token)
    let item = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item.instance, "instance-A");
    assert_eq!(item.messages.len(), 2);
    let lock_token = item.lock_token.clone();

    // While locked, new message arrives
    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 3,
                result: "msg3".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Ack (deletes only messages with lock_token)
    provider
        .ack_orchestration_item(&lock_token, 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await
        .unwrap();

    // Fetch again - should get msg3
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
    assert_eq!(item2.messages.len(), 1);
    assert!(matches!(&item2.messages[0], WorkItem::ActivityCompleted { id: 3, .. }));
}

/// Test 1.8: Ack Only Affects Locked Messages
/// Goal: Verify ack_orchestration_item only acks messages that were locked by the lock_token.
#[tokio::test]
async fn test_ack_only_affects_locked_messages() {
    let provider = Arc::new(create_provider().await);

    // Create instance
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let initial_item = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &initial_item.lock_token,
            1,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Enqueue message 1
    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 1,
                result: "msg1".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch message 1 and get lock_token
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item1.messages.len(), 1);
    let lock_token = item1.lock_token.clone();

    // While item1 is locked, enqueue messages 2 and 3
    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 2,
                result: "msg2".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    provider
        .enqueue_orchestrator_work(
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 3,
                result: "msg3".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Another fetch attempt should return None (instance is locked)
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Ack with lock_token - should only delete message 1 (locked messages)
    provider
        .ack_orchestration_item(&lock_token, 1, vec![], vec![], vec![], ExecutionMetadata::default())
        .await
        .unwrap();

    // Now messages 2 and 3 should be fetchable
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
    assert_eq!(item2.messages.len(), 2, "Should have messages 2 and 3");

    // Verify they're messages 2 and 3
    let ids: Vec<u64> = item2
        .messages
        .iter()
        .filter_map(|msg| match msg {
            WorkItem::ActivityCompleted { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![2, 3], "Should only have messages 2 and 3");
}
