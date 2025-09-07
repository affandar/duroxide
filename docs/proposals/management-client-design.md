# Management Client Design

## Overview

This document outlines the design for a separate `ManagementClient` API for read-only management operations, keeping the `Runtime` focused on orchestration execution while providing rich observability and management capabilities.

## Architecture Principles

### Separation of Concerns
- **`Runtime`**: Orchestration execution, lifecycle management, control operations
- **`ManagementClient`**: Read-only observability, monitoring, analytics, debugging

### Shared Provider
Both `Runtime` and `ManagementClient` use the same `HistoryStore` provider instance, ensuring:
- Consistent data access
- No data synchronization issues  
- Efficient resource usage
- Single source of truth

## API Design

### Core Structure

```rust
// In src/management/mod.rs
use std::sync::Arc;
use chrono::{DateTime, Utc};
use crate::providers::HistoryStore;
use crate::runtime::OrchestrationStatus;

/// Read-only management and observability client
pub struct ManagementClient {
    history_store: Arc<dyn HistoryStore>,
}

impl ManagementClient {
    /// Create a new management client with the same provider as a Runtime
    pub fn new(history_store: Arc<dyn HistoryStore>) -> Self {
        Self { history_store }
    }

    /// Create from an existing Runtime (shares the same provider)
    pub fn from_runtime(runtime: &crate::Runtime) -> Self {
        Self {
            history_store: runtime.history_store().clone(),
        }
    }
}
```

### Instance Discovery & Listing

```rust
impl ManagementClient {
    /// List all orchestration instances with optional filtering
    pub async fn list_instances(&self, filter: Option<InstanceFilter>) -> Result<Vec<InstanceSummary>, ManagementError> {
        self.history_store.list_instances_filtered(filter).await
            .map_err(ManagementError::StorageError)
    }

    /// Get summary information for a specific instance
    pub async fn get_instance(&self, instance_id: &str) -> Result<Option<InstanceSummary>, ManagementError> {
        self.history_store.get_instance_summary(instance_id).await
            .map_err(ManagementError::StorageError)
    }

    /// Search instances by orchestration name
    pub async fn find_instances_by_orchestration(&self, orchestration_name: &str) -> Result<Vec<InstanceSummary>, ManagementError> {
        let filter = InstanceFilter {
            orchestration_name: Some(orchestration_name.to_string()),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// Get instances by status
    pub async fn find_instances_by_status(&self, status: OrchestrationStatus) -> Result<Vec<InstanceSummary>, ManagementError> {
        let filter = InstanceFilter {
            status: Some(vec![status]),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// Get instances created within a time range
    pub async fn find_instances_by_time_range(
        &self, 
        start: DateTime<Utc>, 
        end: DateTime<Utc>
    ) -> Result<Vec<InstanceSummary>, ManagementError> {
        let filter = InstanceFilter {
            created_after: Some(start),
            created_before: Some(end),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// Get child instances of a parent orchestration
    pub async fn find_child_instances(&self, parent_instance_id: &str) -> Result<Vec<InstanceSummary>, ManagementError> {
        let filter = InstanceFilter {
            parent_instance: Some(parent_instance_id.to_string()),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }
}
```

### Detailed Instance Inspection

```rust
impl ManagementClient {
    /// Get comprehensive details about an instance
    pub async fn get_instance_details(&self, instance_id: &str) -> Result<Option<InstanceDetails>, ManagementError> {
        let summary = match self.get_instance(instance_id).await? {
            Some(s) => s,
            None => return Ok(None),
        };

        let executions = self.get_execution_summaries(instance_id).await?;
        let current_execution = if matches!(summary.status, OrchestrationStatus::Running) {
            self.get_current_execution_details(instance_id).await?
        } else {
            None
        };

        Ok(Some(InstanceDetails {
            summary,
            executions,
            current_execution,
        }))
    }

    /// Get execution history for an instance
    pub async fn get_execution_summaries(&self, instance_id: &str) -> Result<Vec<ExecutionSummary>, ManagementError> {
        let execution_ids = self.history_store.list_executions(instance_id).await;
        let mut summaries = Vec::new();

        for exec_id in execution_ids {
            let history = self.history_store.read_with_execution(instance_id, exec_id).await;
            if let Some(summary) = self.build_execution_summary(exec_id, &history) {
                summaries.push(summary);
            }
        }

        Ok(summaries)
    }

    /// Get detailed view of current execution (for running instances)
    pub async fn get_current_execution_details(&self, instance_id: &str) -> Result<Option<ExecutionDetails>, ManagementError> {
        let executions = self.get_execution_summaries(instance_id).await?;
        let latest = executions.last();
        
        if let Some(latest_exec) = latest {
            if matches!(latest_exec.status, OrchestrationStatus::Running) {
                let history = self.history_store.read_with_execution(instance_id, latest_exec.execution_id).await;
                return Ok(Some(self.analyze_execution_state(&history)));
            }
        }

        Ok(None)
    }

    /// Get full event history for an instance
    pub async fn get_instance_history(&self, instance_id: &str) -> Result<Vec<Event>, ManagementError> {
        Ok(self.history_store.read(instance_id).await)
    }

    /// Get event history for a specific execution
    pub async fn get_execution_history(&self, instance_id: &str, execution_id: u64) -> Result<Vec<Event>, ManagementError> {
        Ok(self.history_store.read_with_execution(instance_id, execution_id).await)
    }
}
```

