# Client-Runtime Separation Architecture

## Overview

This document outlines a complete architectural separation between the **Client** (control plane API) and **Runtime** (execution engine), with communication only through the shared `HistoryStore` provider.

## Architecture Principles

### Clean Separation
- **`Client`**: User-facing API for orchestration control and management
- **`DuroxideRuntime`**: Backend execution engine for loading and running orchestrations
- **`HistoryStore`**: Only communication channel between client and runtime

### Communication Model
```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   Client │    │   HistoryStore   │    │ DuroxideRuntime │
│   (Control API) │◄──►│   (Message Bus)  │◄──►│ (Execution)     │
└─────────────────┘    └──────────────────┘    └─────────────────┘
```

## Client API Design

### Core Structure

```rust
// In src/client/mod.rs
use std::sync::Arc;
use chrono::{DateTime, Utc};
use crate::providers::HistoryStore;

/// Client API for orchestration control and management
pub struct Client {
    history_store: Arc<dyn HistoryStore>,
}

impl Client {
    /// Create a new client with a provider
    pub fn new(history_store: Arc<dyn HistoryStore>) -> Self {
        Self { history_store }
    }
}
```

### Orchestration Control APIs

```rust
impl Client {
    /// Start a new orchestration instance
    pub async fn start_orchestration(
        &self,
        instance_id: &str,
        orchestration_name: &str,
        input: &str,
        options: Option<StartOptions>,
    ) -> Result<(), ClientError> {
        let work_item = WorkItem::StartOrchestration {
            instance: instance_id.to_string(),
            orchestration: orchestration_name.to_string(),
            input: input.to_string(),
            version: options.as_ref().and_then(|o| o.version.clone()),
            parent_instance: options.as_ref().and_then(|o| o.parent_instance.clone()),
            parent_id: options.as_ref().and_then(|o| o.parent_id),
        };

        self.history_store
            .enqueue_orchestrator_work(work_item, options.and_then(|o| o.delay_ms))
            .await
            .map_err(ClientError::StorageError)
    }

    /// Start a versioned orchestration
    pub async fn start_orchestration_versioned(
        &self,
        instance_id: &str,
        orchestration_name: &str,
        input: &str,
        version: &str,
    ) -> Result<(), ClientError> {
        let options = StartOptions {
            version: Some(version.to_string()),
            ..Default::default()
        };
        self.start_orchestration(instance_id, orchestration_name, input, Some(options)).await
    }

    /// Cancel a running orchestration
    pub async fn cancel_orchestration(&self, instance_id: &str) -> Result<(), ClientError> {
        let work_item = WorkItem::CancelOrchestration {
            instance: instance_id.to_string(),
        };

        self.history_store
            .enqueue_orchestrator_work(work_item, None)
            .await
            .map_err(ClientError::StorageError)
    }

    /// Raise an external event to an orchestration
    pub async fn raise_event(
        &self,
        instance_id: &str,
        event_name: &str,
        event_data: &str,
    ) -> Result<(), ClientError> {
        let work_item = WorkItem::ExternalEvent {
            instance: instance_id.to_string(),
            name: event_name.to_string(),
            data: event_data.to_string(),
        };

        self.history_store
            .enqueue_orchestrator_work(work_item, None)
            .await
            .map_err(ClientError::StorageError)
    }

    /// Schedule a delayed orchestration start
    pub async fn schedule_orchestration(
        &self,
        instance_id: &str,
        orchestration_name: &str,
        input: &str,
        delay_ms: u64,
    ) -> Result<(), ClientError> {
        let work_item = WorkItem::StartOrchestration {
            instance: instance_id.to_string(),
            orchestration: orchestration_name.to_string(),
            input: input.to_string(),
            version: None,
            parent_instance: None,
            parent_id: None,
        };

        self.history_store
            .enqueue_orchestrator_work(work_item, Some(delay_ms))
            .await
            .map_err(ClientError::StorageError)
    }
}
```

### Management & Observability APIs

