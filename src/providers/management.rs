//! Management and observability provider interface.
//!
//! Separate from the core Provider trait, this interface provides
//! administrative and debugging capabilities.

use crate::Event;

/// Management provider for observability and administrative operations.
///
/// This trait is separate from `Provider` to:
/// - Separate hot-path (runtime) from cold-path (admin) operations
/// - Allow different implementations (e.g., read replicas, analytics DBs)
/// - Enable extension without breaking the core Provider interface
///
/// # Implementation
///
/// Providers can implement this alongside `Provider`:
///
/// ```ignore
/// impl Provider for SqliteProvider { /* runtime ops */ }
/// impl ManagementProvider for SqliteProvider { /* admin ops */ }
/// ```
///
/// # Usage
///
/// ```ignore
/// let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);
/// let mgmt: Arc<dyn ManagementProvider> = store.clone();
///
/// // List all instances
/// let instances = mgmt.list_instances().await?;
///
/// // Get execution details
/// let executions = mgmt.list_executions("order-123").await?;
/// let history = mgmt.read_execution("order-123", 1).await?;
/// ```
#[async_trait::async_trait]
pub trait ManagementProvider: Send + Sync {
    // ===== Instance Discovery =====

    /// List all known instance IDs.
    ///
    /// # Returns
    ///
    /// Vector of instance IDs, typically sorted by creation time (newest first).
    ///
    /// # Use Cases
    ///
    /// - Admin dashboards showing all workflows
    /// - Bulk operations across instances
    /// - Testing (verify instance creation)
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn list_instances(&self) -> Result<Vec<String>, String> {
    ///     SELECT instance_id FROM instances ORDER BY created_at DESC
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns empty Vec if not supported.
    async fn list_instances(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    /// List instances matching a status filter.
    ///
    /// # Parameters
    ///
    /// * `status` - Filter by execution status: "Running", "Completed", "Failed", "ContinuedAsNew"
    ///
    /// # Returns
    ///
    /// Vector of instance IDs with the specified status.
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn list_instances_by_status(&self, status: &str) -> Result<Vec<String>, String> {
    ///     SELECT i.instance_id FROM instances i
    ///     JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
    ///     WHERE e.status = ?
    ///     ORDER BY i.created_at DESC
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns empty Vec if not supported.
    async fn list_instances_by_status(&self, _status: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    // ===== Execution Inspection =====

    /// List all execution IDs for an instance.
    ///
    /// # Returns
    ///
    /// Vector of execution IDs in ascending order: [1], [1, 2], [1, 2, 3], etc.
    ///
    /// # Multi-Execution Context
    ///
    /// When an orchestration uses ContinueAsNew, multiple executions exist:
    /// - Execution 1: Initial run, ends with OrchestrationContinuedAsNew
    /// - Execution 2: Continuation, may end with Completed or another ContinueAsNew
    /// - etc.
    ///
    /// # Use Cases
    ///
    /// - Verify ContinueAsNew created multiple executions
    /// - Debug execution progression
    /// - Audit trail inspection
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String> {
    ///     SELECT execution_id FROM executions
    ///     WHERE instance_id = ?
    ///     ORDER BY execution_id ASC
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns [1] if instance exists, empty Vec otherwise.
    async fn list_executions(&self, _instance: &str) -> Result<Vec<u64>, String> {
        // Default: assume single execution
        Ok(vec![1])
    }

    /// Read history for a specific execution.
    ///
    /// # Parameters
    ///
    /// * `instance` - Instance ID
    /// * `execution_id` - Specific execution to read (1, 2, 3, ...)
    ///
    /// # Returns
    ///
    /// Events for the specified execution, ordered by event_id.
    ///
    /// # Use Cases
    ///
    /// - Debug specific execution in multi-execution instance
    /// - Inspect what happened in execution 1 after ContinueAsNew created execution 2
    /// - Audit trail for specific execution
    ///
    /// # Difference from Provider.read()
    ///
    /// - `Provider.read(instance)` → Returns LATEST execution's history
    /// - `ManagementProvider.read_execution(instance, exec_id)` → Returns SPECIFIC execution's history
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String> {
    ///     SELECT event_data FROM history
    ///     WHERE instance_id = ? AND execution_id = ?
    ///     ORDER BY event_id ASC
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns error indicating not supported.
    async fn read_execution(&self, instance: &str, _execution_id: u64) -> Result<Vec<Event>, String> {
        Err(format!("read_execution not supported for instance: {instance}"))
    }

    /// Get the latest (current) execution ID for an instance.
    ///
    /// # Returns
    ///
    /// * `Ok(execution_id)` - The highest execution ID for this instance
    /// * `Err(msg)` - Instance not found or error
    ///
    /// # Use Cases
    ///
    /// - Determine how many times an instance has continued
    /// - Check current execution number
    /// - Debugging multi-execution workflows
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn latest_execution_id(&self, instance: &str) -> Result<u64, String> {
    ///     SELECT COALESCE(MAX(execution_id), 1) FROM executions WHERE instance_id = ?
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns 1 (assumes single execution).
    async fn latest_execution_id(&self, _instance: &str) -> Result<u64, String> {
        Ok(1)
    }

    // ===== Instance Metadata =====

    /// Get comprehensive information about an instance.
    ///
    /// # Returns
    ///
    /// Metadata about the instance including name, version, status, timestamps.
    ///
    /// # Use Cases
    ///
    /// - Admin dashboard showing instance details
    /// - CLI tools displaying instance info
    /// - Monitoring systems
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
    ///     SELECT i.orchestration_name, i.orchestration_version, i.current_execution_id,
    ///            e.status, e.output, i.created_at, e.completed_at
    ///     FROM instances i
    ///     LEFT JOIN executions e ON i.instance_id = e.instance_id
    ///         AND i.current_execution_id = e.execution_id
    ///     WHERE i.instance_id = ?
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns error indicating not supported.
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        Err(format!("get_instance_info not supported for instance: {instance}"))
    }

    /// Get detailed metadata for a specific execution.
    ///
    /// # Returns
    ///
    /// Information about a specific execution including status, output, event count, timestamps.
    ///
    /// # Use Cases
    ///
    /// - Inspect individual executions in ContinueAsNew workflows
    /// - Debug execution-specific issues
    /// - Performance analysis (event count, duration)
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, String> {
    ///     SELECT status, output, started_at, completed_at,
    ///            (SELECT COUNT(*) FROM history WHERE instance_id = ? AND execution_id = ?) as event_count
    ///     FROM executions
    ///     WHERE instance_id = ? AND execution_id = ?
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns error indicating not supported.
    async fn get_execution_info(&self, instance: &str, _execution_id: u64) -> Result<ExecutionInfo, String> {
        Err(format!("get_execution_info not supported for instance: {instance}"))
    }

    // ===== System Metrics =====

    /// Get system-wide orchestration metrics.
    ///
    /// # Returns
    ///
    /// Aggregate statistics: total instances, running count, completed count, failed count, etc.
    ///
    /// # Use Cases
    ///
    /// - Monitoring dashboards
    /// - Health checks
    /// - Capacity planning
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_system_metrics(&self) -> Result<SystemMetrics, String> {
    ///     SELECT
    ///         COUNT(DISTINCT i.instance_id) as total_instances,
    ///         COUNT(DISTINCT e.execution_id) as total_executions,
    ///         SUM(CASE WHEN e.status = 'Running' THEN 1 ELSE 0 END) as running,
    ///         SUM(CASE WHEN e.status = 'Completed' THEN 1 ELSE 0 END) as completed,
    ///         SUM(CASE WHEN e.status = 'Failed' THEN 1 ELSE 0 END) as failed
    ///     FROM instances i
    ///     JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns default/empty metrics.
    async fn get_system_metrics(&self) -> Result<SystemMetrics, String> {
        Ok(SystemMetrics::default())
    }

    /// Get current queue depths.
    ///
    /// # Returns
    ///
    /// Number of unlocked messages in each queue.
    ///
    /// # Use Cases
    ///
    /// - Monitor backlog
    /// - Capacity planning
    /// - Performance troubleshooting
    ///
    /// # Implementation Example
    ///
    /// ```ignore
    /// async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
    ///     SELECT
    ///         (SELECT COUNT(*) FROM orchestrator_queue WHERE lock_token IS NULL) as orch,
    ///         (SELECT COUNT(*) FROM worker_queue WHERE lock_token IS NULL) as worker,
    ///         (SELECT COUNT(*) FROM timer_queue WHERE lock_token IS NULL) as timer
    /// }
    /// ```
    ///
    /// # Default
    ///
    /// Returns zeros.
    async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
        Ok(QueueDepths::default())
    }
}

// ===== Supporting Types =====

/// Comprehensive instance metadata.
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    pub instance_id: String,
    pub orchestration_name: String,
    pub orchestration_version: String,
    pub current_execution_id: u64,
    pub status: String,         // "Running", "Completed", "Failed", "ContinuedAsNew"
    pub output: Option<String>, // Terminal output or error
    pub created_at: u64,        // Milliseconds since epoch
    pub updated_at: u64,
}

/// Execution-specific metadata.
#[derive(Debug, Clone)]
pub struct ExecutionInfo {
    pub execution_id: u64,
    pub status: String,            // "Running", "Completed", "Failed", "ContinuedAsNew"
    pub output: Option<String>,    // Terminal output, error, or next input
    pub started_at: u64,           // Milliseconds since epoch
    pub completed_at: Option<u64>, // None if still running
    pub event_count: usize,        // Number of events in this execution
}

/// System-wide orchestration metrics.
#[derive(Debug, Clone, Default)]
pub struct SystemMetrics {
    pub total_instances: u64,
    pub total_executions: u64,
    pub running_instances: u64,
    pub completed_instances: u64,
    pub failed_instances: u64,
    pub total_events: u64,
}

/// Queue depth information.
#[derive(Debug, Clone, Default)]
pub struct QueueDepths {
    pub orchestrator_queue: usize, // Unlocked orchestrator messages
    pub worker_queue: usize,       // Unlocked worker messages
    pub timer_queue: usize,        // Unlocked timer messages
}
