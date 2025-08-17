use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, oneshot};
use tokio::task::JoinHandle;
use crate::{Action, Event, OrchestrationContext, run_turn_with};
use crate::providers::{HistoryStore, WorkItem};
use crate::providers::in_memory::InMemoryHistoryStore;
use tracing::{debug, warn, info, error};
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

/// Immutable registry mapping orchestration names to handlers.
#[derive(Clone, Default)]
pub struct OrchestrationRegistry {
    inner: Arc<HashMap<String, Arc<dyn OrchestrationHandler>>>,
}

impl OrchestrationRegistry {
    /// Create a new builder for registering orchestrations.
    pub fn builder() -> OrchestrationRegistryBuilder {
        OrchestrationRegistryBuilder { map: HashMap::new() }
    }

    /// Look up a handler by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name).cloned()
    }
}

/// Builder for `OrchestrationRegistry`.
pub struct OrchestrationRegistryBuilder {
    map: HashMap<String, Arc<dyn OrchestrationHandler>>,
}

impl OrchestrationRegistryBuilder {
    /// Register an orchestration function that returns `Result<String, String>`.
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.map.insert(name.into(), Arc::new(FnOrchestration(f)));
        self
    }

    /// Finalize and produce an `OrchestrationRegistry`.
    pub fn build(self) -> OrchestrationRegistry {
        OrchestrationRegistry { inner: Arc::new(self.map) }
    }
}

/// Work item enqueued to the activity worker.
#[derive(Clone)]
pub struct ActivityWorkItem { pub instance: String, pub id: u64, pub name: String, pub input: String }

/// Work item enqueued to the timer worker.
#[derive(Clone)]
pub struct TimerWorkItem { pub instance: String, pub id: u64, pub fire_at_ms: u64, pub delay_ms: u64 }

/// Messages delivered back to the orchestrator loop by workers and routers.
pub enum OrchestratorMsg {
    ActivityCompleted { instance: String, id: u64, result: String },
    ActivityFailed { instance: String, id: u64, error: String },
    TimerFired { instance: String, id: u64, fire_at_ms: u64 },
    ExternalEvent { instance: String, id: u64, name: String, data: String },
    ExternalByName { instance: String, name: String, data: String },
    SubOrchCompleted { instance: String, id: u64, result: String },
    SubOrchFailed { instance: String, id: u64, error: String },
}

/// Request to start a new orchestration instance.
#[derive(Clone, Debug)]
struct StartRequest { instance: String, orchestration_name: String }

struct CompletionRouter { inboxes: Mutex<HashMap<String, mpsc::UnboundedSender<OrchestratorMsg>>> }

impl CompletionRouter {
    async fn register(&self, instance: &str) -> mpsc::UnboundedReceiver<OrchestratorMsg> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inboxes.lock().await.insert(instance.to_string(), tx);
        rx
    }
    async fn forward(&self, msg: OrchestratorMsg) {
        let key = match &msg {
            OrchestratorMsg::ActivityCompleted { instance, .. }
            | OrchestratorMsg::ActivityFailed { instance, .. }
            | OrchestratorMsg::TimerFired { instance, .. }
            | OrchestratorMsg::ExternalEvent { instance, .. }
            | OrchestratorMsg::ExternalByName { instance, .. }
            | OrchestratorMsg::SubOrchCompleted { instance, .. }
            | OrchestratorMsg::SubOrchFailed { instance, .. } => instance.clone(),
        };
        let kind = kind_of(&msg);
        if let Some(tx) = self.inboxes.lock().await.get(&key) {
            if let Err(_e) = tx.send(msg) {
                warn!(instance=%key, kind=%kind, "router: receiver dropped, dropping message");
            }
        } else {
            warn!(instance=%key, kind=%kind, "router: unknown instance, dropping message");
        }
    }
}

fn kind_of(msg: &OrchestratorMsg) -> &'static str {
    match msg {
        OrchestratorMsg::ActivityCompleted { .. } => "ActivityCompleted",
        OrchestratorMsg::ActivityFailed { .. } => "ActivityFailed",
        OrchestratorMsg::TimerFired { .. } => "TimerFired",
        OrchestratorMsg::ExternalEvent { .. } => "ExternalEvent",
        OrchestratorMsg::ExternalByName { .. } => "ExternalByName",
        OrchestratorMsg::SubOrchCompleted { .. } => "SubOrchCompleted",
        OrchestratorMsg::SubOrchFailed { .. } => "SubOrchFailed",
    }
}

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
    // queue for start requests so callers only enqueue and runtime manages execution
    start_tx: mpsc::UnboundedSender<StartRequest>,
}

