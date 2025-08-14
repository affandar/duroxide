use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use crate::{Action, Event, OrchestrationContext, run_turn};
use crate::providers::HistoryStore;
use crate::providers::in_memory::InMemoryHistoryStore;
use tracing::{debug, warn};

pub mod activity;

#[derive(Clone)]
pub struct ActivityWorkItem { pub instance: String, pub id: u64, pub name: String, pub input: String }

#[derive(Clone)]
pub struct TimerWorkItem { pub instance: String, pub id: u64, pub fire_at_ms: u64, pub delay_ms: u64 }

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
        if let Some(tx) = self.inboxes.lock().await.get(&key) { let _ = tx.send(msg); }
        else { warn!(instance=%key, kind=%kind_of(&msg), "dropping message for unknown instance"); }
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

pub struct Runtime {
    activity_tx: mpsc::Sender<ActivityWorkItem>,
    timer_tx: mpsc::Sender<TimerWorkItem>,
    router_tx: mpsc::UnboundedSender<OrchestratorMsg>,
    router: Arc<CompletionRouter>,
    joins: Mutex<Vec<JoinHandle<()>>>,
    instance_joins: Mutex<Vec<JoinHandle<()>>>,
    history_store: Arc<dyn HistoryStore>,
}

impl Runtime {
    pub async fn start(registry: Arc<activity::ActivityRegistry>) -> Arc<Self> {
        let history_store: Arc<dyn HistoryStore> = Arc::new(InMemoryHistoryStore::default());
        Self::start_with_store(history_store, registry).await
    }

    pub async fn start_with_store(history_store: Arc<dyn HistoryStore>, registry: Arc<activity::ActivityRegistry>) -> Arc<Self> {
        // Install a default subscriber if none set (ok to call many times)
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .try_init();

        let (activity_tx, activity_rx) = mpsc::channel::<ActivityWorkItem>(512);
        let (timer_tx, timer_rx) = mpsc::channel::<TimerWorkItem>(512);
        let (router_tx, mut router_rx) = mpsc::unbounded_channel::<OrchestratorMsg>();
        let router = Arc::new(CompletionRouter { inboxes: Mutex::new(HashMap::new()) });
        let mut joins: Vec<JoinHandle<()>> = Vec::new();

        // spawn activity worker
        let reg_clone = (*registry).clone();
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

        Arc::new(Self { activity_tx, timer_tx, router_tx, router, joins: Mutex::new(joins), instance_joins: Mutex::new(Vec::new()), history_store })
    }

    pub async fn shutdown(self: Arc<Self>) {
        // Abort background tasks; channels will be dropped with Runtime
        let mut joins = self.joins.lock().await;
        for j in joins.drain(..) { j.abort(); }
    }

    pub async fn drain_instances(self: Arc<Self>) {
        let mut joins = self.instance_joins.lock().await;
        while let Some(j) = joins.pop() {
            let _ = j.await;
        }
    }

    pub async fn run_instance_to_completion<O, F, OFut>(
        self: Arc<Self>,
        instance: &str,
        orchestrator: F,
    ) -> (Vec<Event>, O)
    where
        F: Fn(OrchestrationContext) -> OFut + Send + Sync + 'static,
        OFut: std::future::Future<Output = O> + Send + 'static,
        O: Send + 'static,
    {
        // Load existing history from store if any
        let mut history: Vec<Event> = self.history_store.read(instance).await;
        let mut comp_rx = self.router.register(instance).await;

        // Rehydrate pending activities and timers from history
        rehydrate_pending(instance, &history, &self.activity_tx, &self.timer_tx).await;

        loop {
            let baseline_len = history.len();
            let (hist_after, actions, out_opt) = run_turn(history, &orchestrator);
            history = hist_after;
            if let Some(out) = out_opt { return (history, out); }

            for a in actions {
                match a {
                    Action::CallActivity { id, name, input } => {
                        debug!(instance, id, name=%name, "dispatch activity");
                        let _ = self.activity_tx.send(ActivityWorkItem { instance: instance.to_string(), id, name, input }).await;
                    }
                    Action::CreateTimer { id, delay_ms } => {
                        let fire_at_ms = history.iter().rev().find_map(|e| match e {
                            Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None
                        }).unwrap_or(0);
                        debug!(instance, id, fire_at_ms, delay_ms, "dispatch timer");
                        let _ = self.timer_tx.send(TimerWorkItem { instance: instance.to_string(), id, fire_at_ms, delay_ms }).await;
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
                self.history_store.append(instance, new_events).await;
            }
        }
    }

    pub async fn spawn_instance_to_completion<O, F, OFut>(
        self: Arc<Self>,
        instance: &str,
        orchestrator: F,
    ) -> JoinHandle<(Vec<Event>, O)>
    where
        F: Fn(OrchestrationContext) -> OFut + Send + Sync + 'static,
        OFut: std::future::Future<Output = O> + Send + 'static,
        O: Send + 'static,
    {
        let notify = Arc::new(tokio::sync::Notify::new());
        let notify_for_watcher = notify.clone();
        let this_for_task = self.clone();
        let inst = instance.to_string();
        let handle = tokio::spawn(async move {
            let res = this_for_task.run_instance_to_completion::<O, F, OFut>(&inst, orchestrator).await;
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
        let _ = comp_tx.send(OrchestratorMsg::TimerFired { instance: wi.instance, id: wi.id, fire_at_ms: wi.fire_at_ms });
    }
}

fn append_completion(history: &mut Vec<Event>, msg: OrchestratorMsg) {
    match msg {
        OrchestratorMsg::ActivityCompleted { id, result, .. } => history.push(Event::ActivityCompleted { id, result }),
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
    pub async fn raise_event(&self, instance: &str, name: impl Into<String>, data: impl Into<String>) {
        let _ = self.router_tx.send(OrchestratorMsg::ExternalByName {
            instance: instance.to_string(), name: name.into(), data: data.into(),
        });
    }
}

async fn rehydrate_pending(
    instance: &str,
    history: &Vec<Event>,
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
                let _ = activity_tx.send(ActivityWorkItem {
                    instance: instance.to_string(),
                    id: *id,
                    name: name.clone(),
                    input: input.clone(),
                }).await;
            }
        }
    }

    // Re-arm timers that were created but not fired
    for e in history.iter() {
        if let Event::TimerCreated { id, fire_at_ms } = e {
            if !fired_timers.contains(id) {
                // Best-effort remaining delay; if already past, fire immediately (0ms)
                let delay_ms = 0u64.max(*fire_at_ms);
                let _ = timer_tx.send(TimerWorkItem {
                    instance: instance.to_string(),
                    id: *id,
                    fire_at_ms: *fire_at_ms,
                    delay_ms,
                }).await;
            }
        }
    }
}


