use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, oneshot};
use tokio::task::JoinHandle;
use crate::{Action, Event, OrchestrationContext, run_turn_with_claims};
use serde::{de::DeserializeOwned, Serialize};
use crate::_typed_codec::{Json, Codec};
use semver::Version;
use crate::providers::{HistoryStore, WorkItem};
use crate::providers::in_memory::InMemoryHistoryStore;
use tracing::{debug, warn, info, error};
pub mod registry;
pub mod router;
pub mod detect;
pub mod completions;
pub mod dispatch;
pub mod status;
use std::collections::HashSet;
use async_trait::async_trait;

/// Runtime components: activity worker and registry utilities.
pub mod activity;

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
pub enum WaitError { Timeout, Other(String) }

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

/// Work item enqueued to the activity worker.
#[derive(Clone)]
pub struct ActivityWorkItem { pub instance: String, pub id: u64, pub name: String, pub input: String }

/// Work item enqueued to the timer worker.
#[derive(Clone)]
pub struct TimerWorkItem { pub instance: String, pub id: u64, pub fire_at_ms: u64, pub delay_ms: u64 }

pub use router::{CompletionRouter, OrchestratorMsg, StartRequest};

/// In-process runtime that executes activities and timers and persists
/// history via a `HistoryStore`. 
pub struct Runtime {
    activity_tx: mpsc::Sender<ActivityWorkItem>,
    timer_tx: mpsc::Sender<TimerWorkItem>,
    router_tx: mpsc::UnboundedSender<OrchestratorMsg>,
    router: Arc<CompletionRouter>,
    joins: Mutex<Vec<JoinHandle<()>>>,
    instance_joins: Mutex<Vec<JoinHandle<()>>>,
    history_store: Arc<dyn HistoryStore>,
    active_instances: Mutex<HashSet<String>>,
    pending_starts: Mutex<HashSet<String>>,
    result_waiters: Mutex<HashMap<String, Vec<oneshot::Sender<(Vec<Event>, Result<String, String>)>>>>,
    orchestration_registry: OrchestrationRegistry,
    // Pinned versions for instances started in this runtime (in-memory for now)
    pinned_versions: Mutex<HashMap<String, Version>>,
    // queue for start requests so callers only enqueue and runtime manages execution
    start_tx: mpsc::UnboundedSender<StartRequest>,
}

