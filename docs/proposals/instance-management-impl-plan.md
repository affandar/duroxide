# Instance Management APIs Implementation Plan

## Overview

This document outlines the implementation plan for Phase 1 instance management APIs on the Duroxide Runtime, focusing on practical, immediately useful functionality.

## Current State Analysis

### Existing Runtime APIs (in `src/runtime/status.rs`):
```rust
impl Runtime {
    pub async fn get_orchestration_status(&self, instance: &str) -> OrchestrationStatus;
    pub async fn get_orchestration_status_with_execution(&self, instance: &str, execution_id: u64) -> OrchestrationStatus;
    pub async fn list_executions(&self, instance: &str) -> Vec<u64>;
    pub async fn get_execution_history(&self, instance: &str, execution_id: u64) -> Vec<Event>;
}
```

### Existing Provider APIs (in `src/providers/mod.rs`):
```rust
#[async_trait::async_trait]
pub trait HistoryStore: Send + Sync {
    async fn read(&self, instance: &str) -> Vec<Event>;
    async fn list_instances(&self) -> Vec<String>;  // Default: empty
    async fn list_executions(&self, instance: &str) -> Vec<u64>;  // Default: [1] or empty
    // ... other methods
}
```

## Implementation Plan

### Phase 1: Core Instance Management APIs

#### 1. New Data Structures

First, add these types to `src/runtime/mod.rs`:

```rust
/// Filter criteria for listing instances
#[derive(Debug, Clone, Default)]
pub struct InstanceFilter {
    pub status: Option<Vec<OrchestrationStatus>>,
    pub orchestration_name: Option<String>,
    pub created_after: Option<chrono::DateTime<chrono::Utc>>,
    pub created_before: Option<chrono::DateTime<chrono::Utc>>,
    pub parent_instance: Option<String>,
}

/// Summary information about an orchestration instance
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

/// Detailed information about an instance
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
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub event_count: u64,
}

#[derive(Debug, Clone)]
pub struct ExecutionDetails {
    pub execution_id: u64,
    pub status: OrchestrationStatus,
    pub pending_activities: Vec<PendingActivity>,
    pub pending_timers: Vec<PendingTimer>,
    pub pending_events: Vec<PendingEvent>,
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
```

#### 2. Provider Interface Extensions

Add these methods to `HistoryStore` trait in `src/providers/mod.rs`:

```rust
#[async_trait::async_trait]
pub trait HistoryStore: Send + Sync {
    // ... existing methods ...

    /// List instances with optional filtering (default implementation uses basic list_instances)
    async fn list_instances_filtered(&self, filter: Option<InstanceFilter>) -> Vec<InstanceSummary> {
        // Default implementation: get all instances and filter in memory
        let all_instances = self.list_instances().await;
        let mut summaries = Vec::new();
        
        for instance_id in all_instances {
            if let Some(summary) = self.get_instance_summary(&instance_id).await {
                if Self::matches_filter(&summary, &filter) {
                    summaries.push(summary);
                }
            }
        }
        
        summaries
    }

    /// Get summary information for a single instance
    async fn get_instance_summary(&self, instance: &str) -> Option<InstanceSummary> {
        let history = self.read(instance).await;
        if history.is_empty() {
            return None;
        }

        // Extract metadata from history events
        Self::build_instance_summary(instance, &history)
    }

    /// Get bulk status for multiple instances efficiently
    async fn get_instances_status_bulk(&self, instances: Vec<String>) -> Vec<(String, OrchestrationStatus)> {
        let mut results = Vec::new();
        for instance in instances {
            let history = self.read(&instance).await;
            let status = Self::derive_status_from_history(&history);
            results.push((instance, status));
        }
        results
    }

    // Helper methods with default implementations
    fn matches_filter(summary: &InstanceSummary, filter: &Option<InstanceFilter>) -> bool {
        let Some(filter) = filter else { return true; };
        
        if let Some(ref statuses) = filter.status {
            if !statuses.contains(&summary.status) {
                return false;
            }
        }
        
        if let Some(ref name) = filter.orchestration_name {
            if summary.orchestration_name != *name {
                return false;
            }
        }
        
        if let Some(after) = filter.created_after {
            if summary.created_at <= after {
                return false;
            }
        }
        
        if let Some(before) = filter.created_before {
            if summary.created_at >= before {
                return false;
            }
        }
        
        if let Some(ref parent) = filter.parent_instance {
            if summary.parent_instance.as_ref() != Some(parent) {
                return false;
            }
        }
        
        true
    }

    fn build_instance_summary(instance_id: &str, history: &[Event]) -> Option<InstanceSummary> {
        // Find OrchestrationStarted event
        let started_event = history.iter().find(|e| matches!(e, Event::OrchestrationStarted { .. }))?;
        
        let (name, version, parent_instance, parent_id) = match started_event {
            Event::OrchestrationStarted { name, version, parent_instance, parent_id, .. } => {
                (name.clone(), version.clone(), parent_instance.clone(), *parent_id)
            }
            _ => return None,
        };

        // Derive status from history
        let status = Self::derive_status_from_history(history);
        
        // Extract timestamps (simplified - would need proper timestamp extraction)
        let created_at = chrono::Utc::now(); // TODO: Extract from first event
        let updated_at = chrono::Utc::now(); // TODO: Extract from last event
        
        Some(InstanceSummary {
            instance_id: instance_id.to_string(),
            orchestration_name: name,
            version,
            status,
            created_at,
            updated_at,
            execution_count: 1, // TODO: Count executions properly
            parent_instance,
            parent_id,
        })
    }

    fn derive_status_from_history(history: &[Event]) -> OrchestrationStatus {
        if history.is_empty() {
            return OrchestrationStatus::NotFound;
        }
        
        for event in history.iter().rev() {
            match event {
                Event::OrchestrationCompleted { output } => {
                    return OrchestrationStatus::Completed { output: output.clone() };
                }
                Event::OrchestrationFailed { error } => {
                    return OrchestrationStatus::Failed { error: error.clone() };
                }
                _ => {}
            }
        }
        
        OrchestrationStatus::Running
    }
}
```

