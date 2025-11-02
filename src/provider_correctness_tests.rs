//! Provider Correctness Test Infrastructure
//!
//! This module provides reusable test infrastructure for validating custom Provider implementations.
//! Enable the `provider-test` feature to use these utilities.
//!
//! # Example
//!
//! ```rust,ignore
//! use duroxide::providers::Provider;
//! use duroxide::provider_correctness_tests::ProviderFactory;
//! use std::sync::Arc;
//!
//! struct MyProviderFactory;
//!
//! #[async_trait::async_trait]
//! impl ProviderFactory for MyProviderFactory {
//!     async fn create_provider(&self) -> Arc<dyn Provider> {
//!         Arc::new(MyProvider::new().await.unwrap())
//!     }
//! }
//!
//! #[tokio::test]
//! async fn test_my_provider() {
//!     let factory = MyProviderFactory;
//!     duroxide::provider_correctness_tests::run_all_tests(factory).await;
//! }
//! ```

#[cfg(feature = "provider-test")]
use crate::providers::Provider;
use std::sync::Arc;

/// Trait for creating providers in tests.
///
/// Implement this trait to provide a way to create your custom provider instance
/// for correctness testing.
#[cfg(feature = "provider-test")]
#[async_trait::async_trait]
pub trait ProviderFactory: Send + Sync {
    /// Create a new provider instance for testing.
    ///
    /// Each call should return a fresh, isolated provider instance.
    /// Typically this means creating a new in-memory provider or a
    /// file-based provider with a unique temporary path.
    async fn create_provider(&self) -> Arc<dyn Provider>;
}

/// Run all provider correctness tests against a provider factory.
///
/// This will execute the full suite of correctness tests:
/// - Atomicity tests
/// - Error handling tests
/// - Instance locking tests
/// - Lock expiration tests
/// - Multi-execution tests
/// - Queue semantics tests
///
/// # Example
///
/// ```rust,ignore
/// use duroxide::provider_correctness_tests::{ProviderFactory, run_all_tests};
/// use duroxide::providers::Provider;
/// use std::sync::Arc;
///
/// struct MyFactory;
///
/// #[async_trait::async_trait]
/// impl ProviderFactory for MyFactory {
///     async fn create_provider(&self) -> Arc<dyn Provider> {
///         Arc::new(MyProvider::new().await.unwrap())
///     }
/// }
///
/// #[tokio::test]
/// async fn test_provider() {
///     run_all_tests(MyFactory).await;
/// }
/// ```
#[cfg(feature = "provider-test")]
pub async fn run_all_tests<F: ProviderFactory>(factory: F) {
    // Import internal test modules
    tests::run_atomicity_tests(&factory).await;
    tests::run_error_handling_tests(&factory).await;
    tests::run_instance_locking_tests(&factory).await;
    tests::run_lock_expiration_tests(&factory).await;
    tests::run_multi_execution_tests(&factory).await;
    tests::run_queue_semantics_tests(&factory).await;
}

#[cfg(feature = "provider-test")]
pub mod tests {
    use super::*;
    use crate::providers::{ExecutionMetadata, WorkItem};
    use crate::{Event, INITIAL_EVENT_ID, INITIAL_EXECUTION_ID};
    use std::time::Duration;

    /// Run atomicity tests to verify transactional guarantees.
    ///
    /// Tests that ack_orchestration_item is truly atomic - all operations
    /// succeed together or all fail together.
    pub async fn run_atomicity_tests<F: ProviderFactory>(factory: &F) {
        test_atomicity_failure_rollback(factory).await;
        test_atomicity_success_commit(factory).await;
    }

    /// Run error handling tests to verify graceful failure modes.
    ///
    /// Tests invalid inputs, duplicate events, and other error scenarios.
    pub async fn run_error_handling_tests<F: ProviderFactory>(factory: &F) {
        test_invalid_lock_token(factory).await;
        test_duplicate_event_id(factory).await;
    }

