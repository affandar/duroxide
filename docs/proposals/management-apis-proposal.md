# Management and Visibility APIs Proposal

## Overview

This proposal outlines a comprehensive set of management and visibility APIs for the Duroxide Runtime to enable monitoring, debugging, and operational management of orchestration workloads.

## Current State

The Runtime currently provides basic single-instance operations:
- Status checking (`get_orchestration_status`)
- Event raising (`raise_event`) 
- Instance cancellation (`cancel_instance`)
- Waiting for completion (`wait_for_orchestration`)
- Basic introspection (`get_orchestration_descriptor`, `list_executions`)

## Goals

1. **Observability**: Enable monitoring dashboards and alerting
2. **Debugging**: Support troubleshooting failed or stuck orchestrations
3. **Operations**: Bulk management operations for production environments
4. **Performance**: Efficient APIs that don't impact orchestration execution
5. **Security**: Optional filtering and access control hooks

## Proposed API Structure

### 1. Instance Discovery & Listing

```rust
impl Runtime {
    /// List all known orchestration instances with optional filtering
    pub async fn list_instances(&self, filter: Option<InstanceFilter>) -> Vec<InstanceSummary>;
    
    /// Get paginated list of instances for large deployments
    pub async fn list_instances_paginated(&self, request: ListInstancesRequest) -> ListInstancesResponse;
    
    /// Search instances by various criteria
    pub async fn search_instances(&self, query: InstanceQuery) -> Vec<InstanceSummary>;
}

#[derive(Debug, Clone)]
pub struct InstanceFilter {
    pub status: Option<Vec<OrchestrationStatus>>,
    pub orchestration_name: Option<String>,
    pub created_after: Option<chrono::DateTime<chrono::Utc>>,
    pub created_before: Option<chrono::DateTime<chrono::Utc>>,
    pub has_parent: Option<bool>,
    pub parent_instance: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstanceSummary {
    pub instance_id: String,
    pub orchestration_name: String,
    pub version: String,
    pub status: OrchestrationStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub execution_count: u64,
    pub parent_instance: Option<String>,
    pub parent_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ListInstancesRequest {
    pub filter: Option<InstanceFilter>,
    pub page_size: Option<u32>,
    pub page_token: Option<String>,
    pub order_by: Option<InstanceOrderBy>,
}

#[derive(Debug, Clone)]
pub enum InstanceOrderBy {
    CreatedAt,
    UpdatedAt,
    InstanceId,
    OrchestrationName,
}

#[derive(Debug, Clone)]
pub struct ListInstancesResponse {
    pub instances: Vec<InstanceSummary>,
    pub next_page_token: Option<String>,
    pub total_count: Option<u64>,
}
```

### 2. Enhanced Status & History APIs

```rust
impl Runtime {
    /// Get detailed status including execution history metadata
    pub async fn get_instance_details(&self, instance: &str) -> Option<InstanceDetails>;
    
    /// Get status for multiple instances efficiently
    pub async fn get_instances_status(&self, instances: Vec<String>) -> Vec<(String, OrchestrationStatus)>;
    
    /// Get execution history with filtering and pagination
    pub async fn get_execution_history_filtered(&self, request: HistoryRequest) -> HistoryResponse;
    
    /// Get summary statistics for an instance
    pub async fn get_instance_statistics(&self, instance: &str) -> Option<InstanceStatistics>;
}

#[derive(Debug, Clone)]
pub struct InstanceDetails {
    pub summary: InstanceSummary,
    pub executions: Vec<ExecutionSummary>,
    pub current_execution: Option<ExecutionDetails>,
    pub statistics: InstanceStatistics,
}

#[derive(Debug, Clone)]
pub struct ExecutionSummary {
    pub execution_id: u64,
    pub status: OrchestrationStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub event_count: u64,
    pub continue_as_new_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionDetails {
    pub execution_id: u64,
    pub status: OrchestrationStatus,
    pub pending_activities: Vec<PendingActivity>,
    pub pending_timers: Vec<PendingTimer>,
    pub pending_events: Vec<PendingEvent>,
    pub completed_activities: u64,
    pub failed_activities: u64,
}

#[derive(Debug, Clone)]
pub struct PendingActivity {
    pub correlation_id: u64,
    pub name: String,
    pub input: String,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingTimer {
    pub correlation_id: u64,
    pub delay_ms: u64,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub fire_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub correlation_id: u64,
    pub name: String,
    pub subscribed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct InstanceStatistics {
    pub total_activities_scheduled: u64,
    pub total_activities_completed: u64,
    pub total_activities_failed: u64,
    pub total_timers_scheduled: u64,
    pub total_timers_fired: u64,
    pub total_events_subscribed: u64,
    pub total_events_received: u64,
    pub total_execution_time_ms: u64,
    pub average_turn_duration_ms: f64,
}
```

### 3. Bulk Operations