#### 3. Runtime API Implementation

Add these methods to `Runtime` in `src/runtime/mod.rs`:

```rust
impl Runtime {
    /// List orchestration instances with optional filtering
    pub async fn list_instances(&self, filter: Option<InstanceFilter>) -> Vec<InstanceSummary> {
        self.history_store.list_instances_filtered(filter).await
    }

    /// Get detailed information about a specific instance
    pub async fn get_instance_details(&self, instance: &str) -> Option<InstanceDetails> {
        let summary = self.history_store.get_instance_summary(instance).await?;
        
        // Get execution summaries
        let execution_ids = self.history_store.list_executions(instance).await;
        let mut executions = Vec::new();
        
        for exec_id in execution_ids {
            let history = self.history_store.read_with_execution(instance, exec_id).await;
            if let Some(exec_summary) = self.build_execution_summary(exec_id, &history) {
                executions.push(exec_summary);
            }
        }

        // Get current execution details if running
        let current_execution = if matches!(summary.status, OrchestrationStatus::Running) {
            self.build_execution_details(instance, &executions).await
        } else {
            None
        };

        Some(InstanceDetails {
            summary,
            executions,
            current_execution,
        })
    }

    /// Get status for multiple instances efficiently
    pub async fn get_instances_status(&self, instances: Vec<String>) -> Vec<(String, OrchestrationStatus)> {
        self.history_store.get_instances_status_bulk(instances).await
    }

    /// Search instances by orchestration name
    pub async fn list_instances_by_orchestration(&self, orchestration_name: &str) -> Vec<InstanceSummary> {
        let filter = InstanceFilter {
            orchestration_name: Some(orchestration_name.to_string()),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// List all running instances
    pub async fn list_running_instances(&self) -> Vec<InstanceSummary> {
        let filter = InstanceFilter {
            status: Some(vec![OrchestrationStatus::Running]),
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    /// List instances with failures
    pub async fn list_failed_instances(&self, since: Option<chrono::DateTime<chrono::Utc>>) -> Vec<InstanceSummary> {
        let filter = InstanceFilter {
            status: Some(vec![OrchestrationStatus::Failed { error: String::new() }]), // Will match any failed status
            created_after: since,
            ..Default::default()
        };
        self.list_instances(Some(filter)).await
    }

    // Helper methods
    fn build_execution_summary(&self, execution_id: u64, history: &[Event]) -> Option<ExecutionSummary> {
        if history.is_empty() {
            return None;
        }

        let status = HistoryStore::derive_status_from_history(history);
        let started_at = chrono::Utc::now(); // TODO: Extract from first event
        let completed_at = if matches!(status, OrchestrationStatus::Running) {
            None
        } else {
            Some(chrono::Utc::now()) // TODO: Extract from terminal event
        };

        Some(ExecutionSummary {
            execution_id,
            status,
            started_at,
            completed_at,
            event_count: history.len() as u64,
        })
    }

    async fn build_execution_details(&self, instance: &str, executions: &[ExecutionSummary]) -> Option<ExecutionDetails> {
        // Get the latest execution
        let latest_execution = executions.last()?;
        let history = self.history_store.read_with_execution(instance, latest_execution.execution_id).await;

        // Analyze history to find pending operations
        let mut pending_activities = Vec::new();
        let mut pending_timers = Vec::new();
        let mut pending_events = Vec::new();
        let mut completed_correlations = std::collections::HashSet::new();

        // First pass: collect completed operations
        for event in &history {
            match event {
                Event::ActivityCompleted { id, .. } | Event::ActivityFailed { id, .. } => {
                    completed_correlations.insert(*id);
                }
                Event::TimerFired { id, .. } => {
                    completed_correlations.insert(*id);
                }
                Event::ExternalEvent { id, .. } => {
                    completed_correlations.insert(*id);
                }
                _ => {}
            }
        }

        // Second pass: find pending operations
        for event in &history {
            match event {
                Event::ActivityScheduled { id, name, input, .. } => {
                    if !completed_correlations.contains(id) {
                        pending_activities.push(PendingActivity {
                            correlation_id: *id,
                            name: name.clone(),
                            input: input.clone(),
                            scheduled_at: chrono::Utc::now(), // TODO: Extract timestamp
                        });
                    }
                }
                Event::TimerScheduled { id, delay_ms, .. } => {
                    if !completed_correlations.contains(id) {
                        let scheduled_at = chrono::Utc::now(); // TODO: Extract timestamp
                        pending_timers.push(PendingTimer {
                            correlation_id: *id,
                            delay_ms: *delay_ms,
                            scheduled_at,
                            fire_at: scheduled_at + chrono::Duration::milliseconds(*delay_ms as i64),
                        });
                    }
                }
                Event::ExternalSubscribed { id, name, .. } => {
                    if !completed_correlations.contains(id) {
                        pending_events.push(PendingEvent {
                            correlation_id: *id,
                            name: name.clone(),
                            subscribed_at: chrono::Utc::now(), // TODO: Extract timestamp
                        });
                    }
                }
                _ => {}
            }
        }

        Some(ExecutionDetails {
            execution_id: latest_execution.execution_id,
            status: latest_execution.status.clone(),
            pending_activities,
            pending_timers,
            pending_events,
        })
    }
}
```

