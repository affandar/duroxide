//
use crate::providers::{ExecutionMetadata, Provider, WorkItem};
use crate::{Event, OrchestrationContext};
use semver::Version;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

/// Configuration options for the Runtime.
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// Polling interval in milliseconds when dispatcher queues are empty.
    /// Lower values = more responsive, higher CPU usage when idle.
    /// Higher values = less CPU usage, higher latency when idle.
    /// Default: 10ms (100 Hz)
    pub dispatcher_idle_sleep_ms: u64,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            dispatcher_idle_sleep_ms: 10,
        }
    }
}

pub mod registry;
mod timers;
mod state_helpers;

use async_trait::async_trait;
pub use state_helpers::{HistoryManager, WorkItemReader};

pub mod execution;
pub mod orchestration_turn;

/// High-level orchestration status derived from history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestrationStatus {
    NotFound,
    Running,
    Completed { output: String },
    Failed { error: String },
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
        WorkItem::TimerSchedule { .. } => "TimerSchedule",
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
    /// Pinned versions for instances started in this runtime (in-memory for now)
    pinned_versions: Mutex<HashMap<String, Version>>,
    /// Track the current execution ID for each active instance
    current_execution_ids: Mutex<HashMap<String, u64>>,
    /// Runtime configuration options
    options: RuntimeOptions,
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
    /// Compute execution metadata from history delta without inspecting event contents.
    /// This allows the runtime to extract semantic information and pass it to the provider
    /// as pre-computed metadata, preventing the provider from needing orchestration knowledge.
    fn compute_execution_metadata(
        history_delta: &[Event],
        _orchestrator_items: &[WorkItem],
        _current_execution_id: u64,
    ) -> ExecutionMetadata {
        let mut metadata = ExecutionMetadata::default();

        // Scan history_delta for terminal events
        for event in history_delta {
            match event {
                Event::OrchestrationCompleted { output, .. } => {
                    metadata.status = Some("Completed".to_string());
                    metadata.output = Some(output.clone());
                    break;
                }
                Event::OrchestrationFailed { error, .. } => {
                    metadata.status = Some("Failed".to_string());
                    metadata.output = Some(error.clone());
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
        self.history_store.latest_execution_id(instance).await.unwrap_or(crate::INITIAL_EXECUTION_ID)
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
        // Install a default subscriber if none set (ok to call many times)
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .try_init();

        let joins: Vec<JoinHandle<()>> = Vec::new();

        // start request queue + worker
        let runtime = Arc::new(Self {
            joins: Mutex::new(joins),
            history_store,

            orchestration_registry,
            pinned_versions: Mutex::new(HashMap::new()),
            current_execution_ids: Mutex::new(HashMap::new()),

            options,
        });

        // background orchestrator dispatcher (extracted from inline poller)
        let handle = runtime.clone().start_orchestration_dispatcher();
        runtime.joins.lock().await.push(handle);

        // background work dispatcher (executes activities)
        let work_handle = runtime.clone().start_work_dispatcher(activity_registry);
        runtime.joins.lock().await.push(work_handle);

        // background timer dispatcher (scaffold); current implementation handled by run_timer_worker
        // kept here as an explicit lifecycle hook for provider-backed timer queue
        let timer_handle = runtime.clone().start_timer_dispatcher();
        runtime.joins.lock().await.push(timer_handle);

        runtime
    }

    fn start_orchestration_dispatcher(self: Arc<Self>) -> JoinHandle<()> {
        // EXECUTION: consumes provider orchestrator items, drives atomic turns
        tokio::spawn(async move {
            loop {
                if let Some(item) = self.history_store.fetch_orchestration_item().await {
                    // Process orchestration item atomically
                    self.process_orchestration_item(item).await;
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(self.options.dispatcher_idle_sleep_ms)).await;
                }
            }
        })
    }

    async fn process_orchestration_item(self: &Arc<Self>, item: crate::providers::OrchestrationItem) {
        // EXECUTION: builds deltas and commits via ack_orchestration_item
        let instance = &item.instance;
        let lock_token = &item.lock_token;

        // Extract metadata from history and work items
        let history_mgr = HistoryManager::from_history(&item.history);
        let workitem_reader = WorkItemReader::from_messages(&item.messages, &history_mgr, instance);

        // Bail on truly terminal histories (Completed/Failed), or ContinuedAsNew without a CAN start message
        if history_mgr.is_completed || history_mgr.is_failed || (history_mgr.is_continued_as_new && !workitem_reader.is_continue_as_new) {
            warn!(instance = %instance, "Instance is terminal (completed/failed or CAN without start), acking batch without processing");
            self.ack_orchestration_with_changes(
                lock_token,
                item.execution_id,
                vec![],
                vec![],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await;
            return;
        }

        // Decide execution id and history to use
        let (execution_id_to_use, history_to_use) = if workitem_reader.is_continue_as_new {
            (item.execution_id + 1, Vec::new())
        } else {
            (item.execution_id, item.history.clone())
        };

        // Log execution start
        if workitem_reader.has_orchestration_name() {
            debug!(
                instance,
                orchestration = %workitem_reader.orchestration_name,
                execution_id = %execution_id_to_use,
                is_continue_as_new = workitem_reader.is_continue_as_new,
                "Starting execution"
            );
        } else if !workitem_reader.completion_messages.is_empty() {
            // Empty orchestration name with completion messages - just warn and skip
            tracing::warn!(instance = %item.instance, "empty effective batch - this should not happen");
        }

        // Process the execution (unified path)
        let (history_delta, worker_items, timer_items, orchestrator_items, execution_id_for_ack) =
            if workitem_reader.has_orchestration_name() {
                let (hd, wi, ti, oi) = self
                    .handle_orchestration_atomic(
                        instance,
                        &workitem_reader,
                        &history_to_use,
                        execution_id_to_use,
                    )
                    .await;
                (hd, wi, ti, oi, execution_id_to_use)
            } else {
                // Empty effective batch
                (vec![], vec![], vec![], vec![], execution_id_to_use)
            };

        // Atomically commit all changes
        debug!(
            instance,
            "Acking orchestration item: history_delta={}, worker={}, timer={}, orch={}",
            history_delta.len(),
            worker_items.len(),
            timer_items.len(),
            orchestrator_items.len()
        );

        // Compute execution metadata from history_delta (runtime responsibility)
        let metadata = Runtime::compute_execution_metadata(&history_delta, &orchestrator_items, item.execution_id);

        // Robust ack with basic retry on any provider error
        self.ack_orchestration_with_changes(
            lock_token,
            execution_id_for_ack,
            history_delta,
            worker_items,
            timer_items,
            orchestrator_items,
            metadata,
        )
        .await;
    }

    // Helper methods for atomic orchestration processing
    async fn handle_orchestration_atomic(
        self: &Arc<Self>,
        instance: &str,
        workitem_reader: &WorkItemReader,
        existing_history: &[Event],
        execution_id: u64,
    ) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>) {
        let mut history_delta = Vec::new();
        let mut worker_items = Vec::new();
        let mut timer_items = Vec::new();
        let mut orchestrator_items = Vec::new();

        // Create started event if this is a new instance
        if existing_history.is_empty() {
            // Resolve version using registry policy if not explicitly provided
            let (resolved_version, should_pin) = if let Some(v) = &workitem_reader.version {
                (v.to_string(), true)
            } else if let Some((v, _handler)) = self.orchestration_registry.resolve_for_start(&workitem_reader.orchestration_name).await {
                (v.to_string(), true)
            } else {
                // Unregistered orchestration - fail immediately but still create proper history
                history_delta.push(Event::OrchestrationStarted {
                    event_id: crate::INITIAL_EVENT_ID,
                    name: workitem_reader.orchestration_name.clone(),
                    version: "0.0.0".to_string(), // Placeholder version for unregistered
                    input: workitem_reader.input.clone(),
                    parent_instance: workitem_reader.parent_instance.clone(),
                    parent_id: workitem_reader.parent_id,
                });

                let terminal_event = Event::OrchestrationFailed {
                    event_id: crate::INITIAL_EVENT_ID + 1,
                    error: format!("unregistered:{}", &workitem_reader.orchestration_name),
                };
                history_delta.push(terminal_event);
                return (history_delta, worker_items, timer_items, orchestrator_items);
            };

            // Pin the resolved version only if we found a valid orchestration
            if should_pin {
                if let Ok(v) = semver::Version::parse(&resolved_version) {
                    self.pinned_versions.lock().await.insert(instance.to_string(), v);
                }
            }

            history_delta.push(Event::OrchestrationStarted {
                event_id: 1, // First event always has event_id=1
                name: workitem_reader.orchestration_name.clone(),
                version: resolved_version,
                input: workitem_reader.input.clone(),
                parent_instance: workitem_reader.parent_instance.clone(),
                parent_id: workitem_reader.parent_id,
            });
        }

        // Run the atomic execution to get all changes
        let full_history = [existing_history, &history_delta].concat();
        
        // Create a temporary history manager for the full history (existing + new OrchestrationStarted)
        let temp_history_mgr = crate::runtime::state_helpers::HistoryManager::from_history(&full_history);
        
        let (exec_history_delta, exec_worker_items, exec_timer_items, exec_orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(
                instance,
                &temp_history_mgr,
                workitem_reader,
                full_history.clone(),
                execution_id,
            )
            .await;

        // Combine all changes
        history_delta.extend(exec_history_delta);
        worker_items.extend(exec_worker_items);
        timer_items.extend(exec_timer_items);
        orchestrator_items.extend(exec_orchestrator_items);

        (history_delta, worker_items, timer_items, orchestrator_items)
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
                    debug!("{} succeeded", operation_tag);
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
        timer_items: Vec<WorkItem>,
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
                        timer_items.clone(),
                        orchestrator_items.clone(),
                        metadata.clone(),
                    )
                    .await
            },
            "ack_orchestration_item",
            Some(|| {
                let _ = self.history_store.abandon_orchestration_item(lock_token, Some(50));
            }),
        )
        .await;
    }

    fn start_work_dispatcher(self: Arc<Self>, activities: Arc<registry::ActivityRegistry>) -> JoinHandle<()> {
        // EXECUTION: consumes worker queue, executes activities, enqueues completions
        tokio::spawn(async move {
            loop {
                if let Some((item, token)) = self.history_store.dequeue_worker_peek_lock().await {
                    match item {
                        WorkItem::ActivityExecute {
                            instance,
                            execution_id,
                            id,
                            name,
                            input,
                        } => {
                            // Execute activity via registry directly; enqueue completion/failure to orchestrator queue
                            let enqueue_result = if let Some(handler) = activities.get(&name) {
                                match handler.invoke(input).await {
                                    Ok(result) => {
                                        self.history_store
                                            .enqueue_orchestrator_work(
                                                WorkItem::ActivityCompleted {
                                                    instance: instance.clone(),
                                                    execution_id,
                                                    id,
                                                    result,
                                                },
                                                None,
                                            )
                                            .await
                                    }
                                    Err(error) => {
                                        self.history_store
                                            .enqueue_orchestrator_work(
                                                WorkItem::ActivityFailed {
                                                    instance: instance.clone(),
                                                    execution_id,
                                                    id,
                                                    error,
                                                },
                                                None,
                                            )
                                            .await
                                    }
                                }
                            } else {
                                self.history_store
                                    .enqueue_orchestrator_work(
                                        WorkItem::ActivityFailed {
                                            instance: instance.clone(),
                                            execution_id,
                                            id,
                                            error: format!("unregistered:{}", name),
                                        },
                                        None,
                                    )
                                    .await
                            };

                            // Only acknowledge after successful enqueue
                            if enqueue_result.is_ok() {
                                let _ = self.history_store.ack_worker(&token).await;
                            } else {
                                warn!(instance = %instance, execution_id, id, "worker: enqueue to orchestrator failed; not acking");
                            }
                        }
                        other => {
                            error!(?other, "unexpected WorkItem in Worker dispatcher; state corruption");
                            panic!("unexpected WorkItem in Worker dispatcher");
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(self.options.dispatcher_idle_sleep_ms)).await;
                }
            }
        })
    }

    fn start_timer_dispatcher(self: Arc<Self>) -> JoinHandle<()> {
        // EXECUTION: consumes timer queue; uses provider delayed visibility or in-proc timer
        if self.history_store.supports_delayed_visibility() {
            // For providers with delayed visibility support, we still use the timer queue
            // but leverage delayed visibility when enqueuing TimerFired
            return tokio::spawn(async move {
                loop {
                    if let Some((item, token)) = self.history_store.dequeue_timer_peek_lock().await {
                        match item {
                            WorkItem::TimerSchedule {
                                instance,
                                execution_id,
                                id,
                                fire_at_ms,
                            } => {
                                // Calculate delay from now
                                let current_time_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as u64;
                                let delay_ms = fire_at_ms.saturating_sub(current_time_ms);

                                // Enqueue TimerFired with visibility delay
                                let timer_fired = WorkItem::TimerFired {
                                    instance,
                                    execution_id,
                                    id,
                                    fire_at_ms,
                                };

                                // Use the provider's delayed visibility support
                                let _ = self
                                    .history_store
                                    .enqueue_orchestrator_work(timer_fired, Some(delay_ms))
                                    .await;

                                let _ = self.history_store.ack_timer(&token).await;
                            }
                            other => {
                                error!(?other, "unexpected WorkItem in Timer dispatcher; state corruption");
                                panic!("unexpected WorkItem in Timer dispatcher");
                            }
                        }
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(self.options.dispatcher_idle_sleep_ms))
                            .await;
                    }
                }
            });
        }

        // Fallback in-process timer service (refactored)
        let idle_sleep_ms = self.options.dispatcher_idle_sleep_ms;
        tokio::spawn(async move {
            let (svc_jh, svc_tx) =
                crate::runtime::timers::TimerService::start(self.history_store.clone(), idle_sleep_ms);

            // Intake task: keep pulling schedules and forwarding to service
            // The timer service will handle acknowledgment after firing
            let intake_rt = self.clone();
            let intake_tx = svc_tx.clone();
            tokio::spawn(async move {
                loop {
                    if let Some((item, token)) = intake_rt.history_store.dequeue_timer_peek_lock().await {
                        match item {
                            WorkItem::TimerSchedule {
                                instance,
                                execution_id,
                                id,
                                fire_at_ms,
                            } => {
                                // Send to timer service WITH the ack token
                                let timer_with_token = crate::runtime::timers::TimerWithToken {
                                    item: WorkItem::TimerSchedule {
                                        instance,
                                        execution_id,
                                        id,
                                        fire_at_ms,
                                    },
                                    ack_token: token,
                                };
                                let _ = intake_tx.send(timer_with_token);
                                // Do NOT ack here - the timer service will ack after firing
                            }
                            other => {
                                error!(?other, "unexpected WorkItem in Timer dispatcher; state corruption");
                                panic!("unexpected WorkItem in Timer dispatcher");
                            }
                        }
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(idle_sleep_ms)).await;
                    }
                }
            });
            // Keep service join handle alive within dispatcher lifetime
            let _ = svc_jh.await;
        })
    }

    /// Abort background tasks.
    pub async fn shutdown(self: Arc<Self>) {
        // Abort background dispatcher tasks
        let mut joins = self.joins.lock().await;
        for j in joins.drain(..) {
            j.abort();
        }
    }
}
