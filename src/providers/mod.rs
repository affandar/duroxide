use crate::Event;

#[async_trait::async_trait]
pub trait HistoryStore: Send + Sync {
    async fn read(&self, instance: &str) -> Vec<Event>;
    // Must error if appending would exceed the provider's retention cap
    async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String>;
    async fn reset(&self);
    async fn list_instances(&self) -> Vec<String>;
    async fn dump_all_pretty(&self) -> String;
}

// Providers are datastores only; runtime owns queues and workers.

pub mod in_memory;
pub mod fs;