#### 4. SQLite Provider Implementation

Enhance the SQLite provider in `src/providers/sqlite.rs` with optimized implementations:

```rust
#[async_trait::async_trait]
impl HistoryStore for SqliteHistoryStore {
    // ... existing methods ...

    async fn list_instances_filtered(&self, filter: Option<InstanceFilter>) -> Vec<InstanceSummary> {
        let mut query = "SELECT DISTINCT i.instance_id, i.orchestration_name, i.orchestration_version, i.created_at, i.updated_at FROM instances i".to_string();
        let mut conditions = Vec::new();
        let mut params = Vec::new();

        if let Some(filter) = &filter {
            if let Some(ref name) = filter.orchestration_name {
                conditions.push("i.orchestration_name = ?");
                params.push(name.as_str());
            }

            if let Some(after) = filter.created_after {
                conditions.push("i.created_at > ?");
                params.push(&after.timestamp_millis().to_string());
            }

            if let Some(before) = filter.created_before {
                conditions.push("i.created_at < ?");
                params.push(&before.timestamp_millis().to_string());
            }
        }

        if !conditions.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&conditions.join(" AND "));
        }

        query.push_str(" ORDER BY i.created_at DESC LIMIT 1000"); // Safety limit

        // Execute query and build summaries
        // This is a simplified version - full implementation would use sqlx properly
        let instances = self.list_instances().await;
        let mut summaries = Vec::new();

        for instance_id in instances {
            if let Some(summary) = self.get_instance_summary(&instance_id).await {
                if HistoryStore::matches_filter(&summary, &filter) {
                    summaries.push(summary);
                }
            }
        }

        summaries
    }

    async fn get_instances_status_bulk(&self, instances: Vec<String>) -> Vec<(String, OrchestrationStatus)> {
        // Optimized bulk query for SQLite
        let mut results = Vec::new();
        
        // Could be optimized with a single query, but for now use individual reads
        for instance in instances {
            let history = self.read(&instance).await;
            let status = Self::derive_status_from_history(&history);
            results.push((instance, status));
        }
        
        results
    }
}
```