impl Runtime {
    /// Enqueue a new orchestration instance start. The runtime will pick this up
    /// in the background and drive it to completion.
    pub async fn start_orchestration(self: Arc<Self>, instance: &str, orchestration_name: &str, input: impl Into<String>) -> Result<JoinHandle<(Vec<Event>, Result<String, String>)>, String> {
        // Ensure instance exists
        let _ = self.history_store.create_instance(instance).await; // best-effort
        // Append start marker if empty
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            let started = vec![Event::OrchestrationStarted { name: orchestration_name.to_string(), input: input.into() }];
            self.history_store.append(instance, started).await.map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;
            // best-effort: record orchestration name metadata for provider bootstrap
            let _ = self.history_store.set_instance_orchestration(instance, orchestration_name).await;
        } else {
            // Allow duplicate starts as a warning for detached or at-least-once semantics
            warn!(instance, "instance already has history; duplicate start accepted (deduped)");
        }
        // Enqueue a start request; background worker will dedupe and run exactly one execution
        let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orchestration_name.to_string() });
        // Register a oneshot waiter for the result and return a handle awaiting it
        let (tx, rx) = oneshot::channel::<(Vec<Event>, Result<String, String>)>();
        self.result_waiters.lock().await.entry(instance.to_string()).or_default().push(tx);
        Ok(tokio::spawn(async move { rx.await.expect("result")}))
    }

    /// Returns the current status of an orchestration instance by inspecting its history.
    pub async fn get_orchestration_status(&self, instance: &str) -> OrchestrationStatus {
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            return OrchestrationStatus::NotFound;
        }
        for e in hist.iter().rev() {
            match e {
                Event::OrchestrationFailed { error } => return OrchestrationStatus::Failed { error: error.clone() },
                Event::OrchestrationCompleted { output } => return OrchestrationStatus::Completed { output: output.clone() },
                _ => {}
            }
        }
        OrchestrationStatus::Running
    }
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
        builder = builder.register("__system_trace", |input: String| async move {
            // input format: "LEVEL:message"
            Ok(input)
        });
        builder = builder.register("__system_now", |_input: String| async move {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            Ok(now_ms.to_string())
        });
        builder = builder.register("__system_new_guid", |_input: String| async move {
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

        let runtime = Arc::new(Self { activity_tx, timer_tx, router_tx, router, joins: Mutex::new(joins), instance_joins: Mutex::new(Vec::new()), history_store, active_instances: Mutex::new(HashSet::new()), pending_starts: Mutex::new(HashSet::new()), result_waiters: Mutex::new(HashMap::new()), orchestration_registry, start_tx });

        // background worker to process start requests
        let rt_for_worker = runtime.clone();
        let worker = tokio::spawn(async move {
            while let Some(req) = start_rx.recv().await {
                let inst = req.instance.clone();
                let orch = req.orchestration_name.clone();
                // Deduplicate: skip if already pending or active
                {
                    let mut pend = rt_for_worker.pending_starts.lock().await;
                    if pend.contains(&inst) || rt_for_worker.active_instances.lock().await.contains(&inst) {
                        warn!(instance=%inst, orchestration=%orch, "start worker: duplicate start suppressed");
                        continue;
                    }
                    pend.insert(inst.clone());
                }
                // Spawn to completion without awaiting so we can process more starts
                let _handle = rt_for_worker.clone().spawn_instance_to_completion(&inst, &orch);
                // Clear pending flag immediately; active guard prevents double-run
                rt_for_worker.pending_starts.lock().await.remove(&inst);
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
                if let Some(item) = rt_for_poll.history_store.dequeue_work().await {
                    // Helper to check if instance is active; if not, re-enqueue the item to avoid drops
                    let mut maybe_forward = true;
                    let (target_instance, reenqueue): (Option<String>, Option<WorkItem>) = match &item {
                        WorkItem::StartOrchestration { instance, orchestration } => {
                            let _ = orchestration; // unused here
                            (None, None)
                        }
                        WorkItem::ActivityCompleted { instance, id, result } => {
                            (Some(instance.clone()), Some(WorkItem::ActivityCompleted { instance: instance.clone(), id: *id, result: result.clone() }))
                        }
                        WorkItem::ActivityFailed { instance, id, error } => {
                            (Some(instance.clone()), Some(WorkItem::ActivityFailed { instance: instance.clone(), id: *id, error: error.clone() }))
                        }
                        WorkItem::TimerFired { instance, id, fire_at_ms } => {
                            (Some(instance.clone()), Some(WorkItem::TimerFired { instance: instance.clone(), id: *id, fire_at_ms: *fire_at_ms }))
                        }
                        WorkItem::ExternalRaised { instance, name, data } => {
                            (Some(instance.clone()), Some(WorkItem::ExternalRaised { instance: instance.clone(), name: name.clone(), data: data.clone() }))
                        }
                        WorkItem::SubOrchCompleted { parent_instance, parent_id, result } => {
                            (Some(parent_instance.clone()), Some(WorkItem::SubOrchCompleted { parent_instance: parent_instance.clone(), parent_id: *parent_id, result: result.clone() }))
                        }
                        WorkItem::SubOrchFailed { parent_instance, parent_id, error } => {
                            (Some(parent_instance.clone()), Some(WorkItem::SubOrchFailed { parent_instance: parent_instance.clone(), parent_id: *parent_id, error: error.clone() }))
                        }
                    };
                    if let Some(inst) = target_instance {
                        let is_active = rt_for_poll.active_instances.lock().await.contains(&inst);
                        if !is_active {
                            if let Some(it) = reenqueue {
                                let _ = rt_for_poll.history_store.enqueue_work(it).await;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                            maybe_forward = false;
                        }
                    }
                    if !maybe_forward { continue; }
                    match item {
                        WorkItem::StartOrchestration { instance, orchestration } => {
                            // Push to local start queue only; do not re-enqueue to provider to avoid loops
                            let _ = rt_for_poll.start_tx.send(StartRequest { instance, orchestration_name: orchestration });
                        }
                        WorkItem::ActivityCompleted { instance, id, result } => {
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::ActivityCompleted { instance, id, result });
                        }
                        WorkItem::ActivityFailed { instance, id, error } => {
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::ActivityFailed { instance, id, error });
                        }
                        WorkItem::TimerFired { instance, id, fire_at_ms } => {
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::TimerFired { instance, id, fire_at_ms });
                        }
                        WorkItem::ExternalRaised { instance, name, data } => {
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::ExternalByName { instance, name, data });
                        }
                        WorkItem::SubOrchCompleted { parent_instance, parent_id, result } => {
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::SubOrchCompleted { instance: parent_instance, id: parent_id, result });
                        }
                        WorkItem::SubOrchFailed { parent_instance, parent_id, error } => {
                            let _ = rt_for_poll.router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_instance, id: parent_id, error });
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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
        // Look up the orchestration handler
        let orchestration_handler = self.orchestration_registry.get(orchestration_name)
            .unwrap_or_else(|| panic!("orchestration not found: {orchestration_name}"));

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
        // Load existing history from store (now exists)
        let mut history: Vec<Event> = self.history_store.read(instance).await;
        let mut comp_rx = self.router.register(instance).await;

        // Rehydrate pending activities and timers from history
        rehydrate_pending(instance, &history, &self.activity_tx, &self.timer_tx).await;

        // capture input from OrchestrationStarted if present
        let input = history.iter().find_map(|e| match e {
            Event::OrchestrationStarted { name: n, input } if n == orchestration_name => Some(input.clone()),
            _ => None,
        }).unwrap_or_default();
        // If this is a child (ParentLinked exists), remember linkage for notifying parent at terminal
        let parent_link = history.iter().find_map(|e| match e {
            Event::ParentLinked { parent_instance, parent_id } => Some((parent_instance.clone(), *parent_id)),
            _ => None,
        });

        let orchestrator_fn = |ctx: OrchestrationContext| {
            let handler = orchestration_handler.clone();
            let input_clone = input.clone();
            async move { handler.invoke(ctx, input_clone).await }
        };

        let mut turn_index: u64 = 0;
        loop {
            let baseline_len = history.len();
            let (hist_after, actions, _logs, out_opt) = run_turn_with(history, turn_index, &orchestrator_fn);
            history = hist_after;
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
                // Best-effort dispatch for detached orchestration starts that were emitted in this final turn
                for e in deltas {
                    if let Event::OrchestrationChained { id, name, instance: child_inst, input } = e {
                        match self.clone().start_orchestration(&child_inst, &name, input).await {
                            Ok(_h) => { debug!(instance, id, name=%name, child_instance=%child_inst, "started detached orchestration (final turn)"); }
                            Err(err) => { warn!(instance, id, name=%name, child_instance=%child_inst, error=%err, "detached start failed (final turn)"); }
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
                // Notify any waiters
                if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                    for w in waiters {
                        let _ = w.send((history.clone(), out.clone()));
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
                    Action::CallActivity { id, name, input } => {
                        // If this activity already has a completion in history, skip dispatch (idempotent)
                        let already_done = history.iter().rev().any(|e| match e {
                            Event::ActivityCompleted { id: cid, .. } if *cid == id => true,
                            Event::ActivityFailed { id: cid, .. } if *cid == id => true,
                            _ => false,
                        });
                        if already_done {
                            debug!(instance, id, name=%name, "skip dispatch: activity already completed/failed");
                        } else {
                            debug!(instance, id, name=%name, "dispatch activity");
                            if let Err(e) = self.activity_tx.send(ActivityWorkItem { instance: instance.to_string(), id, name, input }).await {
                                panic!("activity dispatch failed: {e}");
                            }
                        }
                    }
                    Action::CreateTimer { id, delay_ms } => {
                        // If this timer already fired, skip dispatch
                        let already_fired = history.iter().rev().any(|e| matches!(e, Event::TimerFired { id: cid, .. } if *cid == id));
                        if already_fired {
                            debug!(instance, id, "skip dispatch: timer already fired");
                            continue;
                        }
                        let fire_at_ms = history.iter().rev().find_map(|e| match e {
                            Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None
                        }).unwrap_or(0);
                        debug!(instance, id, fire_at_ms, delay_ms, "dispatch timer");
                        if let Err(e) = self.timer_tx.send(TimerWorkItem { instance: instance.to_string(), id, fire_at_ms, delay_ms }).await {
                            if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                                for w in waiters { let _ = w.send((history.clone(), Err(format!("timer dispatch failed: {e}")))); }
                            }
                            panic!("timer dispatch failed: {e}");
                        }
                    }
                    Action::WaitExternal { id, name } => {
                        debug!(instance, id, name=%name, "subscribe external");
                        let _ = (id, name); // no-op
                    }
                    Action::StartOrchestrationDetached { id, name, instance: child_inst, input } => {
                        // Fire-and-forget: try to start a new orchestration instance using the provided instance id (no parent prefix).
                        match self.clone().start_orchestration(&child_inst, &name, input).await {
                            Ok(_h) => { debug!(instance, id, name=%name, child_instance=%child_inst, "started detached orchestration"); }
                            Err(e) => { warn!(instance, id, name=%name, child_instance=%child_inst, error=%e, "detached start failed (likely duplicate instance)"); }
                        }
                    }
                    Action::StartSubOrchestration { id, name, instance: child_inst, input } => {
                        let already_done = history.iter().rev().any(|e|
                            matches!(e, Event::SubOrchestrationCompleted { id: cid, .. } if *cid == id)
                            || matches!(e, Event::SubOrchestrationFailed { id: cid, .. } if *cid == id)
                        );
                        if already_done {
                            debug!(instance, id, name=%name, "skip dispatch: sub-orch already completed/failed");
                        } else {
                            // Make globally unique child instance name based on parent instance and deterministic child suffix
                            let child_full = format!("{}::{}", instance, child_inst);
                            let parent_inst = instance.to_string();
                            let name_clone = name.clone();
                            let input_clone = input.clone();
                            let router_tx = self.router_tx.clone();
                            let rt_for_child = self.clone();
                            debug!(instance, id, name=%name, child_instance=%child_full, "start child orchestration");
                            tokio::spawn(async move {
                                match rt_for_child.start_orchestration(&child_full, &name_clone, input_clone).await {
                                    Ok(h) => match h.await {
                                        Ok((_hist, out)) => match out {
                                            Ok(res) => { let _ = router_tx.send(OrchestratorMsg::SubOrchCompleted { instance: parent_inst, id, result: res }); }
                                            Err(err) => { let _ = router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_inst, id, error: err }); }
                                        },
                                        Err(e) => { let _ = router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_inst, id, error: format!("child join error: {e}") }); }
                                    },
                                    Err(e) => { let _ = router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_inst, id, error: e }); }
                                }
                            });
                        }
                    }
                }
            }

            // Receive at least one completion, then drain a bounded batch
            let first = comp_rx.recv().await.expect("completion");
            append_completion(&mut history, first);
            for _ in 0..128 {
                match comp_rx.try_recv() {
                    Ok(msg) => append_completion(&mut history, msg),
                    Err(_) => break,
                }
            }

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