```rust
impl Runtime {
    /// Cancel multiple instances with optional filtering
    pub async fn cancel_instances(&self, request: BulkCancelRequest) -> BulkCancelResponse;
    
    /// Raise events to multiple instances
    pub async fn raise_event_bulk(&self, request: BulkEventRequest) -> BulkEventResponse;
    
    /// Wait for multiple instances to complete
    pub async fn wait_for_instances(&self, request: BulkWaitRequest) -> BulkWaitResponse;
}

#[derive(Debug, Clone)]
pub struct BulkCancelRequest {
    pub instances: Option<Vec<String>>,
    pub filter: Option<InstanceFilter>,
    pub reason: String,
    pub max_instances: Option<u32>, // Safety limit
}

#[derive(Debug, Clone)]
pub struct BulkCancelResponse {
    pub cancelled_instances: Vec<String>,
    pub failed_instances: Vec<(String, String)>, // (instance, error)
    pub total_matched: u64,
}

#[derive(Debug, Clone)]
pub struct BulkEventRequest {
    pub instances: Vec<String>,
    pub event_name: String,
    pub event_data: String,
}

#[derive(Debug, Clone)]
pub struct BulkEventResponse {
    pub delivered_instances: Vec<String>,
    pub failed_instances: Vec<(String, String)>,
    pub no_subscription_instances: Vec<String>,
}
```

### 4. Runtime Diagnostics & Metrics

```rust
impl Runtime {
    /// Get overall runtime health and statistics
    pub async fn get_runtime_metrics(&self) -> RuntimeMetrics;
    
    /// Get provider-level diagnostics
    pub async fn get_provider_diagnostics(&self) -> ProviderDiagnostics;
    
    /// Get queue statistics and health
    pub async fn get_queue_metrics(&self) -> QueueMetrics;
    
    /// Get orchestration type statistics
    pub async fn get_orchestration_metrics(&self) -> Vec<OrchestrationTypeMetrics>;
}

#[derive(Debug, Clone)]
pub struct RuntimeMetrics {
    pub uptime_seconds: u64,
    pub total_instances_started: u64,
    pub total_instances_completed: u64,
    pub total_instances_failed: u64,
    pub active_instances: u64,
    pub total_activities_executed: u64,
    pub total_timers_fired: u64,
    pub total_events_raised: u64,
    pub average_orchestration_duration_ms: f64,
    pub orchestrator_dispatcher_healthy: bool,
    pub worker_dispatcher_healthy: bool,
    pub timer_dispatcher_healthy: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderDiagnostics {
    pub provider_type: String,
    pub connection_healthy: bool,
    pub total_instances_stored: u64,
    pub total_history_events: u64,
    pub average_read_latency_ms: f64,
    pub average_write_latency_ms: f64,
    pub storage_size_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct QueueMetrics {
    pub orchestrator_queue_size: u64,
    pub worker_queue_size: u64,
    pub timer_queue_size: u64,
    pub orchestrator_queue_oldest_item_age_ms: Option<u64>,
    pub worker_queue_oldest_item_age_ms: Option<u64>,
    pub timer_queue_oldest_item_age_ms: Option<u64>,
    pub total_items_processed: u64,
    pub average_processing_time_ms: f64,
}

#[derive(Debug, Clone)]
pub struct OrchestrationTypeMetrics {
    pub orchestration_name: String,
    pub total_started: u64,
    pub total_completed: u64,
    pub total_failed: u64,
    pub active_count: u64,
    pub average_duration_ms: f64,
    pub success_rate: f64,
}
```

### 5. Advanced Query & Analytics

```rust
impl Runtime {
    /// Execute complex queries against orchestration data
    pub async fn query_instances(&self, query: InstanceQuery) -> QueryResponse;
    
    /// Get time-series metrics for monitoring
    pub async fn get_time_series_metrics(&self, request: TimeSeriesRequest) -> TimeSeriesResponse;
    
    /// Get dependency graph for an instance (parent/child relationships)
    pub async fn get_instance_dependencies(&self, instance: &str) -> DependencyGraph;
}

#[derive(Debug, Clone)]
pub struct InstanceQuery {
    pub filter: InstanceFilter,
    pub group_by: Option<Vec<GroupByField>>,
    pub aggregate: Option<Vec<AggregateFunction>>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum GroupByField {
    OrchestrationName,
    Status,
    CreatedDate,
    ParentInstance,
}

#[derive(Debug, Clone)]
pub enum AggregateFunction {
    Count,
    AverageDuration,
    SuccessRate,
    FailureRate,
}

#[derive(Debug, Clone)]
pub struct DependencyGraph {
    pub root_instance: String,
    pub children: Vec<ChildInstance>,
    pub parent: Option<ParentInstance>,
    pub total_descendants: u64,
}

#[derive(Debug, Clone)]
pub struct ChildInstance {
    pub instance_id: String,
    pub orchestration_name: String,
    pub status: OrchestrationStatus,
    pub child_count: u64,
}
```