impl Runtime {
    // Associated constants for runtime behavior
    const COMPLETION_BATCH_LIMIT: usize = 128;
    const REENQUEUE_DELAY_MS: u64 = 5;
    const POLLER_GATE_DELAY_MS: u64 = 5;
    const POLLER_IDLE_SLEEP_MS: u64 = 10;
    async fn start_internal_rx(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
        input: String,
        pin_version: Option<Version>,
    ) -> Result<oneshot::Receiver<(Vec<Event>, Result<String, String>)>, String> {
        // Ensure instance exists (best-effort)
        let _ = self.history_store.create_instance(instance).await;
        // Append start marker if empty
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            if let Some(v) = pin_version {
                self.pinned_versions.lock().await.insert(instance.to_string(), v);
            } else if !self.pinned_versions.lock().await.contains_key(instance) {
                if let Some((resolved_v, _handler)) = self.orchestration_registry.resolve_for_start(orchestration_name).await {
                    self.pinned_versions.lock().await.insert(instance.to_string(), resolved_v.clone());
                }
            }
            let started = vec![Event::OrchestrationStarted { name: orchestration_name.to_string(), input }];
            self
                .history_store
                .append(instance, started)
                .await
                .map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;
            let _ = self
                .history_store
                .set_instance_orchestration(instance, orchestration_name)
                .await;
        } else {
            // Allow duplicate starts as a warning for detached or at-least-once semantics
            warn!(instance, "instance already has history; duplicate start accepted (deduped)");
        }
        // Enqueue a start request; background worker will dedupe and run exactly one execution
        let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orchestration_name.to_string() });
        // Register a oneshot waiter for string result
        let (tx, rx) = oneshot::channel::<(Vec<Event>, Result<String, String>)>();
        self
            .result_waiters
            .lock()
            .await
            .entry(instance.to_string())
            .or_default()
            .push(tx);
        Ok(rx)
    }
    pub(crate) fn now_ms_static() -> u64 { 
        // Best effort wall-clock for timer rehydration
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as u64).unwrap_or(0)
    }
    /// Enqueue a new orchestration instance start. The runtime will pick this up
    /// in the background and drive it to completion.
    /// Start a typed orchestration; input/output are serialized internally.
    pub async fn start_orchestration_typed<In, Out>(self: Arc<Self>, instance: &str, orchestration_name: &str, input: In) -> Result<JoinHandle<(Vec<Event>, Result<Out, String>)>, String>
    where
        In: Serialize,
        Out: DeserializeOwned + Send + 'static,
    {
        let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
        let rx = self.clone().start_internal_rx(instance, orchestration_name, payload, None).await?;
        Ok(tokio::spawn(async move {
            let (hist, res_s) = rx.await.expect("result");
            let res_t: Result<Out, String> = match res_s {
                Ok(s) => Json::decode::<Out>(&s),
                Err(e) => Err(e),
            };
            (hist, res_t)
        }))
    }

    /// Start a typed orchestration with an explicit version (semver string).
    pub async fn start_orchestration_versioned_typed<In, Out>(self: Arc<Self>, instance: &str, orchestration_name: &str, version: impl AsRef<str>, input: In) -> Result<JoinHandle<(Vec<Event>, Result<Out, String>)>, String>
    where
        In: Serialize,
        Out: DeserializeOwned + Send + 'static,
    {
        let v = semver::Version::parse(version.as_ref()).map_err(|e| e.to_string())?;
        let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
        let rx = self.clone().start_internal_rx(instance, orchestration_name, payload, Some(v)).await?;
        Ok(tokio::spawn(async move {
            let (hist, res_s) = rx.await.expect("result");
            let res_t: Result<Out, String> = match res_s {
                Ok(s) => Json::decode::<Out>(&s),
                Err(e) => Err(e),
            };
            (hist, res_t)
        }))
    }

    /// Start an orchestration using raw String input/output (back-compat API).
    pub async fn start_orchestration(self: Arc<Self>, instance: &str, orchestration_name: &str, input: impl Into<String>) -> Result<JoinHandle<(Vec<Event>, Result<String, String>)>, String> {
        let rx = self.clone().start_internal_rx(instance, orchestration_name, input.into(), None).await?;
        Ok(tokio::spawn(async move { rx.await.expect("result") }))
    }

    /// Start an orchestration with an explicit version (string I/O).
    pub async fn start_orchestration_versioned(self: Arc<Self>, instance: &str, orchestration_name: &str, version: impl AsRef<str>, input: impl Into<String>) -> Result<JoinHandle<(Vec<Event>, Result<String, String>)>, String> {
        let v = semver::Version::parse(version.as_ref()).map_err(|e| e.to_string())?;
        let rx = self.clone().start_internal_rx(instance, orchestration_name, input.into(), Some(v)).await?;
        Ok(tokio::spawn(async move { rx.await.expect("result") }))
    }

    // Status helpers implemented in status.rs

    // Status helpers moved to status.rs
    /// Start a new runtime using the in-memory history store.
    pub async fn start(activity_registry: Arc<activity::ActivityRegistry>, orchestration_registry: OrchestrationRegistry) -> Arc<Self> {
        let history_store: Arc<dyn HistoryStore> = Arc::new(InMemoryHistoryStore::default());
        Self::start_with_store(history_store, activity_registry, orchestration_registry).await
    }

    /// Start a new runtime with a custom `HistoryStore` implementation.
    pub async fn start_with_store(history_store: Arc<dyn HistoryStore>, activity_registry: Arc<activity::ActivityRegistry>, orchestration_registry: OrchestrationRegistry) -> Arc<Self> {
        // Install a default subscriber if none set (ok to call many times)
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .try_init();

        let (activity_tx, activity_rx) = mpsc::channel::<ActivityWorkItem>(512);
        let (timer_tx, timer_rx) = mpsc::channel::<TimerWorkItem>(512);
        let (router_tx, mut router_rx) = mpsc::unbounded_channel::<OrchestratorMsg>();
        let router = Arc::new(CompletionRouter { inboxes: Mutex::new(HashMap::new()) });
        let mut joins: Vec<JoinHandle<()>> = Vec::new();

        // spawn activity worker with system trace handler pre-registered
        // copy user registrations
        let mut builder = activity::ActivityRegistryBuilder::from_registry(&activity_registry);
        // add system activities
        builder = builder.register(crate::SYSTEM_TRACE_ACTIVITY, |input: String| async move {
            // input format: "LEVEL:message"
            Ok(input)
        });
        builder = builder.register(crate::SYSTEM_NOW_ACTIVITY, |_input: String| async move {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            Ok(now_ms.to_string())
        });
        builder = builder.register(crate::SYSTEM_NEW_GUID_ACTIVITY, |_input: String| async move {
            // TODO : find the right way to build a guid
            // Pseudo-guid: 32-hex digits from current nanos since epoch
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Ok(format!("{nanos:032x}"))
        });
        let reg_clone = builder.build();
        let store_for_worker = history_store.clone();
        joins.push(tokio::spawn(async move {
            activity::ActivityWorker::new(reg_clone, store_for_worker).run(activity_rx).await
        }));

        // spawn timer worker
        let store_for_timer = history_store.clone();
        joins.push(tokio::spawn(async move { run_timer_worker(timer_rx, store_for_timer).await }));

        // spawn router forwarding task
        let router_clone = router.clone();
        joins.push(tokio::spawn(async move {
            while let Some(msg) = router_rx.recv().await { router_clone.forward(msg).await; }
        }));

        // start request queue + worker
        let (start_tx, mut start_rx) = mpsc::unbounded_channel::<StartRequest>();

        let runtime = Arc::new(Self { activity_tx, timer_tx, router_tx, router, joins: Mutex::new(joins), instance_joins: Mutex::new(Vec::new()), history_store, active_instances: Mutex::new(HashSet::new()), pending_starts: Mutex::new(HashSet::new()), result_waiters: Mutex::new(HashMap::new()), orchestration_registry, start_tx, pinned_versions: Mutex::new(HashMap::new()) });

        // background worker to process start requests
        let rt_for_worker = runtime.clone();
        let worker = tokio::spawn(async move {
            while let Some(req) = start_rx.recv().await {
                let inst = req.instance.clone();
                let orch = req.orchestration_name.clone();
                // If already active, re-enqueue shortly to allow previous execution to release the guard (e.g., ContinueAsNew)
                if rt_for_worker.active_instances.lock().await.contains(&inst) {
                    let tx = rt_for_worker.start_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(Self::REENQUEUE_DELAY_MS)).await;
                        let _ = tx.send(StartRequest { instance: inst, orchestration_name: orch });
                    });
                    continue;
                }
                // Deduplicate: skip if already pending
                {
                    let mut pend = rt_for_worker.pending_starts.lock().await;
                    if pend.contains(&req.instance) {
                        warn!(instance=%req.instance, orchestration=%req.orchestration_name, "start worker: duplicate start suppressed (pending)");
                        continue;
                    }
                    pend.insert(req.instance.clone());
                }
                // Spawn to completion without awaiting so we can process more starts
                let _handle = rt_for_worker.clone().spawn_instance_to_completion(&req.instance, &req.orchestration_name);
                // Clear pending flag immediately; active guard prevents double-run
                rt_for_worker.pending_starts.lock().await.remove(&req.instance);
            }
        });
        runtime.joins.lock().await.push(worker);

        // background poller for provider-backed work queue
        let rt_for_poll = runtime.clone();
        let poller = tokio::spawn(async move {
            // bootstrap: scan existing instances and enqueue starts for incomplete ones
            let instances = rt_for_poll.history_store.list_instances().await;
            for inst in instances {
                let hist = rt_for_poll.history_store.read(&inst).await;
                // Determine orchestration name either from provider meta or from OrchestrationStarted
                let mut orchestration_name = rt_for_poll.history_store.get_instance_orchestration(&inst).await;
                if orchestration_name.is_none() {
                    orchestration_name = hist.iter().find_map(|e| match e {
                        Event::OrchestrationStarted { name, .. } => Some(name.clone()),
                        _ => None,
                    });
                }
                if let Some(name) = orchestration_name {
                    // Skip if terminal event present
                    let is_terminal = hist.iter().rev().any(|e| matches!(e, Event::OrchestrationCompleted { .. } | Event::OrchestrationFailed { .. }));
                    if !is_terminal {
                        let _ = rt_for_poll.start_tx.send(StartRequest { instance: inst.clone(), orchestration_name: name });
                    }
                }
            }

            loop {
                if let Some((item, token)) = rt_for_poll.history_store.dequeue_peek_lock().await {
                    // Helper to check if instance is active; if not, re-enqueue the item to avoid drops
                    let mut maybe_forward = true;
                    enum GatePolicy { RequireActive, RequireInactive }
        let (target_instance, policy): (Option<String>, GatePolicy) = match &item {
            WorkItem::StartOrchestration { instance, orchestration, input } => {
                // Only forward start when instance is not active; if active, abandon to retry later.
        let _ = (orchestration, input); // unused here
                (Some(instance.clone()), GatePolicy::RequireInactive)
                        }
                        WorkItem::ActivityCompleted { instance, id, result } => {
                let _ = (id, result);
                (Some(instance.clone()), GatePolicy::RequireActive)
                        }
                        WorkItem::ActivityFailed { instance, id, error } => {
                let _ = (id, error);
                (Some(instance.clone()), GatePolicy::RequireActive)
                        }
                        WorkItem::TimerFired { instance, id, fire_at_ms } => {
                let _ = (id, fire_at_ms);
                (Some(instance.clone()), GatePolicy::RequireActive)
                        }
                        WorkItem::ExternalRaised { instance, name, data } => {
                let _ = (name, data);
                (Some(instance.clone()), GatePolicy::RequireActive)
                        }
                        WorkItem::SubOrchCompleted { parent_instance, parent_id, result } => {
                let _ = (parent_id, result);
                (Some(parent_instance.clone()), GatePolicy::RequireActive)
                        }
                        WorkItem::SubOrchFailed { parent_instance, parent_id, error } => {
                let _ = (parent_id, error);
                (Some(parent_instance.clone()), GatePolicy::RequireActive)
                        }
                        WorkItem::CancelInstance { instance, reason } => {
                let _ = reason;
                (Some(instance.clone()), GatePolicy::RequireActive)
                        }
                    };
                    if let Some(inst) = target_instance {
                        let is_active = rt_for_poll.active_instances.lock().await.contains(&inst);
                        let should_wait = match policy {
                            GatePolicy::RequireActive => !is_active,
                            GatePolicy::RequireInactive => is_active,
                        };
                        if should_wait {
                            // Abandon so another poll can retry later
                            let kind = match &item {
                                WorkItem::StartOrchestration { .. } => "StartOrchestration",
                                WorkItem::ActivityCompleted { .. } => "ActivityCompleted",
                                WorkItem::ActivityFailed { .. } => "ActivityFailed",
                                WorkItem::TimerFired { .. } => "TimerFired",
                                WorkItem::ExternalRaised { .. } => "ExternalRaised",
                                WorkItem::SubOrchCompleted { .. } => "SubOrchCompleted",
                                WorkItem::SubOrchFailed { .. } => "SubOrchFailed",
                                WorkItem::CancelInstance { .. } => "CancelInstance",
                            };
                            warn!(instance=%inst, kind=%kind, "poller: no active receiver; abandoning work item");
                            let _ = rt_for_poll.history_store.abandon(&token).await;
                            tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_GATE_DELAY_MS)).await;
                            maybe_forward = false;
                        }
                    }
                    if !maybe_forward { continue; }
            match item {
                        WorkItem::StartOrchestration { instance, orchestration, input } => {
                            // Exactly-once: call start_orchestration which writes OrchestrationStarted idempotently.
                            // If instance already exists, start_orchestration dedupes by history.
                            match rt_for_poll.clone().start_orchestration(&instance, &orchestration, input).await {
                                Ok(_h) => { /* spawned; ack below */ },
                                Err(e) => { warn!(instance=%instance, orchestration=%orchestration, error=%e, "start_orchestration failed from queue"); }
                            }
                            // Ack regardless; idempotent start prevents duplicates.
                            let _ = rt_for_poll.history_store.ack(&token).await;
                        }
                        WorkItem::ActivityCompleted { instance, id, result } => {
                let _ = rt_for_poll.router_tx.send(OrchestratorMsg::ActivityCompleted { instance, id, result, ack_token: Some(token) });
                        }
                        WorkItem::ActivityFailed { instance, id, error } => {
                let _ = rt_for_poll.router_tx.send(OrchestratorMsg::ActivityFailed { instance, id, error, ack_token: Some(token) });
                        }
                        WorkItem::TimerFired { instance, id, fire_at_ms } => {
                let _ = rt_for_poll.router_tx.send(OrchestratorMsg::TimerFired { instance, id, fire_at_ms, ack_token: Some(token) });
                        }
                        WorkItem::ExternalRaised { instance, name, data } => {
                            info!(instance=%instance, name=%name, "poller: forwarding external");
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::ExternalByName { instance, name, data, ack_token: Some(token) });
                        }
                        WorkItem::SubOrchCompleted { parent_instance, parent_id, result } => {
                let _ = rt_for_poll.router_tx.send(OrchestratorMsg::SubOrchCompleted { instance: parent_instance, id: parent_id, result, ack_token: Some(token) });
                        }
                        WorkItem::SubOrchFailed { parent_instance, parent_id, error } => {
                let _ = rt_for_poll.router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_instance, id: parent_id, error, ack_token: Some(token) });
                        }
                        WorkItem::CancelInstance { instance, reason } => {
                            // Try direct send to inbox to detect receiver presence; otherwise abandon and retry
                            let mut sent = false;
                            if let Some(tx) = rt_for_poll.router.inboxes.lock().await.get(&instance) {
                                if tx.send(OrchestratorMsg::CancelRequested { instance: instance.clone(), reason: reason.clone(), ack_token: Some(token.clone()) }).is_ok() {
                                    sent = true;
                                }
                            }
                            if !sent {
                                warn!(instance=%instance, "poller: cancel requested but no active receiver; abandoning");
                                let _ = rt_for_poll.history_store.abandon(&token).await;
                                tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_GATE_DELAY_MS)).await;
                            }
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(Self::POLLER_IDLE_SLEEP_MS)).await;
                }
            }
        });
        runtime.joins.lock().await.push(poller);

        runtime
    }

    /// Abort background tasks. Channels are dropped with the runtime.
    pub async fn shutdown(self: Arc<Self>) {
        // Abort background tasks; channels will be dropped with Runtime
        let mut joins = self.joins.lock().await;
        for j in joins.drain(..) { j.abort(); }
    }

    /// Await completion of all outstanding spawned orchestration instances.
    pub async fn drain_instances(self: Arc<Self>) {
        let mut joins = self.instance_joins.lock().await;
        while let Some(j) = joins.pop() {
            let _ = j.await;
        }
    }

    /// Run a single instance to completion by orchestration name, returning
    /// its final history and output.
    pub async fn run_instance_to_completion(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
    ) -> (Vec<Event>, Result<String, String>) {
        // Ensure instance not already active in this runtime
        {
            let mut act = self.active_instances.lock().await;
            if !act.insert(instance.to_string()) {
                panic!("instance already active: {instance}");
            }
        }
    // Look up the orchestration handler later after loading history so we can fail gracefully
    // Prefer pinned version for this instance if present; else fall back to latest by name
    let pinned_v = self.pinned_versions.lock().await.get(instance).cloned();
    let orchestration_handler_opt = if let Some(v) = pinned_v {
        self.orchestration_registry.resolve_exact(orchestration_name, &v)
    } else {
        self.orchestration_registry.get(orchestration_name)
    };

        // Ensure removal of active flag even if the task panics
        struct ActiveGuard { rt: Arc<Runtime>, inst: String }
        impl Drop for ActiveGuard {
            fn drop(&mut self) {
                // best-effort removal; ignore poisoning
                let rt = self.rt.clone();
                let inst = self.inst.clone();
                // spawn a blocking remove since Drop can't be async
                let _ = tokio::spawn(async move { rt.active_instances.lock().await.remove(&inst); });
            }
        }
        let _active_guard = ActiveGuard { rt: self.clone(), inst: instance.to_string() };
        // Instance is expected to be created by start_orchestration; do not create here
        // Load existing history from store (now exists). For ContinueAsNew we may start with a
        // fresh execution that only has OrchestrationStarted; we always want the latest execution.
        let mut history: Vec<Event> = self.history_store.read(instance).await;
        let mut comp_rx = self.router.register(instance).await;

        // TODO : activities should be renqueued at the end of the turn?
        // Rehydrate pending activities and timers from history
        completions::rehydrate_pending(instance, &history, &self.activity_tx, &self.timer_tx).await;

    // Capture input from the most recent OrchestrationStarted for this orchestration
    let current_input = history.iter().rev().find_map(|e| match e {
            Event::OrchestrationStarted { name: n, input } if n == orchestration_name => Some(input.clone()),
            _ => None,
        }).unwrap_or_default();
        // If this is a child (ParentLinked exists), remember linkage for notifying parent at terminal
        let parent_link = history.iter().find_map(|e| match e {
            Event::ParentLinked { parent_instance, parent_id } => Some((parent_instance.clone(), *parent_id)),
            _ => None,
        });

        // If orchestration not registered, fail gracefully and exit
        if orchestration_handler_opt.is_none() {
            let err = format!("unregistered:{}", orchestration_name);
            // Append terminal failed event (idempotent at provider)
            if let Err(e) = self.history_store.append(instance, vec![Event::OrchestrationFailed { error: err.clone() }]).await {
                error!(instance, error=%e, "failed to append OrchestrationFailed for unknown orchestration");
                panic!("history append failed: {e}");
            }
            // Reflect in local history and notify waiters
            history.push(Event::OrchestrationFailed { error: err.clone() });
            if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                for w in waiters { let _ = w.send((history.clone(), Err(err.clone()))); }
            }
            // If this is a child, enqueue failure to parent
            if let Some((pinst, pid)) = parent_link.clone() {
                let _ = self.history_store.enqueue_work(WorkItem::SubOrchFailed { parent_instance: pinst, parent_id: pid, error: err.clone() }).await;
            }
            return (history, Err(err));
        }

        let orchestration_handler = orchestration_handler_opt.unwrap();

        let mut turn_index: u64 = 0;
        // Track completions appended in the previous iteration to validate against current code's awaited ids
        let mut last_appended_completions: Vec<(&'static str, u64)> = Vec::new();
        loop {
            let orchestrator_fn = |ctx: OrchestrationContext| {
                let handler = orchestration_handler.clone();
                let input_clone = current_input.clone();
                async move { handler.invoke(ctx, input_clone).await }
            };
            let baseline_len = history.len();
            let (hist_after, actions, _logs, out_opt, claims) = run_turn_with_claims(history, turn_index, &orchestrator_fn);
            // Determinism guard: If prior history already contained at least one schedule event, and contained no
            // completion events yet (still at the same decision frontier), then the orchestrator must not introduce
            // net-new schedule events that were not present in prior history. This catches code swaps where, e.g.,
            // A1 was scheduled previously but the new code tries to schedule B1 without any new completion.
            if let Some(err) = detect::detect_frontier_nondeterminism(&hist_after[..baseline_len], &hist_after[baseline_len..]) {
                let _ = self.history_store.append(instance, vec![Event::OrchestrationFailed { error: err.clone() }]).await;
                history = hist_after.clone();
                history.push(Event::OrchestrationFailed { error: err.clone() });
                if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                    for w in waiters { let _ = w.send((history.clone(), Err(err.clone()))); }
                }
                return (history, Err(err));
            }
            history = hist_after;
            // (nondeterminism validation happens after completion batches, using last_appended_completions)
            // Validate any completions appended in the previous iteration against what the current code awaited.
            if !last_appended_completions.is_empty() {
                if let Some(err) = detect::detect_await_mismatch(&last_appended_completions, &claims) {
                    let _ = self.history_store.append(instance, vec![Event::OrchestrationFailed { error: err.clone() }]).await;
                    history.push(Event::OrchestrationFailed { error: err.clone() });
                    if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                        for w in waiters { let _ = w.send((history.clone(), Err(err.clone()))); }
                    }
                    return (history, Err(err));
                }
                // Clear after validation
                last_appended_completions.clear();
            }
            // Handle ContinueAsNew as terminal for this execution, regardless of out_opt
            if let Some((input, version)) = actions.iter().find_map(|a| match a { Action::ContinueAsNew { input, version } => Some((input.clone(), version.clone())), _ => None }) {
                self.handle_continue_as_new(instance, orchestration_name, &mut history, input.clone(), version.clone()).await;
                return (history, Ok(String::new()));
            }
            if let Some(out) = out_opt {
                // Persist any deltas produced during this final turn
                let deltas = if history.len() > baseline_len { history[baseline_len..].to_vec() } else { Vec::new() };
                if !deltas.is_empty() {
                    if let Err(e) = self.history_store.append(instance, deltas.clone()).await {
                        error!(instance, turn_index, error=%e, "failed to append final turn events");
                        // Wake any waiters with error to avoid hangs
                        if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                            for w in waiters { let _ = w.send((history.clone(), Err(format!("history append failed: {e}")))); }
                        }
                        panic!("history append failed: {e}");
                    }
                }
                // Exactly-once dispatch for detached orchestration starts recorded in this turn:
                // enqueue provider work-items; poller will perform the idempotent start.
                for e in deltas {
                    if let Event::OrchestrationChained { id, name, instance: child_inst, input } = e {
                        let wi = crate::providers::WorkItem::StartOrchestration { instance: child_inst.clone(), orchestration: name.clone(), input: input.clone() };
                        if let Err(err) = self.history_store.enqueue_work(wi).await {
                            warn!(instance, id, name=%name, child_instance=%child_inst, error=%err, "failed to enqueue detached start; will rely on bootstrap rehydration");
                        } else {
                            debug!(instance, id, name=%name, child_instance=%child_inst, "enqueued detached orchestration start (final turn)");
                        }
                    }
                }
                // Persist terminal event based on result
                let term = match &out {
                    Ok(s) => Event::OrchestrationCompleted { output: s.clone() },
                    Err(e) => Event::OrchestrationFailed { error: e.clone() },
                };
                if let Err(e) = self.history_store.append(instance, vec![term]).await {
                    error!(instance, turn_index, error=%e, "failed to append terminal event");
                    if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                        for w in waiters { let _ = w.send((history.clone(), Err(format!("history append failed: {e}")))); }
                    }
                    panic!("history append failed: {e}");
                }
                // Reflect terminal in local history
                let term_local = match &out {
                    Ok(s) => Event::OrchestrationCompleted { output: s.clone() },
                    Err(e) => Event::OrchestrationFailed { error: e.clone() },
                };
                history.push(term_local);
                // Notify any waiters with string result
                if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                    let out_s: Result<String, String> = match &out {
                        Ok(s) => Ok(s.clone()),
                        Err(e) => Err(e.clone()),
                    };
                    for w in waiters {
                        let _ = w.send((history.clone(), out_s.clone()));
                    }
                }
                // If child, enqueue completion to parent
                if let Some((pinst, pid)) = parent_link.clone() {
                    match &out {
                        Ok(s) => {
                            let _ = self.history_store.enqueue_work(WorkItem::SubOrchCompleted { parent_instance: pinst, parent_id: pid, result: s.clone() }).await;
                        }
                        Err(e) => {
                            let _ = self.history_store.enqueue_work(WorkItem::SubOrchFailed { parent_instance: pinst, parent_id: pid, error: e.clone() }).await;
                        }
                    }
                }
                return (history, out);
            }

            // Persist deltas incrementally to avoid duplicates
            let mut persisted_len = baseline_len;
            let mut appended_any = false;
            if history.len() > persisted_len {
                let new_events = history[persisted_len..].to_vec();
                if let Err(e) = self.history_store.append(instance, new_events).await {
                    error!(instance, turn_index, error=%e, "failed to append scheduled events");
                    if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                        for w in waiters { let _ = w.send((history.clone(), Err(format!("history append failed: {e}")))); }
                    }
                    panic!("history append failed: {e}");
                }
                appended_any = true;
                persisted_len = history.len();
            }

            for a in actions {
                match a {
                    Action::ContinueAsNew { .. } => { /* already handled above */ }
                    Action::CallActivity { id, name, input } => {
                        dispatch::dispatch_call_activity(&self, instance, &history, id, name, input).await;
                    }
                    Action::CreateTimer { id, delay_ms } => {
                        dispatch::dispatch_create_timer(&self, instance, &history, id, delay_ms).await;
                    }
                    Action::WaitExternal { id, name } => {
                        dispatch::dispatch_wait_external(&self, instance, &history, id, name).await;
                    }
                    Action::StartOrchestrationDetached { id, name, version, instance: child_inst, input } => {
                        dispatch::dispatch_start_detached(&self, instance, id, name, version, child_inst, input).await;
                    }
                    Action::StartSubOrchestration { id, name, version, instance: child_suffix, input } => {
                        dispatch::dispatch_start_sub_orchestration(&self, instance, &history, id, name, version, child_suffix, input).await;
                    }
                }
            }

            // Receive at least one completion, then drain a bounded batch
            let len_before_completions = history.len();
            let first = comp_rx.recv().await.expect("completion");
            let mut ack_tokens_persist_after: Vec<String> = Vec::new();
            let mut ack_tokens_immediate: Vec<String> = Vec::new();
            if let (Some(t), changed) = completions::append_completion(&mut history, first) { if changed { ack_tokens_persist_after.push(t); } else { ack_tokens_immediate.push(t); } }
            for _ in 0..Self::COMPLETION_BATCH_LIMIT {
                match comp_rx.try_recv() {
                    Ok(msg) => {
                        if let (Some(t), changed) = completions::append_completion(&mut history, msg) {
                            if changed { ack_tokens_persist_after.push(t); } else { ack_tokens_immediate.push(t); }
                        }
                    },
                    Err(_) => break,
                }
            }
            // Ack immediately for messages that resulted in no history change (e.g., dropped externals)
            for t in ack_tokens_immediate.drain(..) { let _ = self.history_store.ack(&t).await; }

            // Persist any further events appended during completion handling
            if history.len() > persisted_len {
                let new_events = history[persisted_len..].to_vec();
                if let Err(e) = self.history_store.append(instance, new_events).await {
                    error!(instance, turn_index, error=%e, "failed to append history");
                    if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                        for w in waiters { let _ = w.send((history.clone(), Err(format!("history append failed: {e}")))); }
                    }
                    panic!("history append failed: {e}");
                }
                appended_any = true;
                // Ack any peek-locked items now that the history is persisted
                for t in ack_tokens_persist_after.drain(..) { let _ = self.history_store.ack(&t).await; }
            } else {
                // No persistence occurred (duplicate completions); ack tokens if any
                for t in ack_tokens_persist_after.drain(..) { let _ = self.history_store.ack(&t).await; }
            }

            // Record which completions were appended in this iteration to validate on the next run_turn
            detect::collect_last_appended(&history, len_before_completions, &mut last_appended_completions);

            // If a cancel request was appended in this batch, terminate deterministically.
            if history.iter().skip(len_before_completions).any(|e| matches!(e, Event::OrchestrationCancelRequested { .. })) {
                let reason = history.iter().rev().find_map(|e| match e { Event::OrchestrationCancelRequested { reason } => Some(reason.clone()), _ => None }).unwrap_or_else(|| "canceled".to_string());
                let term = Event::OrchestrationFailed { error: format!("canceled: {}", reason) };
                if let Err(e) = self.history_store.append(instance, vec![term.clone()]).await { error!(instance, turn_index, error=%e, "failed to append terminal cancel"); panic!("history append failed: {e}"); }
                history.push(term.clone());
                // Notify any waiters with canceled error
                if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                    for w in waiters { let _ = w.send((history.clone(), Err(format!("canceled: {}", reason)))); }
                }
                // Downward propagation: cancel any scheduled sub-orchestrations without completion
                let scheduled_children: Vec<(u64, String)> = history.iter().filter_map(|e| match e {
                    Event::SubOrchestrationScheduled { id, instance: child, .. } => Some((*id, child.clone())), _ => None
                }).collect();
                let completed_ids: std::collections::HashSet<u64> = history.iter().filter_map(|e| match e { Event::SubOrchestrationCompleted { id, .. } | Event::SubOrchestrationFailed { id, .. } => Some(*id), _ => None }).collect();
                for (id, child_suffix) in scheduled_children { if !completed_ids.contains(&id) { let child_full = format!("{}::{}", instance, child_suffix); let _ = self.history_store.enqueue_work(WorkItem::CancelInstance { instance: child_full, reason: "parent canceled".into() }).await; } }
                return (history, Err(format!("canceled: {}", reason)));
            }

            // Validate that each newly appended completion correlates to a start event of the same kind.
            // If a completion's correlation id points to a different kind of start (or none), classify as nondeterministic.
            if !last_appended_completions.is_empty() {
                if let Some(err) = detect::detect_completion_kind_mismatch(&history[..len_before_completions], &last_appended_completions) {
                    let _ = self.history_store.append(instance, vec![Event::OrchestrationFailed { error: err.clone() }]).await;
                    history.push(Event::OrchestrationFailed { error: err.clone() });
                    if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                        for w in waiters { let _ = w.send((history.clone(), Err(err.clone()))); }
                    }
                    return (history, Err(err));
                }
            }

            if appended_any {
                turn_index = turn_index.saturating_add(1);
            }
        }
    }

    /// Spawn an instance and return a handle that resolves to its history
    /// and output when complete.
    pub fn spawn_instance_to_completion(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
    ) -> JoinHandle<(Vec<Event>, Result<String, String>)> {
        let this_for_task = self.clone();
        let inst = instance.to_string();
        let orch_name = orchestration_name.to_string();
        tokio::spawn(async move { this_for_task.run_instance_to_completion(&inst, &orch_name).await })
    }
}

