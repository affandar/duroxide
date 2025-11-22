//! Provider Validation Infrastructure
//!
//! This module provides reusable test infrastructure for validating custom Provider implementations.
//! Enable the `provider-test` feature to use these utilities.
//!
//! # Example
//!
//! ```rust,ignore
//! use duroxide::providers::Provider;
//! use duroxide::provider_validations::ProviderFactory;
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
//!     duroxide::provider_validations::run_atomicity_tests(&factory).await;
//! }
//! ```

#[cfg(feature = "provider-test")]
use crate::providers::Provider;
use std::sync::Arc;
use std::time::Duration;

/// Trait for creating providers in tests.
///
/// Implement this trait to provide a way to create your custom provider instance
/// for validation testing.
#[cfg(feature = "provider-test")]
#[async_trait::async_trait]
pub trait ProviderFactory: Send + Sync {
    /// Create a new provider instance for testing.
    ///
    /// Each call should return a fresh, isolated provider instance.
    /// Typically this means creating a new in-memory provider or a
    /// file-based provider with a unique temporary path.
    async fn create_provider(&self) -> Arc<dyn Provider>;

    /// Get the lock timeout configured for this provider.
    ///
    /// This is used by validation tests to determine sleep durations
    /// when waiting for lock expiration. The timeout should match
    /// the lock timeout configured in `create_provider()`.
    ///
    /// # Default Implementation
    ///
    /// Returns 1 second if not overridden.
    fn lock_timeout(&self) -> Duration {
        Duration::from_millis(1000)
    }
}