### Bulk Operations & Analytics

```rust
impl ManagementClient {
    /// Get status for multiple instances efficiently
    pub async fn get_bulk_status(&self, instance_ids: Vec<String>) -> Result<Vec<(String, OrchestrationStatus)>, ManagementError> {
        self.history_store.get_instances_status_bulk(instance_ids).await
            .map_err(ManagementError::StorageError)
    }

    /// Get orchestration statistics
    pub async fn get_orchestration_stats(&self, orchestration_name: Option<&str>) -> Result<OrchestrationStats, ManagementError> {
        let filter = orchestration_name.map(|name| InstanceFilter {
            orchestration_name: Some(name.to_string()),
            ..Default::default()
        });

        let instances = self.list_instances(filter).await?;
        
        let mut stats = OrchestrationStats::default();
        for instance in instances {
            stats.total_count += 1;
            match instance.status {
                OrchestrationStatus::Running => stats.running_count += 1,
                OrchestrationStatus::Completed { .. } => stats.completed_count += 1,
                OrchestrationStatus::Failed { .. } => stats.failed_count += 1,
                OrchestrationStatus::NotFound => {}, // Shouldn't happen in listing
            }
            
            if instance.execution_count > 1 {
                stats.continued_count += 1;
            }
        }

        Ok(stats)
    }

    /// Get activity statistics across instances
    pub async fn get_activity_stats(&self, orchestration_name: Option<&str>) -> Result<Vec<ActivityStats>, ManagementError> {
        let instances = if let Some(name) = orchestration_name {
            self.find_instances_by_orchestration(name).await?
        } else {
            self.list_instances(None).await?
        };

        let mut activity_map: std::collections::HashMap<String, ActivityStats> = std::collections::HashMap::new();

        for instance in instances {
            let history = self.get_instance_history(&instance.instance_id).await?;
            self.collect_activity_stats(&history, &mut activity_map);
        }

        Ok(activity_map.into_values().collect())
    }

    /// Get performance metrics for an orchestration type
    pub async fn get_performance_metrics(&self, orchestration_name: &str, since: Option<DateTime<Utc>>) -> Result<PerformanceMetrics, ManagementError> {
        let mut filter = InstanceFilter {
            orchestration_name: Some(orchestration_name.to_string()),
            ..Default::default()
        };
        
        if let Some(since_time) = since {
            filter.created_after = Some(since_time);
        }

        let instances = self.list_instances(Some(filter)).await?;
        
        let mut metrics = PerformanceMetrics::default();
        let mut durations = Vec::new();

        for instance in instances {
            if let Some(duration) = self.calculate_instance_duration(&instance).await? {
                durations.push(duration);
            }
        }

        if !durations.is_empty() {
            durations.sort();
            metrics.avg_duration_ms = durations.iter().sum::<u64>() / durations.len() as u64;
            metrics.min_duration_ms = *durations.first().unwrap();
            metrics.max_duration_ms = *durations.last().unwrap();
            metrics.p50_duration_ms = durations[durations.len() / 2];
            metrics.p95_duration_ms = durations[(durations.len() as f64 * 0.95) as usize];
        }

        Ok(metrics)
    }
}
```

### System Health & Monitoring

