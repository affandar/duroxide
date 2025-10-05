//
use crate::providers::{Provider, WorkItem};
use crate::{Event, OrchestrationContext};
use semver::Version;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

pub mod dispatch;
pub mod registry;
pub mod router;
mod timers;
use async_trait::async_trait;

// CompletionMap removed - replaced with unified cursor model
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

// Legacy VersionedOrchestrationRegistry removed; use OrchestrationRegistry instead

// ActivityWorkItem removed; activities are executed by WorkDispatcher via provider queues

// TimerWorkItem no longer used; timers flow via provider-backed queues

pub use router::{InstanceRouter, OrchestratorMsg};

/// In-process runtime that executes activities and timers and persists
/// history via a `Provider`.
pub struct Runtime {
    // removed: in-proc activity channel and router
    joins: Mutex<Vec<JoinHandle<()>>>,
    // instance_joins removed with spawn_instance_to_completion
    history_store: Arc<dyn Provider>,

    // pending_starts removed
    // result_waiters removed - using polling approach instead
    orchestration_registry: OrchestrationRegistry,
    // Pinned versions for instances started in this runtime (in-memory for now)
    pinned_versions: Mutex<HashMap<String, Version>>,
    /// Track the current execution ID for each active instance
    current_execution_ids: Mutex<HashMap<String, u64>>,
    // StartRequest layer removed; instances are activated directly
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
    // Execution engine: consumes provider queues and persists history atomically.
    /// Internal: apply pure decisions by appending necessary history and dispatching work.
    async fn apply_decisions(self: &Arc<Self>, instance: &str, history: &Vec<Event>, decisions: Vec<crate::Action>) {
        debug!("apply_decisions: {instance} {decisions:#?}");
        for d in decisions {
            match d {
                crate::Action::ContinueAsNew { .. } => { /* handled by caller */ }
                crate::Action::CallActivity { scheduling_event_id, name, input } => {
                    dispatch::dispatch_call_activity(self, instance, history, scheduling_event_id, name, input).await;
                }
                crate::Action::CreateTimer { scheduling_event_id, delay_ms } => {
                    dispatch::dispatch_create_timer(self, instance, history, scheduling_event_id, delay_ms).await;
                }
                crate::Action::WaitExternal { scheduling_event_id, name } => {
                    dispatch::dispatch_wait_external(self, instance, history, scheduling_event_id, name).await;
                }
                crate::Action::StartOrchestrationDetached {
                    scheduling_event_id,
                    name,
                    version,
                    instance: child_inst,
                    input,
                } => {
                    dispatch::dispatch_start_detached(self, instance, scheduling_event_id, name, version, child_inst, input).await;
                }
                crate::Action::StartSubOrchestration {
                    scheduling_event_id,
                    name,
                    version,
                    instance: child_suffix,
                    input,
                } => {
                    dispatch::dispatch_start_sub_orchestration(
                        self,
                        instance,
                        history,
                        scheduling_event_id,
                        name,
                        version,
                        child_suffix,
                        input,
                    )
                    .await;
                }
            }
        }
    }
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
    // Associated constants for runtime behavior
    const POLLER_IDLE_SLEEP_MS: u64 = 10;

    // ensure_instance_active is no longer needed in the direct execution model
    // Each completion message triggers a direct one-shot execution

