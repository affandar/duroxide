use std::sync::Arc;

use crate::providers::{Provider, WorkItem};
use crate::{Event, OrchestrationStatus};
use crate::_typed_codec::{Json, Codec};
use serde::Serialize;

/// Thin client for control-plane operations.
///
/// The Client provides APIs for managing orchestration instances:
/// - Starting orchestrations
/// - Raising external events
/// - Cancelling instances
/// - Checking status
/// - Waiting for completion
///
/// # Design
///
/// The Client communicates with the Runtime **only through the shared Provider** (no direct coupling).
/// This allows the Client to be used from any process, even one without a running Runtime.
///
/// # Thread Safety
///
/// Client is `Clone` and can be safely shared across threads.
///
/// # Example Usage
///
/// ```ignore
/// use duroxide::{Client, OrchestrationStatus};
/// use duroxide::providers::sqlite::SqliteProvider;
/// use std::sync::Arc;
///
/// let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);
/// let client = Client::new(store);
///
/// // Start an orchestration
/// client.start_orchestration("order-123", "ProcessOrder", r#"{"customer_id": "c1"}"#).await?;
///
/// // Check status
/// let status = client.get_orchestration_status("order-123").await;
/// println!("Status: {:?}", status);
///
/// // Wait for completion
/// let result = client.wait_for_orchestration("order-123", std::time::Duration::from_secs(30)).await.unwrap();
/// match result {
///     OrchestrationStatus::Completed { output } => println!("Done: {}", output),
///     OrchestrationStatus::Failed { error } => eprintln!("Failed: {}", error),
///     _ => {}
/// }
/// ```
pub struct Client {
    store: Arc<dyn Provider>,
}

impl Client {
    /// Create a client bound to a Provider instance.
    ///
    /// # Parameters
    ///
    /// * `store` - Arc-wrapped Provider (same instance used by Runtime)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let store = Arc::new(SqliteProvider::new("sqlite::memory:").await.unwrap());
    /// let client = Client::new(store.clone());
    /// // Multiple clients can share the same store
    /// let client2 = client.clone();
    /// ```
    pub fn new(store: Arc<dyn Provider>) -> Self {
        Self { store }
    }

