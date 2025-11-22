//! Instrumented provider wrapper that adds metrics to any provider implementation.

use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;

use super::{Provider, ProviderAdmin, ProviderError, ExecutionMetadata, OrchestrationItem, WorkItem};
use crate::Event;
use crate::runtime::observability::MetricsProvider;

/// Wrapper that adds metrics instrumentation to any Provider implementation.
///
/// This follows the decorator pattern to automatically record:
/// - Operation duration for all provider methods
/// - Error counts by operation type
/// - Success/failure status
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use duroxide::providers::sqlite::SqliteProvider;
/// use duroxide::providers::instrumented::InstrumentedProvider;
/// use duroxide::providers::Provider;
///
/// # async fn example() {
/// let provider = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
/// let metrics = None; // Or Some(Arc<MetricsProvider>)
///
/// // Wrap provider with instrumentation
/// let instrumented: Arc<dyn Provider> = Arc::new(
///     InstrumentedProvider::new(provider, metrics)
/// );
///
/// // All operations now automatically emit metrics!
/// # }
/// ```
pub struct InstrumentedProvider {
    inner: Arc<dyn Provider>,
    metrics: Option<Arc<MetricsProvider>>,
}

impl InstrumentedProvider {
    pub fn new(inner: Arc<dyn Provider>, metrics: Option<Arc<MetricsProvider>>) -> Self {
        Self { inner, metrics }
    }

    #[inline]
    fn record_operation(&self, operation: &str, duration: Duration, status: &str) {
        if let Some(ref m) = self.metrics {
            m.record_provider_operation(operation, duration.as_secs_f64(), status);
        }
    }

    #[inline]
    fn record_error(&self, operation: &str, error: &ProviderError) {
        if let Some(ref m) = self.metrics {
            let error_type = if error.message.contains("deadlock") {
                "deadlock"
            } else if error.message.contains("timeout") {
                "timeout"
            } else if error.message.contains("connection") {
                "connection"
            } else {
                "other"
            };
            m.record_provider_error(operation, error_type);
        }
    }
}

#[async_trait]
impl Provider for InstrumentedProvider {
    async fn fetch_orchestration_item(
        &self,
        lock_timeout: Duration,
    ) -> Result<Option<OrchestrationItem>, ProviderError> {
        let start = std::time::Instant::now();
        let result = self.inner.fetch_orchestration_item(lock_timeout).await;
        let duration = start.elapsed();
        
        self.record_operation("fetch_orchestration_item", duration, if result.is_ok() { "success" } else { "error" });
        if let Err(ref e) = result {
            self.record_error("fetch_orchestration_item", e);
        }
        
        result
    }

    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), ProviderError> {
        let start = std::time::Instant::now();
        let result = self.inner.ack_orchestration_item(lock_token, execution_id, history_delta, worker_items, orchestrator_items, metadata).await;
        let duration = start.elapsed();
        
        self.record_operation("ack_orchestration_item", duration, if result.is_ok() { "success" } else { "error" });
        if let Err(ref e) = result {
            self.record_error("ack_orchestration_item", e);
        }
        
        result
    }

    async fn abandon_orchestration_item(&self, lock_token: &str, delay: Option<Duration>) -> Result<(), ProviderError> {
        self.inner.abandon_orchestration_item(lock_token, delay).await
    }

    async fn read(&self, instance: &str) -> Result<Vec<Event>, ProviderError> {
        let start = std::time::Instant::now();
        let result = self.inner.read(instance).await;
        let duration = start.elapsed();
        
        self.record_operation("read", duration, if result.is_ok() { "success" } else { "error" });
        if let Err(ref e) = result {
            self.record_error("read", e);
        }
        
        result
    }

    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> {
        self.inner.read_with_execution(instance, execution_id).await
    }

    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), ProviderError> {
        self.inner.append_with_execution(instance, execution_id, new_events).await
    }

    async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> {
        self.inner.enqueue_for_worker(item).await
    }

    async fn fetch_work_item(&self, lock_timeout: Duration) -> Result<Option<(WorkItem, String)>, ProviderError> {
        let start = std::time::Instant::now();
        let result = self.inner.fetch_work_item(lock_timeout).await;
        let duration = start.elapsed();
        
        self.record_operation("fetch_work_item", duration, if result.is_ok() { "success" } else { "error" });
        if let Err(ref e) = result {
            self.record_error("fetch_work_item", e);
        }
        
        result
    }

    async fn ack_work_item(&self, token: &str, completion: WorkItem) -> Result<(), ProviderError> {
        let start = std::time::Instant::now();
        let result = self.inner.ack_work_item(token, completion).await;
        let duration = start.elapsed();
        
        self.record_operation("ack_work_item", duration, if result.is_ok() { "success" } else { "error" });
        if let Err(ref e) = result {
            self.record_error("ack_work_item", e);
        }
        
        result
    }

    async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) -> Result<(), ProviderError> {
        self.inner.renew_work_item_lock(token, extend_for).await
    }

    async fn enqueue_for_orchestrator(&self, item: WorkItem, delay: Option<Duration>) -> Result<(), ProviderError> {
        let start = std::time::Instant::now();
        let result = self.inner.enqueue_for_orchestrator(item, delay).await;
        let duration = start.elapsed();
        
        self.record_operation("enqueue_orchestrator", duration, if result.is_ok() { "success" } else { "error" });
        if let Err(ref e) = result {
            self.record_error("enqueue_orchestrator", e);
        }
        
        result
    }

    fn as_management_capability(&self) -> Option<&dyn ProviderAdmin> {
        self.inner.as_management_capability()
    }
}

