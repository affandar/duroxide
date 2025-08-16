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
}

// Providers are datastores only; runtime owns queues and workers.

/// Filesystem-backed provider for local development.
pub mod fs;
/// In-memory provider for tests.
pub mod in_memory;
