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

    /// Get the lock timeout in milliseconds configured for this provider.
    ///
    /// This is used by validation tests to determine sleep durations
    /// when waiting for lock expiration. The timeout should match
    /// the lock timeout configured in `create_provider()`.
    ///
    /// # Default Implementation
    ///
    /// Returns 1000ms (1 second) if not overridden.
    fn lock_timeout_ms(&self) -> u64 {
        1000
    }
}

/// ## Individual Test Suites
///
/// For more granular control, you can run individual test suites:
///
/// ```rust,ignore
/// use duroxide::provider_validations::{ProviderFactory, run_atomicity_tests};
///
/// #[tokio::test]
/// async fn test_atomicity_only() {
///     let factory = MyFactory;
///     run_atomicity_tests(&factory).await;
/// }
/// ```
///
/// Available test suite functions:
/// - `run_atomicity_tests` - Transactional guarantees
/// - `run_error_handling_tests` - Error handling and validation
/// - `run_instance_locking_tests` - Exclusive access guarantees
/// - `run_lock_expiration_tests` - Lock timeout behavior
/// - `run_multi_execution_tests` - ContinueAsNew and execution isolation
/// - `run_queue_semantics_tests` - Work queue behavior
/// - `run_management_tests` - Management API tests

/// Run atomicity tests to verify transactional guarantees.
///
/// Tests that ack_orchestration_item is truly atomic - all operations
/// succeed together or all fail together.
#[cfg(feature = "provider-test")]
pub async fn run_atomicity_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::atomicity::run_tests(factory).await;
}

/// Run error handling tests to verify graceful failure modes.
///
/// Tests invalid inputs, duplicate events, and other error scenarios.
#[cfg(feature = "provider-test")]
pub async fn run_error_handling_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::error_handling::run_tests(factory).await;
}

/// Run instance locking tests to verify exclusive access guarantees.
///
/// Tests that only one dispatcher can process an instance at a time.
#[cfg(feature = "provider-test")]
pub async fn run_instance_locking_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::instance_locking::run_tests(factory).await;
}

/// Run lock expiration tests to verify peek-lock timeout behavior.
///
/// Tests that locks expire and messages become available for retry.
#[cfg(feature = "provider-test")]
pub async fn run_lock_expiration_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::lock_expiration::run_tests(factory).await;
}

/// Run multi-execution tests to verify ContinueAsNew support.
///
/// Tests execution isolation and history partitioning.
#[cfg(feature = "provider-test")]
pub async fn run_multi_execution_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::multi_execution::run_tests(factory).await;
}

/// Run queue semantics tests to verify work queue behavior.
///
/// Tests FIFO ordering, peek-lock semantics, and atomic acks.
#[cfg(feature = "provider-test")]
pub async fn run_queue_semantics_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::queue_semantics::run_tests(factory).await;
}

/// Run management capability tests to verify provider management APIs.
///
/// Tests instance listing, execution queries, and system metrics.
#[cfg(feature = "provider-test")]
pub async fn run_management_tests<F: ProviderFactory>(factory: &F) {
    crate::provider_validation::management::run_tests(factory).await;
}
