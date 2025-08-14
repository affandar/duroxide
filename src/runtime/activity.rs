use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;
use async_trait::async_trait;

use super::{ActivityWorkItem, OrchestratorMsg};

#[async_trait]
pub trait ActivityHandler: Send + Sync {
    async fn invoke(&self, input: String) -> String;
}

pub struct FnActivity<F, Fut>(pub F)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = String> + Send + 'static;

#[async_trait]
impl<F, Fut> ActivityHandler for FnActivity<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = String> + Send + 'static,
{
    async fn invoke(&self, input: String) -> String { (self.0)(input).await }
}

#[derive(Clone, Default)]
pub struct ActivityRegistry { inner: Arc<HashMap<String, Arc<dyn ActivityHandler>>> }

impl ActivityRegistry {
    pub fn builder() -> ActivityRegistryBuilder { ActivityRegistryBuilder { map: HashMap::new() } }
    pub fn get(&self, name: &str) -> Option<Arc<dyn ActivityHandler>> { self.inner.get(name).cloned() }
}

pub struct ActivityRegistryBuilder { map: HashMap<String, Arc<dyn ActivityHandler>> }

impl ActivityRegistryBuilder {
    pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = String> + Send + 'static,
    {
        self.map.insert(name.into(), Arc::new(FnActivity(f)));
        self
    }
    pub fn build(self) -> ActivityRegistry { ActivityRegistry { inner: Arc::new(self.map) } }
}

pub struct ActivityWorker {
    registry: ActivityRegistry,
    completion_tx: mpsc::UnboundedSender<OrchestratorMsg>,
}

impl ActivityWorker {
    pub fn new(registry: ActivityRegistry, completion_tx: mpsc::UnboundedSender<OrchestratorMsg>) -> Self {
        Self { registry, completion_tx }
    }
    pub async fn run(self, mut rx: mpsc::Receiver<ActivityWorkItem>) {
        while let Some(wi) = rx.recv().await {
            if let Some(handler) = self.registry.get(&wi.name) {
                let result = handler.invoke(wi.input).await;
                let _ = self.completion_tx.send(OrchestratorMsg::ActivityCompleted { instance: wi.instance, id: wi.id, result });
            } else {
                let _ = self.completion_tx.send(OrchestratorMsg::ActivityCompleted { instance: wi.instance, id: wi.id, result: format!("echo:{}", wi.input) });
            }
        }
    }
}