fn append_completion(history: &mut Vec<Event>, msg: OrchestratorMsg) {
    match msg {
        OrchestratorMsg::ActivityCompleted { instance, id, result } => {
            // If this completion corresponds to system trace, emit to tracing only
            let is_system_trace = history.iter().rev().any(|e| matches!(e, Event::ActivityScheduled { id: cid, name, .. } if *cid == id && name == "__system_trace"));
            if is_system_trace {
                if let Some((lvl, msg_text)) = result.split_once(':') {
                    match lvl {
                        "ERROR" | "error" => error!(instance=%instance, id, "{}", msg_text),
                        "WARN" | "warn" | "WARNING" | "warning" => warn!(instance=%instance, id, "{}", msg_text),
                        _ => info!(instance=%instance, id, "{}", msg_text),
                    }
                } else {
                    info!(instance=%instance, id, "{}", result);
                }
            }
            history.push(Event::ActivityCompleted { id, result })
        }
        OrchestratorMsg::ActivityFailed { id, error, .. } => history.push(Event::ActivityFailed { id, error }),
        OrchestratorMsg::TimerFired { id, fire_at_ms, .. } => history.push(Event::TimerFired { id, fire_at_ms }),
        OrchestratorMsg::ExternalEvent { id, name, data, .. } => history.push(Event::ExternalEvent { id, name, data }),
        OrchestratorMsg::ExternalByName { instance: _, name, data } => {
            // Find latest subscription id for this name
            if let Some(id) = history.iter().rev().find_map(|e| match e {
                Event::ExternalSubscribed { id, name: n } if n == &name => Some(*id), _ => None
            }) {
                history.push(Event::ExternalEvent { id, name, data });
            }
        }
        OrchestratorMsg::SubOrchCompleted { id, result, .. } => history.push(Event::SubOrchestrationCompleted { id, result }),
        OrchestratorMsg::SubOrchFailed { id, error, .. } => history.push(Event::SubOrchestrationFailed { id, error }),
    }
}

