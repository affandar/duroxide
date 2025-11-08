//! Provider Validation Tests
//!
//! Comprehensive test suite for validating provider implementations.
//! These tests are designed to work with any provider through the `ProviderFactory` trait.

#[cfg(feature = "provider-test")]
pub mod atomicity;
#[cfg(feature = "provider-test")]
pub mod error_handling;
#[cfg(feature = "provider-test")]
pub mod instance_creation;
#[cfg(feature = "provider-test")]
pub mod instance_locking;
#[cfg(feature = "provider-test")]
pub mod lock_expiration;
#[cfg(feature = "provider-test")]
pub mod management;
#[cfg(feature = "provider-test")]
pub mod multi_execution;
#[cfg(feature = "provider-test")]
pub mod queue_semantics;

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

/// Helper function to create an instance by enqueueing, fetching, and acking with metadata
#[cfg(feature = "provider-test")]
pub(crate) async fn create_instance(provider: &dyn crate::providers::Provider, instance: &str) -> Result<(), String> {
    provider
        .enqueue_orchestrator_work(start_item(instance), None)
        .await
        .map_err(|e| e.to_string())?;

    let item = provider
        .fetch_orchestration_item()
        .await
        .ok_or_else(|| "Failed to fetch orchestration item".to_string())?;

    provider
        .ack_orchestration_item(
            &item.lock_token,
            INITIAL_EXECUTION_ID,
            vec![Event::OrchestrationStarted {
                event_id: crate::INITIAL_EVENT_ID,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}
