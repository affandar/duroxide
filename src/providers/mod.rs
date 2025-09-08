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

/// Provider abstraction for durable orchestration execution (persistence + queues).
///
/// Providers must implement the core atomic orchestration methods and basic queue operations.
/// Multi-execution support is required for ContinueAsNew functionality.
/// Management APIs are optional with default no-op implementations.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    // ===== Core Atomic Orchestration Methods (REQUIRED) =====
    // These three methods form the heart of reliable orchestration execution.
    
    /// Fetch the next orchestration work item atomically.
    /// 
    /// This method should:
    /// 1. Dequeue a batch of messages for a single instance from the orchestrator queue
    /// 2. Load the instance's history (or empty history for new instances)
    /// 3. Lock the instance until ack or abandon is called
    /// 
    /// Returns None if no work is available.
    /// 
    /// Example implementation pattern:
    /// ```ignore
    /// let (messages, lock_token) = dequeue_orchestrator_messages()?;
    /// let instance = extract_instance(&messages);
    /// let history = read_instance_history(&instance);
    /// Some(OrchestrationItem { instance, messages, history, lock_token, ... })
    /// ```
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;

    /// Acknowledge successful orchestration processing atomically.
    /// 
    /// This method MUST atomically:
    /// 1. Append history_delta to the instance's history
    /// 2. Enqueue all worker_items to the worker queue
    /// 3. Enqueue all timer_items to the timer queue
    /// 4. Enqueue all orchestrator_items to the orchestrator queue
    /// 5. Release the instance lock
    /// 
    /// If any operation fails, the entire operation should be rolled back if possible.
    /// For simple providers, best-effort atomicity is acceptable.
    async fn ack_orchestration_item(
        &self,
        _lock_token: &str,
        _history_delta: Vec<Event>,
        _worker_items: Vec<WorkItem>,
        _timer_items: Vec<WorkItem>,
        _orchestrator_items: Vec<WorkItem>,
    ) -> Result<(), String>;

    /// Abandon orchestration processing (used for errors/retries).
    /// 
    /// This method should:
    /// 1. Release the instance lock
    /// 2. Make the messages available for reprocessing
    /// 3. Optionally delay visibility by delay_ms milliseconds
    /// 
    /// Used when orchestration processing fails and needs to be retried.
    async fn abandon_orchestration_item(&self, _lock_token: &str, _delay_ms: Option<u64>) -> Result<(), String>;

    // ===== Basic History Access (REQUIRED) =====
    
    /// Read the full history for an instance.
    /// Returns empty Vec if instance doesn't exist.
    /// For multi-execution instances, returns the latest execution's history.
    async fn read(&self, instance: &str) -> Vec<Event>;

    // ===== Worker Queue Operations (REQUIRED) =====
    // Worker queue processes activity executions.
    
    /// Enqueue an activity execution request.
    async fn enqueue_worker_work(&self, _item: WorkItem) -> Result<(), String>;

    /// Dequeue a single work item with peek-lock semantics.
    /// Returns the work item and a lock token that must be used to ack/abandon.
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)>;

    /// Acknowledge successful processing of a work item.
    async fn ack_worker(&self, _token: &str) -> Result<(), String>;

    // ===== Multi-Execution Support (REQUIRED for ContinueAsNew) =====
    // These methods enable orchestrations to continue with new input while maintaining history.
    
    /// Get the latest execution ID for an instance.
    /// Returns None if instance doesn't exist, Some(id) where id >= 1 otherwise.
    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        let h = self.read(instance).await;
        if h.is_empty() { None } else { Some(1) }
    }
    
    /// Read history for a specific execution.
    /// Used primarily for debugging and testing specific executions.
    async fn read_with_execution(&self, instance: &str, _execution_id: u64) -> Vec<Event> {
        self.read(instance).await
    }
    
    /// Append events to a specific execution.
    /// This is typically called internally by ack_orchestration_item.
    async fn append_with_execution(
        &self,
        instance: &str,
        _execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String>;
    
    /// Create a new execution for ContinueAsNew scenarios.
    /// 
    /// This should:
    /// 1. Create a new execution with ID = latest + 1
    /// 2. Write an OrchestrationStarted event with the provided parameters
    /// 
    /// Used when processing WorkItem::ContinueAsNew.
    async fn create_new_execution(
        &self,
        _instance: &str,
        _orchestration: &str,
        _version: &str,
        _input: &str,
        _parent_instance: Option<&str>,
        _parent_id: Option<u64>,
    ) -> Result<u64, String>;

    // ===== Timer Support (REQUIRED only if supports_delayed_visibility returns true) =====
    
    /// Whether this provider natively supports delayed message visibility.
    /// If false (default), the runtime uses an in-process timer service.
    /// If true, the provider must implement timer queue methods below.
    fn supports_delayed_visibility(&self) -> bool {
        false
    }
    
    /// Enqueue a timer to fire at a specific time (only called if supports_delayed_visibility = true).
    async fn enqueue_timer_work(&self, _item: WorkItem) -> Result<(), String>;
    
    /// Dequeue a timer that's ready to fire (only called if supports_delayed_visibility = true).
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)>;
    
    /// Acknowledge a processed timer (only called if supports_delayed_visibility = true).
    async fn ack_timer(&self, _token: &str) -> Result<(), String>;

    // ===== Optional Management APIs =====
    // These have default implementations and are primarily used for testing/debugging.
    
    /// Enqueue a work item to the orchestrator queue.
    /// Note: In normal operation, orchestrator items are enqueued via ack_orchestration_item.
    /// This method is used by raise_event and cancel_instance operations.
    /// 
    /// `delay_ms`: Optional delay in milliseconds before the item becomes visible for processing.
    /// None means immediate visibility. Providers that don't support delayed visibility will ignore the delay.
    async fn enqueue_orchestrator_work(&self, _item: WorkItem, _delay_ms: Option<u64>) -> Result<(), String>;
    
    /// List all known instance IDs.
    /// Default: empty list. Used primarily for testing and debugging.
    async fn list_instances(&self) -> Vec<String>;
    
    /// List all execution IDs for an instance.
    /// Default: returns [1] if instance exists, empty otherwise.
    async fn list_executions(&self, instance: &str) -> Vec<u64>;
    
    // - Add timeout parameter to dequeue_worker_peek_lock and dequeue_timer_peek_lock
    // - Add refresh_worker_lock(token, extend_ms) and refresh_timer_lock(token, extend_ms)
    // - Provider should auto-abandon messages if lock expires without ack
    // This would enable graceful handling of worker crashes and long-running activities
}

/// SQLite-backed provider with full transactional support.
pub mod sqlite;
