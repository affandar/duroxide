use crate::provider_validation::{Event, ExecutionMetadata, start_item};
use crate::provider_validations::ProviderFactory;
use crate::providers::WorkItem;
use std::time::Duration;


/// Test 5.1: Worker Queue FIFO Ordering
/// Goal: Verify worker items dequeued in order.
pub async fn test_worker_queue_fifo_ordering<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing queue semantics: worker queue FIFO ordering");
    let provider = factory.create_provider().await;

    // Enqueue 5 worker items
    for i in 0..5 {
        provider
            .enqueue_worker_work(WorkItem::ActivityExecute {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: i,
                name: format!("Activity{}", i),
                input: format!("input{}", i),
            })
            .await
            .unwrap();
    }

    // Dequeue all 5 and verify order
    for i in 0..5 {
        let (item, _token) = provider.dequeue_worker_peek_lock().await.unwrap();
        match item {
            WorkItem::ActivityExecute { id, name, .. } => {
                assert_eq!(id, i);
                assert_eq!(name, format!("Activity{}", i));
            }
            _ => panic!("Expected ActivityExecute"),
        }
    }

    // Queue should be empty
    assert!(provider.dequeue_worker_peek_lock().await.is_none());
    tracing::info!("✓ Test passed: worker queue FIFO ordering verified");
}

/// Test 5.2: Worker Peek-Lock Semantics
/// Goal: Verify dequeue doesn't remove item until ack.
pub async fn test_worker_peek_lock_semantics<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing queue semantics: worker peek-lock semantics");
    let provider = factory.create_provider().await;

    // Enqueue worker item
    provider
        .enqueue_worker_work(WorkItem::ActivityExecute {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 1,
            name: "Activity1".to_string(),
            input: "input1".to_string(),
        })
        .await
        .unwrap();

    // Dequeue (gets item + token)
    let (item, token) = provider.dequeue_worker_peek_lock().await.unwrap();
    assert!(matches!(item, WorkItem::ActivityExecute { .. }));

    // Attempt second dequeue → should return None
    assert!(provider.dequeue_worker_peek_lock().await.is_none());

    // Ack with token
    provider
        .ack_worker(
            &token,
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 1,
                result: "result1".to_string(),
            },
        )
        .await
        .unwrap();

    // Queue should now be empty
    assert!(provider.dequeue_worker_peek_lock().await.is_none());
    tracing::info!("✓ Test passed: worker peek-lock semantics verified");
}

/// Test 5.3: Worker Ack Atomicity
/// Goal: Verify ack_worker atomically removes item and enqueues completion.
pub async fn test_worker_ack_atomicity<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing queue semantics: worker ack atomicity");
    let provider = factory.create_provider().await;

    // Create instance first (required for orchestrator queue)
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item.lock_token,
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

    // Enqueue worker item
    provider
        .enqueue_worker_work(WorkItem::ActivityExecute {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 1,
            name: "Activity1".to_string(),
            input: "input1".to_string(),
        })
        .await
        .unwrap();

    // Dequeue and get token
    let (_item, token) = provider.dequeue_worker_peek_lock().await.unwrap();

    // Ack with completion
    provider
        .ack_worker(
            &token,
            WorkItem::ActivityCompleted {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 1,
                result: "result1".to_string(),
            },
        )
        .await
        .unwrap();

    // Verify:
    // 1. Worker queue is empty
    assert!(provider.dequeue_worker_peek_lock().await.is_none());

    // 2. Orchestrator queue has completion item
    let orchestration_item = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(orchestration_item.instance, "instance-A");
    assert_eq!(orchestration_item.messages.len(), 1);
    assert!(matches!(
        &orchestration_item.messages[0],
        WorkItem::ActivityCompleted { .. }
    ));
    tracing::info!("✓ Test passed: worker ack atomicity verified");
}

/// Test 5.4: Timer Delayed Visibility
/// Goal: Verify TimerFired items only dequeued when visible_at <= now.
pub async fn test_timer_delayed_visibility<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing queue semantics: timer delayed visibility");
    let provider = factory.create_provider().await;

    // Create instance first
    provider
        .enqueue_orchestrator_work(start_item("instance-A"), None)
        .await
        .unwrap();
    let item = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item.lock_token,
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

    // Create timer with future visibility (delay_ms from now)
    let delay_ms = 5000; // 5 seconds delay

    provider
        .enqueue_orchestrator_work(
            WorkItem::TimerFired {
                instance: "instance-A".to_string(),
                execution_id: 1,
                id: 1,
                fire_at_ms: 0, // This will be set correctly during ack
            },
            Some(delay_ms),
        )
        .await
        .unwrap();

    // Fetch orchestration item immediately → should return None (timer not visible yet)
    assert!(provider.fetch_orchestration_item().await.is_none());

    // Wait for timer to become visible
    tokio::time::sleep(Duration::from_millis(5100)).await;

    // Fetch again → should return TimerFired when visible_at <= now
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item2.instance, "instance-A");
    assert_eq!(item2.messages.len(), 1);
    assert!(matches!(&item2.messages[0], WorkItem::TimerFired { .. }));
    tracing::info!("✓ Test passed: timer delayed visibility verified");
}

/// Test 5.6: Lost Lock Token Handling
/// Goal: Verify locked items eventually become available if token lost.
pub async fn test_lost_lock_token_handling<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing queue semantics: lost lock token handling");
    let provider = factory.create_provider().await;

    // Enqueue worker item
    provider
        .enqueue_worker_work(WorkItem::ActivityExecute {
            instance: "instance-A".to_string(),
            execution_id: 1,
            id: 1,
            name: "Activity1".to_string(),
            input: "input1".to_string(),
        })
        .await
        .unwrap();

    // Dequeue (gets token)
    let (_item, _token) = provider.dequeue_worker_peek_lock().await.unwrap();

    // Lose token (simulate crash)
    // Drop the token without acking

    // Attempt second dequeue → should return None (locked)
    assert!(provider.dequeue_worker_peek_lock().await.is_none());

    // Wait for lock expiration
    tokio::time::sleep(Duration::from_millis(factory.lock_timeout_ms() + 100)).await;

    // Dequeue again → should succeed → item redelivered
    let (item2, _token2) = provider.dequeue_worker_peek_lock().await.unwrap();
    assert!(matches!(item2, WorkItem::ActivityExecute { .. }));
    tracing::info!("✓ Test passed: lost lock token handling verified");
}
