# Management API Improvements Proposal

> **Status**: Proposed  
> **Issue**: [#10](https://github.com/affandar/duroxide/issues/10)  
> **Related**: [Core Improvements Roadmap](proposals/core-improvements-roadmap.md#3-management-api-improvements)

## Overview

This proposal details improvements to the duroxide Management API to address gaps in production workloads: the N+1 query problem, missing data cleanup APIs, pagination/filtering limitations, and query performance at scale.

---

## Table of Contents

1. [Problem Statement](#problem-statement)
2. [Enriched List Responses](#1-enriched-list-responses)
3. [Truncate APIs](#2-truncate-apis)
4. [Pagination & Filtering](#3-pagination--filtering)
5. [Performance Optimizations](#4-performance-optimizations)
6. [Schema Changes](#5-schema-changes)
7. [Error Handling](#6-error-handling)
8. [Implementation Phases](#7-implementation-phases)
9. [Open Questions](#8-open-questions)

---

## Problem Statement

The current management API has significant gaps that limit its usefulness for production workloads:

### N+1 Query Problem

`list_instances()` returns only instance IDs, forcing clients to make N additional calls to `get_instance_info()` for basic dashboard data:

```rust
// Current: O(N+1) queries for N instances
let ids = client.list_all_instances().await?;  // 1 query
for id in &ids {
    let info = client.get_instance_info(id).await?;  // N queries
    println!("{}: {}", info.orchestration_name, info.status);
}
```

For 1,000 instances, this means 1,001 database round-trips. Real-world dashboards become unusable.

### No Data Cleanup APIs

There's no way to:
- Delete completed/failed instances after testing
- Truncate history for eternal orchestrations (continuous/polling workflows)
- Clean up orphaned data from crashed workers
- Bulk delete instances matching criteria

Eternal orchestrations using `continue_as_new` accumulate unbounded history:

```
Instance "daily-report"
├── Execution 1: 500 events (day 1)
├── Execution 2: 500 events (day 2)
├── ... 
└── Execution 365: 500 events (day 365)
    → 182,500 events, growing forever
```

### Performance at Scale

Current implementation has no pagination or efficient filtering:
- `list_instances()` returns ALL instances in memory
- `list_instances_by_status()` scans entire table
- No indexes optimized for management queries
- Dashboard queries compete with hot-path operations

### Missing Query Capabilities

Operators cannot answer basic questions:
- "Show me all failed `OrderWorkflow` instances from last hour"
- "How many `PaymentProcessor` instances are currently running?"
- "Find instances stuck in running state for > 24 hours"

---

## Proposed Solution

This proposal introduces four categories of improvements:
1. **Enriched List Responses** — Return full metadata in single query
2. **Truncate APIs** — Delete instances and prune execution history
3. **Pagination & Filtering** — Scalable queries with cursor-based pagination
4. **Performance Optimizations** — Indexes, batching, and query patterns

---

## 1. Enriched List Responses

### 1.1 Enhanced InstanceInfo Struct

Extend the existing `InstanceInfo` to include additional useful fields:

```rust
/// Comprehensive instance metadata for management operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    // Existing fields
    pub instance_id: String,
    pub orchestration_name: String,
    pub orchestration_version: String,
    pub current_execution_id: u64,
    pub status: String,
    pub output: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    
    // NEW: Additional metadata
    /// Total number of executions (for continue_as_new tracking)
    pub execution_count: u64,
    /// Total events across all executions (storage indicator)
    pub total_event_count: u64,
    /// Input provided when instance was started
    pub input: Option<String>,
    /// Tags for categorization (if tag support added)
    pub tags: Option<Vec<String>>,
}
```

### 1.2 Batch List API

New provider trait method to return enriched data in a single query:

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    /// List all instances with full metadata in a single query.
    /// 
    /// Unlike `list_instances()` which returns only IDs, this method
    /// returns complete `InstanceInfo` for each instance, eliminating
    /// the N+1 query problem.
    ///
    /// # Performance
    /// 
    /// Single SQL query with JOINs and aggregations. For large datasets,
    /// prefer `list_instances_paginated()` to avoid memory issues.
    ///
    /// # Implementation Example
    ///
    /// ```sql
    /// SELECT 
    ///     i.instance_id,
    ///     i.orchestration_name,
    ///     i.orchestration_version,
    ///     i.current_execution_id,
    ///     i.created_at,
    ///     i.updated_at,
    ///     e.status,
    ///     e.output,
    ///     (SELECT COUNT(*) FROM executions WHERE instance_id = i.instance_id) as execution_count,
    ///     (SELECT COUNT(*) FROM history WHERE instance_id = i.instance_id) as total_event_count
    /// FROM instances i
    /// LEFT JOIN executions e ON i.instance_id = e.instance_id 
    ///     AND i.current_execution_id = e.execution_id
    /// ORDER BY i.created_at DESC
    /// ```
    async fn list_instances_with_info(&self) -> Result<Vec<InstanceInfo>, ProviderError>;
}
```

### 1.3 Client API Addition

```rust
impl Client {
    /// List all instances with full metadata.
    ///
    /// Returns complete instance information in a single database query,
    /// avoiding the N+1 problem of calling `list_all_instances()` followed
    /// by `get_instance_info()` for each.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let instances = client.list_instances_with_info().await?;
    /// for info in instances {
    ///     println!("{} ({}): {} - {} events", 
    ///         info.instance_id,
    ///         info.orchestration_name,
    ///         info.status,
    ///         info.total_event_count
    ///     );
    /// }
    /// ```
    pub async fn list_instances_with_info(&self) -> Result<Vec<InstanceInfo>, ClientError>;
}
```

---

## 2. Truncate APIs

### 2.1 Instance Deletion

Complete deletion of an instance and all associated data:

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    /// Delete an instance and all its associated data.
    ///
    /// Removes:
    /// - Instance record from `instances` table
    /// - All executions from `executions` table
    /// - All history events from `history` table
    /// - All pending queue messages for this instance
    /// - Any instance locks
    ///
    /// # Safety
    ///
    /// This operation is IRREVERSIBLE. The instance and all its history
    /// will be permanently deleted.
    ///
    /// # Constraints
    ///
    /// - Instance must NOT be in "Running" status (prevents deleting active work)
    /// - Use `force = true` to override (for cleanup of stuck instances)
    ///
    /// # Returns
    ///
    /// `TruncateResult` with counts of deleted records.
    ///
    /// # Errors
    ///
    /// - `InstanceNotFound` if instance doesn't exist
    /// - `InstanceStillRunning` if instance is active and force=false
    async fn delete_instance(
        &self,
        instance_id: &str,
        force: bool,
    ) -> Result<TruncateResult, ProviderError>;
    
    /// Bulk delete instances matching criteria.
    ///
    /// Efficiently deletes multiple instances in a single transaction.
    /// Useful for cleanup operations.
    ///
    /// # Parameters
    ///
    /// * `filter` - Criteria for selecting instances to delete
    ///
    /// # Returns
    ///
    /// Total counts of deleted records across all instances.
    async fn delete_instances_bulk(
        &self,
        filter: InstanceFilter,
    ) -> Result<TruncateResult, ProviderError>;
}

/// Result of truncation/deletion operations.
#[derive(Debug, Clone, Default)]
pub struct TruncateResult {
    /// Number of instance records deleted
    pub instances_deleted: u64,
    /// Number of execution records deleted
    pub executions_deleted: u64,
    /// Number of history events deleted
    pub events_deleted: u64,
    /// Number of queue messages deleted
    pub messages_deleted: u64,
}
```

### 2.2 Execution History Pruning

For eternal orchestrations, prune old executions while keeping recent ones:

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    /// Prune old executions for an instance, keeping only recent ones.
    ///
    /// This is critical for "eternal" orchestrations that use `continue_as_new`
    /// repeatedly. Without pruning, history grows unbounded.
    ///
    /// # Parameters
    ///
    /// * `instance_id` - Instance to prune
    /// * `keep_last_n` - Number of most recent executions to retain
    ///
    /// # Behavior
    ///
    /// - Deletes executions where `execution_id < (current - keep_last_n)`
    /// - Deletes associated history events
    /// - NEVER deletes the current execution (even if keep_last_n = 0)
    ///
    /// # Example
    ///
    /// Instance has executions [1, 2, 3, 4, 5], current is 5:
    /// - `prune_executions("inst", 2)` → deletes 1, 2, 3; keeps 4, 5
    /// - `prune_executions("inst", 0)` → deletes 1, 2, 3, 4; keeps 5
    ///
    /// # Returns
    ///
    /// Number of executions deleted.
    async fn prune_executions(
        &self,
        instance_id: &str,
        keep_last_n: u64,
    ) -> Result<u64, ProviderError>;

    /// Prune executions older than a specified age.
    ///
    /// # Parameters
    ///
    /// * `instance_id` - Instance to prune
    /// * `older_than` - Delete executions completed before this duration ago
    ///
    /// # Returns
    ///
    /// Number of executions deleted.
    async fn prune_executions_by_age(
        &self,
        instance_id: &str,
        older_than: Duration,
    ) -> Result<u64, ProviderError>;
}
```

### 2.3 Automatic Retention Policy

Allow configuring automatic pruning for eternal orchestrations:

```rust
/// Retention policy for automatic execution pruning.
#[derive(Debug, Clone)]
pub struct ExecutionRetentionPolicy {
    /// Keep at least this many recent executions.
    /// Set to 0 to keep only the current execution.
    pub keep_last_n: u64,
    
    /// Delete executions older than this duration.
    /// Applied after keep_last_n (both conditions must be met for deletion).
    pub keep_duration: Option<Duration>,
    
    /// Run pruning automatically after each continue_as_new.
    /// If false, pruning must be triggered manually or via scheduled job.
    pub auto_prune: bool,
}

impl Default for ExecutionRetentionPolicy {
    fn default() -> Self {
        Self {
            keep_last_n: 10,
            keep_duration: Some(Duration::from_secs(7 * 24 * 60 * 60)), // 7 days
            auto_prune: false,
        }
    }
}

/// Configure retention when starting an orchestration.
impl Client {
    pub async fn start_orchestration_with_options(
        &self,
        name: &str,
        version: &str,
        instance_id: &str,
        input: Option<&str>,
        options: StartOptions,
    ) -> Result<(), ClientError>;
}

#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// Execution retention policy for this instance.
    /// Only meaningful for orchestrations that use continue_as_new.
    pub retention_policy: Option<ExecutionRetentionPolicy>,
    
    /// Tags for categorization and filtering.
    pub tags: Option<Vec<String>>,
}
```

### 2.4 Client API for Truncation

```rust
impl Client {
    /// Delete a completed or failed instance.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Clean up after testing
    /// client.delete_instance("test-instance-123", false).await?;
    ///
    /// // Force delete a stuck instance
    /// client.delete_instance("stuck-instance", true).await?;
    /// ```
    pub async fn delete_instance(
        &self,
        instance_id: &str,
        force: bool,
    ) -> Result<TruncateResult, ClientError>;
    
    /// Prune old executions for an eternal orchestration.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Keep only last 5 executions
    /// let deleted = client.prune_executions("daily-job", 5).await?;
    /// println!("Pruned {} old executions", deleted);
    /// ```
    pub async fn prune_executions(
        &self,
        instance_id: &str,
        keep_last_n: u64,
    ) -> Result<u64, ClientError>;
    
    /// Bulk delete instances matching criteria.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Delete all completed instances older than 30 days
    /// let filter = InstanceFilter::new()
    ///     .with_status("Completed")
    ///     .older_than(Duration::from_secs(30 * 24 * 60 * 60));
    /// let result = client.delete_instances_bulk(filter).await?;
    /// println!("Deleted {} instances", result.instances_deleted);
    /// ```
    pub async fn delete_instances_bulk(
        &self,
        filter: InstanceFilter,
    ) -> Result<TruncateResult, ClientError>;
}
```

---

## 3. Pagination & Filtering

### 3.1 Filter Criteria

```rust
/// Filter criteria for listing instances.
#[derive(Debug, Clone, Default)]
pub struct InstanceFilter {
    /// Filter by orchestration status.
    /// Multiple values = OR (matches any).
    pub status: Option<Vec<String>>,
    
    /// Filter by orchestration name (exact match).
    pub orchestration_name: Option<String>,
    
    /// Filter by orchestration name prefix.
    pub orchestration_name_prefix: Option<String>,
    
    /// Filter by instance ID prefix.
    pub instance_id_prefix: Option<String>,
    
    /// Only include instances created after this timestamp (millis).
    pub created_after: Option<u64>,
    
    /// Only include instances created before this timestamp (millis).
    pub created_before: Option<u64>,
    
    /// Only include instances updated after this timestamp (millis).
    pub updated_after: Option<u64>,
    
    /// Only include instances updated before this timestamp (millis).
    pub updated_before: Option<u64>,
    
    /// Filter by tags (if tag support is implemented).
    /// Multiple values = AND (must have all tags).
    pub tags: Option<Vec<String>>,
}

impl InstanceFilter {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn with_status(mut self, status: &str) -> Self {
        self.status = Some(vec![status.to_string()]);
        self
    }
    
    pub fn with_statuses(mut self, statuses: Vec<&str>) -> Self {
        self.status = Some(statuses.into_iter().map(String::from).collect());
        self
    }
    
    pub fn with_orchestration_name(mut self, name: &str) -> Self {
        self.orchestration_name = Some(name.to_string());
        self
    }
    
    pub fn with_name_prefix(mut self, prefix: &str) -> Self {
        self.orchestration_name_prefix = Some(prefix.to_string());
        self
    }
    
    pub fn created_after(mut self, timestamp: u64) -> Self {
        self.created_after = Some(timestamp);
        self
    }
    
    pub fn older_than(mut self, duration: Duration) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.created_before = Some(now.saturating_sub(duration.as_millis() as u64));
        self
    }
    
    /// Check if any filters are set.
    pub fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.orchestration_name.is_none()
            && self.orchestration_name_prefix.is_none()
            && self.instance_id_prefix.is_none()
            && self.created_after.is_none()
            && self.created_before.is_none()
            && self.updated_after.is_none()
            && self.updated_before.is_none()
            && self.tags.is_none()
    }
}
```

### 3.2 Pagination with Cursors

```rust
/// Cursor-based pagination for scalable listing.
#[derive(Debug, Clone)]
pub struct PaginationOptions {
    /// Maximum number of results to return.
    /// Default: 100, Max: 1000
    pub limit: u32,
    
    /// Opaque cursor from previous page's `next_cursor`.
    /// None = start from beginning.
    pub cursor: Option<String>,
    
    /// Sort order.
    pub order: SortOrder,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum SortOrder {
    /// Newest first (by created_at DESC)
    #[default]
    CreatedDesc,
    /// Oldest first (by created_at ASC)
    CreatedAsc,
    /// Most recently updated first
    UpdatedDesc,
    /// Least recently updated first
    UpdatedAsc,
}

impl Default for PaginationOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            cursor: None,
            order: SortOrder::default(),
        }
    }
}

/// Paginated result set.
#[derive(Debug, Clone)]
pub struct PaginatedResult<T> {
    /// Items in this page.
    pub items: Vec<T>,
    
    /// Cursor for fetching next page.
    /// None if this is the last page.
    pub next_cursor: Option<String>,
    
    /// Total count matching filter (if available).
    /// May be None if counting is too expensive.
    pub total_count: Option<u64>,
    
    /// Whether there are more results.
    pub has_more: bool,
}
```

### 3.3 Provider Trait Additions

```rust
#[async_trait::async_trait]
pub trait ProviderAdmin: Any + Send + Sync {
    // ... existing methods ...

    /// List instances with filtering and pagination.
    ///
    /// # Parameters
    ///
    /// * `filter` - Criteria to filter instances
    /// * `pagination` - Pagination options (limit, cursor, order)
    ///
    /// # Returns
    ///
    /// Paginated result with instances and next cursor.
    ///
    /// # Implementation Notes
    ///
    /// Cursor should encode (created_at, instance_id) for stable pagination
    /// even as new instances are created.
    ///
    /// # Example SQL (CreatedDesc order)
    ///
    /// ```sql
    /// SELECT i.*, e.status, e.output
    /// FROM instances i
    /// LEFT JOIN executions e ON i.instance_id = e.instance_id 
    ///     AND i.current_execution_id = e.execution_id
    /// WHERE 
    ///     (? IS NULL OR e.status IN (?))
    ///     AND (? IS NULL OR i.orchestration_name = ?)
    ///     AND (? IS NULL OR i.orchestration_name LIKE ? || '%')
    ///     AND (? IS NULL OR i.created_at > ?)
    ///     AND (? IS NULL OR i.created_at < ?)
    ///     AND (? IS NULL OR (i.created_at, i.instance_id) < (?, ?))  -- cursor
    /// ORDER BY i.created_at DESC, i.instance_id DESC
    /// LIMIT ?
    /// ```
    async fn list_instances_paginated(
        &self,
        filter: InstanceFilter,
        pagination: PaginationOptions,
    ) -> Result<PaginatedResult<InstanceInfo>, ProviderError>;
    
    /// Count instances matching filter.
    ///
    /// Useful for UI pagination (showing "Page X of Y").
    /// May be expensive on large datasets.
    ///
    /// # Returns
    ///
    /// Total count of instances matching filter.
    async fn count_instances(
        &self,
        filter: InstanceFilter,
    ) -> Result<u64, ProviderError>;
}
```

### 3.4 Client API

```rust
impl Client {
    /// List instances with filtering and pagination.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get first page of running OrderWorkflow instances
    /// let filter = InstanceFilter::new()
    ///     .with_status("Running")
    ///     .with_orchestration_name("OrderWorkflow");
    /// let pagination = PaginationOptions { limit: 50, ..Default::default() };
    /// 
    /// let page1 = client.list_instances_paginated(filter.clone(), pagination).await?;
    /// println!("Found {} instances", page1.items.len());
    /// 
    /// // Get next page
    /// if let Some(cursor) = page1.next_cursor {
    ///     let page2 = client.list_instances_paginated(
    ///         filter,
    ///         PaginationOptions { cursor: Some(cursor), ..Default::default() }
    ///     ).await?;
    /// }
    /// ```
    pub async fn list_instances_paginated(
        &self,
        filter: InstanceFilter,
        pagination: PaginationOptions,
    ) -> Result<PaginatedResult<InstanceInfo>, ClientError>;
    
    /// Count instances matching filter.
    pub async fn count_instances(
        &self,
        filter: InstanceFilter,
    ) -> Result<u64, ClientError>;
}
```

---

## 4. Performance Optimizations

### 4.1 Database Indexes

Add indexes specifically for management query patterns:

```sql
-- Index for filtering by status (via JOIN with executions)
CREATE INDEX IF NOT EXISTS idx_executions_status 
    ON executions(status, instance_id);

-- Index for filtering by orchestration name
CREATE INDEX IF NOT EXISTS idx_instances_name 
    ON instances(orchestration_name, created_at DESC);

-- Index for time-range queries
CREATE INDEX IF NOT EXISTS idx_instances_created 
    ON instances(created_at DESC, instance_id);

-- Index for updated_at queries (find stale instances)
CREATE INDEX IF NOT EXISTS idx_instances_updated 
    ON instances(updated_at DESC);

-- Covering index for common listing query
CREATE INDEX IF NOT EXISTS idx_instances_list_covering 
    ON instances(created_at DESC, instance_id, orchestration_name, orchestration_version, current_execution_id);

-- Index for execution pruning
CREATE INDEX IF NOT EXISTS idx_history_execution 
    ON history(instance_id, execution_id);
```

### 4.2 Query Optimization Patterns

**Avoid COUNT(*) for pagination:**

```rust
// Instead of: SELECT COUNT(*) FROM instances WHERE ...
// Use: fetch limit+1 and check if we got more

async fn list_instances_paginated(...) -> Result<PaginatedResult<InstanceInfo>, ProviderError> {
    // Fetch one extra to determine if there are more pages
    let fetch_limit = pagination.limit + 1;
    
    let rows = sqlx::query(/* ... LIMIT ? */)
        .bind(fetch_limit)
        .fetch_all(&self.pool)
        .await?;
    
    let has_more = rows.len() > pagination.limit as usize;
    let items: Vec<InstanceInfo> = rows
        .into_iter()
        .take(pagination.limit as usize)
        .map(|r| /* convert */)
        .collect();
    
    let next_cursor = if has_more {
        items.last().map(|i| encode_cursor(&i.created_at, &i.instance_id))
    } else {
        None
    };
    
    Ok(PaginatedResult { items, next_cursor, total_count: None, has_more })
}
```

**Cursor encoding for stable pagination:**

```rust
fn encode_cursor(created_at: u64, instance_id: &str) -> String {
    // Use base64 to hide implementation details
    let raw = format!("{}:{}", created_at, instance_id);
    base64::encode(raw)
}

fn decode_cursor(cursor: &str) -> Result<(u64, String), ProviderError> {
    let raw = base64::decode(cursor)
        .map_err(|_| ProviderError::permanent("decode_cursor", "Invalid cursor"))?;
    let s = String::from_utf8(raw)
        .map_err(|_| ProviderError::permanent("decode_cursor", "Invalid cursor encoding"))?;
    let parts: Vec<&str> = s.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(ProviderError::permanent("decode_cursor", "Malformed cursor"));
    }
    let created_at = parts[0].parse::<u64>()
        .map_err(|_| ProviderError::permanent("decode_cursor", "Invalid timestamp in cursor"))?;
    Ok((created_at, parts[1].to_string()))
}
```

### 4.3 Batched Operations

For bulk deletions, use batched transactions to avoid long-running locks:

```rust
async fn delete_instances_bulk(
    &self,
    filter: InstanceFilter,
) -> Result<TruncateResult, ProviderError> {
    const BATCH_SIZE: u32 = 100;
    let mut total_result = TruncateResult::default();
    
    loop {
        // Find next batch of instance IDs to delete
        let pagination = PaginationOptions { limit: BATCH_SIZE, ..Default::default() };
        let page = self.list_instances_paginated(filter.clone(), pagination).await?;
        
        if page.items.is_empty() {
            break;
        }
        
        // Delete this batch in a transaction
        let mut tx = self.pool.begin().await?;
        for info in &page.items {
            let result = self.delete_instance_in_tx(&mut tx, &info.instance_id).await?;
            total_result.instances_deleted += result.instances_deleted;
            total_result.executions_deleted += result.executions_deleted;
            total_result.events_deleted += result.events_deleted;
            total_result.messages_deleted += result.messages_deleted;
        }
        tx.commit().await?;
        
        if !page.has_more {
            break;
        }
    }
    
    Ok(total_result)
}
```

---

## 5. Schema Changes

### 5.1 Migration SQL

```sql
-- Migration: Add management API indexes
-- Version: 2024_xx_management_api_indexes

-- Index for status filtering
CREATE INDEX IF NOT EXISTS idx_executions_status 
    ON executions(status, instance_id);

-- Index for orchestration name filtering
CREATE INDEX IF NOT EXISTS idx_instances_name 
    ON instances(orchestration_name, created_at DESC);

-- Index for time-range queries
CREATE INDEX IF NOT EXISTS idx_instances_created 
    ON instances(created_at DESC, instance_id);

-- Index for finding stale instances
CREATE INDEX IF NOT EXISTS idx_instances_updated 
    ON instances(updated_at DESC);

-- Index for execution pruning
CREATE INDEX IF NOT EXISTS idx_history_execution 
    ON history(instance_id, execution_id);


-- Migration: Add optional instance metadata
-- Version: 2024_xx_instance_metadata

-- Add input column to track original input (optional)
ALTER TABLE instances ADD COLUMN input TEXT;

-- Add tags column for categorization (JSON array, optional)
ALTER TABLE instances ADD COLUMN tags TEXT;

-- Add retention policy (JSON, optional)
ALTER TABLE instances ADD COLUMN retention_policy TEXT;
```

### 5.2 Backward Compatibility

All new columns are nullable with no default, ensuring:
- Existing instances continue to work unchanged
- New features are opt-in
- No data migration required for existing deployments

---

## 6. Error Handling

### 6.1 New Error Variants

```rust
#[derive(Debug, Clone)]
pub enum ManagementError {
    /// Instance not found
    InstanceNotFound { instance_id: String },
    
    /// Cannot delete a running instance without force flag
    InstanceStillRunning { instance_id: String },
    
    /// Invalid filter criteria
    InvalidFilter { message: String },
    
    /// Invalid cursor format
    InvalidCursor { cursor: String },
    
    /// Operation would exceed limits
    LimitExceeded { 
        limit_name: String, 
        limit_value: u64, 
        requested: u64 
    },
}
```

---

## 7. Implementation Phases

### Phase 1: Core APIs (MVP)
- [ ] `list_instances_with_info()` — enriched listing
- [ ] `delete_instance()` — single instance deletion
- [ ] `prune_executions()` — execution history pruning
- [ ] Basic filtering by status and orchestration name

### Phase 2: Pagination & Performance
- [ ] `list_instances_paginated()` with cursor-based pagination
- [ ] `count_instances()` for total counts
- [ ] Database indexes for management queries
- [ ] Batched bulk operations

### Phase 3: Advanced Features
- [ ] `delete_instances_bulk()` with filters
- [ ] Automatic retention policies
- [ ] Tag support for instances
- [ ] Time-range filtering

---

## 8. Open Questions

1. **Retention Policy Storage**: Should retention policies be stored in the instance record, or in a separate configuration table?

2. **Automatic vs Manual Pruning**: Should auto-pruning happen during `continue_as_new`, or via a background maintenance job?

3. **Soft vs Hard Delete**: Should we support soft deletes (mark as deleted but retain data) for audit purposes?

4. **Read Replicas**: Should management queries be routable to read replicas for production workloads?

5. **Metrics Integration**: Should truncation operations emit metrics (events deleted, time taken)?