```rust
impl ManagementClient {
    /// Get overall system health metrics
    pub async fn get_system_health(&self) -> Result<SystemHealth, ManagementError> {
        let all_instances = self.list_instances(None).await?;
        
        let mut health = SystemHealth::default();
        let now = Utc::now();
        
        for instance in all_instances {
            health.total_instances += 1;
            
            match instance.status {
                OrchestrationStatus::Running => {
                    health.running_instances += 1;
                    
                    // Check for long-running instances (potential stuck orchestrations)
                    let age = now.signed_duration_since(instance.created_at);
                    if age.num_hours() > 24 {
                        health.long_running_instances += 1;
                    }
                }
                OrchestrationStatus::Failed { .. } => {
                    health.failed_instances += 1;
                    
                    // Check for recent failures
                    let age = now.signed_duration_since(instance.updated_at);
                    if age.num_hours() < 1 {
                        health.recent_failures += 1;
                    }
                }
                OrchestrationStatus::Completed { .. } => {
                    health.completed_instances += 1;
                }
                _ => {}
            }
        }

        // Calculate health score (0-100)
        if health.total_instances > 0 {
            let success_rate = (health.completed_instances as f64) / (health.total_instances as f64);
            let failure_rate = (health.failed_instances as f64) / (health.total_instances as f64);
            health.health_score = ((success_rate - failure_rate) * 100.0).max(0.0) as u32;
        }

        Ok(health)
    }

    /// Get instances that might need attention
    pub async fn get_problematic_instances(&self) -> Result<ProblematicInstances, ManagementError> {
        let all_instances = self.list_instances(None).await?;
        let now = Utc::now();
        
        let mut problematic = ProblematicInstances::default();
        
        for instance in all_instances {
            match instance.status {
                OrchestrationStatus::Running => {
                    let age = now.signed_duration_since(instance.created_at);
                    
                    // Long-running (>24h)
                    if age.num_hours() > 24 {
                        problematic.long_running.push(instance.clone());
                    }
                    // Stuck (>1h with no updates)
                    else if now.signed_duration_since(instance.updated_at).num_hours() > 1 {
                        problematic.potentially_stuck.push(instance.clone());
                    }
                }
                OrchestrationStatus::Failed { .. } => {
                    let age = now.signed_duration_since(instance.updated_at);
                    if age.num_hours() < 24 {
                        problematic.recent_failures.push(instance);
                    }
                }
                _ => {}
            }
        }

        Ok(problematic)
    }

    /// Get queue depths and processing metrics
    pub async fn get_queue_metrics(&self) -> Result<QueueMetrics, ManagementError> {
        // This would require new provider methods to inspect queues
        // For now, return basic metrics derived from instance states
        
        let running_instances = self.find_instances_by_status(OrchestrationStatus::Running).await?;
        let mut metrics = QueueMetrics::default();
        
        for instance in running_instances {
            if let Some(details) = self.get_current_execution_details(&instance.instance_id).await? {
                metrics.pending_activities += details.pending_activities.len() as u64;
                metrics.pending_timers += details.pending_timers.len() as u64;
                metrics.pending_events += details.pending_events.len() as u64;
            }
        }

        Ok(metrics)
    }
}
```

## Data Structures

```rust
// In src/management/types.rs

#[derive(Debug, Clone)]
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
pub struct OrchestrationStats {
    pub total_count: u64,
    pub running_count: u64,
    pub completed_count: u64,
    pub failed_count: u64,
    pub continued_count: u64,
}

#[derive(Debug, Clone)]
pub struct ActivityStats {
    pub activity_name: String,
    pub total_scheduled: u64,
    pub total_completed: u64,
    pub total_failed: u64,
    pub avg_duration_ms: u64,
    pub failure_rate: f64,
}

#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub avg_duration_ms: u64,
    pub min_duration_ms: u64,
    pub max_duration_ms: u64,
    pub p50_duration_ms: u64,
    pub p95_duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SystemHealth {
    pub total_instances: u64,
    pub running_instances: u64,
    pub completed_instances: u64,
    pub failed_instances: u64,
    pub long_running_instances: u64,
    pub recent_failures: u64,
    pub health_score: u32, // 0-100
}

#[derive(Debug, Clone, Default)]
pub struct ProblematicInstances {
    pub long_running: Vec<InstanceSummary>,
    pub potentially_stuck: Vec<InstanceSummary>,
    pub recent_failures: Vec<InstanceSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct QueueMetrics {
    pub pending_activities: u64,
    pub pending_timers: u64,
    pub pending_events: u64,
    pub orchestrator_queue_depth: u64,
    pub activity_queue_depth: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ManagementError {
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),
    #[error("Invalid filter: {0}")]
    InvalidFilter(String),
}
```

