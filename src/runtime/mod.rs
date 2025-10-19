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
use async_trait::async_trait;

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


// Message types for orchestration completion handling
/// Completion messages delivered to active orchestration instances.
/// These correspond directly to WorkItem completion variants but include ack tokens.
#[derive(Debug, Clone)]
pub enum OrchestratorMsg {
    ActivityCompleted {
        instance: String,
        execution_id: u64,
        id: u64,
        result: String,
        ack_token: Option<String>,
    },
    ActivityFailed {
        instance: String,
        execution_id: u64,
        id: u64,
        error: String,
        ack_token: Option<String>,
    },
    TimerFired {
        instance: String,
        execution_id: u64,
        id: u64,
        fire_at_ms: u64,
        ack_token: Option<String>,
    },
    ExternalByName {
        instance: String,
        name: String,
        data: String,
        ack_token: Option<String>,
    },
    SubOrchCompleted {
        instance: String,
        execution_id: u64,
        id: u64,
        result: String,
        ack_token: Option<String>,
    },
    SubOrchFailed {
        instance: String,
        execution_id: u64,
        id: u64,
        error: String,
        ack_token: Option<String>,
    },
    CancelRequested {
        instance: String,
        reason: String,
        ack_token: Option<String>,
    },
}

/// Convert a WorkItem completion to an OrchestratorMsg with ack token
impl OrchestratorMsg {
    pub fn from_work_item(work_item: WorkItem, ack_token: Option<String>) -> Option<Self> {
        match work_item {
            WorkItem::ActivityCompleted {
                instance,
                execution_id,
                id,
                result,
            } => Some(OrchestratorMsg::ActivityCompleted {
                instance,
                execution_id,
                id,
                result,
                ack_token,
            }),
            WorkItem::ActivityFailed {
                instance,
                execution_id,
                id,
                error,
            } => Some(OrchestratorMsg::ActivityFailed {
                instance,
                execution_id,
                id,
                error,
                ack_token,
            }),
            WorkItem::TimerFired {
                instance,
                execution_id,
                id,
                fire_at_ms,
            } => Some(OrchestratorMsg::TimerFired {
                instance,
                execution_id,
                id,
                fire_at_ms,
                ack_token,
            }),
            WorkItem::ExternalRaised { instance, name, data } => Some(OrchestratorMsg::ExternalByName {
                instance,
                name,
                data,
                ack_token,
            }),
            WorkItem::SubOrchCompleted {
                parent_instance,
                parent_execution_id,
                parent_id,
                result,
            } => Some(OrchestratorMsg::SubOrchCompleted {
                instance: parent_instance,
                execution_id: parent_execution_id,
                id: parent_id,
                result,
                ack_token,
            }),
            WorkItem::SubOrchFailed {
                parent_instance,
                parent_execution_id,
                parent_id,
                error,
            } => Some(OrchestratorMsg::SubOrchFailed {
                instance: parent_instance,
                execution_id: parent_execution_id,
                id: parent_id,
                error,
                ack_token,
            }),
            WorkItem::CancelInstance { instance, reason } => Some(OrchestratorMsg::CancelRequested {
                instance,
                reason,
                ack_token,
            }),
            // Non-completion WorkItems don't convert to OrchestratorMsg
            WorkItem::StartOrchestration { .. }
            | WorkItem::ActivityExecute { .. }
            | WorkItem::TimerSchedule { .. }
            | WorkItem::ContinueAsNew { .. } => None,
        }
    }
}