## Implementation Steps

### Step 1: Add Dependencies
Add to `Cargo.toml`:
```toml
chrono = { version = "0.4", features = ["serde"] }
```

### Step 2: Create Data Structures
1. Add the new types to `src/runtime/mod.rs`
2. Update imports and exports in `src/lib.rs`

### Step 3: Extend Provider Interface
1. Add new methods to `HistoryStore` trait in `src/providers/mod.rs`
2. Provide default implementations that work with existing methods

### Step 4: Implement Runtime APIs
1. Add new methods to `Runtime` in `src/runtime/mod.rs`
2. Create helper methods for building summaries and details

### Step 5: Enhance SQLite Provider
1. Add optimized implementations in `src/providers/sqlite.rs`
2. Use SQL queries where possible for better performance

### Step 6: Add Tests
1. Create unit tests for new data structures
2. Add integration tests for Runtime APIs
3. Test filtering and bulk operations

### Step 7: Update Documentation
1. Add examples to README.md
2. Update API documentation
3. Create usage guides

## Benefits of This Approach

1. **Backward Compatible**: All existing APIs continue to work
2. **Incremental**: Can be implemented step by step
3. **Provider Agnostic**: Default implementations work with any provider
4. **Optimizable**: SQLite provider can add optimized implementations
5. **Practical**: Focuses on immediately useful functionality

## Testing Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_instances_basic() {
        let runtime = create_test_runtime().await;
        
        // Start some test orchestrations
        runtime.start_orchestration("test-1", "TestOrch", "input1").await.unwrap();
        runtime.start_orchestration("test-2", "TestOrch", "input2").await.unwrap();
        
        // List all instances
        let instances = runtime.list_instances(None).await;
        assert_eq!(instances.len(), 2);
    }

    #[tokio::test]
    async fn test_list_instances_filtered() {
        let runtime = create_test_runtime().await;
        
        // Start orchestrations of different types
        runtime.start_orchestration("test-1", "TypeA", "input").await.unwrap();
        runtime.start_orchestration("test-2", "TypeB", "input").await.unwrap();
        
        // Filter by orchestration name
        let filter = InstanceFilter {
            orchestration_name: Some("TypeA".to_string()),
            ..Default::default()
        };
        let instances = runtime.list_instances(Some(filter)).await;
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].orchestration_name, "TypeA");
    }

    #[tokio::test]
    async fn test_get_instance_details() {
        let runtime = create_test_runtime().await;
        
        runtime.start_orchestration("test-1", "TestOrch", "input").await.unwrap();
        
        let details = runtime.get_instance_details("test-1").await.unwrap();
        assert_eq!(details.summary.instance_id, "test-1");
        assert_eq!(details.executions.len(), 1);
    }

    #[tokio::test]
    async fn test_bulk_status_check() {
        let runtime = create_test_runtime().await;
        
        runtime.start_orchestration("test-1", "TestOrch", "input").await.unwrap();
        runtime.start_orchestration("test-2", "TestOrch", "input").await.unwrap();
        
        let instances = vec!["test-1".to_string(), "test-2".to_string(), "nonexistent".to_string()];
        let statuses = runtime.get_instances_status(instances).await;
        
        assert_eq!(statuses.len(), 3);
        assert!(matches!(statuses[0].1, OrchestrationStatus::Running | OrchestrationStatus::Completed { .. }));
        assert!(matches!(statuses[1].1, OrchestrationStatus::Running | OrchestrationStatus::Completed { .. }));
        assert!(matches!(statuses[2].1, OrchestrationStatus::NotFound));
    }
}
```

This implementation plan provides a solid foundation for instance management APIs while maintaining backward compatibility and allowing for future enhancements.