## Usage Examples

```rust
// In examples/management_demo.rs

use duroxide::{Runtime, ManagementClient};
use duroxide::providers::sqlite::SqliteHistoryStore;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create shared provider
    let store = Arc::new(SqliteHistoryStore::new("sqlite:./management_demo.db").await?);
    
    // Create runtime and management client sharing the same provider
    let runtime = Runtime::new(store.clone()).await?;
    let management = ManagementClient::new(store);
    
    // Or create management client from runtime
    // let management = ManagementClient::from_runtime(&runtime);
    
    // Start some orchestrations
    runtime.start_orchestration("order-1", "ProcessOrder", r#"{"order_id": 123}"#).await?;
    runtime.start_orchestration("order-2", "ProcessOrder", r#"{"order_id": 124}"#).await?;
    runtime.start_orchestration("payment-1", "ProcessPayment", r#"{"amount": 100}"#).await?;
    
    // Management operations
    println!("=== System Health ===");
    let health = management.get_system_health().await?;
    println!("Total instances: {}", health.total_instances);
    println!("Running: {}, Completed: {}, Failed: {}", 
             health.running_instances, health.completed_instances, health.failed_instances);
    println!("Health score: {}/100", health.health_score);
    
    println!("\n=== All Instances ===");
    let instances = management.list_instances(None).await?;
    for instance in instances {
        println!("{}: {} ({})", instance.instance_id, instance.orchestration_name, instance.status);
    }
    
    println!("\n=== Order Processing Instances ===");
    let order_instances = management.find_instances_by_orchestration("ProcessOrder").await?;
    for instance in order_instances {
        println!("{}: {} executions", instance.instance_id, instance.execution_count);
        
        // Get detailed view
        if let Some(details) = management.get_instance_details(&instance.instance_id).await? {
            if let Some(current) = details.current_execution {
                println!("  - {} pending activities", current.pending_activities.len());
                println!("  - {} pending timers", current.pending_timers.len());
            }
        }
    }
    
    println!("\n=== Performance Metrics ===");
    let perf = management.get_performance_metrics("ProcessOrder", None).await?;
    println!("Avg duration: {}ms", perf.avg_duration_ms);
    println!("P95 duration: {}ms", perf.p95_duration_ms);
    
    println!("\n=== Orchestration Stats ===");
    let stats = management.get_orchestration_stats(Some("ProcessOrder")).await?;
    println!("Total: {}, Running: {}, Completed: {}, Failed: {}", 
             stats.total_count, stats.running_count, stats.completed_count, stats.failed_count);
    
    println!("\n=== Problematic Instances ===");
    let problematic = management.get_problematic_instances().await?;
    if !problematic.long_running.is_empty() {
        println!("Long-running instances:");
        for instance in problematic.long_running {
            println!("  - {}: running for {:?}", instance.instance_id, 
                     chrono::Utc::now().signed_duration_since(instance.created_at));
        }
    }
    
    Ok(())
}
```

## Implementation Strategy

### Phase 1: Core Structure
1. Create `src/management/mod.rs` and `src/management/types.rs`
2. Implement basic `ManagementClient` with instance listing
3. Add provider interface extensions with default implementations
4. Create comprehensive test suite

### Phase 2: Analytics & Metrics
1. Implement statistics and performance metrics
2. Add bulk operations for efficiency
3. Create health monitoring capabilities
4. Add queue metrics and system diagnostics

### Phase 3: Advanced Features
1. Real-time monitoring capabilities
2. Advanced filtering and search
3. Export capabilities (JSON, CSV)
4. Integration with monitoring systems

## Benefits

1. **Clean Separation**: Management operations don't interfere with orchestration execution
2. **Shared Data**: Same provider ensures consistent view of data
3. **Read-Only Safety**: No risk of accidentally modifying orchestration state
4. **Rich Analytics**: Comprehensive view of system health and performance
5. **Operational Focus**: Designed specifically for DevOps and monitoring use cases
6. **Extensible**: Easy to add new management capabilities without affecting runtime

This design provides a powerful, safe, and efficient way to observe and manage Duroxide orchestrations while maintaining clear architectural boundaries.