pub fn kind_of(msg: &OrchestratorMsg) -> &'static str {
    match msg {
        OrchestratorMsg::ActivityCompleted { .. } => "ActivityCompleted",
        OrchestratorMsg::ActivityFailed { .. } => "ActivityFailed",
        OrchestratorMsg::TimerFired { .. } => "TimerFired",
        OrchestratorMsg::ExternalByName { .. } => "ExternalByName",
        OrchestratorMsg::SubOrchCompleted { .. } => "SubOrchCompleted",
        OrchestratorMsg::SubOrchFailed { .. } => "SubOrchFailed",
        OrchestratorMsg::CancelRequested { .. } => "CancelRequested",
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
        self.history_store.latest_execution_id(instance).await.unwrap_or(1)
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

        // Determine terminal state from history and whether batch contains CAN
        let mut has_completed = false;
        let mut has_failed = false;
        let mut has_continued_as_new = false;
        for e in item.history.iter().rev() {
            match e {
                Event::OrchestrationCompleted { .. } => {
                    has_completed = true;
                    break;
                }
                Event::OrchestrationFailed { .. } => {
                    has_failed = true;
                    break;
                }
                Event::OrchestrationContinuedAsNew { .. } => {
                    has_continued_as_new = true;
                    break;
                }
                _ => {}
            }
        }
        let has_can_msg = item
            .messages
            .iter()
            .any(|w| matches!(w, WorkItem::ContinueAsNew { .. }));

        // Bail on truly terminal histories (Completed/Failed), or ContinuedAsNew without a CAN start message
        if has_completed || has_failed || (has_continued_as_new && !has_can_msg) {
            warn!(instance = %instance, "Instance is terminal (completed/failed or CAN without start), acking batch without processing");
            let _ = self
                .history_store
                .ack_orchestration_item(
                    lock_token,
                    item.execution_id,
                    vec![], // No history changes
                    vec![], // No worker items
                    vec![], // No timer items
                    vec![], // No orchestrator items
                    ExecutionMetadata::default(),
                )
                .await;
            return;
        }

        // Separate Start/ContinueAsNew from completion messages
        let mut start_or_continue: Option<WorkItem> = None;
        let mut completion_messages: Vec<OrchestratorMsg> = Vec::new();

        for work_item in &item.messages {
            match work_item {
                WorkItem::StartOrchestration { .. } | WorkItem::ContinueAsNew { .. } => {
                    if start_or_continue.is_some() {
                        // Handle duplicate start gracefully - just warn and ignore
                        warn!(instance, "Duplicate Start/ContinueAsNew in batch - ignoring duplicate");
                        continue;
                    }
                    start_or_continue = Some(work_item.clone());
                }
                _ => {
                    // Convert to OrchestratorMsg with shared token
                    if let Some(msg) = OrchestratorMsg::from_work_item(work_item.clone(), Some(lock_token.clone())) {
                        completion_messages.push(msg);
                    }
                }
            }
        }

        // Process the execution
        let (history_delta, worker_items, timer_items, orchestrator_items, execution_id_for_ack) =
            if let Some(start_item) = start_or_continue {
                // Both StartOrchestration and ContinueAsNew are handled identically.
                // For ContinueAsNew, we start a fresh execution with empty history and execution_id = current + 1.
                let (orchestration, input, version, parent_instance, parent_id, is_can) = match &start_item {
                    WorkItem::StartOrchestration {
                        orchestration,
                        input,
                        version,
                        parent_instance,
                        parent_id,
                        ..
                    } => (
                        orchestration.as_str(),
                        input.as_str(),
                        version.as_deref(),
                        parent_instance.as_deref(),
                        *parent_id,
                        false,
                    ),
                    WorkItem::ContinueAsNew {
                        orchestration,
                        input,
                        version,
                        ..
                    } => (
                        orchestration.as_str(),
                        input.as_str(),
                        version.as_deref(),
                        None,
                        None,
                        true,
                    ),
                    _ => unreachable!(),
                };

                // Decide execution id and history to use
                let (execution_id_to_use, history_to_use) = if is_can {
                    (item.execution_id + 1, Vec::new())
                } else {
                    (item.execution_id, item.history.clone())
                };

                debug!(
                    instance,
                    orchestration = %orchestration,
                    execution_id = %execution_id_to_use,
                    is_continue_as_new = is_can,
                    "Starting execution"
                );

                let (hd, wi, ti, oi) = self
                    .handle_start_orchestration_atomic(
                        instance,
                        orchestration,
                        input,
                        version,
                        parent_instance,
                        parent_id,
                        completion_messages,
                        &history_to_use,
                        execution_id_to_use,
                        lock_token,
                    )
                    .await;

                (hd, wi, ti, oi, execution_id_to_use)
            } else if !completion_messages.is_empty() {
                // Only completion messages - process them
                let (hd, wi, ti, oi) = self
                    .handle_completion_batch_atomic(
                        instance,
                        completion_messages,
                        &item.history,
                        item.execution_id,
                        lock_token,
                    )
                    .await;
                (hd, wi, ti, oi, item.execution_id)
            } else {
                // Empty effective batch - just ack
                (vec![], vec![], vec![], vec![], item.execution_id)
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
        let mut attempts: u32 = 0;
        let max_attempts: u32 = 5;
        loop {
            match self
                .history_store
                .ack_orchestration_item(
                    lock_token,
                    execution_id_for_ack,
                    history_delta.clone(),
                    worker_items.clone(),
                    timer_items.clone(),
                    orchestrator_items.clone(),
                    metadata.clone(),
                )
                .await
            {
                Ok(()) => {
                    debug!(instance, "Successfully acked orchestration item");
                    break;
                }
                Err(e) => {
                    if attempts < max_attempts {
                        let backoff_ms = 10u64.saturating_mul(1 << attempts);
                        warn!(instance, attempts, backoff_ms, error = %e, "Ack failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        attempts += 1;
                        continue;
                    } else {
                        warn!(instance, attempts, error = %e, "Failed to ack orchestration item");
                        // Best-effort: abandon so it can be retried later
                        let _ = self
                            .history_store
                            .abandon_orchestration_item(lock_token, Some(50))
                            .await;
                        break;
                    }
                }
            }
        }
    }

    // Helper methods for atomic orchestration processing
    async fn handle_start_orchestration_atomic(
        self: &Arc<Self>,
        instance: &str,
        orchestration: &str,
        input: &str,
        version: Option<&str>,
        parent_instance: Option<&str>,
        parent_id: Option<u64>,
        completion_messages: Vec<OrchestratorMsg>,
        existing_history: &[Event],
        execution_id: u64,
        _lock_token: &str,
    ) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>) {
        let mut history_delta = Vec::new();
        let mut worker_items = Vec::new();
        let mut timer_items = Vec::new();
        let mut orchestrator_items = Vec::new();

        // Create started event if this is a new instance
        if existing_history.is_empty() {
            // Resolve version using registry policy if not explicitly provided
            let (resolved_version, should_pin) = if let Some(v) = version {
                (v.to_string(), true)
            } else if let Some((v, _handler)) = self.orchestration_registry.resolve_for_start(orchestration).await {
                (v.to_string(), true)
            } else {
                // Use 0.0.0 for unregistered orchestrations (won't be pinned)
                ("0.0.0".to_string(), false)
            };

            // Pin the resolved version only if we found a valid orchestration
            if should_pin {
                if let Ok(v) = semver::Version::parse(&resolved_version) {
                    self.pinned_versions.lock().await.insert(instance.to_string(), v);
                }
            }

            history_delta.push(Event::OrchestrationStarted {
                event_id: 1, // First event always has event_id=1
                name: orchestration.to_string(),
                version: resolved_version,
                input: input.to_string(),
                parent_instance: parent_instance.map(|s| s.to_string()),
                parent_id,
            });
        }

        // Run the atomic execution to get all changes
        let full_history = [existing_history, &history_delta].concat();
        let (exec_history_delta, exec_worker_items, exec_timer_items, exec_orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(
                instance,
                orchestration,
                full_history.clone(),
                execution_id,
                completion_messages,
            )
            .await;

        // Combine all changes
        history_delta.extend(exec_history_delta);
        worker_items.extend(exec_worker_items);
        timer_items.extend(exec_timer_items);
        orchestrator_items.extend(exec_orchestrator_items);

        (history_delta, worker_items, timer_items, orchestrator_items)
    }

    async fn handle_completion_batch_atomic(
        self: &Arc<Self>,
        instance: &str,
        completion_messages: Vec<OrchestratorMsg>,
        existing_history: &[Event],
        execution_id: u64,
        _lock_token: &str,
    ) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>) {
        // If the instance has no history yet, it hasn't been started. Completion-only
        // messages (e.g., external events) may arrive due to races. Drop them safely.
        if existing_history.is_empty() {
            // Double-check latest persisted history to avoid racing with a just-started instance.
            let latest = self.history_store.read(instance).await;
            if latest.is_empty() {
                let kinds: Vec<&'static str> = completion_messages
                    .iter()
                    .map(|m| kind_of(m))
                    .collect();
                warn!(
                    instance,
                    count = completion_messages.len(),
                    kinds = ?kinds,
                    "dropping completion-only batch for unstarted instance"
                );
                return (vec![], vec![], vec![], vec![]);
            } else {
                debug!(
                    instance,
                    "completion-only batch: detected newly-started instance after re-read"
                );
                // Use the latest history instead of the empty snapshot
                let initial_history = latest;
                let (history_delta, worker_items, timer_items, orchestrator_items, _result) = self
                    .clone()
                    .run_single_execution_atomic(
                        instance,
                        &initial_history
                            .iter()
                            .rev()
                            .find_map(|e| match e {
                                Event::OrchestrationStarted { name, .. } => Some(name.clone()),
                                _ => None,
                            })
                            .unwrap_or_else(|| "Unknown".to_string()),
                        initial_history.clone(),
                        execution_id,
                        completion_messages,
                    )
                    .await;
                return (history_delta, worker_items, timer_items, orchestrator_items);
            }
        }

        // Extract orchestration name from history
        let orchestration_name = existing_history
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::OrchestrationStarted { name, .. } => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "Unknown".to_string());

        let initial_history = existing_history.to_vec();
        let (history_delta, worker_items, timer_items, orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(
                instance,
                &orchestration_name,
                initial_history.clone(),
                execution_id,
                completion_messages,
            )
            .await;

        (history_delta, worker_items, timer_items, orchestrator_items)
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

    /// Abort background tasks. Channels are dropped with the runtime.
    pub async fn shutdown(self: Arc<Self>) {
        // Abort background tasks; channels will be dropped with Runtime
        let mut joins = self.joins.lock().await;
        for j in joins.drain(..) {
            j.abort();
        }
    }

}
