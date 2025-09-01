use crate::_typed_codec::{Codec, Json};
use crate::providers::in_memory::InMemoryHistoryStore;
use crate::providers::{HistoryStore, QueueKind, WorkItem};
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
use std::collections::HashSet;


pub mod completion_map;
pub mod orchestration_turn;
pub mod execution;
pub mod completion_aware_futures;


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
    active_instances: Mutex<HashSet<String>>,
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
    async fn apply_decisions(
        self: &Arc<Self>,
        instance: &str,
        history: &Vec<Event>,
        decisions: Vec<crate::Action>,
    ) {
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

    async fn ensure_instance_active(self: &Arc<Self>, instance: &str, orchestration_name: &str) -> bool {
        if self.active_instances.lock().await.contains(instance) {
            return false;
        }
        
        // Load history and collect any pending completion messages for this instance
        let history = self.history_store.read(instance).await;
        let completion_messages = Vec::new(); // TODO: Collect pending messages from orchestrator queue
        
        let inner = self.clone().spawn_instance_to_completion(instance, orchestration_name, history, completion_messages);
        // Wrap to normalize handle type to JoinHandle<()>
        let wrapper = tokio::spawn(async move {
            let _ = inner.await;
        });
        self.instance_joins.lock().await.push(wrapper);
        true
    }


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
        // Ensure instance exists (best-effort)
        let _ = self.history_store.create_instance(instance).await;
        // Append start marker if empty
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            // Determine version to pin and to write into the start event
            let maybe_resolved: Option<Version> = if let Some(v) = pin_version {
                Some(v)
            } else if let Some(v) = self.pinned_versions.lock().await.get(instance).cloned() {
                Some(v)
            } else if let Some((resolved_v, _handler)) =
                self.orchestration_registry.resolve_for_start(orchestration_name).await
            {
                Some(resolved_v)
            } else {
                None
            };
            // If resolved, pin for handler resolution and write that version; otherwise use a fallback string version
            let version_str_for_event: String = if let Some(v) = maybe_resolved.clone() {
                self.pinned_versions
                    .lock()
                    .await
                    .insert(instance.to_string(), v.clone());
                v.to_string()
            } else {
                "0.0.0".to_string()
            };
            let started = vec![Event::OrchestrationStarted {
                name: orchestration_name.to_string(),
                version: version_str_for_event,
                input,
                parent_instance,
                parent_id,
            }];
            self.history_store
                .append(instance, started)
                .await
                .map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;   // TODO : CR : this error would be fatal
        } else {
            // Allow duplicate starts as a warning for detached or at-least-once semantics
            warn!(
                instance,
                "instance already has history; duplicate start accepted (deduped)"
            );
        }
        
        // Enqueue a start request; background worker will dedupe and run exactly one execution
        self.ensure_instance_active(instance, orchestration_name).await;
        
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
            active_instances: Mutex::new(HashSet::new()),
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
                if let Some((item, token)) = self.history_store.dequeue_peek_lock(QueueKind::Orchestrator).await {
                    match item {
                        // Orchestration lifecycle events - start new executions
                        WorkItem::StartOrchestration { ref instance, ref orchestration, ref input, ref version }
                        | WorkItem::ContinueAsNew { ref instance, ref orchestration, ref input, ref version } => {
                            let event_type = match &item {
                                WorkItem::StartOrchestration { .. } => "StartOrchestration",
                                WorkItem::ContinueAsNew { .. } => "ContinueAsNew",
                                _ => unreachable!(),
                            };
                            debug!("{event_type}: {instance} {orchestration} {input} (version: {:?})", version);
                            self.start_orchestration_execution(instance, orchestration, input.clone(), version.clone(), token).await;
                        }
                        
                        // All completion events - deliver to active instances via router
                        completion_item => {
                            // Extract execution ID for validation (if applicable)
                            let exec_id = match &completion_item {
                                WorkItem::ActivityCompleted { execution_id, .. }
                                | WorkItem::ActivityFailed { execution_id, .. }
                                | WorkItem::TimerFired { execution_id, .. } => Some(*execution_id),
                                WorkItem::SubOrchCompleted { parent_execution_id, .. }
                                | WorkItem::SubOrchFailed { parent_execution_id, .. } => Some(*parent_execution_id),
                                WorkItem::ExternalRaised { .. }
                                | WorkItem::CancelInstance { .. } => None,
                                // Invalid items for orchestrator queue
                                other => {
                                    error!(?other, "unexpected WorkItem in Orchestrator dispatcher; state corruption");
                                    panic!("unexpected WorkItem in Orchestrator dispatcher");
                                }
                            };
                            
                            // Debug log the completion event
                            match &completion_item {
                                WorkItem::ActivityCompleted { instance, execution_id, id, result } => {
                                    debug!("ActivityCompleted: {instance} {execution_id} {id} {result}");
                                }
                                WorkItem::ActivityFailed { instance, execution_id, id, error } => {
                                    debug!("ActivityFailed: {instance} {execution_id} {id} {error}");
                                }
                                WorkItem::TimerFired { instance, execution_id, id, fire_at_ms } => {
                                    debug!("TimerFired: {instance} {execution_id} {id} {fire_at_ms}");
                                    debug!("TimerFired completion delivered: {instance} {execution_id} {id}");
                                }
                                WorkItem::ExternalRaised { instance, name, data } => {
                                    debug!("ExternalRaised: {instance} {name} {data}");
                                }
                                WorkItem::SubOrchCompleted { parent_instance, parent_execution_id, parent_id, result } => {
                                    debug!("SubOrchCompleted: {parent_instance} {parent_execution_id} {parent_id} {result}");
                                }
                                WorkItem::SubOrchFailed { parent_instance, parent_execution_id, parent_id, error } => {
                                    debug!("SubOrchFailed: {parent_instance} {parent_execution_id} {parent_id} {error}");
                                }
                                WorkItem::CancelInstance { instance, reason } => {
                                    debug!("CancelInstance: {instance} {reason}");
                                }
                                _ => unreachable!(), // Already handled above
                            }
                            
                            // Handle completion by triggering execution with the completion message
                            self.handle_completion_item(completion_item, exec_id, token).await;
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
                }
            }
        })
    }

    fn start_work_dispatcher(self: Arc<Self>, activities: Arc<registry::ActivityRegistry>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Some((item, token)) = self.history_store.dequeue_peek_lock(QueueKind::Worker).await {
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
                                            .enqueue_work(
                                                QueueKind::Orchestrator,
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
                                            .enqueue_work(
                                                QueueKind::Orchestrator,
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
                                    .enqueue_work(
                                        QueueKind::Orchestrator,
                                        WorkItem::ActivityFailed {
                                            instance: instance.clone(),
                                            execution_id,
                                            id,
                                            error: format!("unregistered:{}", name),
                                        },
                                    )
                                    .await;
                            }
                            let _ = self.history_store.ack(QueueKind::Worker, &token).await;
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
                    if let Some((item, token)) = self.history_store.dequeue_peek_lock(QueueKind::Timer).await {
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
                                    .enqueue_work(
                                        QueueKind::Orchestrator,
                                        WorkItem::TimerFired {
                                            instance,
                                            execution_id,
                                            id,
                                            fire_at_ms,
                                        },
                                    )
                                    .await;
                                let _ = self.history_store.ack(QueueKind::Timer, &token).await;
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
                    if let Some((item, token)) = intake_rt.history_store.dequeue_peek_lock(QueueKind::Timer).await {
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
                                let _ = intake_rt.history_store.ack(QueueKind::Timer, &token).await;
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
            this_for_task.run_single_execution(&inst, &orch_name, initial_history, completion_messages).await 
        })
    }

    /// Unified handler for starting orchestration executions (both new and ContinueAsNew)
    async fn start_orchestration_execution(
        self: &Arc<Self>,
        instance: &str,
        orchestration: &str,
        input: String,
        version: Option<String>,
        token: String,
    ) {
        debug!(
            "Starting orchestration execution: {} {} {} (version: {:?})", 
            instance, orchestration, input, version
        );

        // Ensure instance exists and create initial history if needed
        let _ = self.history_store.create_instance(instance).await;
        let mut history = self.history_store.read(instance).await;
        
        if history.is_empty() {
            // Determine version to pin and to write into the start event
            let maybe_resolved: Option<Version> = if let Some(v) = version.clone().and_then(|s| semver::Version::parse(&s).ok()) {
                Some(v)
            } else if let Some(v) = self.pinned_versions.lock().await.get(instance).cloned() {
                Some(v)
            } else if let Some((resolved_v, _handler)) =
                self.orchestration_registry.resolve_for_start(orchestration).await
            {
                Some(resolved_v)
            } else {
                None
            };
            
            // If resolved, pin for handler resolution and write that version; otherwise use a fallback string version
            let version_str_for_event: String = if let Some(v) = maybe_resolved.clone() {
                self.pinned_versions
                    .lock()
                    .await
                    .insert(instance.to_string(), v.clone());
                v.to_string()
            } else {
                "0.0.0".to_string()
            };
            
            let started = vec![Event::OrchestrationStarted {
                name: orchestration.to_string(),
                version: version_str_for_event,
                input: input.clone(),
                parent_instance: None, // Detached orchestrations have no parent
                parent_id: None,
            }];
            
            if let Err(e) = self.history_store.append(instance, started.clone()).await {
                error!(instance, error = %e, "failed to append OrchestrationStarted");
                let _ = self.history_store.abandon(QueueKind::Orchestrator, &token).await;
                return;
            }
            
            history.extend(started);
        } else {
            // Allow duplicate starts as a warning for detached or at-least-once semantics
            warn!(
                instance,
                "instance already has history; duplicate start accepted (deduped)"
            );
        }

        let completion_messages = Vec::new(); // For now, start with empty messages
        
        let (_final_history, result) = self.clone().run_single_execution(
            instance, 
            orchestration, 
            history, 
            completion_messages
        ).await;
        
        // Log the result
        match &result {
            Ok(output) => {
                debug!(instance, orchestration, output, "Orchestration execution completed successfully");
            }
            Err(error) => {
                debug!(instance, orchestration, error, "Orchestration execution failed");
            }
        }

        // Acknowledge the start request
        let _ = self.history_store.ack(QueueKind::Orchestrator, &token).await;
    }

    /// Handle completion items by triggering execution with the completion message
    async fn handle_completion_item(
        self: &Arc<Self>,
        work_item: WorkItem,
        exec_id: Option<u64>,
        token: String,
    ) {
        // Extract instance from the work item
        let instance = match &work_item {
            WorkItem::ActivityCompleted { instance, .. }
            | WorkItem::ActivityFailed { instance, .. }
            | WorkItem::TimerFired { instance, .. }
            | WorkItem::ExternalRaised { instance, .. }
            | WorkItem::CancelInstance { instance, .. } => instance.clone(),
            WorkItem::SubOrchCompleted { parent_instance, .. }
            | WorkItem::SubOrchFailed { parent_instance, .. } => parent_instance.clone(),
            _ => {
                error!(?work_item, "unexpected WorkItem in completion handler");
                let _ = self.history_store.abandon(QueueKind::Orchestrator, &token).await;
                return;
            }
        };

        // Validate execution ID if provided
        if let Some(completion_exec_id) = exec_id {
            let current_exec_id = self.history_store.latest_execution_id(&instance).await.unwrap_or(1);
            if completion_exec_id != current_exec_id {
                if completion_exec_id < current_exec_id {
                    warn!(
                        instance = %instance,
                        completion_execution_id = completion_exec_id,
                        current_execution_id = current_exec_id,
                        "ignoring completion from older execution (likely from ContinueAsNew)"
                    );
                } else {
                    warn!(
                        instance = %instance,
                        completion_execution_id = completion_exec_id,
                        current_execution_id = current_exec_id,
                        "ignoring completion from future execution (unexpected)"
                    );
                }
                let _ = self.history_store.ack(QueueKind::Orchestrator, &token).await;
                return;
            }
        }

        // Convert WorkItem to OrchestratorMsg
        if let Some(msg) = crate::runtime::router::OrchestratorMsg::from_work_item(work_item, Some(token.clone())) {
            // Get orchestration name and check if already completed
            let history = self.history_store.read(&instance).await;
            
            // Check if orchestration is already completed
            let is_completed = history.iter().rev().any(|e| matches!(e, 
                Event::OrchestrationCompleted { .. } | 
                Event::OrchestrationFailed { .. } |
                Event::OrchestrationContinuedAsNew { .. }
            ));
            
            if is_completed {
                debug!(instance, "ignoring completion for already completed orchestration");
                let _ = self.history_store.ack(QueueKind::Orchestrator, &token).await;
                return;
            }
            
            let orchestration_name = history.iter().rev().find_map(|e| match e {
                Event::OrchestrationStarted { name, .. } => Some(name.clone()),
                _ => None,
            });

            if let Some(orch_name) = orchestration_name {
                // Run execution with this completion message
                let completion_messages = vec![msg];
                let (_final_history, result) = self.clone().run_single_execution(
                    &instance,
                    &orch_name,
                    history,
                    completion_messages,
                ).await;

                // Log the result
                match &result {
                    Ok(output) => {
                        debug!(instance, orch_name, output, "Completion execution completed successfully");
                    }
                    Err(error) => {
                        debug!(instance, orch_name, error, "Completion execution failed");
                    }
                }

                // Acknowledge the completion
                let _ = self.history_store.ack(QueueKind::Orchestrator, &token).await;
            } else {
                error!(instance, "no OrchestrationStarted found in history for completion");
                let _ = self.history_store.abandon(QueueKind::Orchestrator, &token).await;
            }
        } else {
            // WorkItem doesn't convert to OrchestratorMsg, just acknowledge
            let _ = self.history_store.ack(QueueKind::Orchestrator, &token).await;
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
            .enqueue_work(
                QueueKind::Orchestrator,
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
            .enqueue_work(
                QueueKind::Orchestrator,
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


