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

/// Immutable registry mapping orchestration names to versioned handlers.
#[derive(Clone, Default)]
pub struct OrchestrationRegistry {
    // name -> versions -> handler
    inner: Arc<HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>>,
    // name -> policy (defaults to Latest)
    policy: Arc<tokio::sync::Mutex<HashMap<String, VersionPolicy>>>,
}

#[derive(Clone, Debug)]
pub enum VersionPolicy { Latest, Exact(Version) }

#[derive(Clone, Default)]
pub struct VersionedOrchestrationRegistry {
    // name -> versions -> handler
    inner: Arc<HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>>,
    // name -> policy (defaults to Latest)
    policy: Arc<tokio::sync::Mutex<HashMap<String, VersionPolicy>>>,
}

impl OrchestrationRegistry {
    /// Create a new builder for registering orchestrations.
    pub fn builder() -> OrchestrationRegistryBuilder {
        OrchestrationRegistryBuilder { map: HashMap::new(), policy: HashMap::new() }
    }

    /// Resolve a handler for a start without explicit version using current policy (default Latest).
    pub async fn resolve_for_start(&self, name: &str) -> Option<(Version, Arc<dyn OrchestrationHandler>)> {
        let pol = self.policy.lock().await.get(name).cloned().unwrap_or(VersionPolicy::Latest);
        match pol {
            VersionPolicy::Latest => {
                let m = self.inner.get(name)?;
                let (v, h) = m.iter().next_back()?;
                Some((v.clone(), h.clone()))
            }
            VersionPolicy::Exact(v) => {
                let h = self.inner.get(name)?.get(&v)?.clone();
                Some((v, h))
            }
        }
    }

    /// Look up the latest handler by name (back-compat for replay without version).
    pub fn get(&self, name: &str) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name)?.iter().next_back().map(|(_v, h)| h.clone())
    }

    pub fn resolve_exact(&self, name: &str, v: &Version) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name)?.get(v).cloned()
    }

    pub async fn set_version_policy(&self, name: &str, policy: VersionPolicy) {
        self.policy.lock().await.insert(name.to_string(), policy);
    }
    pub async fn unpin(&self, name: &str) { self.set_version_policy(name, VersionPolicy::Latest).await; }

    pub fn list_orchestration_names(&self) -> Vec<String> { self.inner.keys().cloned().collect() }
    pub fn list_orchestration_versions(&self, name: &str) -> Vec<Version> {
        self.inner.get(name).map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }
}

impl VersionedOrchestrationRegistry {
    pub fn builder() -> VersionedOrchestrationRegistryBuilder { VersionedOrchestrationRegistryBuilder { map: HashMap::new(), policy: HashMap::new() } }
    pub fn get_exact(&self, name: &str, v: &Version) -> Option<Arc<dyn OrchestrationHandler>> {
        self.inner.get(name)?.get(v).cloned()
    }
    pub async fn resolve_for_start(&self, name: &str) -> Option<(Version, Arc<dyn OrchestrationHandler>)> {
        let pol = self.policy.lock().await.get(name).cloned().unwrap_or(VersionPolicy::Latest);
        match pol {
            VersionPolicy::Latest => {
                let m = self.inner.get(name)?;
                let (v, h) = m.iter().next_back()?;
                Some((v.clone(), h.clone()))
            }
            VersionPolicy::Exact(v) => {
                let h = self.inner.get(name)?.get(&v)?.clone();
                Some((v, h))
            }
        }
    }
    pub async fn set_version_policy(&self, name: &str, policy: VersionPolicy) { self.policy.lock().await.insert(name.to_string(), policy); }
    pub async fn unpin(&self, name: &str) { self.set_version_policy(name, VersionPolicy::Latest).await; }
}

pub struct VersionedOrchestrationRegistryBuilder {
    map: HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>,
    policy: HashMap<String, VersionPolicy>,
}

impl VersionedOrchestrationRegistryBuilder {
    pub fn register_versioned<F, Fut>(mut self, name: impl Into<String>, version: impl AsRef<str>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        let name = name.into();
        let v = Version::parse(version.as_ref()).expect("semver");
        self.map.entry(name).or_default().insert(v, Arc::new(FnOrchestration(f)));
        self
    }
    pub fn set_policy(mut self, name: impl Into<String>, policy: VersionPolicy) -> Self {
        self.policy.insert(name.into(), policy);
        self
    }
    pub fn build(self) -> VersionedOrchestrationRegistry {
        VersionedOrchestrationRegistry { inner: Arc::new(self.map), policy: Arc::new(tokio::sync::Mutex::new(self.policy)) }
    }
}

