use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use crate::{Action, Event, OrchestrationContext, run_turn_with};
use crate::providers::HistoryStore;
use crate::providers::in_memory::InMemoryHistoryStore;
use tracing::{debug, warn, info, error};
use std::collections::HashSet;
use async_trait::async_trait;

/// Runtime components: activity worker and registry utilities.
pub mod activity;

/// Trait implemented by orchestration handlers that can be invoked by the runtime.
#[async_trait]
pub trait OrchestrationHandler: Send + Sync {
    async fn invoke(&self, ctx: OrchestrationContext) -> String;
}

/// Function wrapper that implements `OrchestrationHandler`.
pub struct FnOrchestration<F, Fut>(pub F)
where
    F: Fn(OrchestrationContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = String> + Send + 'static;

#[async_trait]
impl<F, Fut> OrchestrationHandler for FnOrchestration<F, Fut>
where
    F: Fn(OrchestrationContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = String> + Send + 'static,
{
    async fn invoke(&self, ctx: OrchestrationContext) -> String {
        (self.0)(ctx).await
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
    /// Register an orchestration function that returns a `String`.
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(OrchestrationContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = String> + Send + 'static,
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
}

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
            | OrchestratorMsg::ExternalByName { instance, .. } => instance.clone(),
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
    orchestration_registry: OrchestrationRegistry,
}

impl Runtime {
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
        builder = builder.register_result("__system_trace", |input: String| async move {
            // input format: "LEVEL:message"
            Ok(input)
        });
        builder = builder.register_result("__system_now", |_input: String| async move {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            Ok(now_ms.to_string())
        });
        builder = builder.register_result("__system_new_guid", |_input: String| async move {
            // Pseudo-guid: 32-hex digits from current nanos since epoch
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Ok(format!("{nanos:032x}"))
        });
        let reg_clone = builder.build();
        let rt_tx = router_tx.clone();
        joins.push(tokio::spawn(async move {
            activity::ActivityWorker::new(reg_clone, rt_tx).run(activity_rx).await
        }));

        // spawn timer worker
        let rt_tx2 = router_tx.clone();
        joins.push(tokio::spawn(async move { run_timer_worker(timer_rx, rt_tx2).await }));

        // spawn router forwarding task
        let router_clone = router.clone();
        joins.push(tokio::spawn(async move {
            while let Some(msg) = router_rx.recv().await { router_clone.forward(msg).await; }
        }));

        Arc::new(Self { activity_tx, timer_tx, router_tx, router, joins: Mutex::new(joins), instance_joins: Mutex::new(Vec::new()), history_store, active_instances: Mutex::new(HashSet::new()), orchestration_registry })
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
    ) -> (Vec<Event>, String) {
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
        // Ensure instance exists; create if missing; propagate error
        if let Err(e) = self.history_store.create_instance(instance).await {
            panic!("failed to create instance {instance}: {e}");
        }
        // Load existing history from store (now exists)
        let mut history: Vec<Event> = self.history_store.read(instance).await;
        let mut comp_rx = self.router.register(instance).await;

        // Rehydrate pending activities and timers from history
        rehydrate_pending(instance, &history, &self.activity_tx, &self.timer_tx).await;

        let orchestrator_fn = |ctx: OrchestrationContext| {
            let handler = orchestration_handler.clone();
            async move { handler.invoke(ctx).await }
        };

        let mut turn_index: u64 = 0;
        loop {
            let baseline_len = history.len();
            let (hist_after, actions, _logs, out_opt) = run_turn_with(history, turn_index, &orchestrator_fn);
            history = hist_after;
            if let Some(out) = out_opt { return (history, out); }

            for a in actions {
                match a {
                    Action::CallActivity { id, name, input } => {
                        debug!(instance, id, name=%name, "dispatch activity");
                        if let Err(e) = self.activity_tx.send(ActivityWorkItem { instance: instance.to_string(), id, name, input }).await {
                            panic!("activity dispatch failed: {e}");
                        }
                    }
                    Action::CreateTimer { id, delay_ms } => {
                        let fire_at_ms = history.iter().rev().find_map(|e| match e {
                            Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None
                        }).unwrap_or(0);
                        debug!(instance, id, fire_at_ms, delay_ms, "dispatch timer");
                        if let Err(e) = self.timer_tx.send(TimerWorkItem { instance: instance.to_string(), id, fire_at_ms, delay_ms }).await {
                            panic!("timer dispatch failed: {e}");
                        }
                    }
                    Action::WaitExternal { id, name } => {
                        debug!(instance, id, name=%name, "subscribe external");
                        let _ = (id, name); // no-op
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

            // Persist new events appended during this turn
            if history.len() > baseline_len {
                let new_events = history[baseline_len..].to_vec();
                if let Err(e) = self.history_store.append(instance, new_events).await {
                    error!(instance, turn_index, error=%e, "failed to append history");
                    // Surface as panic for now to preserve determinism
                    panic!("history append failed: {e}");
                }
                turn_index = turn_index.saturating_add(1);
            }
        }
    }

    /// Spawn an instance and return a handle that resolves to its history
    /// and output when complete.
    pub async fn spawn_instance_to_completion(
        self: Arc<Self>,
        instance: &str,
        orchestration_name: &str,
    ) -> JoinHandle<(Vec<Event>, String)> {
        let notify = Arc::new(tokio::sync::Notify::new());
        let notify_for_watcher = notify.clone();
        let this_for_task = self.clone();
        let inst = instance.to_string();
        let orch_name = orchestration_name.to_string();
        let handle = tokio::spawn(async move {
            let res = this_for_task.run_instance_to_completion(&inst, &orch_name).await;
            notify.notify_waiters();
            res
        });
        // watcher task to allow draining all instances generically
        let watcher = tokio::spawn(async move { notify_for_watcher.notified().await; });
        self.instance_joins.lock().await.push(watcher);
        handle
    }
}

async fn run_timer_worker(mut rx: mpsc::Receiver<TimerWorkItem>, comp_tx: mpsc::UnboundedSender<OrchestratorMsg>) {
    while let Some(wi) = rx.recv().await {
        // Real-time sleep based on requested delay
        tokio::time::sleep(std::time::Duration::from_millis(wi.delay_ms)).await;
        let inst = wi.instance.clone();
        let id = wi.id;
        let fire_at = wi.fire_at_ms;
        if let Err(_e) = comp_tx.send(OrchestratorMsg::TimerFired { instance: inst.clone(), id, fire_at_ms: fire_at }) {
            warn!(instance=%inst, id=%id, "timer worker: router receiver dropped, dropping TimerFired");
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
    }
}

impl Runtime {
    /// Raise an external event by name into a running instance.
    pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>) {
        let name_str = name.into();
        let data_str = data.into();
        if let Err(_e) = self.router_tx.send(OrchestratorMsg::ExternalByName {
            instance: instance.to_string(), name: name_str.clone(), data: data_str,
        }) {
            warn!(instance, name=%name_str, "raise_event: router dropped, dropping ExternalByName");
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
                // Best-effort remaining delay; if already past, fire immediately (0ms)
                let delay_ms = *fire_at_ms;
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


