use crate::Event;

/// Logical queues for provider-backed work distribution.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum QueueKind {
    Orchestrator,
    Worker,
    Timer,
}

/// Provider-backed work queue items the runtime consumes continually.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum WorkItem {
    StartOrchestration {
        instance: String,
        orchestration: String,
        input: String,
    },
    ActivityExecute {
        instance: String,
        id: u64,
        name: String,
        input: String,
    },
    ActivityCompleted {
        instance: String,
        id: u64,
        result: String,
    },
    ActivityFailed {
        instance: String,
        id: u64,
        error: String,
    },
    TimerSchedule {
        instance: String,
        id: u64,
        fire_at_ms: u64,
    },
    TimerFired {
        instance: String,
        id: u64,
        fire_at_ms: u64,
    },
    ExternalRaised {
        instance: String,
        name: String,
        data: String,
    },
    SubOrchCompleted {
        parent_instance: String,
        parent_id: u64,
        result: String,
    },
    SubOrchFailed {
        parent_instance: String,
        parent_id: u64,
        error: String,
    },
    CancelInstance {
        instance: String,
        reason: String,
    },
}

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
    async fn create_instance(&self, _instance: &str) -> Result<(), String> {
        Ok(())
    }

    /// Remove an existing instance and its history. Default no-op.
    async fn remove_instance(&self, _instance: &str) -> Result<(), String> {
        Ok(())
    }

    /// Remove multiple instances. Default implementation calls `remove_instance` for each id.
    async fn remove_instances(&self, instances: &[String]) -> Result<(), String> {
        for id in instances {
            self.remove_instance(id).await?;
        }
        Ok(())
    }

    /// Enqueue a work item for the runtime to act on, into the specified logical queue.
    async fn enqueue_work(&self, _kind: QueueKind, _item: WorkItem) -> Result<(), String> {
        Err("work queue not supported".into())
    }

    /// Dequeue-next from the specified logical queue using peek-lock semantics. Returns (item, token) when supported.
    /// The item remains invisible until `ack(token)` or `abandon(token)` is called
    /// (implementations may use a best-effort invisibility without timeouts).
    /// Default: not supported.
    async fn dequeue_peek_lock(&self, _kind: QueueKind) -> Option<(WorkItem, String)> {
        None
    }

    /// Acknowledge a previously peek-locked token in the specified queue, permanently removing it.
    /// Default: no-op success for providers that don't support peek-lock.
    async fn ack(&self, _kind: QueueKind, _token: &str) -> Result<(), String> {
        Ok(())
    }

    /// Abandon a previously peek-locked token in the specified queue, making the item visible again.
    /// Default: no-op success for providers that don't support peek-lock.
    async fn abandon(&self, _kind: QueueKind, _token: &str) -> Result<(), String> {
        Ok(())
    }

    // Metadata APIs removed; orchestration info is derived from history only.

    // --- Multi-execution scaffolding (default single-execution fallback) ---
    /// Return latest execution id for an instance (default: 1 if history exists).
    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        let h = self.read(instance).await;
        if h.is_empty() { None } else { Some(1) }
    }

    /// List all execution ids (default: [1] if history exists).
    async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let h = self.read(instance).await;
        if h.is_empty() { Vec::new() } else { vec![1] }
    }

    /// Read history for a specific execution (default: same as `read`).
    async fn read_with_execution(&self, instance: &str, _execution_id: u64) -> Vec<Event> {
        self.read(instance).await
    }

    /// Append events for a specific execution (default: same as `append`).
    async fn append_with_execution(
        &self,
        instance: &str,
        _execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String> {
        self.append(instance, new_events).await
    }

    /// Reset for ContinueAsNew: create a new execution with OrchestrationStarted including version and optional parent linkage.
    /// Default: not supported. Providers must implement explicit multi-execution semantics.
    async fn reset_for_continue_as_new(
        &self,
        _instance: &str,
        _orchestration: &str,
        _version: &str,
        _input: &str,
        _parent_instance: Option<&str>,
        _parent_id: Option<u64>,
    ) -> Result<u64, String> {
        Err("reset_for_continue_as_new not supported by this provider".into())
    }
}

// Providers are datastores only; runtime owns queues and workers.

/// Filesystem-backed provider for local development.
pub mod fs;
/// In-memory provider for tests.
pub mod in_memory;