### 6. Streaming & Real-time APIs

```rust
impl Runtime {
    /// Stream instance status changes in real-time
    pub async fn stream_instance_events(&self, filter: Option<InstanceFilter>) -> impl Stream<Item = InstanceEvent>;
    
    /// Stream runtime metrics for monitoring dashboards
    pub async fn stream_runtime_metrics(&self, interval_ms: u64) -> impl Stream<Item = RuntimeMetrics>;
    
    /// Watch specific instances for changes
    pub async fn watch_instances(&self, instances: Vec<String>) -> impl Stream<Item = InstanceStatusChange>;
}

#[derive(Debug, Clone)]
pub struct InstanceEvent {
    pub instance_id: String,
    pub event_type: InstanceEventType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum InstanceEventType {
    Started,
    Completed,
    Failed,
    ContinuedAsNew,
    Cancelled,
    ActivityCompleted,
    ActivityFailed,
    TimerFired,
    EventReceived,
}
```

## Implementation Strategy

### Phase 1: Core Management APIs (High Priority)
- `list_instances` with basic filtering
- `get_instance_details` for debugging
- `get_instances_status` for bulk status checks
- `get_runtime_metrics` for basic monitoring

### Phase 2: Bulk Operations (Medium Priority)
- `cancel_instances` for operational management
- `raise_event_bulk` for batch processing
- Enhanced filtering and pagination

### Phase 3: Advanced Analytics (Lower Priority)
- Time-series metrics
- Dependency graphs
- Complex querying
- Streaming APIs

### Phase 4: Performance & Scale (Future)
- Caching for frequently accessed data
- Async background metric collection
- Provider-specific optimizations

## Provider Interface Extensions

```rust
#[async_trait::async_trait]
pub trait HistoryStore: Send + Sync {
    // ... existing methods ...
    
    // New management methods
    async fn list_instances_filtered(&self, filter: Option<InstanceFilter>) -> Vec<InstanceSummary>;
    async fn get_instance_statistics(&self, instance: &str) -> Option<InstanceStatistics>;
    async fn get_provider_diagnostics(&self) -> ProviderDiagnostics;
    async fn get_queue_metrics(&self) -> QueueMetrics;
    
    // Bulk operations
    async fn get_instances_status_bulk(&self, instances: Vec<String>) -> Vec<(String, OrchestrationStatus)>;
    
    // Optional: Advanced querying (providers can return "not supported")
    async fn query_instances(&self, query: InstanceQuery) -> Result<QueryResponse, String> {
        Err("Advanced querying not supported by this provider".to_string())
    }
}
```

## Security & Access Control

```rust
pub trait ManagementAccessControl: Send + Sync {
    async fn can_list_instances(&self, context: &AccessContext) -> bool;
    async fn can_view_instance(&self, context: &AccessContext, instance: &str) -> bool;
    async fn can_cancel_instance(&self, context: &AccessContext, instance: &str) -> bool;
    async fn can_view_metrics(&self, context: &AccessContext) -> bool;
}

pub struct AccessContext {
    pub user_id: Option<String>,
    pub roles: Vec<String>,
    pub source_ip: Option<std::net::IpAddr>,
}

impl Runtime {
    pub fn with_access_control(self, access_control: Arc<dyn ManagementAccessControl>) -> Self;
}
```

## Configuration

```rust
#[derive(Debug, Clone)]
pub struct ManagementConfig {
    pub enable_metrics: bool,
    pub enable_streaming: bool,
    pub max_list_instances: u32,
    pub max_bulk_operations: u32,
    pub metrics_retention_hours: u32,
    pub cache_ttl_seconds: u32,
}

impl Runtime {
    pub fn with_management_config(self, config: ManagementConfig) -> Self;
}
```

## Benefits

1. **Operational Excellence**: Enables production monitoring and management
2. **Developer Experience**: Rich debugging and introspection capabilities
3. **Scalability**: Efficient bulk operations for large deployments
4. **Observability**: Comprehensive metrics and real-time monitoring
5. **Security**: Optional access control for multi-tenant scenarios

## Considerations

1. **Performance Impact**: Management APIs should not affect orchestration execution performance
2. **Provider Capabilities**: Not all providers may support advanced features
3. **Memory Usage**: Caching and metrics collection need memory management
4. **API Stability**: Management APIs should be versioned separately from core APIs
5. **Backward Compatibility**: New APIs should not break existing code

## Migration Path

1. Add new management methods as optional traits initially
2. Implement basic versions in SQLite provider
3. Add runtime wrapper methods with feature flags
4. Gradually enhance with more advanced capabilities
5. Eventually make management APIs part of core provider contract

This proposal provides a comprehensive foundation for operational management of Duroxide orchestrations while maintaining flexibility for different deployment scenarios and provider capabilities.
