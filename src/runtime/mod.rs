use crate::_typed_codec::{Codec, Json};
use crate::providers::in_memory::InMemoryHistoryStore;
use crate::providers::{HistoryStore, WorkItem};
use crate::{Event, OrchestrationContext};
use semver::Version;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

pub mod dispatch;
pub mod registry;
pub mod router;
pub mod status;
mod timers;
use async_trait::async_trait;

pub mod completion_aware_futures;
pub mod completion_map;
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
/// history via a `HistoryStore`.
pub struct Runtime {
    // removed: in-proc activity channel and router
    joins: Mutex<Vec<JoinHandle<()>>>,
    instance_joins: Mutex<Vec<JoinHandle<()>>>,
    history_store: Arc<dyn HistoryStore>,

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
    /// Internal: apply pure decisions by appending necessary history and dispatching work.
    async fn apply_decisions(self: &Arc<Self>, instance: &str, history: &Vec<Event>, decisions: Vec<crate::Action>) {
        debug!("apply_decisions: {instance} {decisions:#?}");
        for d in decisions {
            match d {
                crate::Action::ContinueAsNew { .. } => { /* handled by caller */ }
                crate::Action::CallActivity { id, name, input } => {
                    dispatch::dispatch_call_activity(self, instance, history, id, name, input).await;
                }
                crate::Action::CreateTimer { id, delay_ms } => {
                    dispatch::dispatch_create_timer(self, instance, history, id, delay_ms).await;
                }
                crate::Action::WaitExternal { id, name } => {
                    dispatch::dispatch_wait_external(self, instance, history, id, name).await;
                }
                crate::Action::StartOrchestrationDetached {
                    id,
                    name,
                    version,
                    instance: child_inst,
                    input,
                } => {
                    dispatch::dispatch_start_detached(self, instance, id, name, version, child_inst, input).await;
                }
                crate::Action::StartSubOrchestration {
                    id,
                    name,
                    version,
                    instance: child_suffix,
                    input,
                } => {
                    dispatch::dispatch_start_sub_orchestration(
                        self,
                        instance,
                        history,
                        id,
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
        // Just queue the StartOrchestration work item - let start_orchestration_execution handle everything
        let start_work_item = WorkItem::StartOrchestration {
            instance: instance.to_string(),
            orchestration: orchestration_name.to_string(),
            input,
            version: pin_version.map(|v| v.to_string()),
            parent_instance,
            parent_id,
        };

        if let Err(e) = self
            .history_store
            .enqueue_orchestrator_work(start_work_item)
            .await
        {
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

    /// Enqueue a new orchestration instance start. The runtime will pick this up
    /// in the background and drive it to completion.
    /// Start a typed orchestration; input/output are serialized internally.
    /// Use wait_for_orchestration_typed to wait for completion.
    pub async fn start_orchestration_typed<In>(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        input: In,
    ) -> Result<(), String>
    where
        In: Serialize,
    {
        let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
        self.clone()
            .start_internal_wait(instance, orchestration_name, payload, None, None, None)
            .await
    }

    /// Start a typed orchestration with an explicit version (semver string).
    /// Use wait_for_orchestration_typed to wait for completion.
    pub async fn start_orchestration_versioned_typed<In>(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        version: impl AsRef<str>,
        input: In,
    ) -> Result<(), String>
    where
        In: Serialize,
    {
        let v = semver::Version::parse(version.as_ref()).map_err(|e| e.to_string())?;
        let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
        self.clone()
            .start_internal_wait(instance, orchestration_name, payload, Some(v), None, None)
            .await
    }

    /// Start an orchestration using raw String input/output (back-compat API).
    /// Use wait_for_orchestration to wait for completion.
    pub async fn start_orchestration(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        input: impl Into<String>,
    ) -> Result<(), String> {
        self.clone()
            .start_internal_wait(instance, orchestration_name, input.into(), None, None, None)
            .await
    }

    /// Start an orchestration with an explicit version (string I/O).
    /// Use wait_for_orchestration to wait for completion.
    pub async fn start_orchestration_versioned(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        version: impl AsRef<str>,
        input: impl Into<String>,
    ) -> Result<(), String> {
        let v = semver::Version::parse(version.as_ref()).map_err(|e| e.to_string())?;
        self.clone()
            .start_internal_wait(instance, orchestration_name, input.into(), Some(v), None, None)
            .await
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

    // Status helpers implemented in status.rs

    // Status helpers moved to status.rs
    /// Start a new runtime using the in-memory history store.
    pub async fn start(
        activity_registry: Arc<registry::ActivityRegistry>,
        orchestration_registry: OrchestrationRegistry,
    ) -> Arc<Self> {
        let history_store: Arc<dyn HistoryStore> = Arc::new(InMemoryHistoryStore::default());
        Self::start_with_store(history_store, activity_registry, orchestration_registry).await
    }

    /// Start a new runtime with a custom `HistoryStore` implementation.
    pub async fn start_with_store(
        history_store: Arc<dyn HistoryStore>,
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
            instance_joins: Mutex::new(Vec::new()),
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
            let _ = self.history_store.ack_orchestration_item(
                lock_token,
                vec![], // No history changes
                vec![], // No worker items
                vec![], // No timer items
                vec![], // No orchestrator items
            ).await;
            return;
        }

        // Separate Start/ContinueAsNew from completion messages
        let mut start_or_continue: Option<WorkItem> = None;
        let mut completion_messages: Vec<OrchestratorMsg> = Vec::new();

        for work_item in &item.messages {
            match work_item {
                WorkItem::StartOrchestration { .. } | WorkItem::ContinueAsNew { .. } => {
                    if start_or_continue.is_some() {
                        // Corrupted state - terminate orchestration
                        error!("Multiple Start/ContinueAsNew in batch - corrupted state");
                        let _ = self.history_store.abandon_orchestration_item(lock_token, Some(5000)).await;
                        return;
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
        let (history_delta, worker_items, timer_items, orchestrator_items) = if let Some(start_item) = start_or_continue {
            match start_item {
                WorkItem::StartOrchestration { orchestration, input, version, parent_instance, parent_id, .. } => {
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
                    ).await
                }
                WorkItem::ContinueAsNew { orchestration, input, version, .. } => {
                    self.handle_continue_as_new_atomic(
                        instance,
                        &orchestration,
                        &input,
                        version.as_deref(),
                        completion_messages,
                        &item.history,
                        item.execution_id,
                        lock_token,
                    ).await
                }
                _ => unreachable!(),
            }
        } else if !completion_messages.is_empty() {
            // Only completion messages - process them
            self.handle_completion_batch_atomic(instance, completion_messages, &item.history, item.execution_id, lock_token).await
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
        let _ = self.history_store.ack_orchestration_item(
            lock_token,
            history_delta,
            worker_items,
            timer_items,
            orchestrator_items,
        ).await;
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
            .run_single_execution_atomic(instance, orchestration, full_history.clone(), execution_id, completion_messages)
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
            name: orchestration.to_string(),
            version: resolved_version,
            input: input.to_string(),
            parent_instance: None,
            parent_id: None,
        });
        
        debug!(instance, "ContinueAsNew creating new execution with input: {}", input);
        debug!(
            instance,
            "Initial history for new execution: {:?}",
            history_delta
        );

        // Run the new execution with just the OrchestrationStarted event
        let initial_history = history_delta.clone();
        let (exec_history_delta, exec_worker_items, exec_timer_items, exec_orchestrator_items, _result) = self
            .clone()
            .run_single_execution_atomic(instance, orchestration, initial_history.clone(), execution_id, completion_messages)
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
            .run_single_execution_atomic(instance, &orchestration_name, initial_history.clone(), execution_id, completion_messages)
            .await;

        (history_delta, worker_items, timer_items, orchestrator_items)
    }

    // TODO: Remove these old methods once all tests are migrated to the new atomic API
    #[allow(dead_code)]
    async fn process_orchestrator_batch(self: &Arc<Self>, items: Vec<WorkItem>, token: String) {
        // Extract instance from first item (all items in batch are for same instance)
        let instance = match items.first() {
            Some(item) => match item {
                WorkItem::StartOrchestration { instance, .. }
                | WorkItem::ActivityCompleted { instance, .. }
                | WorkItem::ActivityFailed { instance, .. }
                | WorkItem::TimerFired { instance, .. }
                | WorkItem::ExternalRaised { instance, .. }
                | WorkItem::CancelInstance { instance, .. }
                | WorkItem::ContinueAsNew { instance, .. } => instance.clone(),
                WorkItem::SubOrchCompleted { parent_instance, .. }
                | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance.clone(),
                _ => {
                    error!("Empty batch or invalid item");
                    let _ = self.history_store.ack_orchestrator(&token).await;
                    return;
                }
            },
            None => {
                // Empty batch - just ack
                let _ = self.history_store.ack_orchestrator(&token).await;
                return;
            }
        };

        // Check if instance is already terminal
        let history = self.history_store.read(&instance).await;
        let is_terminal = history.iter().rev().any(|e| {
            matches!(
                e,
                Event::OrchestrationCompleted { .. }
                    | Event::OrchestrationFailed { .. }
                    | Event::OrchestrationContinuedAsNew { .. }
            )
        });

        if is_terminal {
            warn!(instance = %instance, "Instance is terminal, acking batch without processing");
            let _ = self.history_store.ack_orchestrator(&token).await;
            return;
        }

        // Separate Start/ContinueAsNew from completion messages
        let mut start_or_continue: Option<WorkItem> = None;
        let mut completion_messages: Vec<OrchestratorMsg> = Vec::new();

        for item in items {
            match &item {
                WorkItem::StartOrchestration { .. } | WorkItem::ContinueAsNew { .. } => {
                    if start_or_continue.is_some() {
                        // TODO : CR : this is a corrupted state, the orchestration must be terminated with the appropriate error instead of retrying 
                        error!("Multiple Start/ContinueAsNew in batch - corrupted state");
                        let _ = self.history_store.abandon_orchestrator(&token).await;
                        return;
                    }
                    start_or_continue = Some(item);
                }
                _ => {
                    // Convert to OrchestratorMsg with shared token
                    if let Some(msg) = OrchestratorMsg::from_work_item(item, Some(token.clone())) {
                        completion_messages.push(msg);
                    }
                }
            }
        }

        // Process Start/ContinueAsNew if present
        if let Some(start_item) = start_or_continue {
            match start_item {
                WorkItem::StartOrchestration {
                    instance,
                    orchestration,
                    input,
                    version,
                                    parent_instance,
                                    parent_id,
                                } => {
                                    debug!(
                        "StartOrchestration: {instance} {orchestration} {input} (version: {:?})",
                        version
                    );
                    self.start_orchestration_execution_with_completions(
                        &instance,
                        &orchestration,
                        input,
                        version,
                                    parent_instance,
                                    parent_id,
                        token,
                        completion_messages,
                    )
                    .await;
                }
                WorkItem::ContinueAsNew {
                    instance,
                    orchestration,
                    input,
                    version,
                                } => {
                                    debug!(
                        "ContinueAsNew: {instance} {orchestration} {input} (version: {:?})",
                        version
                    );
                    self.start_orchestration_execution_with_completions(
                        &instance,
                        &orchestration,
                        input,
                        version,
                        None, // ContinueAsNew doesn't have parent
                        None,
                        token,
                        completion_messages,
                    )
                    .await;
                }
                _ => unreachable!(),
            }
        } else if !completion_messages.is_empty() {
            // Only completion messages - process them
            self.handle_completion_batch(&instance, completion_messages, token).await;
                } else {
            // Empty effective batch - just ack
            let _ = self.history_store.ack_orchestrator(&token).await;
                }
    }

    fn start_work_dispatcher(self: Arc<Self>, activities: Arc<registry::ActivityRegistry>) -> JoinHandle<()> {
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
                            if let Some(handler) = activities.get(&name) {
                                match handler.invoke(input).await {
                                    Ok(result) => {
                                        let _ = self
                                            .history_store
                                                                                .enqueue_orchestrator_work(
                                                WorkItem::ActivityCompleted {
                                                    instance: instance.clone(),
                                                    execution_id,
                                                    id,
                                                    result,
                                                },
                                            )
                                            .await;
                                    }
                                    Err(error) => {
                                        let _ = self
                                            .history_store
                                            .enqueue_orchestrator_work(
                                                WorkItem::ActivityFailed {
                                                    instance: instance.clone(),
                                                    execution_id,
                                                    id,
                                                    error,
                                                },
                                            )
                                            .await;
                                    }
                                }
                            } else {
                                let _ = self
                                    .history_store
                                    .enqueue_orchestrator_work(
                                        WorkItem::ActivityFailed {
                                            instance: instance.clone(),
                                            execution_id,
                                            id,
                                            error: format!("unregistered:{}", name),
                                        },
                                    )
                                    .await;
                            }
                            let _ = self.history_store.ack_worker(&token).await;
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
        if self.history_store.supports_delayed_visibility() {
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
                                // Provider supports delayed visibility: enqueue TimerFired with fire_at_ms and let provider deliver when due
                                let _ = self
                                    .history_store
                                    .enqueue_orchestrator_work(
                                        WorkItem::TimerFired {
                                            instance,
                                            execution_id,
                                            id,
                                            fire_at_ms,
                                        },
                                    )
                                    .await;
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

            // Intake task: keep pulling schedules and forwarding to service, then ack
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
                                let _ = intake_tx.send(WorkItem::TimerSchedule {
                                    instance,
                                    execution_id,
                                    id,
                                    fire_at_ms,
                                });
                                let _ = intake_rt.history_store.ack_timer(&token).await;
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

    /// Await completion of all outstanding spawned orchestration instances.
    pub async fn drain_instances(self: Arc<Self>) {
        let mut joins = self.instance_joins.lock().await;
        while let Some(j) = joins.pop() {
            let _ = j.await;
        }
    }

    /// Spawn an instance and return a handle that resolves to its history
    /// and output when complete.
    pub fn spawn_instance_to_completion(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        initial_history: Vec<Event>,
        completion_messages: Vec<OrchestratorMsg>,
    ) -> JoinHandle<(Vec<Event>, Result<String, String>)> {
        let this_for_task = self.clone();
        let inst = instance.to_string();
        let orch_name = orchestration_name.to_string();

        tokio::spawn(async move {
            this_for_task
                .run_single_execution(&inst, &orch_name, initial_history, completion_messages)
                .await
        })
    }

    /// Unified handler for starting orchestration executions (both new and ContinueAsNew)
    #[allow(dead_code)]
    async fn start_orchestration_execution_with_completions(
        self: &Arc<Self>,
        instance: &str,
        orchestration: &str,
        input: String,
        version: Option<String>,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
        token: String,
        completion_messages: Vec<OrchestratorMsg>,
    ) {
        // Ensure instance exists
        let _ = self.history_store.create_instance(instance).await;
        
        // Check if instance already exists
        let history = self.history_store.read(instance).await;

        if history.is_empty() {
            // Determine version to use
            let (version_to_use, should_pin) = if let Some(v) = version {
                (v, true)
                } else if let Some(v) = self.pinned_versions.lock().await.get(instance).cloned() {
                (v.to_string(), false) // Already pinned
            } else if let Some((v, _handler)) = self.orchestration_registry.resolve_for_start(orchestration).await {
                (v.to_string(), true)
                } else {
                // Don't pin if orchestration doesn't exist
                ("0.0.0".to_string(), false)
            };
            
            // Pin the resolved version only if we found a valid orchestration
            if should_pin {
                if let Ok(v) = semver::Version::parse(&version_to_use) {
                    self.pinned_versions.lock().await.insert(instance.to_string(), v);
                }
            }
            
            // Create initial history with OrchestrationStarted
            let started = vec![Event::OrchestrationStarted {
                name: orchestration.to_string(),
                version: version_to_use,
                input: input.clone(),
                parent_instance,
                parent_id,
            }];

            if let Err(e) = self.history_store.append(instance, started.clone()).await {
                error!(instance, error = %e, "failed to append OrchestrationStarted");
                let _ = self.history_store.abandon_orchestrator(&token).await;
                return;
            }

            let history = started;

            // Run execution with the completion messages
        let (_final_history, result) = self
            .clone()
            .run_single_execution(instance, orchestration, history, completion_messages)
            .await;

        // Log the result
        match &result {
            Ok(output) => {
                debug!(
                    instance,
                        orchestration, output, "Orchestration execution with completions completed successfully"
                );
            }
            Err(error) => {
                    debug!(instance, orchestration, error, "Orchestration execution with completions failed");
                }
            }
            
            // Note: Acknowledgment is handled by OrchestrationTurn after persistence
            // We don't ack here to avoid double-ack
        } else {
            // Instance already started - run with the completion messages
            let (_final_history, result) = self
                .clone()
                .run_single_execution(instance, orchestration, history, completion_messages)
                .await;

            match &result {
                Ok(output) => {
                    debug!(
                        instance,
                        orchestration, output, "Completion batch for existing instance completed"
                    );
                }
                Err(error) => {
                    debug!(instance, orchestration, error, "Completion batch for existing instance failed");
                }
            }
        }
    }



    // TODO : CR : this function is very similar to the above function, maybe refactor it.
    #[allow(dead_code)]
    async fn handle_completion_batch(self: &Arc<Self>, instance: &str, completion_messages: Vec<OrchestratorMsg>, token: String) {
            // Get orchestration name and check if already completed
        let history = self.history_store.read(instance).await;

        // TODO : CR : I don't think is_completed can ever be true here because we already checked it in the process_orchestrator_batch function
            // Check if orchestration is already completed
            let is_completed = history.iter().rev().any(|e| {
                matches!(
                    e,
                    Event::OrchestrationCompleted { .. }
                        | Event::OrchestrationFailed { .. }
                        | Event::OrchestrationContinuedAsNew { .. }
                )
            });

            if is_completed {
            debug!(instance, "ignoring completion batch for already completed orchestration");
            let _ = self.history_store.ack_orchestrator(&token).await;
                return;
            }

            let orchestration_name = history.iter().rev().find_map(|e| match e {
                Event::OrchestrationStarted { name, .. } => Some(name.clone()),
                _ => None,
            });

            if let Some(orch_name) = orchestration_name {
            // Run execution with the completion messages
                let (_final_history, result) = self
                    .clone()
                .run_single_execution(instance, &orch_name, history, completion_messages)
                    .await;

                // Log the result
                match &result {
                    Ok(output) => {
                        debug!(
                            instance,
                        orch_name, output, "Completion batch execution completed successfully"
                        );
                    }
                    Err(error) => {
                    debug!(instance, orch_name, error, "Completion batch execution failed");
                    }
                }

            // Note: Acknowledgment is handled by OrchestrationTurn after persistence
            // We don't ack here to avoid double-ack
            } else {
            error!(instance, "no OrchestrationStarted found in history for completion batch");
            let _ = self.history_store.abandon_orchestrator(&token).await;
            }
        }


}

impl Runtime {
    /// Raise an external event by name into a running instance.
    pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>) {
        let name_str = name.into();
        let data_str = data.into();
        // Best-effort: only enqueue if a subscription for this name exists in the latest execution
        let hist = self.history_store.read(instance).await;
        let has_subscription = hist
            .iter()
            .any(|e| matches!(e, Event::ExternalSubscribed { name, .. } if name == &name_str));
        if !has_subscription {
            warn!(instance, event_name=%name_str, "raise_event: dropping external event with no active subscription");
            return;
        }
        if let Err(e) = self
            .history_store
            .enqueue_orchestrator_work(
                WorkItem::ExternalRaised {
                    instance: instance.to_string(),
                    name: name_str.clone(),
                    data: data_str.clone(),
                },
            )
            .await
        {
            warn!(instance, name=%name_str, error=%e, "raise_event: failed to enqueue ExternalRaised");
        }
        info!(instance, name=%name_str, data=%data_str, "raise_event: enqueued external");
    }

    /// Request cancellation of a running orchestration instance.
    pub async fn cancel_instance(&self, instance: &str, reason: impl Into<String>) {
        let reason_s = reason.into();
        // Only enqueue if instance is active, else best-effort: provider queue will deliver when it becomes active
        let _ = self
            .history_store
            .enqueue_orchestrator_work(
                WorkItem::CancelInstance {
                    instance: instance.to_string(),
                    reason: reason_s,
                },
            )
            .await;
    }

    /// Wait until the orchestration reaches a terminal state (Completed/Failed) or the timeout elapses.
    pub async fn wait_for_orchestration(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<OrchestrationStatus, WaitError> {
        let deadline = std::time::Instant::now() + timeout;
        // quick path
        match self.get_orchestration_status(instance).await {
            OrchestrationStatus::Completed { output } => return Ok(OrchestrationStatus::Completed { output }),
            OrchestrationStatus::Failed { error } => return Ok(OrchestrationStatus::Failed { error }),
            _ => {}
        }
        // poll with backoff
        let mut delay_ms: u64 = 5;
        while std::time::Instant::now() < deadline {
            match self.get_orchestration_status(instance).await {
                OrchestrationStatus::Completed { output } => return Ok(OrchestrationStatus::Completed { output }),
                OrchestrationStatus::Failed { error } => return Ok(OrchestrationStatus::Failed { error }),
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms.saturating_mul(2)).min(100);
                }
            }
        }
        Err(WaitError::Timeout)
    }

    /// Typed variant: returns Ok(Ok<T>) on Completed with decoded output, Ok(Err(String)) on Failed.
    pub async fn wait_for_orchestration_typed<Out: serde::de::DeserializeOwned>(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<Result<Out, String>, WaitError> {
        match self.wait_for_orchestration(instance, timeout).await? {
            OrchestrationStatus::Completed { output } => match crate::_typed_codec::Json::decode::<Out>(&output) {
                Ok(v) => Ok(Ok(v)),
                Err(e) => Err(WaitError::Other(format!("decode failed: {e}"))),
            },
            OrchestrationStatus::Failed { error } => Ok(Err(error)),
            _ => unreachable!("wait_for_orchestration returns only terminal or timeout"),
        }
    }

    /// Blocking wrapper around wait_for_orchestration.
    pub fn wait_for_orchestration_blocking(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<OrchestrationStatus, WaitError> {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| handle.block_on(self.wait_for_orchestration(instance, timeout)))
        } else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| WaitError::Other(e.to_string()))?;
            rt.block_on(self.wait_for_orchestration(instance, timeout))
        }
    }

    /// Blocking wrapper for typed wait.
    pub fn wait_for_orchestration_typed_blocking<Out: serde::de::DeserializeOwned>(
        &self,
        instance: &str,
        timeout: std::time::Duration,
    ) -> Result<Result<Out, String>, WaitError> {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| handle.block_on(self.wait_for_orchestration_typed::<Out>(instance, timeout)))
        } else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| WaitError::Other(e.to_string()))?;
            rt.block_on(self.wait_for_orchestration_typed::<Out>(instance, timeout))
        }
    }
}