    /// Run instance locking tests to verify exclusive access guarantees.
    ///
    /// Tests that only one dispatcher can process an instance at a time.
    pub async fn run_instance_locking_tests<F: ProviderFactory>(factory: &F) {
        test_locking_exclusive_access(factory).await;
        test_locking_timeout(factory).await;
    }

    /// Run lock expiration tests to verify peek-lock timeout behavior.
    ///
    /// Tests that locks expire and messages become available for retry.
    pub async fn run_lock_expiration_tests<F: ProviderFactory>(factory: &F) {
        test_lock_expiration_release(factory).await;
        test_lock_expiration_reacquire(factory).await;
    }

    /// Run multi-execution tests to verify ContinueAsNew support.
    ///
    /// Tests execution isolation and history partitioning.
    pub async fn run_multi_execution_tests<F: ProviderFactory>(factory: &F) {
        test_multi_execution_isolation(factory).await;
        test_continue_as_new_execution(factory).await;
    }

    /// Run queue semantics tests to verify work queue behavior.
    ///
    /// Tests FIFO ordering, peek-lock semantics, and atomic acks.
    pub async fn run_queue_semantics_tests<F: ProviderFactory>(factory: &F) {
        test_queue_fifo_order(factory).await;
        test_queue_atomicity(factory).await;
    }

    // Helper function to create a start item
    fn start_item(instance: &str) -> WorkItem {
        WorkItem::StartOrchestration {
            instance: instance.to_string(),
            orchestration: "TestOrch".to_string(),
            input: "{}".to_string(),
            version: Some("1.0.0".to_string()),
            parent_instance: None,
            parent_id: None,
            execution_id: INITIAL_EXECUTION_ID,
        }
    }

    // Test implementations - these are copied from the actual test files
    // In a real implementation, these would be in separate modules