```rust
impl Client {
    /// Get orchestration status (read-only)
    pub async fn get_orchestration_status(&self, instance_id: &str) -> Result<OrchestrationStatus, ClientError> {
        let history = self.history_store.read(instance_id).await;
        Ok(Self::derive_status_from_history(&history))
    }

    /// Wait for orchestration completion with timeout
    pub async fn wait_for_completion(
        &self,
        instance_id: &str,
        timeout_ms: Option<u64>,
    ) -> Result<OrchestrationStatus, ClientError> {
        let start_time = std::time::Instant::now();
        let timeout_duration = timeout_ms.map(std::time::Duration::from_millis);

        loop {
            let status = self.get_orchestration_status(instance_id).await?;
            
            match status {
                OrchestrationStatus::Completed { .. } | OrchestrationStatus::Failed { .. } => {
                    return Ok(status);
                }
                OrchestrationStatus::NotFound => {
                    return Err(ClientError::InstanceNotFound(instance_id.to_string()));
                }
                OrchestrationStatus::Running => {
                    // Continue waiting
                }
            }

            if let Some(timeout) = timeout_duration {
                if start_time.elapsed() > timeout {
                    return Err(ClientError::Timeout);
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// List all orchestration instances
    pub async fn list_instances(&self, filter: Option<InstanceFilter>) -> Result<Vec<InstanceSummary>, ClientError> {
        let all_instances = self.history_store.list_instances().await;
        let mut summaries = Vec::new();

        for instance_id in all_instances {
            let history = self.history_store.read(&instance_id).await;
            if let Some(summary) = Self::build_instance_summary(&instance_id, &history) {
                if Self::matches_filter(&summary, &filter) {
                    summaries.push(summary);
                }
            }
        }

        Ok(summaries)
    }

    /// Get detailed instance information
    pub async fn get_instance_details(&self, instance_id: &str) -> Result<Option<InstanceDetails>, ClientError> {
        let history = self.history_store.read(instance_id).await;
        if history.is_empty() {
            return Ok(None);
        }

        let summary = Self::build_instance_summary(instance_id, &history)
            .ok_or_else(|| ClientError::InvalidInstance(instance_id.to_string()))?;

        let executions = self.get_execution_summaries(instance_id).await?;
        let current_execution = if matches!(summary.status, OrchestrationStatus::Running) {
            self.analyze_current_execution(instance_id, &history).await?
        } else {
            None
        };

        Ok(Some(InstanceDetails {
            summary,
            executions,
            current_execution,
        }))
    }

    /// Get execution history
    pub async fn get_execution_history(&self, instance_id: &str, execution_id: Option<u64>) -> Result<Vec<Event>, ClientError> {
        let history = if let Some(exec_id) = execution_id {
            self.history_store.read_with_execution(instance_id, exec_id).await
        } else {
            self.history_store.read(instance_id).await
        };
        Ok(history)
    }

    /// Get bulk status for multiple instances
    pub async fn get_bulk_status(&self, instance_ids: Vec<String>) -> Result<Vec<(String, OrchestrationStatus)>, ClientError> {
        let mut results = Vec::new();
        for instance_id in instance_ids {
            let status = self.get_orchestration_status(&instance_id).await?;
            results.push((instance_id, status));
        }
        Ok(results)
    }

    /// Get system health metrics
    pub async fn get_system_health(&self) -> Result<SystemHealth, ClientError> {
        let instances = self.list_instances(None).await?;
        let mut health = SystemHealth::default();
        let now = Utc::now();

        for instance in instances {
            health.total_instances += 1;
            match instance.status {
                OrchestrationStatus::Running => {
                    health.running_instances += 1;
                    let age = now.signed_duration_since(instance.created_at);
                    if age.num_hours() > 24 {
                        health.long_running_instances += 1;
                    }
                }
                OrchestrationStatus::Completed { .. } => health.completed_instances += 1,
                OrchestrationStatus::Failed { .. } => {
                    health.failed_instances += 1;
                    let age = now.signed_duration_since(instance.updated_at);
                    if age.num_hours() < 1 {
                        health.recent_failures += 1;
                    }
                }
                _ => {}
            }
        }

        if health.total_instances > 0 {
            let success_rate = health.completed_instances as f64 / health.total_instances as f64;
            let failure_rate = health.failed_instances as f64 / health.total_instances as f64;
            health.health_score = ((success_rate - failure_rate) * 100.0).max(0.0) as u32;
        }

        Ok(health)
    }

    /// Find instances by orchestration name
    pub async fn find_instances_by_orchestration(&self, orchestration_name: &str) -> Result<Vec<InstanceSummary>, ClientError> {
        let filter = InstanceFilter {
            orchestration_name: Some(orchestration_name.to_string()),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// Find running instances
    pub async fn find_running_instances(&self) -> Result<Vec<InstanceSummary>, ClientError> {
        let filter = InstanceFilter {
            status: Some(vec![OrchestrationStatus::Running]),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// Find failed instances
    pub async fn find_failed_instances(&self, since: Option<DateTime<Utc>>) -> Result<Vec<InstanceSummary>, ClientError> {
        let filter = InstanceFilter {
            status: Some(vec![OrchestrationStatus::Failed { error: String::new() }]),
            created_after: since,
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }
}
```