/// Builder for `OrchestrationRegistry`.
pub struct OrchestrationRegistryBuilder {
    map: HashMap<String, std::collections::BTreeMap<Version, Arc<dyn OrchestrationHandler>>>,
    policy: HashMap<String, VersionPolicy>,
}

impl OrchestrationRegistryBuilder {
    /// Register a typed orchestration function. Input/output are serialized internally.
    /// Register a string-IO orchestrator (back-compat)
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        let name = name.into();
        let v = Version::parse("1.0.0").unwrap();
        let entry = self.map.entry(name.clone()).or_default();
        if entry.contains_key(&v) {
            panic!("duplicate orchestration registration: {}@{} (explicitly register a later version)", name, v);
        }
        entry.insert(v, Arc::new(FnOrchestration(f)));
        self
    }

    /// Register a typed orchestrator; serialized internally.
    pub fn register_typed<In, Out, F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        In: DeserializeOwned + Send + 'static,
        Out: Serialize + Send + 'static,
        F: Fn(OrchestrationContext, In) -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = Result<Out, String>> + Send + 'static,
    {
        let f_clone = f.clone();
        let wrapper = move |ctx: OrchestrationContext, input_s: String| {
            let f_inner = f_clone.clone();
            async move {
                let input: In = Json::decode(&input_s)?;
                let out: Out = f_inner(ctx, input).await?;
                Json::encode(&out)
            }
        };
        let name = name.into();
        let v = Version::parse("1.0.0").unwrap();
        self.map.entry(name).or_default().insert(v, Arc::new(FnOrchestration(wrapper)));
        self
    }

    pub fn register_versioned<F, Fut>(mut self, name: impl Into<String>, version: impl AsRef<str>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        let name = name.into();
        let v = Version::parse(version.as_ref()).expect("semver");
        let entry = self.map.entry(name.clone()).or_default();
        if entry.contains_key(&v) {
            panic!("duplicate orchestration registration: {}@{}", name, v);
        }
        if let Some((latest, _)) = entry.iter().next_back() {
            if &v <= latest {
                panic!("non-monotonic orchestration version for {}: {} is not later than existing latest {}", name, v, latest);
            }
        }
        entry.insert(v, Arc::new(FnOrchestration(f)));
        self
    }

    pub fn set_policy(mut self, name: impl Into<String>, policy: VersionPolicy) -> Self {
        self.policy.insert(name.into(), policy);
        self
    }

    /// Finalize and produce an `OrchestrationRegistry`.
    pub fn build(self) -> OrchestrationRegistry {
        OrchestrationRegistry { inner: Arc::new(self.map), policy: Arc::new(tokio::sync::Mutex::new(self.policy)) }
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
    ActivityCompleted { instance: String, id: u64, result: String, ack_token: Option<String> },
    ActivityFailed { instance: String, id: u64, error: String, ack_token: Option<String> },
    TimerFired { instance: String, id: u64, fire_at_ms: u64, ack_token: Option<String> },
    ExternalEvent { instance: String, id: u64, name: String, data: String, ack_token: Option<String> },
    ExternalByName { instance: String, name: String, data: String, ack_token: Option<String> },
    SubOrchCompleted { instance: String, id: u64, result: String, ack_token: Option<String> },
    SubOrchFailed { instance: String, id: u64, error: String, ack_token: Option<String> },
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
    // Pinned versions for instances started in this runtime (in-memory for now)
    pinned_versions: Mutex<HashMap<String, Version>>,
    // queue for start requests so callers only enqueue and runtime manages execution
    start_tx: mpsc::UnboundedSender<StartRequest>,
}

