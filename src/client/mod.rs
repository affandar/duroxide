use std::sync::Arc;

use crate::providers::{HistoryStore, WorkItem};
use crate::{Event, OrchestrationStatus};
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

    /// Get the current status of an orchestration by inspecting its history.
    pub async fn get_orchestration_status(&self, instance: &str) -> OrchestrationStatus {
        let hist = self.store.read(instance).await;
        // Find terminal events first
        for e in hist.iter().rev() {
            match e {
                Event::OrchestrationCompleted { output } => {
                    return OrchestrationStatus::Completed { output: output.clone() }
                }
                Event::OrchestrationFailed { error } => {
                    return OrchestrationStatus::Failed { error: error.clone() }
                }
                _ => {}
            }
        }
        // If we ever saw a start, it's running
        if hist.iter().any(|e| matches!(e, Event::OrchestrationStarted { .. })) {
            OrchestrationStatus::Running
        } else {
            OrchestrationStatus::NotFound
        }
    }

    /// Wait until terminal state or timeout using provider reads.
    pub async fn wait_for_orchestration(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<OrchestrationStatus, crate::runtime::WaitError> {
        let deadline = std::time::Instant::now() + timeout;
        // quick path
        match self.get_orchestration_status(instance).await {
            OrchestrationStatus::Completed { output } =>
                return Ok(OrchestrationStatus::Completed { output }),
            OrchestrationStatus::Failed { error } =>
                return Ok(OrchestrationStatus::Failed { error }),
            _ => {}
        }
        // poll with backoff
        let mut delay_ms: u64 = 5;
        while std::time::Instant::now() < deadline {
            match self.get_orchestration_status(instance).await {
                OrchestrationStatus::Completed { output } =>
                    return Ok(OrchestrationStatus::Completed { output }),
                OrchestrationStatus::Failed { error } =>
                    return Ok(OrchestrationStatus::Failed { error }),
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms.saturating_mul(2)).min(100);
                }
            }
        }
        Err(crate::runtime::WaitError::Timeout)
    }

    /// Typed wait helper: decodes output on Completed, returns Err(String) on Failed.
    pub async fn wait_for_orchestration_typed<Out: serde::de::DeserializeOwned>(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<Result<Out, String>, crate::runtime::WaitError> {
        match self.wait_for_orchestration(instance, timeout).await? {
            OrchestrationStatus::Completed { output } => match Json::decode::<Out>(&output) {
                Ok(v) => Ok(Ok(v)),
                Err(e) => Err(crate::runtime::WaitError::Other(format!("decode failed: {e}"))),
            },
            OrchestrationStatus::Failed { error } => Ok(Err(error)),
            _ => unreachable!("wait_for_orchestration returns only terminal or timeout"),
        }
    }

    /// List all execution ids for an instance.
    pub async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let hist = self.store.read(instance).await;
        if hist.is_empty() { Vec::new() } else { vec![1] }
    }

    /// Return execution history for a specific execution id.
    pub async fn get_execution_history(&self, instance: &str, _execution_id: u64) -> Vec<crate::Event> {
        self.store.read(instance).await
    }
}
