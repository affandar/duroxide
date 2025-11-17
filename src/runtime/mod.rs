//
use crate::providers::{ExecutionMetadata, Provider, WorkItem};
use crate::{Event, OrchestrationContext};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::warn;

/// Configuration options for the Runtime.
///
/// # Example
///
/// ```rust,no_run
/// # use duroxide::runtime::{RuntimeOptions, ObservabilityConfig, LogFormat};
/// let options = RuntimeOptions {
///     orchestration_concurrency: 4,
///     worker_concurrency: 8,
///     orchestrator_lock_timeout_secs: 10,         // 10 seconds for orchestration turns
///     worker_lock_timeout_secs: 300,              // 5 minutes for long-running activities
///     worker_lock_renewal_buffer_secs: 30,        // Renew 30s before expiration (at 270s)
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

    /// Lock timeout in seconds for orchestrator queue items.
    /// When an orchestration message is dequeued, it's locked for this duration.
    /// Orchestration turns are typically fast (milliseconds), so a shorter timeout is appropriate.
    /// If processing doesn't complete within this time, the lock expires and the message is retried.
    /// Default: 5 seconds
    pub orchestrator_lock_timeout_secs: u64,

    /// Lock timeout in seconds for worker queue items (activities).
    /// When an activity is dequeued, it's locked for this duration.
    /// Activities can be long-running (minutes), so a longer timeout is appropriate.
    /// If processing doesn't complete within this time, the lock expires and the activity is retried.
    /// Higher values = more tolerance for long-running activities.
    /// Lower values = faster retry on failures, but may timeout legitimate work.
    /// Default: 30 seconds
    pub worker_lock_timeout_secs: u64,

    /// Buffer time in seconds before lock expiration to trigger renewal.
    ///
    /// Lock renewal strategy:
    /// - If worker_lock_timeout_secs >= 15: Renew at (timeout - buffer_secs)
    /// - If worker_lock_timeout_secs < 15: Renew at 0.5 * timeout (buffer_secs ignored)
    ///
    /// Example with default values (timeout=30s, buffer=5s):
    /// - Initial lock: expires at T+30s
    /// - First renewal: at T+25s (30-5), extends to T+55s
    /// - Second renewal: at T+50s (55-5), extends to T+80s
    ///
    /// Example with short timeout (timeout=10s, buffer ignored):
    /// - Initial lock: expires at T+10s
    /// - First renewal: at T+5s (10*0.5), extends to T+15s
    /// - Second renewal: at T+10s (15*0.5), extends to T+20s
    ///
    /// Default: 5 seconds
    pub worker_lock_renewal_buffer_secs: u64,

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
            orchestrator_lock_timeout_secs: 5,
            worker_lock_timeout_secs: 30,
            worker_lock_renewal_buffer_secs: 5,
            observability: ObservabilityConfig::default(),
        }
    }
}

mod dispatchers;
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
    /// Unique runtime instance ID (4-char hex, generated on start)
    runtime_id: String,
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
        let hist = self.history_store.read(instance).await.unwrap_or_default();
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
    ///
    /// If `current_execution_id` is provided and the instance matches, use it directly.
    /// Otherwise, check in-memory tracking, then fall back to INITIAL_EXECUTION_ID.
    async fn get_execution_id_for_instance(&self, instance: &str, current_execution_id: Option<u64>) -> u64 {
        // If this is the current instance being processed, use the provided execution_id
        if let Some(exec_id) = current_execution_id {
            // Update in-memory tracking for future calls
            self.current_execution_ids
                .lock()
                .await
                .insert(instance.to_string(), exec_id);
            return exec_id;
        }

        // First check in-memory tracking
        if let Some(&exec_id) = self.current_execution_ids.lock().await.get(instance) {
            return exec_id;
        }

        // Fall back to INITIAL_EXECUTION_ID (no longer querying Provider::latest_execution_id)
        crate::INITIAL_EXECUTION_ID
    }


    /// Start a new runtime using the in-memory SQLite provider.
    pub async fn start(
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
    ) -> Arc<Self> {
        let history_store: Arc<dyn Provider> = Arc::new(
            crate::providers::sqlite::SqliteProvider::new_in_memory()
                .await
                .expect("in-memory SQLite provider creation should never fail"),
        );
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

        // Generate unique runtime instance ID (4-char hex)
        use std::time::{SystemTime, UNIX_EPOCH};
        let runtime_id = format!(
            "{:04x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| (d.as_nanos() & 0xFFFF) as u16)
                .unwrap_or(0)
        );

        // start request queue + worker
        let runtime = Arc::new(Self {
            joins: Mutex::new(joins),
            history_store,

            orchestration_registry,
            current_execution_ids: Mutex::new(HashMap::new()),
            shutdown_flag: Arc::new(AtomicBool::new(false)),

            options,
            observability_handle,
            runtime_id,
        });

        // background orchestrator dispatcher (extracted from inline poller)
        let handle = runtime.clone().start_orchestration_dispatcher();
        runtime.joins.lock().await.push(handle);

        // background work dispatcher (executes activities)
        let work_handle = runtime.clone().start_work_dispatcher(activity_registry);
        runtime.joins.lock().await.push(work_handle);

        runtime
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