    /// Start an orchestration and ensure the instance is active.
    /// Callers should use wait_for_orchestration to wait for completion.
    async fn start_internal_wait(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        input: String,
        pin_version: Option<Version>,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    ) -> Result<(), String> {
        // Just queue the StartOrchestration work item - let the engine handle duplicates
        let start_work_item = WorkItem::StartOrchestration {
            instance: instance.to_string(),
            orchestration: orchestration_name.to_string(),
            input,
            version: pin_version.map(|v| v.to_string()),
            parent_instance,
            parent_id,
        };

        if let Err(e) = self.history_store.enqueue_orchestrator_work(start_work_item, None).await {
            return Err(format!("failed to enqueue StartOrchestration work item: {}", e));
        }

        Ok(())
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

    /// Internal: start an orchestration and record parent linkage.
    pub(crate) async fn start_orchestration_with_parent(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        input: impl Into<String>,
        parent_instance: String,
        parent_id: u64,
        version: Option<semver::Version>,
    ) -> Result<(), String> {
        self.clone()
            .start_internal_wait(
                instance,
                orchestration_name,
                input.into(),
                version,
                Some(parent_instance),
                Some(parent_id),
            )
            .await
    }

    
    /// Start a new runtime using the in-memory SQLite provider.
    pub async fn start(
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
    ) -> Arc<Self> {
        let history_store: Arc<dyn Provider> = Arc::new(crate::providers::sqlite::SqliteProvider::new_in_memory().await.unwrap());
        Self::start_with_store(history_store, activity_registry, orchestration_registry).await
    }

    /// Start a new runtime with a custom `Provider` implementation.
    pub async fn start_with_store(
        history_store: Arc<dyn Provider>,
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
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
                    tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
                }
            }
        })
    }

    async fn process_orchestration_item(self: &Arc<Self>, item: crate::providers::OrchestrationItem) {
        // EXECUTION: builds deltas and commits via ack_orchestration_item
        let instance = &item.instance;
        let lock_token = &item.lock_token;

        debug!(
            instance,
            "Processing orchestration item: messages={}, history_len={}, first_msg={:?}",
            item.messages.len(),
            item.history.len(),
            item.messages.first().map(|m| std::mem::discriminant(m))
        );

        // Check if instance is already terminal
        let is_terminal = item.history.iter().rev().any(|e| {
            matches!(
                e,
                Event::OrchestrationCompleted { .. }
                    | Event::OrchestrationFailed { .. }
                    | Event::OrchestrationContinuedAsNew { .. }
            )
        });

        if is_terminal {
            warn!(instance = %instance, "Instance is terminal, acking batch without processing");
            let _ = self
                .history_store
                .ack_orchestration_item(
                    lock_token,
                    vec![], // No history changes
                    vec![], // No worker items
                    vec![], // No timer items
                    vec![], // No orchestrator items
                )
                .await;
            return;
        }

        // Separate Start/ContinueAsNew from completion messages
        let mut start_or_continue: Option<WorkItem> = None;
        let mut completion_messages: Vec<OrchestratorMsg> = Vec::new();

        debug!(instance, "Processing {} work items", item.messages.len());
        for work_item in &item.messages {
            debug!(instance, "Work item: {:?}", work_item);
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
        let (history_delta, worker_items, timer_items, orchestrator_items) = if let Some(start_item) = start_or_continue
        {
            match start_item {
                WorkItem::StartOrchestration {
                    orchestration,
                    input,
                    version,
                    parent_instance,
                    parent_id,
                    ..
                } => {
                    self.handle_start_orchestration_atomic(
                        instance,
                        &orchestration,
                        &input,
                        version.as_deref(),
                        parent_instance.as_deref(),
                        parent_id,
                        completion_messages,
                        &item.history,
                        item.execution_id,
                        lock_token,
                    )
                    .await
                }
                WorkItem::ContinueAsNew {
                    orchestration,
                    input,
                    version,
                    ..
                } => {
                    debug!(instance, "Handling ContinueAsNew with input: {}", input);
                    self.handle_continue_as_new_atomic(
                        instance,
                        &orchestration,
                        &input,
                        version.as_deref(),
                        completion_messages,
                        &item.history,
                        item.execution_id,
                        lock_token,
                    )
                    .await
                }
                _ => unreachable!(),
            }
        } else if !completion_messages.is_empty() {
            // Only completion messages - process them
            self.handle_completion_batch_atomic(
                instance,
                completion_messages,
                &item.history,
                item.execution_id,
                lock_token,
            )
            .await
        } else {
            // Empty effective batch - just ack
            (vec![], vec![], vec![], vec![])
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
        
        if !worker_items.is_empty() {
            debug!(instance, "Worker items to enqueue: {:?}", worker_items);
        }
        match self
            .history_store
            .ack_orchestration_item(lock_token, history_delta, worker_items, timer_items, orchestrator_items)
            .await
        {
            Ok(()) => debug!(instance, "Successfully acked orchestration item"),
            Err(e) => warn!(instance, error = %e, "Failed to ack orchestration item"),
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
                event_id: 1,  // First event always has event_id=1
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

    async fn handle_continue_as_new_atomic(
        self: &Arc<Self>,
        instance: &str,
        orchestration: &str,
        input: &str,
        version: Option<&str>,
        completion_messages: Vec<OrchestratorMsg>,
        _existing_history: &[Event],
        execution_id: u64,
        _lock_token: &str,
    ) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>) {
        let mut history_delta = Vec::new();
        let mut worker_items = Vec::new();
        let mut timer_items = Vec::new();
        let mut orchestrator_items = Vec::new();

        // For ContinueAsNew, we're starting a new execution
        // Resolve version using registry policy if not explicitly provided
        let (resolved_version, should_pin) = if let Some(v) = version {
            (v.to_string(), true)
        } else if let Some((v, _handler)) = self.orchestration_registry.resolve_for_start(orchestration).await {
            (v.to_string(), true)
        } else {
            // Use 0.0.0 for unregistered orchestrations (won't be pinned)
            ("0.0.0".to_string(), false)
        };

        // Pin the resolved version for the new execution only if we found a valid orchestration
        if should_pin {
            if let Ok(v) = semver::Version::parse(&resolved_version) {
                self.pinned_versions.lock().await.insert(instance.to_string(), v);
            }
        }

        // Add the OrchestrationStarted event for the new execution
        history_delta.push(Event::OrchestrationStarted {
            event_id: 1,  // First event of new execution always has event_id=1
            name: orchestration.to_string(),
            version: resolved_version,
            input: input.to_string(),
            parent_instance: None,
            parent_id: None,
        });

        debug!(instance, "ContinueAsNew creating new execution with input: {}", input);
        debug!(instance, "Initial history for new execution: {:?}", history_delta);
        debug!(instance, "First event in history_delta: {:?}", history_delta.first());

        // Run the new execution with just the OrchestrationStarted event
        let initial_history = history_delta.clone();
        let (exec_history_delta, exec_worker_items, exec_timer_items, exec_orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(
                instance,
                orchestration,
                initial_history.clone(),
                execution_id, // Use current execution ID for now
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
                    .map(|m| crate::runtime::router::kind_of(m))
                    .collect();
                warn!(
                    instance,
                    count = completion_messages.len(),
                    kinds = ?kinds,
                    "dropping completion-only batch for unstarted instance"
                );
                return (vec![], vec![], vec![], vec![]);
            } else {
                debug!(instance, "completion-only batch: detected newly-started instance after re-read");
                // Use the latest history instead of the empty snapshot
                let initial_history = latest;
                let (history_delta, worker_items, timer_items, orchestrator_items, _result) = self
                    .clone()
                    .run_single_execution_atomic(
                        instance,
                        &initial_history
                            .iter()
                            .rev()
                            .find_map(|e| match e { Event::OrchestrationStarted { name, .. } => Some(name.clone()), _ => None })
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
                            debug!(instance = %instance, execution_id, id, name = %name, "worker: dequeued ActivityExecute");
                            // Execute activity via registry directly; enqueue completion/failure to orchestrator queue
                            let enqueue_result = if let Some(handler) = activities.get(&name) {
                                match handler.invoke(input).await {
                                    Ok(result) => {
                                        debug!(instance = %instance, execution_id, id, "worker: activity succeeded, enqueue completion");
                                        self.history_store
                                            .enqueue_orchestrator_work(WorkItem::ActivityCompleted {
                                                instance: instance.clone(),
                                                execution_id,
                                                id,
                                                result,
                                            }, None)
                                            .await
                                    }
                                    Err(error) => {
                                        debug!(instance = %instance, execution_id, id, error = %error, "worker: activity failed, enqueue failure");
                                        self.history_store
                                            .enqueue_orchestrator_work(WorkItem::ActivityFailed {
                                                instance: instance.clone(),
                                                execution_id,
                                                id,
                                                error,
                                            }, None)
                                            .await
                                    }
                                }
                            } else {
                                debug!(instance = %instance, execution_id, id, name = %name, "worker: unregistered activity, enqueue failure");
                                self.history_store
                                    .enqueue_orchestrator_work(WorkItem::ActivityFailed {
                                        instance: instance.clone(),
                                        execution_id,
                                        id,
                                        error: format!("unregistered:{}", name),
                                    }, None)
                                    .await
                            };
                            
                            // Only acknowledge after successful enqueue
                            if enqueue_result.is_ok() {
                                debug!(instance = %instance, execution_id, id, "worker: acking worker lock");
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
                    tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
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
                                let _ = self.history_store.enqueue_orchestrator_work(timer_fired, Some(delay_ms)).await;
                                
                                let _ = self.history_store.ack_timer(&token).await;
                            }
                            other => {
                                error!(?other, "unexpected WorkItem in Timer dispatcher; state corruption");
                                panic!("unexpected WorkItem in Timer dispatcher");
                            }
                        }
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
                    }
                }
            });
        }

        // Fallback in-process timer service (refactored)
        tokio::spawn(async move {
            let (svc_jh, svc_tx) =
                crate::runtime::timers::TimerService::start(self.history_store.clone(), Self::POLLER_IDLE_SLEEP_MS);

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
                        tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
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

    // drain_instances removed with spawn_instance_to_completion

    // spawn_instance_to_completion removed; atomic path is the only execution path
}