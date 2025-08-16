use crate::Event;

/// Storage abstraction for append-only orchestration history per instance.
#[async_trait::async_trait]
pub trait HistoryStore: Send + Sync {
    /// Read full history for an instance.
    async fn read(&self, instance: &str) -> Vec<Event>;
    /// Append events for an instance; should fail if provider limits would be exceeded.
    async fn append(&self, instance: &str, new_events: Vec<Event>) -> Result<(), String>;
    /// Clear provider data (test utility).
    async fn reset(&self);
    /// Enumerate known instances.
    async fn list_instances(&self) -> Vec<String>;
    /// Return a pretty-printed dump of all instances (test utility).
    async fn dump_all_pretty(&self) -> String;

    /// Create a new, empty instance. Implementations should return an error if the
    /// instance already exists. Default no-op for stores that don't track instances eagerly.
    async fn create_instance(&self, _instance: &str) -> Result<(), String> { Ok(()) }

    /// Remove an existing instance and its history. Default no-op.
    async fn remove_instance(&self, _instance: &str) -> Result<(), String> { Ok(()) }

    /// Remove multiple instances. Default implementation calls `remove_instance` for each id.
    async fn remove_instances(&self, instances: &[String]) -> Result<(), String> {
        for id in instances { self.remove_instance(id).await?; }
        Ok(())
    }
}

// Providers are datastores only; runtime owns queues and workers.

/// In-memory provider for tests.
pub mod in_memory;
/// Filesystem-backed provider for local development.
pub mod fs;


