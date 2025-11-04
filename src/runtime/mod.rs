//
use crate::providers::{ExecutionMetadata, Provider, WorkItem};
use crate::{Event, OrchestrationContext};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, warn};

/// Configuration options for the Runtime.
///
/// # Example
///
/// ```rust,no_run
/// # use duroxide::runtime::{RuntimeOptions, ObservabilityConfig, LogFormat};
/// let options = RuntimeOptions {
///     orchestration_concurrency: 4,
///     worker_concurrency: 8,
///     observability: ObservabilityConfig {
///         log_format: LogFormat::Compact,
///         log_level: "info".to_string(),
///         ..Default::default()
///     },
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// Polling interval in milliseconds when dispatcher queues are empty.
    /// Lower values = more responsive, higher CPU usage when idle.
    /// Higher values = less CPU usage, higher latency when idle.
    /// Default: 100ms (10 Hz)
    pub dispatcher_idle_sleep_ms: u64,

    /// Number of concurrent orchestration workers.
    /// Each worker can process one orchestration turn at a time.
    /// Higher values = more parallel orchestration execution.
    /// Default: 2
    pub orchestration_concurrency: usize,

    /// Number of concurrent worker dispatchers.
    /// Each worker can execute one activity at a time.
    /// Higher values = more parallel activity execution.
    /// Default: 2
    pub worker_concurrency: usize,

    /// Observability configuration for metrics and logging.
    /// Requires the `observability` feature flag for full functionality.
    /// Default: Disabled with basic logging
    pub observability: ObservabilityConfig,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            dispatcher_idle_sleep_ms: 100,
            orchestration_concurrency: 2,
            worker_concurrency: 2,
            observability: ObservabilityConfig::default(),
        }
    }
}

pub mod observability;
pub mod registry;
mod state_helpers;

use async_trait::async_trait;
pub use state_helpers::{HistoryManager, WorkItemReader};

pub mod execution;
pub mod replay_engine;

pub use observability::{LogFormat, ObservabilityConfig};

/// High-level orchestration status derived from history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestrationStatus {
    /// Instance does not exist
    NotFound,
    /// Instance is currently executing
    Running,
    /// Instance completed successfully with output
    Completed { output: String },
    /// Instance failed with structured error details.
    /// Use `details.category()` to distinguish infrastructure/configuration/application errors.
    Failed { details: crate::ErrorDetails },
}

/// Error type returned by orchestration wait helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitError {
    Timeout,
    Other(String),
}

/// Trait implemented by orchestration handlers that can be invoked by the runtime.
#[async_trait]
pub trait OrchestrationHandler: Send + Sync {
    async fn invoke(&self, ctx: OrchestrationContext, input: String) -> Result<String, String>;
}

/// Function wrapper that implements `OrchestrationHandler`.
pub struct FnOrchestration<F, Fut>(pub F)
where
    F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static;

#[async_trait]
impl<F, Fut> OrchestrationHandler for FnOrchestration<F, Fut>
where
    F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    async fn invoke(&self, ctx: OrchestrationContext, input: String) -> Result<String, String> {
        (self.0)(ctx, input).await
    }
}

/// Immutable registry mapping orchestration names to versioned handlers.
pub use crate::runtime::registry::{OrchestrationRegistry, OrchestrationRegistryBuilder, VersionPolicy};

pub fn kind_of(msg: &WorkItem) -> &'static str {
    match msg {
        WorkItem::StartOrchestration { .. } => "StartOrchestration",
        WorkItem::ActivityExecute { .. } => "ActivityExecute",
        WorkItem::ActivityCompleted { .. } => "ActivityCompleted",
        WorkItem::ActivityFailed { .. } => "ActivityFailed",
        WorkItem::TimerFired { .. } => "TimerFired",
        WorkItem::ExternalRaised { .. } => "ExternalRaised",
        WorkItem::SubOrchCompleted { .. } => "SubOrchCompleted",
        WorkItem::SubOrchFailed { .. } => "SubOrchFailed",
        WorkItem::CancelInstance { .. } => "CancelInstance",
        WorkItem::ContinueAsNew { .. } => "ContinueAsNew",
    }
}

