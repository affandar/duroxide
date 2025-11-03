//! Provider Validation Tests
//!
//! Comprehensive test suite for validating provider implementations.
//! These tests are designed to work with any provider through the `ProviderFactory` trait.

#[cfg(feature = "provider-test")]
pub mod atomicity;
#[cfg(feature = "provider-test")]
pub mod error_handling;
#[cfg(feature = "provider-test")]
pub mod instance_locking;
#[cfg(feature = "provider-test")]
pub mod lock_expiration;
#[cfg(feature = "provider-test")]
pub mod multi_execution;
#[cfg(feature = "provider-test")]
pub mod queue_semantics;
#[cfg(feature = "provider-test")]
pub mod management;

#[cfg(feature = "provider-test")]
use crate::provider_validations::ProviderFactory;
#[cfg(feature = "provider-test")]
use crate::INITIAL_EXECUTION_ID;
#[cfg(feature = "provider-test")]
use crate::providers::WorkItem;

/// Re-export common types for use in test modules
#[cfg(feature = "provider-test")]
pub use crate::Event;
#[cfg(feature = "provider-test")]
pub use crate::providers::ExecutionMetadata;

/// Helper function to create a start item for an instance
#[cfg(feature = "provider-test")]
pub(crate) fn start_item(instance: &str) -> WorkItem {
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

/// Run all validation tests
#[cfg(feature = "provider-test")]
pub async fn run_all_tests<F: ProviderFactory>(factory: &F) {
    atomicity::run_tests(factory).await;
    error_handling::run_tests(factory).await;
    instance_locking::run_tests(factory).await;
    lock_expiration::run_tests(factory).await;
    multi_execution::run_tests(factory).await;
    queue_semantics::run_tests(factory).await;
    management::run_tests(factory).await;
}