## Runtime API Design

### Core Structure

```rust
// In src/runtime/mod.rs (refactored)
use std::sync::Arc;
use crate::providers::HistoryStore;

/// Backend execution engine for orchestrations
pub struct DuroxideRuntime {
    history_store: Arc<dyn HistoryStore>,
    orchestration_registry: OrchestrationRegistry,
    activity_registry: ActivityRegistry,
    dispatchers: Vec<tokio::task::JoinHandle<()>>,
}

impl DuroxideRuntime {
    /// Create a new runtime with a provider
    pub fn new(history_store: Arc<dyn HistoryStore>) -> Self {
        Self {
            history_store,
            orchestration_registry: OrchestrationRegistry::new(),
            activity_registry: ActivityRegistry::new(),
            dispatchers: Vec::new(),
        }
    }
}
```

### Runtime Execution APIs (Internal)

```rust
impl DuroxideRuntime {
    /// Register an orchestration function
    pub fn register_orchestration<F, Fut>(&mut self, name: &str, func: F) -> &mut Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.orchestration_registry.register(name, func);
        self
    }

    /// Register a versioned orchestration
    pub fn register_versioned<F, Fut>(&mut self, name: &str, version: &str, func: F) -> &mut Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.orchestration_registry.register_versioned(name, version, func);
        self
    }

    /// Register an activity function
    pub fn register_activity<F, Fut>(&mut self, name: &str, func: F) -> &mut Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.activity_registry.register(name, func);
        self
    }

    /// Start the runtime (begins processing work items)
    pub async fn start(&mut self) -> Result<(), RuntimeError> {
        // Start orchestrator dispatcher
        let orchestrator_dispatcher = self.start_orchestrator_dispatcher().await?;
        self.dispatchers.push(orchestrator_dispatcher);

        // Start activity dispatcher
        let activity_dispatcher = self.start_activity_dispatcher().await?;
        self.dispatchers.push(activity_dispatcher);

        // Start timer service
        let timer_service = self.start_timer_service().await?;
        self.dispatchers.push(timer_service);

        Ok(())
    }

    /// Stop the runtime gracefully
    pub async fn stop(&mut self) {
        for handle in self.dispatchers.drain(..) {
            handle.abort();
        }
    }

    /// Check if runtime is running
    pub fn is_running(&self) -> bool {
        !self.dispatchers.is_empty() && self.dispatchers.iter().all(|h| !h.is_finished())
    }

    // Internal dispatcher methods
    async fn start_orchestrator_dispatcher(&self) -> Result<tokio::task::JoinHandle<()>, RuntimeError> {
        let store = self.history_store.clone();
        let registry = self.orchestration_registry.clone();
        
        let handle = tokio::spawn(async move {
            loop {
                match store.dequeue_orchestrator_work().await {
                    Ok(Some((work_item, ack_token))) => {
                        if let Err(e) = Self::process_orchestration_work(work_item, ack_token, &store, &registry).await {
                            eprintln!("Error processing orchestration work: {}", e);
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    Err(e) => {
                        eprintln!("Error dequeuing orchestration work: {}", e);
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    }
                }
            }
        });

        Ok(handle)
    }

    async fn start_activity_dispatcher(&self) -> Result<tokio::task::JoinHandle<()>, RuntimeError> {
        let store = self.history_store.clone();
        let registry = self.activity_registry.clone();
        
        let handle = tokio::spawn(async move {
            loop {
                match store.dequeue_activity_work().await {
                    Ok(Some((work_item, ack_token))) => {
                        if let Err(e) = Self::process_activity_work(work_item, ack_token, &store, &registry).await {
                            eprintln!("Error processing activity work: {}", e);
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    Err(e) => {
                        eprintln!("Error dequeuing activity work: {}", e);
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    }
                }
            }
        });

        Ok(handle)
    }

    async fn start_timer_service(&self) -> Result<tokio::task::JoinHandle<()>, RuntimeError> {
        let store = self.history_store.clone();
        
        let handle = tokio::spawn(async move {
            let timer_service = crate::runtime::timers::TimerService::new(store);
            timer_service.run().await;
        });

        Ok(handle)
    }

    // No public APIs for orchestration control - only execution
}
```

## Data Structures & Types