    /// Start an orchestration instance with string input.
    ///
    /// # Parameters
    ///
    /// * `instance` - Unique instance ID (e.g., "order-123", "user-payment-456")
    /// * `orchestration` - Name of registered orchestration (e.g., "ProcessOrder")
    /// * `input` - JSON string input (will be passed to orchestration)
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Instance was enqueued for processing
    /// * `Err(msg)` - Failed to enqueue (storage error)
    ///
    /// # Behavior
    ///
    /// - Enqueues a StartOrchestration work item
    /// - Returns immediately (doesn't wait for orchestration to start/complete)
    /// - Use `wait_for_orchestration()` to wait for completion
    ///
    /// # Instance ID Requirements
    ///
    /// - Must be unique across all orchestrations
    /// - Can be any string (alphanumeric + hyphens recommended)
    /// - Reusing an instance ID that already exists will fail
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::Client;
    /// # use std::sync::Arc;
    /// # async fn example(client: Client) -> Result<(), String> {
    /// // Start with JSON string input
    /// client.start_orchestration(
    ///     "order-123",
    ///     "ProcessOrder",
    ///     r#"{"customer_id": "c1", "items": ["item1", "item2"]}"#
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
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
    ///
    /// # Purpose
    ///
    /// Send a signal/message to a running orchestration that is waiting for an external event.
    /// The orchestration must have called `ctx.schedule_wait(event_name)` to receive the event.
    ///
    /// # Parameters
    ///
    /// * `instance` - Instance ID of the running orchestration
    /// * `event_name` - Name of the event (must match `schedule_wait` name)
    /// * `data` - Payload data (JSON string, passed to orchestration)
    ///
    /// # Behavior
    ///
    /// - Enqueues ExternalRaised work item to orchestrator queue
    /// - If instance isn't waiting for this event (yet), it's buffered
    /// - Event is matched by NAME (not correlation ID)
    /// - Multiple events with same name can be raised
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::Client;
    /// # async fn example(client: Client) -> Result<(), String> {
    /// // Orchestration waiting for approval
    /// // ctx.schedule_wait("ApprovalEvent").into_event().await
    ///
    /// // External system/human approves
    /// client.raise_event(
    ///     "order-123",
    ///     "ApprovalEvent",
    ///     r#"{"approved": true, "by": "manager@company.com"}"#
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - Human approval workflows
    /// - Webhook callbacks
    /// - Inter-orchestration communication
    /// - External system integration
    ///
    /// # Error Cases
    ///
    /// - Instance doesn't exist: Event is buffered, orchestration processes when started
    /// - Instance already completed: Event is ignored gracefully
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
    ///
    /// # Purpose
    ///
    /// Gracefully cancel a running orchestration. The orchestration will complete its current
    /// turn and then fail deterministically with a "canceled: {reason}" error.
    ///
    /// # Parameters
    ///
    /// * `instance` - Instance ID to cancel
    /// * `reason` - Reason for cancellation (included in error message)
    ///
    /// # Behavior
    ///
    /// 1. Enqueues CancelInstance work item
    /// 2. Runtime appends OrchestrationCancelRequested event
    /// 3. Next turn, orchestration sees cancellation and fails deterministically
    /// 4. Final status: `OrchestrationStatus::Failed { error: "canceled: {reason}" }`
    ///
    /// # Deterministic Cancellation
    ///
    /// Cancellation is **deterministic** - the orchestration fails at a well-defined point:
    /// - Not mid-activity (activities complete)
    /// - Not mid-turn (current turn finishes)
    /// - Failure is recorded in history (replays consistently)
    ///
    /// # Propagation
    ///
    /// If the orchestration has child sub-orchestrations, they are also cancelled.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Cancel a long-running order
    /// client.cancel_instance("order-123", "Customer requested cancellation").await?;
    ///
    /// // Wait for cancellation to complete
    /// let status = client.wait_for_orchestration("order-123", std::time::Duration::from_secs(5)).await?;
    /// match status {
    ///     OrchestrationStatus::Failed { error } if error.starts_with("canceled:") => {
    ///         println!("Successfully cancelled");
    ///     }
    ///     _ => {}
    /// }
    /// ```
    ///
    /// # Error Cases
    ///
    /// - Instance already completed: Cancellation is no-op
    /// - Instance doesn't exist: Cancellation is no-op
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
    ///
    /// # Purpose
    ///
    /// Query the current state of an orchestration instance without waiting.
    ///
    /// # Parameters
    ///
    /// * `instance` - Instance ID to query
    ///
    /// # Returns
    ///
    /// * `OrchestrationStatus::NotFound` - Instance doesn't exist
    /// * `OrchestrationStatus::Running` - Instance is still executing
    /// * `OrchestrationStatus::Completed { output }` - Instance completed successfully
    /// * `OrchestrationStatus::Failed { error }` - Instance failed (includes cancellations)
    ///
    /// # Behavior
    ///
    /// - Reads instance history from provider
    /// - Scans for terminal events (Completed/Failed)
    /// - For multi-execution instances (ContinueAsNew), returns status of LATEST execution
    ///
    /// # Performance
    ///
    /// This method reads from storage (not cached). For polling, use `wait_for_orchestration` instead.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use duroxide::{Client, OrchestrationStatus};
    /// # async fn example(client: Client) -> Result<(), String> {
    /// let status = client.get_orchestration_status("order-123").await;
    ///
    /// match status {
    ///     OrchestrationStatus::NotFound => println!("Instance not found"),
    ///     OrchestrationStatus::Running => println!("Still processing"),
    ///     OrchestrationStatus::Completed { output } => println!("Done: {}", output),
    ///     OrchestrationStatus::Failed { error } => eprintln!("Error: {}", error),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_orchestration_status(&self, instance: &str) -> OrchestrationStatus {
        let hist = self.store.read(instance).await;
        // Find terminal events first
        for e in hist.iter().rev() {
            match e {
                Event::OrchestrationCompleted { output, .. } => {
                    return OrchestrationStatus::Completed { output: output.clone() }
                }
                Event::OrchestrationFailed { error, .. } => {
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
    ///
    /// # Purpose
    ///
    /// Poll for orchestration completion with exponential backoff, returning when terminal or timeout.
    ///
    /// # Parameters
    ///
    /// * `instance` - Instance ID to wait for
    /// * `timeout` - Maximum time to wait before returning timeout error
    ///
    /// # Returns
    ///
    /// * `Ok(OrchestrationStatus::Completed { output })` - Orchestration completed successfully
    /// * `Ok(OrchestrationStatus::Failed { error })` - Orchestration failed (includes cancellations)
    /// * `Err(WaitError::Timeout)` - Timeout elapsed while still Running
    /// * `Err(WaitError::Other(msg))` - Unexpected error
    ///
    /// **Note:** Never returns `NotFound` or `Running` - only terminal states or timeout.
    ///
    /// # Polling Behavior
    ///
    /// - First check: Immediate (no delay)
    /// - Subsequent checks: Exponential backoff starting at 5ms, doubling each iteration, max 100ms
    /// - Continues until terminal state or timeout
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Start orchestration
    /// client.start_orchestration("order-123", "ProcessOrder", "{}").await?;
    ///
    /// // Wait up to 30 seconds
    /// match client.wait_for_orchestration("order-123", std::time::Duration::from_secs(30)).await {
    ///     Ok(OrchestrationStatus::Completed { output }) => {
    ///         println!("Success: {}", output);
    ///     }
    ///     Ok(OrchestrationStatus::Failed { error }) => {
    ///         eprintln!("Orchestration failed: {}", error);
    ///     }
    ///     Err(WaitError::Timeout) => {
    ///         println!("Still running after 30s, instance: order-123");
    ///         // Instance is still running - can wait more or cancel
    ///     }
    ///     _ => unreachable!("wait_for_orchestration only returns terminal or timeout"),
    /// }
    /// ```
    ///
    /// # Use Cases
    ///
    /// - Synchronous request/response workflows
    /// - Testing (wait for workflow to complete)
    /// - CLI tools (block until done)
    /// - Health checks
    ///
    /// # For Long-Running Workflows
    ///
    /// Don't wait for hours/days:
    /// ```ignore
    /// // Start workflow
    /// client.start_orchestration("batch-job", "ProcessBatch", "{}").await.unwrap();
    ///
    /// // DON'T wait for hours
    /// // let status = client.wait_for_orchestration("batch-job", Duration::from_hours(24)).await;
    ///
    /// // DO poll periodically
    /// loop {
    ///     match client.get_orchestration_status("batch-job").await {
    ///         OrchestrationStatus::Completed { .. } => break,
    ///         OrchestrationStatus::Failed { .. } => break,
    ///         _ => tokio::time::sleep(std::time::Duration::from_secs(60)).await,
    ///     }
    /// }
    /// ```
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