/// In-process runtime that executes activities and timers and persists
/// history via a `Provider`.
pub struct Runtime {
    joins: Mutex<Vec<JoinHandle<()>>>,
    history_store: Arc<dyn Provider>,
    orchestration_registry: OrchestrationRegistry,
    /// Track the current execution ID for each active instance
    current_execution_ids: Mutex<HashMap<String, u64>>,
    /// Shutdown flag checked by dispatchers
    shutdown_flag: Arc<AtomicBool>,
    /// Runtime configuration options
    options: RuntimeOptions,
    /// Observability handle for metrics and logging
    observability_handle: Option<observability::ObservabilityHandle>,
}

/// Introspection: descriptor of an orchestration derived from history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationDescriptor {
    pub name: String,
    pub version: String,
    pub parent_instance: Option<String>,
    pub parent_id: Option<u64>,
}

impl Runtime {
    #[inline]
    fn record_orchestration_completion(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_orchestration_completion();
        }
    }

    #[inline]
    fn record_orchestration_application_error(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_orchestration_application_error();
        }
    }

    #[inline]
    fn record_orchestration_infrastructure_error(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_orchestration_infrastructure_error();
        }
    }

    #[inline]
    fn record_orchestration_configuration_error(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_orchestration_configuration_error();
        }
    }

    #[inline]
    fn record_activity_success(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_activity_success();
        }
    }

    #[inline]
    fn record_activity_app_error(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_activity_app_error();
        }
    }

    #[inline]
    fn record_activity_config_error(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_activity_config_error();
        }
    }

    #[inline]
    fn record_activity_infra_error(&self) {
        if let Some(handle) = &self.observability_handle {
            handle.record_activity_infra_error();
        }
    }

    pub fn metrics_snapshot(&self) -> Option<observability::MetricsSnapshot> {
        self.observability_handle
            .as_ref()
            .and_then(|handle| handle.metrics_snapshot())
    }

    /// Compute execution metadata from history delta without inspecting event contents.
    /// This allows the runtime to extract semantic information and pass it to the provider
    /// as pre-computed metadata, preventing the provider from needing orchestration knowledge.
    fn compute_execution_metadata(
        history_delta: &[Event],
        _orchestrator_items: &[WorkItem],
        _current_execution_id: u64,
    ) -> ExecutionMetadata {
        let mut metadata = ExecutionMetadata::default();

        // Scan history_delta for OrchestrationStarted (first event) and terminal events
        for event in history_delta {
            match event {
                Event::OrchestrationStarted { name, version, .. } => {
                    // Capture orchestration metadata from start event
                    metadata.orchestration_name = Some(name.clone());
                    metadata.orchestration_version = Some(version.clone());
                }
                Event::OrchestrationCompleted { output, .. } => {
                    metadata.status = Some("Completed".to_string());
                    metadata.output = Some(output.clone());
                    break;
                }
                Event::OrchestrationFailed { details, .. } => {
                    metadata.status = Some("Failed".to_string());
                    metadata.output = Some(details.display_message());
                    break;
                }
                Event::OrchestrationContinuedAsNew { input, .. } => {
                    metadata.status = Some("ContinuedAsNew".to_string());
                    metadata.output = Some(input.clone());
                    // Don't set create_next_execution - the new execution will be started
                    // by WorkItem::ContinueAsNew being processed like StartOrchestration
                    break;
                }
                _ => {}
            }
        }

        metadata
    }

    // Execution engine: consumes provider queues and persists history atomically.
    /// Return the most recent descriptor `{ name, version, parent_instance?, parent_id? }` for an instance.
    /// Returns `None` if the instance/history does not exist or no OrchestrationStarted is present.
    pub async fn get_orchestration_descriptor(
        &self,
        instance: &str,
    ) -> Option<crate::runtime::OrchestrationDescriptor> {
        let hist = self.history_store.read(instance).await;
        for e in hist.iter().rev() {
            if let Event::OrchestrationStarted {
                name,
                version,
                parent_instance,
                parent_id,
                ..
            } = e
            {
                return Some(crate::runtime::OrchestrationDescriptor {
                    name: name.clone(),
                    version: version.clone(),
                    parent_instance: parent_instance.clone(),
                    parent_id: *parent_id,
                });
            }
        }
        None
    }

    /// Get the current execution ID for an instance, or fetch from store if not tracked
    async fn get_execution_id_for_instance(&self, instance: &str) -> u64 {
        // First check in-memory tracking
        if let Some(&exec_id) = self.current_execution_ids.lock().await.get(instance) {
            return exec_id;
        }

        // Fall back to querying the store
        self.history_store
            .latest_execution_id(instance)
            .await
            .unwrap_or(crate::INITIAL_EXECUTION_ID)
    }

    /// Generate a unique worker ID suffix using last 5 chars of a GUID
    fn generate_worker_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        // Thread-local counter for uniqueness
        thread_local! {
            static COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        let counter = COUNTER.with(|c| {
            let val = c.get();
            c.set(val.wrapping_add(1));
            val
        });

        // Generate a short unique suffix (last 5 hex chars)
        format!("{:05x}", ((timestamp ^ (counter as u128)) & 0xFFFFF))
    }

    /// Start a new runtime using the in-memory SQLite provider.
    pub async fn start(
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
    ) -> Arc<Self> {
        let history_store: Arc<dyn Provider> =
            Arc::new(crate::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
        Self::start_with_store(history_store, activity_registry, orchestration_registry).await
    }

    /// Start a new runtime with a custom `Provider` implementation.
    pub async fn start_with_store(
        history_store: Arc<dyn Provider>,
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
    ) -> Arc<Self> {
        Self::start_with_options(
            history_store,
            activity_registry,
            orchestration_registry,
            RuntimeOptions::default(),
        )
        .await
    }

    /// Start a new runtime with custom options.
    pub async fn start_with_options(
        history_store: Arc<dyn Provider>,
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
        options: RuntimeOptions,
    ) -> Arc<Self> {
        // Initialize observability (metrics + structured logging)
        let observability_handle = observability::ObservabilityHandle::init(&options.observability).ok(); // Gracefully degrade if observability fails to initialize

        let joins: Vec<JoinHandle<()>> = Vec::new();

        // start request queue + worker
        let runtime = Arc::new(Self {
            joins: Mutex::new(joins),
            history_store,

            orchestration_registry,
            current_execution_ids: Mutex::new(HashMap::new()),
            shutdown_flag: Arc::new(AtomicBool::new(false)),

            options,
            observability_handle,
        });

        // background orchestrator dispatcher (extracted from inline poller)
        let handle = runtime.clone().start_orchestration_dispatcher();
        runtime.joins.lock().await.push(handle);

        // background work dispatcher (executes activities)
        let work_handle = runtime.clone().start_work_dispatcher(activity_registry);
        runtime.joins.lock().await.push(work_handle);

        runtime
    }

    fn start_orchestration_dispatcher(self: Arc<Self>) -> JoinHandle<()> {
        // EXECUTION: spawns N concurrent orchestration workers
        // Instance-level locking in provider prevents concurrent processing of same instance
        let concurrency = self.options.orchestration_concurrency;
        let shutdown = self.shutdown_flag.clone();

        tokio::spawn(async move {
            let mut worker_handles = Vec::new();

            for _worker_idx in 0..concurrency {
                let rt = self.clone();
                let shutdown = shutdown.clone();
                // Generate unique worker ID with GUID suffix
                let worker_id = format!("orch-{}", Self::generate_worker_suffix());
                let handle = tokio::spawn(async move {
                    // debug!("Orchestration worker {} started", worker_id);
                    loop {
                        // Check shutdown flag before fetching
                        if shutdown.load(Ordering::Relaxed) {
                            // debug!("Orchestration worker {} exiting", worker_id);
                            break;
                        }

                        if let Some(item) = rt.history_store.fetch_orchestration_item().await {
                            // Process orchestration item atomically
                            // Provider ensures no other worker has this instance locked
                            rt.process_orchestration_item(item, &worker_id).await;
                        } else {
                            tokio::time::sleep(std::time::Duration::from_millis(rt.options.dispatcher_idle_sleep_ms))
                                .await;
                        }
                    }
                });
                worker_handles.push(handle);
            }

            // Wait for all workers to complete
            for handle in worker_handles {
                let _ = handle.await;
            }
            // debug!("Orchestration dispatcher exited");
        })
    }

    async fn process_orchestration_item(self: &Arc<Self>, item: crate::providers::OrchestrationItem, worker_id: &str) {
        // EXECUTION: builds deltas and commits via ack_orchestration_item
        let instance = &item.instance;
        let lock_token = &item.lock_token;

        // Extract metadata from history and work items
        let temp_history_mgr = HistoryManager::from_history(&item.history);
        let workitem_reader = WorkItemReader::from_messages(&item.messages, &temp_history_mgr, instance);

        // Bail on truly terminal histories (Completed/Failed), or ContinuedAsNew without a CAN start message
        if temp_history_mgr.is_completed
            || temp_history_mgr.is_failed
            || (temp_history_mgr.is_continued_as_new && !workitem_reader.is_continue_as_new)
        {
            warn!(instance = %instance, "Instance is terminal (completed/failed or CAN without start), acking batch without processing");
            self.ack_orchestration_with_changes(
                lock_token,
                item.execution_id,
                vec![],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await;
            return;
        }

        // Extract version before moving temp_history_mgr
        let version = temp_history_mgr.version().unwrap_or_else(|| "unknown".to_string());

        // Decide execution id and history to use for this execution
        let (execution_id_to_use, mut history_mgr) = if workitem_reader.is_continue_as_new {
            // ContinueAsNew - start with empty history for new execution
            (item.execution_id + 1, HistoryManager::from_history(&[]))
        } else {
            // Normal execution - use existing history
            (item.execution_id, temp_history_mgr)
        };

        // Log execution start with structured context
        if workitem_reader.has_orchestration_name() {
            tracing::debug!(
                target: "duroxide::runtime",
                instance_id = %instance,
                execution_id = %execution_id_to_use,
                orchestration_name = %workitem_reader.orchestration_name,
                orchestration_version = %version,
                worker_id = %worker_id,
                is_continue_as_new = %workitem_reader.is_continue_as_new,
                "Orchestration started"
            );
        } else if !workitem_reader.completion_messages.is_empty() {
            // Empty orchestration name with completion messages - just warn and skip
            tracing::warn!(instance = %item.instance, "empty effective batch - this should not happen");
        }

        // Process the execution (unified path)
        let (worker_items, orchestrator_items, execution_id_for_ack) = if workitem_reader.has_orchestration_name() {
            let (wi, oi) = self
                .handle_orchestration_atomic(instance, &mut history_mgr, &workitem_reader, execution_id_to_use)
                .await;
            (wi, oi, execution_id_to_use)
        } else {
            // Empty effective batch
            (vec![], vec![], execution_id_to_use)
        };

        // Atomically commit all changes
        let history_delta = history_mgr.delta();
        // debug!(
        //     instance,
        //     "Acking orchestration item: history_delta={}, worker={}, orch={}",
        //     history_delta.len(),
        //     worker_items.len(),
        //     orchestrator_items.len()
        // );

        // Compute execution metadata from history_delta (runtime responsibility)
        let metadata = Runtime::compute_execution_metadata(history_delta, &orchestrator_items, item.execution_id);

        // Log orchestration completion/failure
        if let Some(ref status) = metadata.status {
            let version = metadata.orchestration_version.as_deref().unwrap_or("unknown");
            let orch_name = metadata.orchestration_name.as_deref().unwrap_or("unknown");
            let event_count = history_mgr.full_history().len();

            match status.as_str() {
                "Completed" => {
                    tracing::debug!(
                        target: "duroxide::runtime",
                        instance_id = %instance,
                        execution_id = %execution_id_for_ack,
                        orchestration_name = %orch_name,
                        orchestration_version = %version,
                        worker_id = %worker_id,
                        history_events = %event_count,
                        "Orchestration completed"
                    );
                }
                "Failed" => {
                    // Extract error type from history_delta to determine log level
                    let error_type = history_delta
                        .iter()
                        .find_map(|event| {
                            if let Event::OrchestrationFailed { details, .. } = event {
                                Some(details.category())
                            } else {
                                None
                            }
                        })
                        .unwrap_or("unknown");

                    // Only log as ERROR for infrastructure/configuration errors
                    // Application errors are expected business logic failures
                    match error_type {
                        "infrastructure" | "configuration" => {
                            tracing::error!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_type,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed"
                            );
                        }
                        "application" => {
                            tracing::warn!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_type,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed (application error)"
                            );
                        }
                        _ => {
                            tracing::error!(
                                target: "duroxide::runtime",
                                instance_id = %instance,
                                execution_id = %execution_id_for_ack,
                                orchestration_name = %orch_name,
                                orchestration_version = %version,
                                worker_id = %worker_id,
                                history_events = %event_count,
                                error_type = %error_type,
                                error = metadata.output.as_deref().unwrap_or("unknown"),
                                "Orchestration failed (unknown error type)"
                            );
                        }
                    }
                }
                "ContinuedAsNew" => {
                    tracing::debug!(
                        target: "duroxide::runtime",
                        instance_id = %instance,
                        execution_id = %execution_id_for_ack,
                        orchestration_name = %orch_name,
                        orchestration_version = %version,
                        worker_id = %worker_id,
                        history_events = %event_count,
                        "Orchestration continued as new"
                    );
                }
                _ => {}
            }
        }

        // Robust ack with basic retry on any provider error
        self.ack_orchestration_with_changes(
            lock_token,
            execution_id_for_ack,
            history_delta.to_vec(),
            worker_items,
            orchestrator_items,
            metadata,
        )
        .await;
    }

    // Helper methods for atomic orchestration processing
    async fn handle_orchestration_atomic(
        self: &Arc<Self>,
        instance: &str,
        history_mgr: &mut HistoryManager,
        workitem_reader: &WorkItemReader,
        execution_id: u64,
    ) -> (Vec<WorkItem>, Vec<WorkItem>) {
        let mut worker_items = Vec::new();
        let mut orchestrator_items = Vec::new();

        // Create started event if this is a new instance
        if history_mgr.is_empty() {
            // Resolve version: use provided version or get from registry policy
            let resolved_version = if let Some(v) = &workitem_reader.version {
                v.to_string()
            } else if let Some(v) = self
                .orchestration_registry
                .resolve_version(&workitem_reader.orchestration_name)
                .await
            {
                v.to_string()
            } else {
                // Not found in registry - fail with unregistered error
                history_mgr.append(Event::OrchestrationStarted {
                    event_id: crate::INITIAL_EVENT_ID,
                    name: workitem_reader.orchestration_name.clone(),
                    version: "0.0.0".to_string(), // Placeholder version for unregistered
                    input: workitem_reader.input.clone(),
                    parent_instance: workitem_reader.parent_instance.clone(),
                    parent_id: workitem_reader.parent_id,
                });

                history_mgr.append_failed(crate::ErrorDetails::Configuration {
                    kind: crate::ConfigErrorKind::UnregisteredOrchestration,
                    resource: workitem_reader.orchestration_name.clone(),
                    message: None,
                });
                self.record_orchestration_configuration_error();
                return (worker_items, orchestrator_items);
            };

            history_mgr.append(Event::OrchestrationStarted {
                event_id: 1, // First event always has event_id=1
                name: workitem_reader.orchestration_name.clone(),
                version: resolved_version,
                input: workitem_reader.input.clone(),
                parent_instance: workitem_reader.parent_instance.clone(),
                parent_id: workitem_reader.parent_id,
            });
        }

        // Run the atomic execution to get all changes
        let (_exec_history_delta, exec_worker_items, exec_orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(instance, history_mgr, workitem_reader, execution_id)
            .await;

        // Combine all changes (history already in history_mgr via mutation)
        worker_items.extend(exec_worker_items);
        orchestrator_items.extend(exec_orchestrator_items);

        (worker_items, orchestrator_items)
    }

    /// Execute a function with retry logic and exponential backoff
    async fn execute_with_retry<F, R, E>(&self, operation: F, operation_tag: &str, failure_handler: Option<impl Fn()>)
    where
        F: Fn() -> R,
        R: std::future::Future<Output = Result<(), E>>,
        E: std::fmt::Display,
    {
        let mut attempts: u32 = 0;
        let max_attempts: u32 = 5;

        loop {
            match operation().await {
                Ok(()) => {
                    // debug!("{} succeeded", operation_tag);
                    break;
                }
                Err(e) => {
                    if attempts < max_attempts {
                        let backoff_ms = 10u64.saturating_mul(1 << attempts);
                        warn!(attempts, backoff_ms, error = %e, "{} failed; retrying", operation_tag);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        attempts += 1;
                        continue;
                    } else {
                        warn!(attempts, error = %e, "Failed to {}", operation_tag);
                        if let Some(handler) = failure_handler {
                            handler();
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Acknowledge an orchestration item with changes, using retry logic
    async fn ack_orchestration_with_changes(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) {
        self.execute_with_retry(
            || async {
                self.history_store
                    .ack_orchestration_item(
                        lock_token,
                        execution_id,
                        history_delta.clone(),
                        worker_items.clone(),
                        orchestrator_items.clone(),
                        metadata.clone(),
                    )
                    .await
            },
            "ack_orchestration_item",
            Some(|| {
                drop(self.history_store.abandon_orchestration_item(lock_token, Some(50)));
            }),
        )
        .await;
    }

    fn start_work_dispatcher(self: Arc<Self>, activities: Arc<registry::ActivityRegistry>) -> JoinHandle<()> {
        // EXECUTION: spawns N concurrent worker dispatchers
        // Activities are independent work units that can run in parallel
        let concurrency = self.options.worker_concurrency;
        let shutdown = self.shutdown_flag.clone();

        tokio::spawn(async move {
            let mut worker_handles = Vec::new();

            for _worker_idx in 0..concurrency {
                let rt = self.clone();
                let activities = activities.clone();
                let shutdown = shutdown.clone();
                // Generate unique worker ID with GUID suffix
                let worker_id = format!("work-{}", Self::generate_worker_suffix());
                let handle = tokio::spawn(async move {
                    // debug!("Worker dispatcher {} started", worker_id);
                    loop {
                        // Check shutdown flag before fetching
                        if shutdown.load(Ordering::Relaxed) {
                            // debug!("Worker dispatcher {} exiting", worker_id);
                            break;
                        }

                        if let Some((item, token)) = rt.history_store.dequeue_worker_peek_lock().await {
                            match item {
                                WorkItem::ActivityExecute {
                                    instance,
                                    execution_id,
                                    id,
                                    name,
                                    input,
                                } => {
                                    let descriptor = rt.get_orchestration_descriptor(&instance).await;
                                    let (orch_name, orch_version) = if let Some(desc) = descriptor {
                                        (desc.name, desc.version)
                                    } else {
                                        ("unknown".to_string(), "unknown".to_string())
                                    };
                                    let activity_ctx = crate::ActivityContext::new(
                                        instance.clone(),
                                        execution_id,
                                        orch_name,
                                        orch_version,
                                        name.clone(),
                                        id,
                                    );

                                    // Log activity start with all correlation fields
                                    tracing::debug!(
                                        target: "duroxide::runtime",
                                        instance_id = %instance,
                                        execution_id = %execution_id,
                                        activity_name = %name,
                                        activity_id = %id,
                                        worker_id = %worker_id,
                                        "Activity started"
                                    );
                                    let start_time = std::time::Instant::now();

                                    // Execute activity and atomically ack with completion
                                    enum ActivityOutcome {
                                        Success,
                                        AppError,
                                        ConfigError,
                                    }

                                    let (ack_result, outcome) = if let Some(handler) = activities.get(&name) {
                                        match handler.invoke(activity_ctx, input).await {
                                            Ok(result) => {
                                                let duration_ms = start_time.elapsed().as_millis() as u64;
                                                tracing::debug!(
                                                    target: "duroxide::runtime",
                                                    instance_id = %instance,
                                                    execution_id = %execution_id,
                                                    activity_name = %name,
                                                    activity_id = %id,
                                                    worker_id = %worker_id,
                                                    outcome = "success",
                                                    duration_ms = %duration_ms,
                                                    result_size = %result.len(),
                                                    "Activity completed"
                                                );
                                                (
                                                    rt.history_store
                                                        .ack_worker(
                                                            &token,
                                                            WorkItem::ActivityCompleted {
                                                                instance: instance.clone(),
                                                                execution_id,
                                                                id,
                                                                result,
                                                            },
                                                        )
                                                        .await,
                                                    ActivityOutcome::Success,
                                                )
                                            }
                                            Err(error) => {
                                                let duration_ms = start_time.elapsed().as_millis() as u64;
                                                tracing::warn!(
                                                    target: "duroxide::runtime",
                                                    instance_id = %instance,
                                                    execution_id = %execution_id,
                                                    activity_name = %name,
                                                    activity_id = %id,
                                                    worker_id = %worker_id,
                                                    outcome = "app_error",
                                                    duration_ms = %duration_ms,
                                                    error = %error,
                                                    "Activity failed (application error)"
                                                );
                                                (
                                                    rt.history_store
                                                        .ack_worker(
                                                            &token,
                                                            WorkItem::ActivityFailed {
                                                                instance: instance.clone(),
                                                                execution_id,
                                                                id,
                                                                details: crate::ErrorDetails::Application {
                                                                    kind: crate::AppErrorKind::ActivityFailed,
                                                                    message: error,
                                                                    retryable: false,
                                                                },
                                                            },
                                                        )
                                                        .await,
                                                    ActivityOutcome::AppError,
                                                )
                                            }
                                        }
                                    } else {
                                        let duration_ms = start_time.elapsed().as_millis() as u64;
                                        tracing::error!(
                                            target: "duroxide::runtime",
                                            instance_id = %instance,
                                            execution_id = %execution_id,
                                            activity_name = %name,
                                            activity_id = %id,
                                            worker_id = %worker_id,
                                            outcome = "system_error",
                                            error_type = "unregistered",
                                            duration_ms = %duration_ms,
                                            "Activity failed (unregistered)"
                                        );
                                        (
                                            rt.history_store
                                                .ack_worker(
                                                    &token,
                                                    WorkItem::ActivityFailed {
                                                        instance: instance.clone(),
                                                        execution_id,
                                                        id,
                                                        details: crate::ErrorDetails::Configuration {
                                                            kind: crate::ConfigErrorKind::UnregisteredActivity,
                                                            resource: name.clone(),
                                                            message: None,
                                                        },
                                                    },
                                                )
                                                .await,
                                            ActivityOutcome::ConfigError,
                                        )
                                    };

                                    match ack_result {
                                        Ok(()) => match outcome {
                                            ActivityOutcome::Success => rt.record_activity_success(),
                                            ActivityOutcome::AppError => rt.record_activity_app_error(),
                                            ActivityOutcome::ConfigError => rt.record_activity_config_error(),
                                        },
                                        Err(e) => {
                                            warn!(
                                                instance = %instance,
                                                execution_id,
                                                id,
                                                worker_id = %worker_id,
                                                error = %e,
                                                "worker: atomic ack failed"
                                            );
                                            rt.record_activity_infra_error();
                                        }
                                    }
                                }
                                other => {
                                    error!(?other, "unexpected WorkItem in Worker dispatcher; state corruption");
                                    panic!("unexpected WorkItem in Worker dispatcher");
                                }
                            }
                        } else {
                            tokio::time::sleep(std::time::Duration::from_millis(rt.options.dispatcher_idle_sleep_ms))
                                .await;
                        }
                    }
                });
                worker_handles.push(handle);
            }

            // Wait for all workers to complete
            for handle in worker_handles {
                let _ = handle.await;
            }

            // debug!("Work dispatcher exited");
        })
    }

    /// Shutdown the runtime.
    ///
    /// # Parameters
    ///
    /// * `timeout_ms` - How long to wait for graceful shutdown:
    ///   - `None`: Default 1000ms
    ///   - `Some(0)`: Immediate abort
    ///   - `Some(ms)`: Wait specified milliseconds
    pub async fn shutdown(self: Arc<Self>, timeout_ms: Option<u64>) {
        let timeout_ms = timeout_ms.unwrap_or(1000);

        if timeout_ms == 0 {
            warn!("Immediate shutdown - aborting all tasks");
            let mut joins = self.joins.lock().await;
            for j in joins.drain(..) {
                j.abort();
            }
            return;
        }

        // debug!("Graceful shutdown (timeout: {}ms)", timeout_ms);

        // Set shutdown flag - workers check this between iterations
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // Give workers time to notice and exit gracefully
        tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;

        // Check if any tasks are still running (need to be aborted)
        let mut joins = self.joins.lock().await;

        // Abort any remaining tasks
        for j in joins.drain(..) {
            j.abort();
        }

        // debug!("Runtime shut down");

        // Shutdown observability last (after all workers stopped)
        // Note: We can't move out of Arc here, so observability shutdown happens when Runtime is dropped
        // or if we could restructure to take ownership in shutdown
    }
}