async fn run_timer_worker(mut rx: mpsc::Receiver<TimerWorkItem>, store: Arc<dyn HistoryStore>) {
    while let Some(wi) = rx.recv().await {
        // Real-time sleep based on requested delay
        tokio::time::sleep(std::time::Duration::from_millis(wi.delay_ms)).await;
        let inst = wi.instance.clone();
        let id = wi.id;
        let fire_at = wi.fire_at_ms;
        if let Err(e) = store.enqueue_work(WorkItem::TimerFired { instance: inst.clone(), id, fire_at_ms: fire_at }).await {
            warn!(instance=%inst, id=%id, error=%e, "timer worker: failed to enqueue TimerFired");
        }
    }
}

// moved to completions.rs

impl Runtime {
    /// Raise an external event by name into a running instance.
    pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>) {
        let name_str = name.into();
        let data_str = data.into();
        // Best-effort: only enqueue if a subscription for this name exists in the latest execution
        let hist = self.history_store.read(instance).await;
        let has_subscription = hist.iter().any(|e| matches!(e, Event::ExternalSubscribed { name, .. } if name == &name_str));
        if !has_subscription {
            warn!(instance, event_name=%name_str, "raise_event: dropping external event with no active subscription");
            return;
        }
    if let Err(e) = self.history_store.enqueue_work(WorkItem::ExternalRaised { instance: instance.to_string(), name: name_str.clone(), data: data_str.clone() }).await {
            warn!(instance, name=%name_str, error=%e, "raise_event: failed to enqueue ExternalRaised");
        }
    info!(instance, name=%name_str, data=%data_str, "raise_event: enqueued external");
    }

    /// Request cancellation of a running orchestration instance.
    pub async fn cancel_instance(&self, instance: &str, reason: impl Into<String>) {
        let reason_s = reason.into();
        // Only enqueue if instance is active, else best-effort: provider queue will deliver when it becomes active
        let _ = self.history_store.enqueue_work(WorkItem::CancelInstance { instance: instance.to_string(), reason: reason_s }).await;
    }

    /// Wait until the orchestration reaches a terminal state (Completed/Failed) or the timeout elapses.
    pub async fn wait_for_orchestration(&self, instance: &str, timeout: std::time::Duration) -> Result<OrchestrationStatus, WaitError> {
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
    pub async fn wait_for_orchestration_typed<Out: serde::de::DeserializeOwned>(&self, instance: &str, timeout: std::time::Duration) -> Result<Result<Out, String>, WaitError> {
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
    pub fn wait_for_orchestration_blocking(&self, instance: &str, timeout: std::time::Duration) -> Result<OrchestrationStatus, WaitError> {
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
    pub fn wait_for_orchestration_typed_blocking<Out: serde::de::DeserializeOwned>(&self, instance: &str, timeout: std::time::Duration) -> Result<Result<Out, String>, WaitError> {
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

// moved to completions.rs