/// ## Individual Test Functions
///
/// For granular control and easier debugging, you can run individual test functions:
///
/// ```rust,ignore
/// use duroxide::provider_validations::{ProviderFactory, test_atomicity_failure_rollback};
///
/// #[tokio::test]
/// async fn test_atomicity_failure_rollback_only() {
///     let factory = MyFactory;
///     test_atomicity_failure_rollback(&factory).await;
/// }
/// ```
///
/// Available test functions:
///
/// **Instance Creation Tests:**
/// - `test_instance_creation_via_metadata` - Verify instances created via ack metadata, not on enqueue
/// - `test_no_instance_creation_on_enqueue` - Verify no instance created when enqueueing
/// - `test_null_version_handling` - Verify NULL version handled correctly
/// - `test_sub_orchestration_instance_creation` - Verify sub-orchestrations follow same pattern
///
/// **Atomicity Tests:**
/// - `test_atomicity_failure_rollback` - Verify ack failure rolls back all operations
/// - `test_multi_operation_atomic_ack` - Verify complex ack succeeds atomically
/// - `test_lock_released_only_on_successful_ack` - Verify lock only released on success
/// - `test_concurrent_ack_prevention` - Verify only one ack succeeds with same token
///
/// **Error Handling Tests:**
/// - `test_invalid_lock_token_on_ack` - Verify invalid lock tokens are rejected
/// - `test_duplicate_event_id_rejection` - Verify duplicate events are rejected
/// - `test_missing_instance_metadata` - Verify missing instances handled gracefully
/// - `test_corrupted_serialization_data` - Verify corrupted data handled gracefully
/// - `test_lock_expiration_during_ack` - Verify expired locks are rejected
///
/// **Instance Locking Tests:**
/// - `test_exclusive_instance_lock` - Verify only one dispatcher can process an instance
/// - `test_lock_token_uniqueness` - Verify each fetch generates unique lock token
/// - `test_invalid_lock_token_rejection` - Verify invalid tokens rejected for ack/abandon
/// - `test_concurrent_instance_fetching` - Verify concurrent fetches don't duplicate instances
/// - `test_completions_arriving_during_lock_blocked` - Verify new messages blocked during lock
/// - `test_cross_instance_lock_isolation` - Verify locks don't block other instances
/// - `test_message_tagging_during_lock` - Verify only fetched messages deleted on ack
/// - `test_ack_only_affects_locked_messages` - Verify ack only affects locked messages
/// - `test_multi_threaded_lock_contention` - Verify locks prevent concurrent processing (multi-threaded)
/// - `test_multi_threaded_no_duplicate_processing` - Verify no duplicate processing (multi-threaded)
/// - `test_multi_threaded_lock_expiration_recovery` - Verify lock expiration recovery (multi-threaded)
///
/// **Lock Expiration Tests:**
/// - `test_lock_expires_after_timeout` - Verify locks expire after timeout
/// - `test_abandon_releases_lock_immediately` - Verify abandon releases lock immediately
/// - `test_lock_renewal_on_ack` - Verify successful ack releases lock immediately
/// - `test_concurrent_lock_attempts_respect_expiration` - Verify concurrent attempts respect expiration
/// - `test_worker_lock_renewal_success` - Verify worker lock can be renewed with valid token
/// - `test_worker_lock_renewal_invalid_token` - Verify renewal fails with invalid token
/// - `test_worker_lock_renewal_after_expiration` - Verify renewal fails after lock expires
/// - `test_worker_lock_renewal_extends_timeout` - Verify renewal properly extends lock timeout
/// - `test_worker_lock_renewal_after_ack` - Verify renewal fails after item has been acked
///
/// **Multi-Execution Tests:**
/// - `test_execution_isolation` - Verify each execution has separate history
/// - `test_latest_execution_detection` - Verify read() returns latest execution
/// - `test_execution_id_sequencing` - Verify execution IDs increment correctly
/// - `test_continue_as_new_creates_new_execution` - Verify ContinueAsNew creates new execution
/// - `test_execution_history_persistence` - Verify all executions' history persists independently
///
/// **Queue Semantics Tests:**
/// - `test_worker_queue_fifo_ordering` - Verify worker items dequeued in FIFO order
/// - `test_worker_peek_lock_semantics` - Verify dequeue doesn't remove item until ack
/// - `test_worker_ack_atomicity` - Verify ack_worker atomically removes item and enqueues completion
/// - `test_timer_delayed_visibility` - Verify TimerFired items only dequeued when visible
/// - `test_lost_lock_token_handling` - Verify locked items become available after expiration
///
/// **Management Tests:**
/// - `test_list_instances` - Verify list_instances returns all instance IDs
/// - `test_list_instances_by_status` - Verify list_instances_by_status filters correctly
/// - `test_list_executions` - Verify list_executions returns all execution IDs
/// - `test_get_instance_info` - Verify get_instance_info returns metadata
/// - `test_get_execution_info` - Verify get_execution_info returns execution metadata
/// - `test_get_system_metrics` - Verify get_system_metrics returns accurate counts
/// - `test_get_queue_depths` - Verify get_queue_depths returns current queue sizes
#[cfg(feature = "provider-test")]
pub use crate::provider_validation::instance_creation::{
    test_instance_creation_via_metadata, test_no_instance_creation_on_enqueue, test_null_version_handling,
    test_sub_orchestration_instance_creation,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::atomicity::{
    test_atomicity_failure_rollback, test_concurrent_ack_prevention, test_lock_released_only_on_successful_ack,
    test_multi_operation_atomic_ack,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::error_handling::{
    test_corrupted_serialization_data, test_duplicate_event_id_rejection, test_invalid_lock_token_on_ack,
    test_lock_expiration_during_ack, test_missing_instance_metadata,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::instance_locking::{
    test_ack_only_affects_locked_messages, test_completions_arriving_during_lock_blocked,
    test_concurrent_instance_fetching, test_cross_instance_lock_isolation, test_exclusive_instance_lock,
    test_invalid_lock_token_rejection, test_lock_token_uniqueness, test_message_tagging_during_lock,
    test_multi_threaded_lock_contention, test_multi_threaded_lock_expiration_recovery,
    test_multi_threaded_no_duplicate_processing,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::lock_expiration::{
    test_abandon_releases_lock_immediately, test_concurrent_lock_attempts_respect_expiration,
    test_lock_expires_after_timeout, test_lock_renewal_on_ack, test_worker_lock_renewal_after_ack,
    test_worker_lock_renewal_after_expiration, test_worker_lock_renewal_extends_timeout,
    test_worker_lock_renewal_invalid_token, test_worker_lock_renewal_success,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::multi_execution::{
    test_continue_as_new_creates_new_execution, test_execution_history_persistence, test_execution_id_sequencing,
    test_execution_isolation, test_latest_execution_detection,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::queue_semantics::{
    test_lost_lock_token_handling, test_timer_delayed_visibility, test_worker_ack_atomicity,
    test_worker_peek_lock_semantics, test_worker_queue_fifo_ordering,
};

#[cfg(feature = "provider-test")]
pub use crate::provider_validation::management::{
    test_get_execution_info, test_get_instance_info, test_get_queue_depths, test_get_system_metrics,
    test_list_executions, test_list_instances, test_list_instances_by_status,
};
