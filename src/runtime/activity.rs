use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;
use async_trait::async_trait;

use super::ActivityWorkItem;
use crate::providers::{HistoryStore, WorkItem, QueueKind};
use crate::_typed_codec::{Json, Codec};
use serde::{de::DeserializeOwned, Serialize};

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
    /// Register a string-IO activity (back-compat).
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.map.insert(name.into(), Arc::new(FnActivity(f)));
        self
    }

    /// Register a typed activity function. Input/output are serialized internally.
    pub fn register_typed<In, Out, F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        In: DeserializeOwned + Send + 'static,
        Out: Serialize + Send + 'static,
        F: Fn(In) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Out, String>> + Send + 'static,
    {
        let f_clone = std::sync::Arc::new(f);
        let wrapper = move |input_s: String| {
            let f_inner = f_clone.clone();
            async move {
                let input: In = Json::decode(&input_s)?;
                let out: Out = (f_inner)(input).await?;
                Json::encode(&out)
            }
        };
        self.map.insert(name.into(), Arc::new(FnActivity(wrapper)));
        self
    }
    /// Finalize and produce an `ActivityRegistry`.
    pub fn build(self) -> ActivityRegistry { ActivityRegistry { inner: Arc::new(self.map) } }
}

/// Worker that receives `ActivityWorkItem`s and executes registered handlers,
/// reporting results via the orchestrator message channel.
pub struct ActivityWorker {
    registry: ActivityRegistry,
    history_store: std::sync::Arc<dyn HistoryStore>,
}

impl ActivityWorker {
    /// Create a new `ActivityWorker` with the given registry and completion channel.
    pub fn new(registry: ActivityRegistry, history_store: std::sync::Arc<dyn HistoryStore>) -> Self {
        Self { registry, history_store }
    }
    /// Run the worker loop until the input channel is closed.
    pub async fn run(self, mut rx: mpsc::Receiver<ActivityWorkItem>) {
        while let Some(wi) = rx.recv().await {
            if let Some(handler) = self.registry.get(&wi.name) {
                match handler.invoke(wi.input).await {
                    Ok(result) => {
                        if let Err(e) = self.history_store.enqueue_work(QueueKind::Orchestrator, WorkItem::ActivityCompleted { instance: wi.instance, id: wi.id, result }).await {
                            panic!("activity worker: enqueue completion failed (id={}): {}", wi.id, e);
                        }
                    }
                    Err(error) => {
                        if let Err(e) = self.history_store.enqueue_work(QueueKind::Orchestrator, WorkItem::ActivityFailed { instance: wi.instance, id: wi.id, error }).await {
                            panic!("activity worker: enqueue failure failed (id={}): {}", wi.id, e);
                        }
                    }
                }
            } else if let Err(e) = self.history_store.enqueue_work(QueueKind::Orchestrator, WorkItem::ActivityFailed {
                instance: wi.instance,
                id: wi.id,
                error: format!("unregistered:{}", wi.name),
            }).await {
                panic!("activity worker: enqueue unregistered failure failed (id={}): {}", wi.id, e);
            }
        }
    }
}



