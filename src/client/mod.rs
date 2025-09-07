use std::sync::Arc;

use crate::providers::{HistoryStore, WorkItem};
use crate::_typed_codec::{Json, Codec};
use serde::Serialize;

/// Thin client for control-plane operations.
///
/// This client is intentionally limited to enqueue-only APIs that communicate
/// with the runtime exclusively through the shared HistoryStore provider.
pub struct DuroxideClient {
    store: Arc<dyn HistoryStore>,
}

impl DuroxideClient {
    /// Create a client bound to a HistoryStore instance.
    pub fn new(store: Arc<dyn HistoryStore>) -> Self {
        Self { store }
    }

    /// Start an orchestration instance with string input.
    pub async fn start_orchestration(
        &self,
        instance: &str,
        orchestration: &str,
        input: impl Into<String>,
    ) -> Result<(), String> {
        let item = WorkItem::StartOrchestration {
            instance: instance.to_string(),
            orchestration: orchestration.to_string(),
            input: input.into(),
            version: None,
            parent_instance: None,
            parent_id: None,
        };
        self.store.enqueue_orchestrator_work(item, None).await
    }

    /// Start an orchestration instance pinned to a specific version.
    pub async fn start_orchestration_versioned(
        &self,
        instance: &str,
        orchestration: &str,
        version: impl Into<String>,
        input: impl Into<String>,
    ) -> Result<(), String> {
        let item = WorkItem::StartOrchestration {
            instance: instance.to_string(),
            orchestration: orchestration.to_string(),
            input: input.into(),
            version: Some(version.into()),
            parent_instance: None,
            parent_id: None,
        };
        self.store.enqueue_orchestrator_work(item, None).await
    }

    // Note: No delayed scheduling API. Clients should use normal start APIs.

    /// Start an orchestration with typed input (serialized to JSON).
    pub async fn start_orchestration_typed<In: Serialize>(
        &self,
        instance: &str,
        orchestration: &str,
        input: In,
    ) -> Result<(), String> {
        let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
        self.start_orchestration(instance, orchestration, payload).await
    }

    /// Start a versioned orchestration with typed input (serialized to JSON).
    pub async fn start_orchestration_versioned_typed<In: Serialize>(
        &self,
        instance: &str,
        orchestration: &str,
        version: impl Into<String>,
        input: In,
    ) -> Result<(), String> {
        let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
        self.start_orchestration_versioned(instance, orchestration, version, payload).await
    }

    /// Raise an external event into a running orchestration instance.
    pub async fn raise_event(
        &self,
        instance: &str,
        event_name: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<(), String> {
        let item = WorkItem::ExternalRaised {
            instance: instance.to_string(),
            name: event_name.into(),
            data: data.into(),
        };
        self.store.enqueue_orchestrator_work(item, None).await
    }

    /// Request cancellation of an orchestration instance.
    pub async fn cancel_instance(
        &self,
        instance: &str,
        reason: impl Into<String>,
    ) -> Result<(), String> {
        let item = WorkItem::CancelInstance {
            instance: instance.to_string(),
            reason: reason.into(),
        };
        self.store.enqueue_orchestrator_work(item, None).await
    }
}