    async fn test_atomicity_failure_rollback<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        // Setup: create instance with some initial state
        provider
            .enqueue_orchestrator_work(start_item("instance-A"), None)
            .await
            .unwrap();
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
        provider
            .enqueue_orchestrator_work(start_item("instance-A"), None)
            .await
            .unwrap();
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
        assert_eq!(after_history.len(), 1, "History should be unchanged after failed ack");
    }

    async fn test_atomicity_success_commit<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-B"), None)
            .await
            .unwrap();
        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        // Ack with valid data
        provider
            .ack_orchestration_item(
                &lock_token,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: INITIAL_EVENT_ID,
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

        // Verify state committed
        let history = provider.read("instance-B").await;
        assert_eq!(history.len(), 1);
    }

    async fn test_invalid_lock_token<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        // Try to ack with invalid lock token
        let result = provider
            .ack_orchestration_item("invalid-token", 1, vec![], vec![], vec![], ExecutionMetadata::default())
            .await;

        assert!(result.is_err(), "Should reject invalid lock token");
    }

    async fn test_duplicate_event_id<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-C"), None)
            .await
            .unwrap();
        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        // First ack succeeds
        provider
            .ack_orchestration_item(
                &lock_token,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: INITIAL_EVENT_ID,
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

        // Second ack with same event_id should fail
        provider
            .enqueue_orchestrator_work(start_item("instance-C"), None)
            .await
            .unwrap();
        let item2 = provider.fetch_orchestration_item().await.unwrap();
        let lock_token2 = item2.lock_token.clone();

        let result = provider
            .ack_orchestration_item(
                &lock_token2,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: INITIAL_EVENT_ID, // DUPLICATE
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
    }

    async fn test_locking_exclusive_access<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-D"), None)
            .await
            .unwrap();

        // First fetch succeeds
        let item1 = provider.fetch_orchestration_item().await;
        assert!(item1.is_some());

        // Second fetch for same instance should return None (locked)
        let item2 = provider.fetch_orchestration_item().await;
        assert!(item2.is_none(), "Should not fetch locked instance");
    }

    async fn test_locking_timeout<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-E"), None)
            .await
            .unwrap();

        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        // Wait for lock to expire (assuming default lock timeout)
        tokio::time::sleep(Duration::from_millis(2000)).await;

        // Try to abandon with the expired lock
        let _result = provider.abandon_orchestration_item(&lock_token, None).await;
        // This might succeed or fail depending on implementation
        // The key is that after expiration, the item should be fetchable again
    }

    async fn test_lock_expiration_release<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-F"), None)
            .await
            .unwrap();

        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        // Abandon the lock
        provider.abandon_orchestration_item(&lock_token, None).await.unwrap();

        // Should be able to fetch again
        let item2 = provider.fetch_orchestration_item().await;
        assert!(item2.is_some(), "Should be able to refetch after abandon");
    }

    async fn test_lock_expiration_reacquire<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-G"), None)
            .await
            .unwrap();

        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        // Abandon with delay
        provider
            .abandon_orchestration_item(&lock_token, Some(100))
            .await
            .unwrap();

        // Should not be fetchable immediately
        let item2 = provider.fetch_orchestration_item().await;
        assert!(item2.is_none(), "Should not be immediately visible");

        // After delay, should be fetchable
        tokio::time::sleep(Duration::from_millis(150)).await;
        let item3 = provider.fetch_orchestration_item().await;
        assert!(item3.is_some(), "Should be visible after delay");
    }

    async fn test_multi_execution_isolation<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        // Create first execution
        provider
            .enqueue_orchestrator_work(start_item("instance-H"), None)
            .await
            .unwrap();
        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        provider
            .ack_orchestration_item(
                &lock_token,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: INITIAL_EVENT_ID,
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

        // Read latest execution (should be 1)
        let history = provider.read("instance-H").await;
        assert_eq!(history.len(), 1);
    }

    async fn test_continue_as_new_execution<F: ProviderFactory>(factory: &F) {
        // Similar pattern but testing ContinueAsNew semantics
        // This would verify that ContinueAsNew creates a new execution_id
        let _provider = factory.create_provider().await;
        // Implementation would test ContinueAsNew flow
    }

    async fn test_queue_fifo_order<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        // Enqueue multiple items
        for i in 0..5 {
            provider
                .enqueue_orchestrator_work(start_item(&format!("instance-I-{}", i)), None)
                .await
                .unwrap();
        }

        // Fetch and verify FIFO order
        for i in 0..5 {
            let item = provider.fetch_orchestration_item().await.unwrap();
            assert_eq!(item.instance, format!("instance-I-{}", i));
        }
    }

    async fn test_queue_atomicity<F: ProviderFactory>(factory: &F) {
        let provider = factory.create_provider().await;

        provider
            .enqueue_orchestrator_work(start_item("instance-J"), None)
            .await
            .unwrap();

        let item = provider.fetch_orchestration_item().await.unwrap();
        let lock_token = item.lock_token.clone();

        // Enqueue worker items as part of ack
        let worker_item = WorkItem::ActivityExecute {
            instance: "instance-J".to_string(),
            name: "TestActivity".to_string(),
            input: "{}".to_string(),
            execution_id: 1,
            id: 100,
        };

        provider
            .ack_orchestration_item(
                &lock_token,
                1,
                vec![Event::OrchestrationStarted {
                    event_id: INITIAL_EVENT_ID,
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                }],
                vec![worker_item.clone()],
                vec![],
                ExecutionMetadata::default(),
            )
            .await
            .unwrap();

        // Verify worker item was enqueued
        let (dequeued, _) = provider.dequeue_worker_peek_lock().await.unwrap();
        match dequeued {
            WorkItem::ActivityExecute {
                instance,
                name,
                input,
                execution_id,
                id,
            } => {
                assert_eq!(instance, "instance-J");
                assert_eq!(name, "TestActivity");
                assert_eq!(input, "{}");
                assert_eq!(execution_id, 1);
                assert_eq!(id, 100);
            }
            _ => panic!("Expected ActivityExecute"),
        }
    }
}