impl Runtime {
    fn detect_frontier_nondeterminism(&self, prior: &[Event], deltas: &[Event]) -> Option<String> {
        let prior_has_any_schedule = prior.iter().any(|e| matches!(
            e,
            Event::ActivityScheduled { .. }
                | Event::TimerCreated { .. }
                | Event::ExternalSubscribed { .. }
                | Event::SubOrchestrationScheduled { .. }
        ));
        let prior_has_any_completion = prior.iter().any(|e| matches!(
            e,
            Event::ActivityCompleted { .. }
                | Event::ActivityFailed { .. }
                | Event::TimerFired { .. }
                | Event::ExternalEvent { .. }
                | Event::SubOrchestrationCompleted { .. }
                | Event::SubOrchestrationFailed { .. }
        ));
        if prior_has_any_schedule && !prior_has_any_completion {
            for e in deltas.iter() {
                let existed_before = match e {
                    Event::ActivityScheduled { name, input, .. } => prior.iter().any(|pe| matches!(pe, Event::ActivityScheduled { name: pn, input: pi, .. } if pn == name && pi == input)),
                    Event::TimerCreated { id: did, .. } => prior.iter().any(|pe| matches!(pe, Event::TimerCreated { id: pid, .. } if pid == did)),
                    Event::ExternalSubscribed { name, .. } => prior.iter().any(|pe| matches!(pe, Event::ExternalSubscribed { name: pn, .. } if pn == name)),
                    Event::SubOrchestrationScheduled { name, input, .. } => prior.iter().any(|pe| matches!(pe, Event::SubOrchestrationScheduled { name: pn, input: pi, .. } if pn == name && pi == input)),
                    _ => true,
                };
                if !existed_before {
                    return Some("nondeterministic: new schedule was introduced at same decision frontier (no new completion)".to_string());
                }
            }
        }
        None
    }

