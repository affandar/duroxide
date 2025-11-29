use std::sync::Arc;

use crate::_typed_codec::{Codec, Json};
use crate::providers::{
    ExecutionInfo, InstanceInfo, Provider, ProviderAdmin, ProviderError, QueueDepths, SystemMetrics, WorkItem,
};
use crate::{EventKind, OrchestrationStatus};
use serde::Serialize;

/// Client-specific error type that wraps provider errors and adds client-specific errors.
///
/// This enum allows callers to distinguish between:
/// - Provider errors (storage failures, can be retryable or permanent)
/// - Client-specific errors (validation, capability not available, etc.)
#[derive(Debug, Clone)]
pub enum ClientError {
    /// Provider operation failed (wraps ProviderError)
    Provider(ProviderError),

    /// Management capability not available
    ManagementNotAvailable,

    /// Invalid input (client validation)
    InvalidInput { message: String },

    /// Operation timed out
    Timeout,
}

impl ClientError {
    /// Check if this error is retryable (only applies to Provider errors)
    pub fn is_retryable(&self) -> bool {
        match self {
            ClientError::Provider(e) => e.is_retryable(),
            ClientError::ManagementNotAvailable => false,
            ClientError::InvalidInput { .. } => false,
            ClientError::Timeout => true,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Provider(e) => write!(f, "{}", e),
            ClientError::ManagementNotAvailable => write!(
                f,
                "Management features not available - provider doesn't implement ProviderAdmin"
            ),
            ClientError::InvalidInput { message } => write!(f, "Invalid input: {}", message),
            ClientError::Timeout => write!(f, "Operation timed out"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ProviderError> for ClientError {
    fn from(e: ProviderError) -> Self {
        ClientError::Provider(e)
    }
}

// Constants for polling behavior in wait_for_orchestration
/// Initial delay between status polls (5ms)
const INITIAL_POLL_DELAY_MS: u64 = 5;

/// Maximum delay between status polls (100ms)
const MAX_POLL_DELAY_MS: u64 = 100;

/// Multiplier for exponential backoff
const POLL_DELAY_MULTIPLIER: u64 = 2;

/// Client for orchestration control-plane operations with automatic capability discovery.
///
/// The Client provides APIs for managing orchestration instances:
/// - Starting orchestrations
/// - Raising external events
/// - Cancelling instances
/// - Checking status
/// - Waiting for completion
/// - Rich management features (when available)
///
/// # Automatic Capability Discovery
///
/// The Client automatically discovers provider capabilities through the `Provider::as_management_capability()` method.
/// When a provider implements `ProviderAdmin`, rich management features become available:
///
/// ```ignore
/// let client = Client::new(provider);
///
/// // Control plane (always available)
/// client.start_orchestration("order-1", "ProcessOrder", "{}").await?;
///
/// // Management (automatically discovered)
/// if client.has_management_capability() {
///     let instances = client.list_all_instances().await?;
///     let metrics = client.get_system_metrics().await?;
/// } else {
///     println!("Management features not available");
/// }
/// ```
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
/// use duroxide::ClientError;
/// let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);
/// let client = Client::new(store);
///
/// // Start an orchestration
/// client.start_orchestration("order-123", "ProcessOrder", r#"{"customer_id": "c1"}"#).await?;
///
/// // Check status
/// let status = client.get_orchestration_status("order-123").await?;
/// println!("Status: {:?}", status);
///
/// // Wait for completion
/// let result = client.wait_for_orchestration("order-123", std::time::Duration::from_secs(30)).await.unwrap();
/// match result {
///     OrchestrationStatus::Completed { output } => println!("Done: {}", output),
///     OrchestrationStatus::Failed { details } => {
///         eprintln!("Failed ({}): {}", details.category(), details.display_message());
///     }
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
    /// # use duroxide::{Client, ClientError};
    /// # use std::sync::Arc;
    /// # async fn example(client: Client) -> Result<(), ClientError> {
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
        instance: impl Into<String>,
        orchestration: impl Into<String>,
        input: impl Into<String>,
    ) -> Result<(), ClientError> {
        let item = WorkItem::StartOrchestration {
            instance: instance.into(),
            orchestration: orchestration.into(),
            input: input.into(),
            version: None,
            parent_instance: None,
            parent_id: None,
            execution_id: crate::INITIAL_EXECUTION_ID,
        };
        self.store
            .enqueue_for_orchestrator(item, None)
            .await
            .map_err(ClientError::from)
    }

    /// Start an orchestration instance pinned to a specific version.
    pub async fn start_orchestration_versioned(
        &self,
        instance: impl Into<String>,
        orchestration: impl Into<String>,
        version: impl Into<String>,
        input: impl Into<String>,
    ) -> Result<(), ClientError> {
        let item = WorkItem::StartOrchestration {
            instance: instance.into(),
            orchestration: orchestration.into(),
            input: input.into(),
            version: Some(version.into()),
            parent_instance: None,
            parent_id: None,
            execution_id: crate::INITIAL_EXECUTION_ID,
        };
        self.store
            .enqueue_for_orchestrator(item, None)
            .await
            .map_err(ClientError::from)
    }

    // Note: No delayed scheduling API. Clients should use normal start APIs.

    /// Start an orchestration with typed input (serialized to JSON).
    pub async fn start_orchestration_typed<In: Serialize>(
        &self,
        instance: impl Into<String>,
        orchestration: impl Into<String>,
        input: In,
    ) -> Result<(), ClientError> {
        let payload = Json::encode(&input).map_err(|e| ClientError::InvalidInput {
            message: format!("encode: {e}"),
        })?;
        self.start_orchestration(instance, orchestration, payload).await
    }

    /// Start a versioned orchestration with typed input (serialized to JSON).
    pub async fn start_orchestration_versioned_typed<In: Serialize>(
        &self,
        instance: impl Into<String>,
        orchestration: impl Into<String>,
        version: impl Into<String>,
        input: In,
    ) -> Result<(), ClientError> {
        let payload = Json::encode(&input).map_err(|e| ClientError::InvalidInput {
            message: format!("encode: {e}"),
        })?;
        self.start_orchestration_versioned(instance, orchestration, version, payload)
            .await
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
    /// # use duroxide::{Client, ClientError};
    /// # async fn example(client: Client) -> Result<(), ClientError> {
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
        instance: impl Into<String>,
        event_name: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<(), ClientError> {
        let item = WorkItem::ExternalRaised {
            instance: instance.into(),
            name: event_name.into(),
            data: data.into(),
        };
        self.store
            .enqueue_for_orchestrator(item, None)
            .await
            .map_err(ClientError::from)
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
    /// 4. Final status: `OrchestrationStatus::Failed { details: Application::Cancelled }`
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
    ///     OrchestrationStatus::Failed { details } if matches!(
    ///         details,
    ///         duroxide::ErrorDetails::Application {
    ///             kind: duroxide::AppErrorKind::Cancelled { .. },
    ///             ..
    ///         }
    ///     ) => {
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
        instance: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<(), ClientError> {
        let item = WorkItem::CancelInstance {
            instance: instance.into(),
            reason: reason.into(),
        };
        self.store
            .enqueue_for_orchestrator(item, None)
            .await
            .map_err(ClientError::from)
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
    /// # use duroxide::{Client, ClientError, OrchestrationStatus};
    /// # async fn example(client: Client) -> Result<(), ClientError> {
    /// let status = client.get_orchestration_status("order-123").await?;
    ///
    /// match status {
    ///     OrchestrationStatus::NotFound => println!("Instance not found"),
    ///     OrchestrationStatus::Running => println!("Still processing"),
    ///     OrchestrationStatus::Completed { output } => println!("Done: {}", output),
    ///     OrchestrationStatus::Failed { details } => eprintln!("Error: {}", details.display_message()),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_orchestration_status(&self, instance: &str) -> Result<OrchestrationStatus, ClientError> {
        let hist = self.store.read(instance).await.map_err(ClientError::from)?;
        // Find terminal events first
        for e in hist.iter().rev() {
            match &e.kind {
                EventKind::OrchestrationCompleted { output } => {
                    return Ok(OrchestrationStatus::Completed { output: output.clone() });
                }
                EventKind::OrchestrationFailed { details } => {
                    return Ok(OrchestrationStatus::Failed {
                        details: details.clone(),
                    });
                }
                _ => {}
            }
        }
        // If we ever saw a start, it's running
        if hist.iter().any(|e| matches!(&e.kind, EventKind::OrchestrationStarted { .. })) {
            Ok(OrchestrationStatus::Running)
        } else {
            Ok(OrchestrationStatus::NotFound)
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
    /// * `Ok(OrchestrationStatus::Failed { details })` - Orchestration failed (includes cancellations)
    /// * `Err(ClientError::Timeout)` - Timeout elapsed while still Running
    /// * `Err(ClientError::Provider(e))` - Provider/Storage error
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
    ///     Ok(OrchestrationStatus::Failed { details }) => {
    ///         eprintln!("Failed ({}): {}", details.category(), details.display_message());
    ///     }
    ///     Err(ClientError::Timeout) => {
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
    ) -> Result<OrchestrationStatus, ClientError> {
        let deadline = std::time::Instant::now() + timeout;
        // quick path
        match self.get_orchestration_status(instance).await {
            Ok(OrchestrationStatus::Completed { output }) => return Ok(OrchestrationStatus::Completed { output }),
            Ok(OrchestrationStatus::Failed { details }) => return Ok(OrchestrationStatus::Failed { details }),
            Err(e) => return Err(e),
            _ => {}
        }
        // poll with backoff
        let mut delay_ms: u64 = INITIAL_POLL_DELAY_MS;
        while std::time::Instant::now() < deadline {
            match self.get_orchestration_status(instance).await {
                Ok(OrchestrationStatus::Completed { output }) => return Ok(OrchestrationStatus::Completed { output }),
                Ok(OrchestrationStatus::Failed { details }) => return Ok(OrchestrationStatus::Failed { details }),
                Err(e) => return Err(e),
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms.saturating_mul(POLL_DELAY_MULTIPLIER)).min(MAX_POLL_DELAY_MS);
                }
            }
        }
        Err(ClientError::Timeout)
    }

    /// Typed wait helper: decodes output on Completed, returns Err(String) on Failed.
    pub async fn wait_for_orchestration_typed<Out: serde::de::DeserializeOwned>(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<Result<Out, String>, ClientError> {
        match self.wait_for_orchestration(instance, timeout).await? {
            OrchestrationStatus::Completed { output } => match Json::decode::<Out>(&output) {
                Ok(v) => Ok(Ok(v)),
                Err(e) => Err(ClientError::InvalidInput {
                    message: format!("decode failed: {e}"),
                }),
            },
            OrchestrationStatus::Failed { details } => Ok(Err(details.display_message())),
            _ => unreachable!("wait_for_orchestration returns only terminal or timeout"),
        }
    }

    // ===== Capability Discovery =====

    /// Check if management capabilities are available.
    ///
    /// # Returns
    ///
    /// `true` if the provider implements `ProviderAdmin`, `false` otherwise.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let instances = client.list_all_instances().await?;
    /// } else {
    ///     println!("Management features not available");
    /// }
    /// ```
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }

    /// Automatically discover management capabilities from the provider.
    ///
    /// # Returns
    ///
    /// `Ok(&dyn ManagementCapability)` if available, `Err(String)` if not.
    ///
    /// # Internal Use
    ///
    /// This method is used internally by management methods to access capabilities.
    fn discover_management(&self) -> Result<&dyn ProviderAdmin, ClientError> {
        self.store
            .as_management_capability()
            .ok_or(ClientError::ManagementNotAvailable)
    }

    // ===== Rich Management Methods =====

    /// List all orchestration instances.
    ///
    /// # Returns
    ///
    /// Vector of instance IDs, typically sorted by creation time (newest first).
    ///
    /// # Errors
    ///
    /// Returns `Err("Management features not available")` if the provider doesn't implement `ProviderAdmin`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let instances = client.list_all_instances().await?;
    ///     for instance in instances {
    ///         println!("Instance: {}", instance);
    ///     }
    /// }
    /// ```
    pub async fn list_all_instances(&self) -> Result<Vec<String>, ClientError> {
        self.discover_management()?
            .list_instances()
            .await
            .map_err(ClientError::from)
    }

    /// List instances matching a status filter.
    ///
    /// # Parameters
    ///
    /// * `status` - Filter by execution status: "Running", "Completed", "Failed", "ContinuedAsNew"
    ///
    /// # Returns
    ///
    /// Vector of instance IDs with the specified status.
    ///
    /// # Errors
    ///
    /// Returns `Err("Management features not available")` if the provider doesn't implement `ProviderAdmin`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let running = client.list_instances_by_status("Running").await?;
    ///     let completed = client.list_instances_by_status("Completed").await?;
    ///     println!("Running: {}, Completed: {}", running.len(), completed.len());
    /// }
    /// ```
    pub async fn list_instances_by_status(&self, status: &str) -> Result<Vec<String>, ClientError> {
        self.discover_management()?
            .list_instances_by_status(status)
            .await
            .map_err(ClientError::from)
    }

    /// Get comprehensive information about an instance.
    ///
    /// # Parameters
    ///
    /// * `instance` - The ID of the orchestration instance.
    ///
    /// # Returns
    ///
    /// Detailed instance information including status, output, and metadata.
    ///
    /// # Errors
    ///
    /// Returns `Err("Management features not available")` if the provider doesn't implement `ProviderAdmin`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let info = client.get_instance_info("order-123").await?;
    ///     println!("Instance {}: {} ({})", info.instance_id, info.orchestration_name, info.status);
    /// }
    /// ```
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, ClientError> {
        self.discover_management()?
            .get_instance_info(instance)
            .await
            .map_err(ClientError::from)
    }

    /// Get detailed information about a specific execution.
    ///
    /// # Parameters
    ///
    /// * `instance` - The ID of the orchestration instance.
    /// * `execution_id` - The specific execution ID.
    ///
    /// # Returns
    ///
    /// Detailed execution information including status, output, and event count.
    ///
    /// # Errors
    ///
    /// Returns `Err("Management features not available")` if the provider doesn't implement `ProviderAdmin`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let info = client.get_execution_info("order-123", 1).await?;
    ///     println!("Execution {}: {} events, status: {}", info.execution_id, info.event_count, info.status);
    /// }
    /// ```
    pub async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, ClientError> {
        self.discover_management()?
            .get_execution_info(instance, execution_id)
            .await
            .map_err(ClientError::from)
    }

    /// List all execution IDs for an instance.
    ///
    /// Returns execution IDs in ascending order: [1], [1, 2], [1, 2, 3], etc.
    /// Each execution represents either the initial run or a continuation via ContinueAsNew.
    ///
    /// # Parameters
    ///
    /// * `instance` - The instance ID to query
    ///
    /// # Returns
    ///
    /// Vector of execution IDs in ascending order.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The provider doesn't support management capabilities
    /// - The database query fails
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let executions = client.list_executions("order-123").await?;
    ///     println!("Instance has {} executions", executions.len()); // [1, 2, 3]
    /// }
    /// ```
    pub async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, ClientError> {
        let mgmt = self.discover_management()?;
        mgmt.list_executions(instance).await.map_err(ClientError::from)
    }

    /// Read the full event history for a specific execution within an instance.
    ///
    /// Returns all events for the specified execution in chronological order.
    /// Each execution has its own independent history starting from OrchestrationStarted.
    ///
    /// # Parameters
    ///
    /// * `instance` - The instance ID
    /// * `execution_id` - The specific execution ID (starts at 1)
    ///
    /// # Returns
    ///
    /// Vector of events in chronological order (oldest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The provider doesn't support management capabilities
    /// - The instance or execution doesn't exist
    /// - The database query fails
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let history = client.read_execution_history("order-123", 1).await?;
    ///     for event in history {
    ///         println!("Event: {:?}", event);
    ///     }
    /// }
    /// ```
    pub async fn read_execution_history(
        &self,
        instance: &str,
        execution_id: u64,
    ) -> Result<Vec<crate::Event>, ClientError> {
        let mgmt = self.discover_management()?;
        mgmt.read_history_with_execution_id(instance, execution_id)
            .await
            .map_err(ClientError::from)
    }

    /// Get system-wide metrics for the orchestration engine.
    ///
    /// # Returns
    ///
    /// System metrics including instance counts, execution counts, and status breakdown.
    ///
    /// # Errors
    ///
    /// Returns `Err("Management features not available")` if the provider doesn't implement `ProviderAdmin`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let metrics = client.get_system_metrics().await?;
    ///     println!("System health: {} running, {} completed, {} failed",
    ///         metrics.running_instances, metrics.completed_instances, metrics.failed_instances);
    /// }
    /// ```
    pub async fn get_system_metrics(&self) -> Result<SystemMetrics, ClientError> {
        self.discover_management()?
            .get_system_metrics()
            .await
            .map_err(ClientError::from)
    }

    /// Get the current depths of the internal work queues.
    ///
    /// # Returns
    ///
    /// Queue depths for orchestrator, worker, and timer queues.
    ///
    /// # Errors
    ///
    /// Returns `Err("Management features not available")` if the provider doesn't implement `ProviderAdmin`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let client = Client::new(provider);
    /// if client.has_management_capability() {
    ///     let queues = client.get_queue_depths().await?;
    ///     println!("Queue depths - Orchestrator: {}, Worker: {}, Timer: {}",
    ///         queues.orchestrator_queue, queues.worker_queue, queues.timer_queue);
    /// }
    /// ```
    pub async fn get_queue_depths(&self) -> Result<QueueDepths, ClientError> {
        self.discover_management()?
            .get_queue_depths()
            .await
            .map_err(ClientError::from)
    }
}
