use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;
use async_trait::async_trait;

use super::{ActivityWorkItem, OrchestratorMsg};

/// Trait implemented by activity handlers that can be invoked by the runtime.
#[async_trait]
pub trait ActivityHandler: Send + Sync {
    async fn invoke(&self, input: String) -> Result<String, String>;
}

pub struct FnActivity<F, Fut>(pub F)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static;

#[async_trait]
impl<F, Fut> ActivityHandler for FnActivity<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    async fn invoke(&self, input: String) -> Result<String, String> { (self.0)(input).await }
}

/// Immutable registry mapping activity names to handlers.
#[derive(Clone, Default)]
pub struct ActivityRegistry { inner: Arc<HashMap<String, Arc<dyn ActivityHandler>>> }

impl ActivityRegistry {
    /// Create a new builder for registering activities.
    pub fn builder() -> ActivityRegistryBuilder { ActivityRegistryBuilder { map: HashMap::new() } }
    /// Look up a handler by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ActivityHandler>> { self.inner.get(name).cloned() }
}

/// Builder for `ActivityRegistry`.
pub struct ActivityRegistryBuilder { map: HashMap<String, Arc<dyn ActivityHandler>> }

impl ActivityRegistryBuilder {
    /// Initialize a new builder from an existing registry.
    pub fn from_registry(reg: &ActivityRegistry) -> Self {
        let mut map: HashMap<String, Arc<dyn ActivityHandler>> = HashMap::new();
        for (k, v) in reg.inner.iter() { map.insert(k.clone(), v.clone()); }
        ActivityRegistryBuilder { map }
    }
    // Convenience: register an activity whose future yields String (treated as Ok)
    /// Register a function as an activity that returns a `String`.
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = String> + Send + 'static,
    {
        self.map.insert(name.into(), Arc::new(FnActivity(move |input: String| {
            let fut = f(input);
            async move {
                let s = fut.await;
                Ok::<String, String>(s)
            }
        })));
        self
    }

    // Typed: register an activity that returns Result<String, String>
    /// Register a function as an activity that returns `Result<String, String>`.
    pub fn register_result<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.map.insert(name.into(), Arc::new(FnActivity(f)));
        self
    }
    /// Finalize and produce an `ActivityRegistry`.
    pub fn build(self) -> ActivityRegistry { ActivityRegistry { inner: Arc::new(self.map) } }
}

/// Worker that receives `ActivityWorkItem`s and executes registered handlers,
/// reporting results via the orchestrator message channel.
pub struct ActivityWorker {
    registry: ActivityRegistry,
    completion_tx: mpsc::UnboundedSender<OrchestratorMsg>,
}

impl ActivityWorker {
    /// Create a new `ActivityWorker` with the given registry and completion channel.
    pub fn new(registry: ActivityRegistry, completion_tx: mpsc::UnboundedSender<OrchestratorMsg>) -> Self {
        Self { registry, completion_tx }
    }
    /// Run the worker loop until the input channel is closed.
    pub async fn run(self, mut rx: mpsc::Receiver<ActivityWorkItem>) {
        while let Some(wi) = rx.recv().await {
            if let Some(handler) = self.registry.get(&wi.name) {
                match handler.invoke(wi.input).await {
                    Ok(result) => {
                        if let Err(_e) = self.completion_tx.send(OrchestratorMsg::ActivityCompleted { instance: wi.instance, id: wi.id, result }) {
                            panic!("activity worker: router dropped while sending completion (id={})", wi.id);
                        }
                    }
                    Err(error) => {
                        if let Err(_e) = self.completion_tx.send(OrchestratorMsg::ActivityFailed { instance: wi.instance, id: wi.id, error }) {
                            panic!("activity worker: router dropped while sending failure (id={})", wi.id);
                        }
                    }
                }
            } else if let Err(_e) = self.completion_tx.send(OrchestratorMsg::ActivityFailed {
                instance: wi.instance,
                id: wi.id,
                error: format!("unregistered:{}", wi.name),
            }) {
                panic!("activity worker: router dropped while sending unregistered failure (id={})", wi.id);
            }
        }
    }
}