impl Runtime {
    /// Raise an external event by name into a running instance.
    pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>) {
        let name_str = name.into();
        let data_str = data.into();
        if let Err(e) = self.history_store.enqueue_work(WorkItem::ExternalRaised { instance: instance.to_string(), name: name_str.clone(), data: data_str }).await {
            warn!(instance, name=%name_str, error=%e, "raise_event: failed to enqueue ExternalRaised");
        }
    }
}

async fn rehydrate_pending(
    instance: &str,
    history: &[Event],
    activity_tx: &mpsc::Sender<ActivityWorkItem>,
    timer_tx: &mpsc::Sender<TimerWorkItem>,
) {
    use std::collections::HashSet;
    let mut completed_activities: HashSet<u64> = HashSet::new();
    let mut fired_timers: HashSet<u64> = HashSet::new();

    for e in history.iter() {
        match e {
            Event::ActivityCompleted { id, .. } => { completed_activities.insert(*id); }
            Event::TimerFired { id, .. } => { fired_timers.insert(*id); }
            _ => {}
        }
    }

    // Re-enqueue activities that were scheduled but not completed
    for e in history.iter() {
        if let Event::ActivityScheduled { id, name, input } = e {
            if !completed_activities.contains(id) {
                if let Err(e) = activity_tx.send(ActivityWorkItem {
                    instance: instance.to_string(),
                    id: *id,
                    name: name.clone(),
                    input: input.clone(),
                }).await {
                    warn!(instance, id=%id, name=%name, error=%e, "rehydrate: failed to enqueue activity");
                }
            }
        }
    }

    // Re-arm timers that were created but not fired
    for e in history.iter() {
        if let Event::TimerCreated { id, fire_at_ms } = e {
            if !fired_timers.contains(id) {
                // Compute remaining delay relative to current time; if past due, fire immediately (0ms)
                let now_ms: u64 = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let delay_ms = fire_at_ms.saturating_sub(now_ms);
                if let Err(e) = timer_tx.send(TimerWorkItem {
                    instance: instance.to_string(),
                    id: *id,
                    fire_at_ms: *fire_at_ms,
                    delay_ms,
                }).await {
                    warn!(instance, id=%id, error=%e, "rehydrate: failed to enqueue timer");
                }
            }
        }
    }
}


