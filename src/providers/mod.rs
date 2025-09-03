use crate::Event;

/// Orchestration item containing all data needed to process an instance atomically.
#[derive(Debug, Clone)]
pub struct OrchestrationItem {
    pub instance: String,
    pub orchestration_name: String,
    pub execution_id: u64,
    pub version: String,
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
}

/// Provider-backed work queue items the runtime consumes continually.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum WorkItem {
    StartOrchestration {
        instance: String,
        orchestration: String,
        input: String,
        version: Option<String>,
        parent_instance: Option<String>,
        parent_id: Option<u64>,
    },
    ActivityExecute {
        instance: String,
        execution_id: u64,
        id: u64,
        name: String,
        input: String,
    },
    ActivityCompleted {
        instance: String,
        execution_id: u64,
        id: u64,
        result: String,
    },
    ActivityFailed {
        instance: String,
        execution_id: u64,
        id: u64,
        error: String,
    },
    TimerSchedule {
        instance: String,
        execution_id: u64,
        id: u64,
        fire_at_ms: u64,
    },
    TimerFired {
        instance: String,
        execution_id: u64,
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
        parent_execution_id: u64,
        parent_id: u64,
        result: String,
    },
    SubOrchFailed {
        parent_instance: String,
        parent_execution_id: u64,
        parent_id: u64,
        error: String,
    },
    CancelInstance {
        instance: String,
        reason: String,
    },
    ContinueAsNew {
        instance: String,
        orchestration: String,
        input: String,
        version: Option<String>,
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

    /// Create a new, empty instance. Expected to be idempotent;
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

    // ===== Orchestrator Queue Methods (enqueue only; legacy dequeue/ack removed) =====

    /// Enqueue a work item to the orchestrator queue.
    async fn enqueue_orchestrator_work(&self, _item: WorkItem) -> Result<(), String> {
        Err("orchestrator queue not supported".into())
    }

    // ===== Worker Queue Methods (single-item operations) =====

    /// Enqueue a work item to the worker queue.
    async fn enqueue_worker_work(&self, _item: WorkItem) -> Result<(), String> {
        Err("worker queue not supported".into())
    }

    /// Dequeue a single work item from the worker queue.
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        None
    }

    /// Acknowledge a worker message, permanently removing it.
    async fn ack_worker(&self, _token: &str) -> Result<(), String> {
        Ok(())
    }

    /// Abandon a worker message, making it visible again.
    async fn abandon_worker(&self, _token: &str) -> Result<(), String> {
        Ok(())
    }

    // ===== Timer Queue Methods (single-item operations) =====

    /// Enqueue a work item to the timer queue.
    async fn enqueue_timer_work(&self, _item: WorkItem) -> Result<(), String> {
        Err("timer queue not supported".into())
    }

    /// Dequeue a single work item from the timer queue.
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> {
        None
    }

    /// Acknowledge a timer message, permanently removing it.
    async fn ack_timer(&self, _token: &str) -> Result<(), String> {
        Ok(())
    }

    /// Abandon a timer message, making it visible again.
    async fn abandon_timer(&self, _token: &str) -> Result<(), String> {
        Ok(())
    }

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

    /// Create a new execution with OrchestrationStarted including version and optional parent linkage.
    /// Default: not supported. Providers must implement explicit multi-execution semantics.
    async fn create_new_execution(
        &self,
        _instance: &str,
        _orchestration: &str,
        _version: &str,
        _input: &str,
        _parent_instance: Option<&str>,
        _parent_id: Option<u64>,
    ) -> Result<u64, String> {
        Err("create_new_execution not supported by this provider".into())
    }

    /// Whether the provider supports delayed visibility for timer messages.
    /// If true, the runtime can rely on the provider to deliver TimerFired at the scheduled time.
    /// Default: false, which enables the in-process timer fallback service.
    fn supports_delayed_visibility(&self) -> bool {
        false
    }

    // ===== New Atomic Orchestration Methods =====

    /// Fetch next orchestration item (batch of messages + history) atomically.
    /// This locks the instance until ack_orchestration_item or abandon_orchestration_item is called.
    /// Returns None if no work is available.
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        None
    }

    /// Acknowledge orchestration item atomically.
    /// - Appends new history events
    /// - Enqueues work items to appropriate queues (worker, timer, orchestrator)
    /// - Releases the lock
    /// All operations must succeed or all must fail (best effort for fs/in_memory providers).
    async fn ack_orchestration_item(
        &self,
        _lock_token: &str,
        _history_delta: Vec<Event>,
        _worker_items: Vec<WorkItem>,
        _timer_items: Vec<WorkItem>,
        _orchestrator_items: Vec<WorkItem>,
    ) -> Result<(), String> {
        Err("ack_orchestration_item not supported by this provider".into())
    }

    /// Abandon orchestration item with optional visibility delay.
    /// Makes the instance available again after delay_ms (if provided).
    /// Used for error scenarios or backoff strategies.
    async fn abandon_orchestration_item(&self, _lock_token: &str, _delay_ms: Option<u64>) -> Result<(), String> {
        Err("abandon_orchestration_item not supported by this provider".into())
    }
}

/// Filesystem-backed provider for local development.
pub mod fs;
/// In-memory provider for tests.
pub mod in_memory;