```rust
// In src/client/types.rs

#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub version: Option<String>,
    pub parent_instance: Option<String>,
    pub parent_id: Option<u64>,
    pub delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct InstanceFilter {
    pub status: Option<Vec<OrchestrationStatus>>,
    pub orchestration_name: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub parent_instance: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct InstanceSummary {
    pub instance_id: String,
    pub orchestration_name: String,
    pub version: String,
    pub status: OrchestrationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub execution_count: u64,
    pub parent_instance: Option<String>,
    pub parent_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct InstanceDetails {
    pub summary: InstanceSummary,
    pub executions: Vec<ExecutionSummary>,
    pub current_execution: Option<ExecutionDetails>,
}

#[derive(Debug, Clone)]
pub struct ExecutionSummary {
    pub execution_id: u64,
    pub status: OrchestrationStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub event_count: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ExecutionDetails {
    pub execution_id: u64,
    pub status: OrchestrationStatus,
    pub pending_activities: Vec<PendingActivity>,
    pub pending_timers: Vec<PendingTimer>,
    pub pending_events: Vec<PendingEvent>,
    pub last_event_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingActivity {
    pub correlation_id: u64,
    pub name: String,
    pub input: String,
    pub scheduled_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingTimer {
    pub correlation_id: u64,
    pub delay_ms: u64,
    pub scheduled_at: DateTime<Utc>,
    pub fire_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub correlation_id: u64,
    pub name: String,
    pub subscribed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct SystemHealth {
    pub total_instances: u64,
    pub running_instances: u64,
    pub completed_instances: u64,
    pub failed_instances: u64,
    pub long_running_instances: u64,
    pub recent_failures: u64,
    pub health_score: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),
    #[error("Invalid instance: {0}")]
    InvalidInstance(String),
    #[error("Operation timeout")]
    Timeout,
    #[error("Invalid filter: {0}")]
    InvalidFilter(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("Registry error: {0}")]
    RegistryError(String),
    #[error("Dispatcher error: {0}")]
    DispatcherError(String),
}
```

## Usage Examples

### Client Usage (User-facing)

```rust
// In examples/client_usage.rs
use duroxide::{Client, StartOptions};
use duroxide::providers::sqlite::SqliteHistoryStore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create provider
    let store = Arc::new(SqliteHistoryStore::new("sqlite:./client_demo.db").await?);
    
    // Create client
    let client = Client::new(store);
    
    // Start orchestrations
    client.start_orchestration("order-1", "ProcessOrder", r#"{"order_id": 123}"#, None).await?;
    client.start_orchestration("order-2", "ProcessOrder", r#"{"order_id": 124}"#, None).await?;
    
    // Start with options
    let options = StartOptions {
        version: Some("2.0.0".to_string()),
        delay_ms: Some(5000), // Start in 5 seconds
        ..Default::default()
    };
    client.start_orchestration("order-3", "ProcessOrder", r#"{"order_id": 125}"#, Some(options)).await?;
    
    // Control operations
    client.raise_event("order-1", "PaymentReceived", r#"{"amount": 100}"#).await?;
    client.cancel_orchestration("order-2").await?;
    
    // Wait for completion
    let status = client.wait_for_completion("order-1", Some(30000)).await?;
    println!("Order-1 completed with status: {:?}", status);
    
    // Management operations
    let health = client.get_system_health().await?;
    println!("System health: {}/100", health.health_score);
    
    let running = client.find_running_instances().await?;
    println!("Running instances: {}", running.len());
    
    let order_instances = client.find_instances_by_orchestration("ProcessOrder").await?;
    for instance in order_instances {
        println!("{}: {} ({})", instance.instance_id, instance.orchestration_name, instance.status);
        
        if let Some(details) = client.get_instance_details(&instance.instance_id).await? {
            if let Some(current) = details.current_execution {
                println!("  - {} pending activities", current.pending_activities.len());
            }
        }
    }
    
    Ok(())
}
```

### Runtime Usage (Backend)