    fn detect_await_mismatch(&self, last: &[(&'static str, u64)], claims: &crate::ClaimedIdsSnapshot) -> Option<String> {
        for (kind, id) in last {
            let ok = match *kind {
                "activity" => claims.activities.contains(id),
                "timer" => claims.timers.contains(id),
                "external" => claims.externals.contains(id),
                "suborchestration" => claims.sub_orchestrations.contains(id),
                _ => true,
            };
            if !ok {
                return Some(format!("nondeterministic: {} completion observed for id={} that current code did not await", kind, id));
            }
        }
        None
    }

    fn collect_last_appended(&self, history: &[Event], start_idx: usize, out: &mut Vec<(&'static str, u64)>) {
        for e in history[start_idx..].iter() {
            match e {
                Event::ActivityCompleted { id, .. } | Event::ActivityFailed { id, .. } => { out.push(("activity", *id)); }
                Event::TimerFired { id, .. } => { out.push(("timer", *id)); }
                Event::ExternalEvent { id, .. } => { out.push(("external", *id)); }
                Event::SubOrchestrationCompleted { id, .. } | Event::SubOrchestrationFailed { id, .. } => { out.push(("suborchestration", *id)); }
                _ => {}
            }
        }
    }

    fn detect_completion_kind_mismatch(&self, prior: &[Event], last: &[(&'static str, u64)]) -> Option<String> {
        for (kind, id) in last {
            let scheduled_kind = prior.iter().find_map(|e| match e {
                Event::ActivityScheduled { id: cid, .. } if cid == id => Some("activity"),
                Event::TimerCreated { id: cid, .. } if cid == id => Some("timer"),
                Event::ExternalSubscribed { id: cid, .. } if cid == id => Some("external"),
                Event::SubOrchestrationScheduled { id: cid, .. } if cid == id => Some("suborchestration"),
                _ => None,
            });
            match scheduled_kind {
                Some(sk) if sk == *kind => {}
                Some(sk) => {
                    return Some(format!(
                        "nondeterministic: completion kind '{}' for id={} does not match scheduled kind '{}'",
                        kind, id, sk
                    ));
                }
                None => {
                    return Some(format!(
                        "nondeterministic: completion kind '{}' for id={} has no matching schedule in prior history",
                        kind, id
                    ));
                }
            }
        }
        None
    }
    /// Enqueue a new orchestration instance start. The runtime will pick this up
    /// in the background and drive it to completion.
    /// Start a typed orchestration; input/output are serialized internally.
    pub async fn start_orchestration_typed<In, Out>(self: Arc<Self>, instance: &str, orchestration_name: &str, input: In) -> Result<JoinHandle<(Vec<Event>, Result<Out, String>)>, String>
    where
        In: Serialize,
        Out: DeserializeOwned + Send + 'static,
    {
        // Ensure instance exists
        let _ = self.history_store.create_instance(instance).await; // best-effort
        // Append start marker if empty
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
            // Only set pin if not already pinned (e.g., by parent for sub-orchestration)
            if !self.pinned_versions.lock().await.contains_key(instance) {
                if let Some((resolved_v, _handler)) = self.orchestration_registry.resolve_for_start(orchestration_name).await {
                    self.pinned_versions.lock().await.insert(instance.to_string(), resolved_v.clone());
                }
            }
            let started = vec![Event::OrchestrationStarted { name: orchestration_name.to_string(), input: payload }];
            self.history_store.append(instance, started).await.map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;
            let _ = self.history_store.set_instance_orchestration(instance, orchestration_name).await;
        } else {
            // Allow duplicate starts as a warning for detached or at-least-once semantics
            warn!(instance, "instance already has history; duplicate start accepted (deduped)");
        }
        // Enqueue a start request; background worker will dedupe and run exactly one execution
        let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orchestration_name.to_string() });
        // Register a oneshot waiter for string result and return a handle decoding to typed Out
        let (tx, rx) = oneshot::channel::<(Vec<Event>, Result<String, String>)>();
        self.result_waiters.lock().await.entry(instance.to_string()).or_default().push(tx);
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
        // Ensure instance exists
        let _ = self.history_store.create_instance(instance).await; // best-effort
        // Append start marker if empty
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            let payload = Json::encode(&input).map_err(|e| format!("encode: {e}"))?;
            self.pinned_versions.lock().await.insert(instance.to_string(), v);
            let started = vec![Event::OrchestrationStarted { name: orchestration_name.to_string(), input: payload }];
            self.history_store.append(instance, started).await.map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;
            let _ = self.history_store.set_instance_orchestration(instance, orchestration_name).await;
        } else {
            warn!(instance, "instance already has history; duplicate start accepted (deduped)");
        }
        let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orchestration_name.to_string() });
        let (tx, rx) = oneshot::channel::<(Vec<Event>, Result<String, String>)>();
        self.result_waiters.lock().await.entry(instance.to_string()).or_default().push(tx);
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
        // Ensure instance exists
        let _ = self.history_store.create_instance(instance).await;
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            if !self.pinned_versions.lock().await.contains_key(instance) {
                if let Some((resolved_v, _handler)) = self.orchestration_registry.resolve_for_start(orchestration_name).await {
                    self.pinned_versions.lock().await.insert(instance.to_string(), resolved_v.clone());
                }
            }
            let started = vec![Event::OrchestrationStarted { name: orchestration_name.to_string(), input: input.into() }];
            self.history_store.append(instance, started).await.map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;
            let _ = self.history_store.set_instance_orchestration(instance, orchestration_name).await;
        } else {
            warn!(instance, "instance already has history; duplicate start accepted (deduped)");
        }
        let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orchestration_name.to_string() });
        let (tx, rx) = oneshot::channel::<(Vec<Event>, Result<String, String>)>();
        self.result_waiters.lock().await.entry(instance.to_string()).or_default().push(tx);
        Ok(tokio::spawn(async move { rx.await.expect("result") }))
    }

    /// Start an orchestration with an explicit version (string I/O).
    pub async fn start_orchestration_versioned(self: Arc<Self>, instance: &str, orchestration_name: &str, version: impl AsRef<str>, input: impl Into<String>) -> Result<JoinHandle<(Vec<Event>, Result<String, String>)>, String> {
        let v = semver::Version::parse(version.as_ref()).map_err(|e| e.to_string())?;
        let _ = self.history_store.create_instance(instance).await;
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            self.pinned_versions.lock().await.insert(instance.to_string(), v);
            let started = vec![Event::OrchestrationStarted { name: orchestration_name.to_string(), input: input.into() }];
            self.history_store.append(instance, started).await.map_err(|e| format!("failed to append OrchestrationStarted: {e}"))?;
            let _ = self.history_store.set_instance_orchestration(instance, orchestration_name).await;
        } else {
            warn!(instance, "instance already has history; duplicate start accepted (deduped)");
        }
        let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orchestration_name.to_string() });
        let (tx, rx) = oneshot::channel::<(Vec<Event>, Result<String, String>)>();
        self.result_waiters.lock().await.entry(instance.to_string()).or_default().push(tx);
        Ok(tokio::spawn(async move { rx.await.expect("result") }))
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

    /// Return the latest execution status for an instance.
    /// For now, this aliases to `get_orchestration_status` until multi-execution storage is implemented.
    pub async fn get_orchestration_status_latest(&self, instance: &str) -> OrchestrationStatus {
        self.get_orchestration_status(instance).await
    }

    /// Return status for a specific execution. Currently single-execution only; `execution_id` is ignored.
    pub async fn get_orchestration_status_with_execution(&self, instance: &str, _execution_id: u64) -> OrchestrationStatus {
        self.get_orchestration_status(instance).await
    }

    /// List all execution ids for an instance. Currently returns a single execution [1].
    pub async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() { Vec::new() } else { vec![1] }
    }

    /// Return execution history for a specific execution id. Currently returns the single history.
    pub async fn get_execution_history(&self, instance: &str, _execution_id: u64) -> Vec<Event> {
        self.history_store.read(instance).await
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
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
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
                    };
                    if let Some(inst) = target_instance {
                        let is_active = rt_for_poll.active_instances.lock().await.contains(&inst);
                        let should_wait = match policy {
                            GatePolicy::RequireActive => !is_active,
                            GatePolicy::RequireInactive => is_active,
                        };
                        if should_wait {
                // Abandon so another poll can retry later
                let _ = rt_for_poll.history_store.abandon(&token).await;
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
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
        rehydrate_pending(instance, &history, &self.activity_tx, &self.timer_tx).await;

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
            if let Some(err) = self.detect_frontier_nondeterminism(&hist_after[..baseline_len], &hist_after[baseline_len..]) {
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
                if let Some(err) = self.detect_await_mismatch(&last_appended_completions, &claims) {
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
                // Append terminal continued-as-new event and create a new execution
                let term = Event::OrchestrationContinuedAsNew { input: input.clone() };
                if let Err(e) = self.history_store.append(instance, vec![term.clone()]).await {
                    error!(instance, turn_index, error=%e, "failed to append continued-as-new");
                    panic!("history append failed: {e}");
                }
                // Reflect terminal CAN in local history for handle consumers
                history.push(term);
                // Resolve target version: explicit arg wins; else use registry policy resolution
                let orch_name = self.history_store.get_instance_orchestration(instance).await
                    .unwrap_or_else(|| orchestration_name.to_string());
                if let Some(ver_str) = version {
                    if let Ok(v) = semver::Version::parse(&ver_str) {
                        self.pinned_versions.lock().await.insert(instance.to_string(), v);
                    }
                } else if let Some((v, _h)) = self.orchestration_registry.resolve_for_start(&orch_name).await {
                    self.pinned_versions.lock().await.insert(instance.to_string(), v);
                }
                // Reset to a new execution and enqueue start
                let _new_exec = self.history_store.reset_for_continue_as_new(instance, &orch_name, &input).await
                    .unwrap_or(1);
                // Enqueue start locally; provider queue not needed for ContinueAsNew and can starve other items
                let _ = self.start_tx.send(StartRequest { instance: instance.to_string(), orchestration_name: orch_name.clone() });
                let _ = input; // already persisted via reset_for_continue_as_new
                // Notify any waiters so initial handle resolves (empty output for continued-as-new)
                if let Some(waiters) = self.result_waiters.lock().await.remove(instance) {
                    for w in waiters { let _ = w.send((history.clone(), Ok(String::new()))); }
                }
                // Treat as terminal for this execution; return empty output
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
                    Action::StartOrchestrationDetached { id, name, version, instance: child_inst, input } => {
                        // Enqueue detached orchestration start for exactly-once start via provider poller
                        // Resolve version pin for the child instance (explicit or policy)
                        if let Some(ver_str) = version.clone() { if let Ok(v) = semver::Version::parse(&ver_str) { self.pinned_versions.lock().await.insert(child_inst.clone(), v); } }
                        else if let Some((v, _h)) = self.orchestration_registry.resolve_for_start(&name).await { self.pinned_versions.lock().await.insert(child_inst.clone(), v); }
                        let wi = crate::providers::WorkItem::StartOrchestration { instance: child_inst.clone(), orchestration: name.clone(), input: input.clone() };
                        if let Err(e) = self.history_store.enqueue_work(wi).await {
                            warn!(instance, id, name=%name, child_instance=%child_inst, error=%e, "failed to enqueue detached start; will rely on bootstrap rehydration");
                        } else {
                            debug!(instance, id, name=%name, child_instance=%child_inst, "enqueued detached orchestration start");
                        }
                    }
                    Action::StartSubOrchestration { id, name, version, instance: child_inst, input } => {
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
                            // Resolve version pin for the child
                            if let Some(ver_str) = version.clone() {
                                if let Ok(v) = semver::Version::parse(&ver_str) {
                                    rt_for_child.pinned_versions.lock().await.insert(child_full.clone(), v);
                                }
                            } else if let Some((v, _h)) = self.orchestration_registry.resolve_for_start(&name).await {
                                rt_for_child.pinned_versions.lock().await.insert(child_full.clone(), v);
                            }
                            // Do not suppress; rely on idempotent start and correlation
                            debug!(instance, id, name=%name, child_instance=%child_full, "start child orchestration");
                            tokio::spawn(async move {
                                match rt_for_child.start_orchestration(&child_full, &name_clone, input_clone).await {
                                    Ok(h) => match h.await {
                                        Ok((_hist, out)) => match out {
                                            Ok(res) => { let _ = router_tx.send(OrchestratorMsg::SubOrchCompleted { instance: parent_inst, id, result: res, ack_token: None }); }
                                            Err(err) => { let _ = router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_inst, id, error: err, ack_token: None }); }
                                        },
                                        Err(e) => { let _ = router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_inst, id, error: format!("child join error: {e}"), ack_token: None }); }
                                    },
                                    Err(e) => { let _ = router_tx.send(OrchestratorMsg::SubOrchFailed { instance: parent_inst, id, error: e, ack_token: None }); }
                                }
                            });
                        }
                    }
                }
            }

            // Receive at least one completion, then drain a bounded batch
            let len_before_completions = history.len();
            let first = comp_rx.recv().await.expect("completion");
            let mut ack_tokens_persist_after: Vec<String> = Vec::new();
            let mut ack_tokens_immediate: Vec<String> = Vec::new();
            if let (Some(t), changed) = append_completion(&mut history, first) { if changed { ack_tokens_persist_after.push(t); } else { ack_tokens_immediate.push(t); } }
            for _ in 0..128 {
                match comp_rx.try_recv() {
                    Ok(msg) => {
                        if let (Some(t), changed) = append_completion(&mut history, msg) {
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
            self.collect_last_appended(&history, len_before_completions, &mut last_appended_completions);

            // Validate that each newly appended completion correlates to a start event of the same kind.
            // If a completion's correlation id points to a different kind of start (or none), classify as nondeterministic.
            if !last_appended_completions.is_empty() {
                if let Some(err) = self.detect_completion_kind_mismatch(&history[..len_before_completions], &last_appended_completions) {
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

fn append_completion(history: &mut Vec<Event>, msg: OrchestratorMsg) -> (Option<String>, bool) {
    match msg {
    OrchestratorMsg::ActivityCompleted { instance, id, result, ack_token } => {
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
            history.push(Event::ActivityCompleted { id, result });
            return (ack_token, true);
        }
        OrchestratorMsg::ActivityFailed { id, error, ack_token, .. } => { history.push(Event::ActivityFailed { id, error }); return (ack_token, true); }
        OrchestratorMsg::TimerFired { id, fire_at_ms, ack_token, .. } => { history.push(Event::TimerFired { id, fire_at_ms }); return (ack_token, true); }
        OrchestratorMsg::ExternalEvent { id, name, data, ack_token, .. } => { history.push(Event::ExternalEvent { id, name, data }); return (ack_token, true); }
        OrchestratorMsg::ExternalByName { instance, name, data, ack_token } => {
            // Find latest subscription id for this name
            if let Some(id) = history.iter().rev().find_map(|e| match e {
                Event::ExternalSubscribed { id, name: n } if n == &name => Some(*id), _ => None
            }) {
                history.push(Event::ExternalEvent { id, name, data });
                return (ack_token, true);
            } else {
                // No matching subscription recorded yet in this execution; drop and warn
                warn!(instance=%instance, event_name=%name, "dropping external event with no active subscription");
            }
            return (ack_token, false);
        }
        OrchestratorMsg::SubOrchCompleted { id, result, ack_token, .. } => { history.push(Event::SubOrchestrationCompleted { id, result }); return (ack_token, true); }
        OrchestratorMsg::SubOrchFailed { id, error, ack_token, .. } => { history.push(Event::SubOrchestrationFailed { id, error }); return (ack_token, true); }
    }
}

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