```rust
// In examples/runtime_usage.rs
use duroxide::{DuroxideRuntime, OrchestrationContext};
use duroxide::providers::sqlite::SqliteHistoryStore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create provider (same as client)
    let store = Arc::new(SqliteHistoryStore::new("sqlite:./client_demo.db").await?);
    
    // Create runtime
    let mut runtime = DuroxideRuntime::new(store);
    
    // Register orchestrations
    runtime.register_orchestration("ProcessOrder", |ctx, input| async move {
        println!("Processing order: {}", input);
        
        // Schedule activities
        let payment_result = ctx.schedule_activity("ProcessPayment", &input).await?;
        let inventory_result = ctx.schedule_activity("ReserveInventory", &input).await?;
        
        // Wait for external event
        let shipping_info = ctx.wait_for_external_event("ShippingReady").await?;
        
        // Complete order
        let completion_result = ctx.schedule_activity("CompleteOrder", &shipping_info).await?;
        
        Ok(completion_result)
    });
    
    // Register activities
    runtime.register_activity("ProcessPayment", |input| async move {
        println!("Processing payment: {}", input);
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        Ok(r#"{"status": "paid"}"#.to_string())
    });
    
    runtime.register_activity("ReserveInventory", |input| async move {
        println!("Reserving inventory: {}", input);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        Ok(r#"{"status": "reserved"}"#.to_string())
    });
    
    runtime.register_activity("CompleteOrder", |input| async move {
        println!("Completing order: {}", input);
        Ok(r#"{"status": "completed"}"#.to_string())
    });
    
    // Start runtime (begins processing)
    runtime.start().await?;
    
    println!("Runtime started. Processing orchestrations...");
    
    // Keep running
    loop {
        if !runtime.is_running() {
            println!("Runtime stopped");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    
    Ok(())
}
```

### Combined Usage (Development)

```rust
// In examples/combined_usage.rs
use duroxide::{Client, DuroxideRuntime, OrchestrationContext};
use duroxide::providers::sqlite::SqliteHistoryStore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Shared provider
    let store = Arc::new(SqliteHistoryStore::new("sqlite:./combined_demo.db").await?);
    
    // Create and start runtime
    let mut runtime = DuroxideRuntime::new(store.clone());
    runtime.register_orchestration("HelloWorld", |ctx, input| async move {
        println!("Hello from orchestration: {}", input);
        let result = ctx.schedule_activity("SayHello", &input).await?;
        Ok(result)
    });
    runtime.register_activity("SayHello", |input| async move {
        Ok(format!("Hello, {}!", input))
    });
    runtime.start().await?;
    
    // Create client (shares same provider)
    let client = Client::new(store);
    
    // Use client to control orchestrations
    client.start_orchestration("hello-1", "HelloWorld", "World", None).await?;
    
    let status = client.wait_for_completion("hello-1", Some(5000)).await?;
    println!("Result: {:?}", status);
    
    // Stop runtime
    runtime.stop().await;
    
    Ok(())
}
```

## Implementation Plan

### Phase 1: Core Separation (Week 1)
1. **Create `src/client/mod.rs`** with `Client`
2. **Refactor `src/runtime/mod.rs`** to `DuroxideRuntime` (execution only)
3. **Move control APIs** from Runtime to Client
4. **Update `src/lib.rs`** to export both `Client` and `DuroxideRuntime`

### Phase 2: Client APIs (Week 2)
1. **Implement orchestration control** (start, cancel, raise_event)
2. **Add management APIs** (list, get_details, health)
3. **Create comprehensive error handling**
4. **Add client-side validation**

### Phase 3: Runtime Cleanup (Week 3)
1. **Remove control APIs** from Runtime
2. **Focus Runtime on execution** (dispatchers, registries)
3. **Optimize Runtime performance**
4. **Add Runtime health monitoring**

### Phase 4: Testing & Documentation (Week 4)
1. **Create separate test suites** for Client and Runtime
2. **Add integration tests** with shared provider
3. **Update all documentation** and examples
4. **Performance testing** and optimization

## Benefits

### 1. **Clean Architecture**
- **Single Responsibility**: Client controls, Runtime executes
- **Clear Boundaries**: Communication only via HistoryStore
- **Scalable**: Can deploy Client and Runtime separately

### 2. **Operational Flexibility**
```rust
// Development: Both in same process
let store = Arc::new(SqliteHistoryStore::new("sqlite:./dev.db").await?);
let client = Client::new(store.clone());
let runtime = DuroxideRuntime::new(store);

// Production: Separate processes/services
// Service A: Client API
let client_store = Arc::new(SqliteHistoryStore::new("sqlite://prod.db").await?);
let client = Client::new(client_store);

// Service B: Runtime Engine  
let runtime_store = Arc::new(SqliteHistoryStore::new("sqlite://prod.db").await?);
let runtime = DuroxideRuntime::new(runtime_store);
```

### 3. **Security & Safety**
- **Client**: Limited to control operations, can't break execution
- **Runtime**: Isolated execution environment
- **Provider**: Single source of truth with proper isolation

### 4. **Testing & Development**
- **Unit test Client** without running orchestrations
- **Unit test Runtime** without external control
- **Integration test** via shared provider
- **Mock providers** for testing scenarios

This architecture provides true separation of concerns while maintaining the flexibility to run both components together during development or separately in production environments.
