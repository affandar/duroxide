# PostgreSQL Provider Implementation Plan

**Target:** Azure Database for PostgreSQL - Flexible Server  
**Status:** Planning  
**Estimated Timeline:** 2-3 weeks  
**Complexity:** Medium (80% code reuse from SQLite)

---

## Executive Summary

This document outlines the implementation plan for a PostgreSQL-based Duroxide provider. Unlike Azure Storage Tables/Queues, PostgreSQL offers **true ACID transactions**, making it architecturally similar to SQLite but with cloud-scale performance and availability. The provider will work on any PostgreSQL 12+ deployment (Azure, AWS RDS, Google Cloud SQL, self-hosted), with Azure PostgreSQL Flexible Server as the primary target for managed deployment.

### Key Benefits

- ✅ **True ACID guarantees** - no atomicity compromises
- ✅ **80% code reuse** from proven SQLite implementation
- ✅ **Native queue semantics** via `FOR UPDATE SKIP LOCKED`
- ✅ **Production-ready** from day one
- ✅ **Cloud-agnostic** - portable across providers
- ✅ **Better concurrency** than SQLite
- ✅ **Familiar tooling** - standard SQL, pgAdmin, etc.

---

## Architecture Overview

### Service Comparison

| Component | SQLite Provider | PostgreSQL Provider |
|-----------|----------------|---------------------|
| **Storage** | Local file | Managed database service |
| **Transactions** | ACID (local) | ACID (distributed) |
| **Concurrency** | Limited (file locks) | Excellent (MVCC) |
| **Scalability** | Single process | Multi-worker, connection pooling |
| **High Availability** | Manual replication | Built-in HA/failover |
| **Backups** | Manual | Automated |
| **Cost** | Free | ~$20-200/month depending on tier |

### Design Philosophy

**Core Principle:** Maintain 1:1 semantic compatibility with SQLite provider while leveraging PostgreSQL's strengths.

- Same schema structure (with PostgreSQL types)
- Same Provider trait implementation
- Same transaction semantics
- Enhanced with PostgreSQL features (JSONB, better indexes, connection pooling)

---

## PostgreSQL-Specific Advantages

### 1. Superior Lock Semantics

PostgreSQL's `FOR UPDATE SKIP LOCKED` provides exactly what Duroxide needs:

```sql
-- SQLite approach: optimistic locking with retry
UPDATE queue SET lock_token = ? WHERE id = ? AND lock_token IS NULL;
-- May fail due to conflicts

-- PostgreSQL approach: pessimistic locking, no conflicts
SELECT * FROM queue
WHERE available
FOR UPDATE SKIP LOCKED  -- Skips locked rows, never blocks!
LIMIT 1;
```

### 2. JSONB Storage

Events and work items can be stored as native JSONB with indexing:

```sql
-- Query events by type efficiently
SELECT * FROM history 
WHERE event_data->>'type' = 'ActivityScheduled';

-- Index JSON fields
CREATE INDEX idx_event_type ON history ((event_data->>'type'));
```

### 3. Connection Pooling

Unlike SQLite, PostgreSQL handles concurrent connections natively:

```rust
PgPoolOptions::new()
    .max_connections(50)  // Multiple dispatchers, workers
    .acquire_timeout(Duration::from_secs(30))
    .connect(database_url)
```

### 4. Advanced Visibility Control

Native timestamp comparison without epoch conversion:

```sql
-- Timer queue with precise firing
SELECT * FROM timer_queue
WHERE fire_at <= NOW()
  AND (locked_until IS NULL OR locked_until <= NOW())
FOR UPDATE SKIP LOCKED;
```

---

## Schema Design

### Migration from SQLite

The schema is nearly identical, with PostgreSQL-specific enhancements:

```sql
-- migrations/postgres/001_initial_schema.sql

-- Instance metadata
CREATE TABLE IF NOT EXISTS instances (
    instance_id TEXT PRIMARY KEY,
    orchestration_name TEXT NOT NULL,
    orchestration_version TEXT NOT NULL,
    current_execution_id BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Multi-execution support
CREATE TABLE IF NOT EXISTS executions (
    instance_id TEXT NOT NULL,
    execution_id BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Running',
    output TEXT,
    started_at TIMESTAMPTZ DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (instance_id, execution_id),
    FOREIGN KEY (instance_id) REFERENCES instances(instance_id) ON DELETE CASCADE
);

-- Event history (append-only log)
CREATE TABLE IF NOT EXISTS history (
    instance_id TEXT NOT NULL,
    execution_id BIGINT NOT NULL,
    event_id BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    event_data JSONB NOT NULL,  -- PostgreSQL native JSON type
    created_at TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (instance_id, execution_id, event_id),
    FOREIGN KEY (instance_id, execution_id) 
        REFERENCES executions(instance_id, execution_id) ON DELETE CASCADE
);

-- Orchestrator queue (peek-lock semantics)
CREATE TABLE IF NOT EXISTS orchestrator_queue (
    id BIGSERIAL PRIMARY KEY,
    instance_id TEXT NOT NULL,
    work_item JSONB NOT NULL,
    visible_at TIMESTAMPTZ DEFAULT NOW(),
    lock_token TEXT,
    locked_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Worker queue (activity execution)
CREATE TABLE IF NOT EXISTS worker_queue (
    id BIGSERIAL PRIMARY KEY,
    work_item JSONB NOT NULL,
    lock_token TEXT,
    locked_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Timer queue (delayed visibility support)
CREATE TABLE IF NOT EXISTS timer_queue (
    id BIGSERIAL PRIMARY KEY,
    work_item JSONB NOT NULL,
    fire_at TIMESTAMPTZ NOT NULL,
    lock_token TEXT,
    locked_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Performance indexes
CREATE INDEX idx_orch_visible ON orchestrator_queue(visible_at, lock_token) 
    WHERE lock_token IS NULL OR locked_until <= NOW();
CREATE INDEX idx_orch_instance ON orchestrator_queue(instance_id);
CREATE INDEX idx_orch_lock ON orchestrator_queue(lock_token) WHERE lock_token IS NOT NULL;

CREATE INDEX idx_worker_available ON worker_queue(id) 
    WHERE lock_token IS NULL OR locked_until <= NOW();
CREATE INDEX idx_worker_lock ON worker_queue(lock_token) WHERE lock_token IS NOT NULL;

CREATE INDEX idx_timer_fire ON timer_queue(fire_at, lock_token)
    WHERE lock_token IS NULL OR locked_until <= NOW();
CREATE INDEX idx_timer_lock ON timer_queue(lock_token) WHERE lock_token IS NOT NULL;

CREATE INDEX idx_history_lookup ON history(instance_id, execution_id, event_id);
CREATE INDEX idx_history_instance ON history(instance_id);

-- JSONB indexes for advanced queries (optional, for management APIs)
CREATE INDEX idx_event_type ON history USING GIN ((event_data->>'type'));
CREATE INDEX idx_work_item_type ON orchestrator_queue USING GIN ((work_item->>'type'));
```

### Key Differences from SQLite

| Feature | SQLite | PostgreSQL |
|---------|--------|------------|
| Auto-increment | `INTEGER PRIMARY KEY AUTOINCREMENT` | `BIGSERIAL PRIMARY KEY` |
| Timestamps | `TIMESTAMP` (no timezone) | `TIMESTAMPTZ` (with timezone) |
| JSON | `TEXT` (serialized) | `JSONB` (native, indexed) |
| Current time | `CURRENT_TIMESTAMP` | `NOW()` (more idiomatic) |
| Integer size | `INTEGER` (flexible) | `BIGINT` (explicit 64-bit) |
| Foreign keys | Optional | Enforced by default |
| Partial indexes | Supported | Supported (with `WHERE` clause) |

---

## Implementation Structure

### File Organization

```
src/providers/
├── mod.rs                      # Export both providers
├── sqlite.rs                   # Existing SQLite provider
└── postgres.rs                 # New PostgreSQL provider

migrations/
├── sqlite/
│   └── 20240101000000_initial_schema.sql
└── postgres/
    ├── 001_initial_schema.sql
    ├── 002_indexes.sql          # Separated for optimization
    └── 003_partitioning.sql     # Optional: for massive scale

tests/
├── postgres_provider_test.rs   # Port from sqlite_provider_test.rs
└── postgres_azure_integration_test.rs  # Azure-specific tests

docs/
├── postgresql-provider-plan.md # This document
└── postgresql-deployment.md    # Deployment guide (to be created)
```

### Core Implementation

```rust
// src/providers/postgres.rs

use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{Row, Transaction, Postgres};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::debug;

use super::{
    ExecutionInfo, InstanceInfo, ManagementCapability, OrchestrationItem, 
    Provider, QueueDepths, SystemMetrics, WorkItem, ExecutionMetadata
};
use crate::Event;

/// PostgreSQL-backed provider with full ACID guarantees
///
/// This provider is compatible with any PostgreSQL 12+ deployment:
/// - Azure Database for PostgreSQL - Flexible Server (primary target)
/// - AWS RDS for PostgreSQL
/// - Google Cloud SQL for PostgreSQL
/// - Self-hosted PostgreSQL
///
/// Features:
/// - True ACID transactions across all operations
/// - Native peek-lock semantics via FOR UPDATE SKIP LOCKED
/// - JSONB storage for efficient querying
/// - Connection pooling for high concurrency
/// - Built-in high availability (when deployed on managed service)
pub struct PostgresProvider {
    pool: PgPool,
    lock_timeout: Duration,
}

impl PostgresProvider {
    /// Create a new PostgreSQL provider
    ///
    /// # Arguments
    /// * `database_url` - PostgreSQL connection string
    ///   - Azure: `postgresql://username:password@server.postgres.database.azure.com:5432/duroxide?sslmode=require`
    ///   - Local: `postgresql://localhost/duroxide`
    ///
    /// # Example
    /// ```rust
    /// let provider = PostgresProvider::new(
    ///     "postgresql://user:pass@localhost/duroxide"
    /// ).await?;
    /// ```
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(20)
            .acquire_timeout(Duration::from_secs(30))
            .idle_timeout(Duration::from_secs(600))
            .max_lifetime(Duration::from_secs(1800))
            .connect(database_url)
            .await?;

        Ok(Self {
            pool,
            lock_timeout: Duration::from_secs(30),
        })
    }

    /// Create a new PostgreSQL provider with custom connection pool settings
    pub async fn new_with_config(
        database_url: &str,
        max_connections: u32,
        lock_timeout: Duration,
    ) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(30))
            .connect(database_url)
            .await?;

        Ok(Self { pool, lock_timeout })
    }

    /// Run migrations to set up schema
    ///
    /// This should be called once during deployment to create tables.
    /// Safe to call multiple times (idempotent).
    pub async fn run_migrations(&self) -> Result<(), sqlx::Error> {
        // Run schema creation
        sqlx::query(include_str!("../../migrations/postgres/001_initial_schema.sql"))
            .execute(&self.pool)
            .await?;
        
        Ok(())
    }

    /// Helper: Get current timestamp in milliseconds
    fn now_millis() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// Helper: Convert milliseconds to PostgreSQL timestamp
    fn millis_to_timestamp(millis: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(millis)
            .unwrap_or_else(chrono::Utc::now)
    }

    /// Internal: Enqueue orchestrator work with optional delay
    async fn enqueue_orchestrator_work_with_delay(
        &self,
        item: WorkItem,
        delay_ms: Option<u64>,
    ) -> Result<(), String> {
        let work_item = serde_json::to_value(&item).map_err(|e| e.to_string())?;
        let instance = self.extract_instance(&item)?;

        // Handle StartOrchestration - create instance and execution
        if let WorkItem::StartOrchestration {
            orchestration,
            version,
            execution_id,
            ..
        } = &item
        {
            let version = version.as_deref().unwrap_or("1.0.0");
            
            sqlx::query(
                r#"
                INSERT INTO instances (instance_id, orchestration_name, orchestration_version)
                VALUES ($1, $2, $3)
                ON CONFLICT (instance_id) DO NOTHING
                "#,
            )
            .bind(&instance)
            .bind(orchestration)
            .bind(version)
            .execute(&self.pool)
            .await
            .map_err(|e| e.to_string())?;

            sqlx::query(
                r#"
                INSERT INTO executions (instance_id, execution_id)
                VALUES ($1, $2)
                ON CONFLICT (instance_id, execution_id) DO NOTHING
                "#,
            )
            .bind(&instance)
            .bind(*execution_id as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        }

        // Calculate visibility timestamp
        let visible_at = if let Some(delay_ms) = delay_ms {
            Self::millis_to_timestamp(Self::now_millis() + delay_ms as i64)
        } else {
            chrono::Utc::now()
        };

        sqlx::query(
            r#"
            INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(&instance)
        .bind(work_item)
        .bind(visible_at)
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Extract instance_id from WorkItem
    fn extract_instance(&self, item: &WorkItem) -> Result<String, String> {
        match item {
            WorkItem::StartOrchestration { instance, .. }
            | WorkItem::ActivityCompleted { instance, .. }
            | WorkItem::ActivityFailed { instance, .. }
            | WorkItem::TimerFired { instance, .. }
            | WorkItem::ExternalRaised { instance, .. }
            | WorkItem::CancelInstance { instance, .. }
            | WorkItem::ContinueAsNew { instance, .. } => Ok(instance.clone()),
            WorkItem::SubOrchCompleted { parent_instance, .. }
            | WorkItem::SubOrchFailed { parent_instance, .. } => Ok(parent_instance.clone()),
            _ => Err("Invalid work item type".to_string()),
        }
    }

    /// Get event discriminant name for indexing
    fn event_type_name(event: &Event) -> &'static str {
        match event {
            Event::ExecutionStarted { .. } => "ExecutionStarted",
            Event::ActivityScheduled { .. } => "ActivityScheduled",
            Event::ActivityCompleted { .. } => "ActivityCompleted",
            Event::ActivityFailed { .. } => "ActivityFailed",
            Event::TimerCreated { .. } => "TimerCreated",
            Event::TimerFired { .. } => "TimerFired",
            Event::OrchestrationCompleted { .. } => "OrchestrationCompleted",
            Event::OrchestrationFailed { .. } => "OrchestrationFailed",
            Event::SubOrchestrationCreated { .. } => "SubOrchestrationCreated",
            Event::SubOrchestrationCompleted { .. } => "SubOrchestrationCompleted",
            Event::SubOrchestrationFailed { .. } => "SubOrchestrationFailed",
            Event::EventRaised { .. } => "EventRaised",
            Event::ContinuedAsNew { .. } => "ContinuedAsNew",
            Event::Cancelled { .. } => "Cancelled",
        }
    }
}

#[async_trait::async_trait]
impl Provider for PostgresProvider {
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        let mut tx = self.pool.begin().await.ok()?;

        // PostgreSQL magic: FOR UPDATE SKIP LOCKED
        // This gives us atomic peek-lock with no contention!
        let row = sqlx::query(
            r#"
            SELECT id, instance_id
            FROM orchestrator_queue
            WHERE visible_at <= NOW()
              AND (locked_until IS NULL OR locked_until <= NOW())
            ORDER BY id ASC
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *tx)
        .await
        .ok()??;

        let instance_id: String = row.get("instance_id");
        let lock_token = uuid::Uuid::new_v4().to_string();
        let locked_until = chrono::Utc::now() + self.lock_timeout;

        // Lock ALL messages for this instance
        sqlx::query(
            r#"
            UPDATE orchestrator_queue
            SET lock_token = $1, locked_until = $2
            WHERE instance_id = $3
              AND (locked_until IS NULL OR locked_until <= NOW())
            "#,
        )
        .bind(&lock_token)
        .bind(locked_until)
        .bind(&instance_id)
        .execute(&mut *tx)
        .await
        .ok()?;

        // Fetch all locked messages
        let message_rows = sqlx::query(
            r#"
            SELECT work_item
            FROM orchestrator_queue
            WHERE lock_token = $1
            ORDER BY id ASC
            "#,
        )
        .bind(&lock_token)
        .fetch_all(&mut *tx)
        .await
        .ok()?;

        let messages: Vec<WorkItem> = message_rows
            .iter()
            .filter_map(|row| {
                let val: serde_json::Value = row.get("work_item");
                serde_json::from_value(val).ok()
            })
            .collect();

        // Load instance metadata
        let metadata_row = sqlx::query(
            r#"
            SELECT orchestration_name, orchestration_version, current_execution_id
            FROM instances
            WHERE instance_id = $1
            "#,
        )
        .bind(&instance_id)
        .fetch_optional(&mut *tx)
        .await
        .ok()?;

        let (orchestration_name, orchestration_version, current_execution_id) = if let Some(row) = metadata_row {
            (
                row.get::<String, _>("orchestration_name"),
                row.get::<String, _>("orchestration_version"),
                row.get::<i64, _>("current_execution_id") as u64,
            )
        } else {
            // Derive from first message
            if let Some(WorkItem::StartOrchestration {
                orchestration,
                version,
                execution_id,
                ..
            }) = messages.first()
            {
                (
                    orchestration.clone(),
                    version.clone().unwrap_or_else(|| "1.0.0".to_string()),
                    *execution_id,
                )
            } else {
                return None;
            }
        };

        // Load history for current execution
        let history_rows = sqlx::query(
            r#"
            SELECT event_data
            FROM history
            WHERE instance_id = $1 AND execution_id = $2
            ORDER BY event_id ASC
            "#,
        )
        .bind(&instance_id)
        .bind(current_execution_id as i64)
        .fetch_all(&mut *tx)
        .await
        .ok()?;

        let history: Vec<Event> = history_rows
            .iter()
            .filter_map(|row| {
                let val: serde_json::Value = row.get("event_data");
                serde_json::from_value(val).ok()
            })
            .collect();

        tx.commit().await.ok()?;

        Some(OrchestrationItem {
            instance: instance_id,
            orchestration_name,
            execution_id: current_execution_id,
            version: orchestration_version,
            history,
            messages,
            lock_token,
        })
    }

    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        timer_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), String> {
        // Single transaction for full ACID guarantee
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;

        // Get instance_id from lock_token
        let instance: String = sqlx::query_scalar(
            "SELECT DISTINCT instance_id FROM orchestrator_queue WHERE lock_token = $1",
        )
        .bind(lock_token)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Invalid lock token: {}", e))?;

        // 1. Idempotently create execution row
        sqlx::query(
            r#"
            INSERT INTO executions (instance_id, execution_id, status, started_at)
            VALUES ($1, $2, 'Running', NOW())
            ON CONFLICT (instance_id, execution_id) DO NOTHING
            "#,
        )
        .bind(&instance)
        .bind(execution_id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        // 2. Update instance current_execution_id
        sqlx::query(
            r#"
            UPDATE instances
            SET current_execution_id = GREATEST(current_execution_id, $1),
                updated_at = NOW()
            WHERE instance_id = $2
            "#,
        )
        .bind(execution_id as i64)
        .bind(&instance)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        // 3. Append history_delta
        for event in &history_delta {
            if event.event_id() == 0 {
                return Err("event_id must be set by runtime".to_string());
            }

            let event_json = serde_json::to_value(event).map_err(|e| e.to_string())?;
            let event_type = Self::event_type_name(event);

            sqlx::query(
                r#"
                INSERT INTO history (instance_id, execution_id, event_id, event_type, event_data)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (instance_id, execution_id, event_id) DO NOTHING
                "#,
            )
            .bind(&instance)
            .bind(execution_id as i64)
            .bind(event.event_id() as i64)
            .bind(event_type)
            .bind(event_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }

        // 4. Update execution metadata
        if let Some(status) = &metadata.status {
            sqlx::query(
                r#"
                UPDATE executions
                SET status = $1, output = $2, completed_at = NOW()
                WHERE instance_id = $3 AND execution_id = $4
                "#,
            )
            .bind(status)
            .bind(&metadata.output)
            .bind(&instance)
            .bind(execution_id as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }

        // 5. Enqueue worker items
        for item in worker_items {
            let work_json = serde_json::to_value(&item).map_err(|e| e.to_string())?;
            sqlx::query(
                r#"
                INSERT INTO worker_queue (work_item)
                VALUES ($1)
                "#,
            )
            .bind(work_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }

        // 6. Enqueue timer items
        for item in timer_items {
            if let WorkItem::TimerSchedule { fire_at_ms, .. } = &item {
                let work_json = serde_json::to_value(&item).map_err(|e| e.to_string())?;
                let fire_at = Self::millis_to_timestamp(*fire_at_ms as i64);

                sqlx::query(
                    r#"
                    INSERT INTO timer_queue (work_item, fire_at)
                    VALUES ($1, $2)
                    "#,
                )
                .bind(work_json)
                .bind(fire_at)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            }
        }

        // 7. Enqueue orchestrator items
        for item in orchestrator_items {
            let work_json = serde_json::to_value(&item).map_err(|e| e.to_string())?;
            let target_instance = self.extract_instance(&item)?;

            // Special case: StartOrchestration
            if let WorkItem::StartOrchestration {
                orchestration,
                version,
                execution_id: exec_id,
                ..
            } = &item
            {
                let version = version.as_deref().unwrap_or("1.0.0");

                sqlx::query(
                    r#"
                    INSERT INTO instances (instance_id, orchestration_name, orchestration_version)
                    VALUES ($1, $2, $3)
                    ON CONFLICT (instance_id) DO NOTHING
                    "#,
                )
                .bind(&target_instance)
                .bind(orchestration)
                .bind(version)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;

                sqlx::query(
                    r#"
                    INSERT INTO executions (instance_id, execution_id)
                    VALUES ($1, $2)
                    ON CONFLICT (instance_id, execution_id) DO NOTHING
                    "#,
                )
                .bind(&target_instance)
                .bind(*exec_id as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            }

            sqlx::query(
                r#"
                INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
                VALUES ($1, $2, NOW())
                "#,
            )
            .bind(&target_instance)
            .bind(work_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }

        // 8. Delete locked messages
        sqlx::query("DELETE FROM orchestrator_queue WHERE lock_token = $1")
            .bind(lock_token)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

        // Commit transaction - all or nothing!
        tx.commit().await.map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn abandon_orchestration_item(&self, lock_token: &str, delay_ms: Option<u64>) -> Result<(), String> {
        let visible_at = if let Some(delay) = delay_ms {
            Self::millis_to_timestamp(Self::now_millis() + delay as i64)
        } else {
            chrono::Utc::now()
        };

        sqlx::query(
            r#"
            UPDATE orchestrator_queue
            SET lock_token = NULL, locked_until = NULL, visible_at = $1
            WHERE lock_token = $2
            "#,
        )
        .bind(visible_at)
        .bind(lock_token)
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn read(&self, instance: &str) -> Vec<Event> {
        // Get latest execution ID
        let execution_id: Option<i64> = sqlx::query_scalar(
            "SELECT COALESCE(MAX(execution_id), 1) FROM executions WHERE instance_id = $1",
        )
        .bind(instance)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();

        let execution_id = execution_id.unwrap_or(1);

        // Load events
        let rows = sqlx::query(
            r#"
            SELECT event_data
            FROM history
            WHERE instance_id = $1 AND execution_id = $2
            ORDER BY event_id ASC
            "#,
        )
        .bind(instance)
        .bind(execution_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        rows.iter()
            .filter_map(|row| {
                let val: serde_json::Value = row.get("event_data");
                serde_json::from_value(val).ok()
            })
            .collect()
    }

    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;

        for event in &new_events {
            let event_json = serde_json::to_value(event).map_err(|e| e.to_string())?;
            let event_type = Self::event_type_name(event);

            sqlx::query(
                r#"
                INSERT INTO history (instance_id, execution_id, event_id, event_type, event_data)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (instance_id, execution_id, event_id) DO NOTHING
                "#,
            )
            .bind(instance)
            .bind(execution_id as i64)
            .bind(event.event_id() as i64)
            .bind(event_type)
            .bind(event_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }

        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    // Worker queue operations
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        let work_json = serde_json::to_value(&item).map_err(|e| e.to_string())?;
        
        sqlx::query("INSERT INTO worker_queue (work_item) VALUES ($1)")
            .bind(work_json)
            .execute(&self.pool)
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        let mut tx = self.pool.begin().await.ok()?;

        let row = sqlx::query(
            r#"
            SELECT id, work_item
            FROM worker_queue
            WHERE lock_token IS NULL OR locked_until <= NOW()
            ORDER BY id ASC
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *tx)
        .await
        .ok()??;

        let id: i64 = row.get("id");
        let work_item_val: serde_json::Value = row.get("work_item");
        let work_item: WorkItem = serde_json::from_value(work_item_val).ok()?;

        let lock_token = uuid::Uuid::new_v4().to_string();
        let locked_until = chrono::Utc::now() + self.lock_timeout;

        sqlx::query(
            r#"
            UPDATE worker_queue
            SET lock_token = $1, locked_until = $2
            WHERE id = $3
            "#,
        )
        .bind(&lock_token)
        .bind(locked_until)
        .bind(id)
        .execute(&mut *tx)
        .await
        .ok()?;

        tx.commit().await.ok()?;

        Some((work_item, lock_token))
    }

    async fn ack_worker(&self, token: &str, completion: WorkItem) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;

        // Delete from worker queue
        sqlx::query("DELETE FROM worker_queue WHERE lock_token = $1")
            .bind(token)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

        // Enqueue completion to orchestrator queue
        let instance = self.extract_instance(&completion)?;
        let work_json = serde_json::to_value(&completion).map_err(|e| e.to_string())?;

        sqlx::query(
            r#"
            INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
            VALUES ($1, $2, NOW())
            "#,
        )
        .bind(&instance)
        .bind(work_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    // Orchestrator queue
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
        self.enqueue_orchestrator_work_with_delay(item, delay_ms).await
    }

    // Timer queue (REQUIRED - all providers must support delayed visibility)

    async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String> {
        if let WorkItem::TimerSchedule { fire_at_ms, .. } = &item {
            let work_json = serde_json::to_value(&item).map_err(|e| e.to_string())?;
            let fire_at = Self::millis_to_timestamp(*fire_at_ms as i64);

            sqlx::query(
                r#"
                INSERT INTO timer_queue (work_item, fire_at)
                VALUES ($1, $2)
                "#,
            )
            .bind(work_json)
            .bind(fire_at)
            .execute(&self.pool)
            .await
            .map_err(|e| e.to_string())?;

            Ok(())
        } else {
            Err("Invalid timer work item".to_string())
        }
    }

    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> {
        let mut tx = self.pool.begin().await.ok()?;

        let row = sqlx::query(
            r#"
            SELECT id, work_item
            FROM timer_queue
            WHERE fire_at <= NOW()
              AND (lock_token IS NULL OR locked_until <= NOW())
            ORDER BY fire_at ASC
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *tx)
        .await
        .ok()??;

        let id: i64 = row.get("id");
        let work_item_val: serde_json::Value = row.get("work_item");
        let work_item: WorkItem = serde_json::from_value(work_item_val).ok()?;

        let lock_token = uuid::Uuid::new_v4().to_string();
        let locked_until = chrono::Utc::now() + self.lock_timeout;

        sqlx::query(
            r#"
            UPDATE timer_queue
            SET lock_token = $1, locked_until = $2
            WHERE id = $3
            "#,
        )
        .bind(&lock_token)
        .bind(locked_until)
        .bind(id)
        .execute(&mut *tx)
        .await
        .ok()?;

        tx.commit().await.ok()?;

        Some((work_item, lock_token))
    }

    async fn ack_timer(&self, token: &str, completion: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;

        // Delete from timer queue
        sqlx::query("DELETE FROM timer_queue WHERE lock_token = $1")
            .bind(token)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

        // Enqueue TimerFired to orchestrator queue
        let instance = self.extract_instance(&completion)?;
        let work_json = serde_json::to_value(&completion).map_err(|e| e.to_string())?;

        let visible_at = if let Some(delay) = delay_ms {
            Self::millis_to_timestamp(Self::now_millis() + delay as i64)
        } else {
            chrono::Utc::now()
        };

        sqlx::query(
            r#"
            INSERT INTO orchestrator_queue (instance_id, work_item, visible_at)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(&instance)
        .bind(work_json)
        .bind(visible_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    // Management APIs
    async fn latest_execution_id(&self, instance: &str) -> Option<u64> {
        sqlx::query_scalar(
            "SELECT MAX(execution_id) FROM executions WHERE instance_id = $1"
        )
        .bind(instance)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .map(|id: i64| id as u64)
    }

    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
        let rows = sqlx::query(
            r#"
            SELECT event_data
            FROM history
            WHERE instance_id = $1 AND execution_id = $2
            ORDER BY event_id ASC
            "#,
        )
        .bind(instance)
        .bind(execution_id as i64)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        rows.iter()
            .filter_map(|row| {
                let val: serde_json::Value = row.get("event_data");
                serde_json::from_value(val).ok()
            })
            .collect()
    }

    async fn list_instances(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT instance_id FROM instances ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default()
    }

    async fn list_executions(&self, instance: &str) -> Vec<u64> {
        sqlx::query_scalar(
            "SELECT execution_id FROM executions WHERE instance_id = $1 ORDER BY execution_id ASC"
        )
        .bind(instance)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|id: i64| id as u64)
        .collect()
    }
}

// Implement ManagementCapability for advanced queries
#[async_trait::async_trait]
impl ManagementCapability for PostgresProvider {
    async fn get_instance_info(&self, instance: &str) -> Option<InstanceInfo> {
        let row = sqlx::query(
            r#"
            SELECT orchestration_name, orchestration_version, current_execution_id, created_at
            FROM instances
            WHERE instance_id = $1
            "#
        )
        .bind(instance)
        .fetch_optional(&self.pool)
        .await
        .ok()??;

        Some(InstanceInfo {
            instance_id: instance.to_string(),
            orchestration_name: row.get("orchestration_name"),
            orchestration_version: row.get("orchestration_version"),
            current_execution_id: row.get::<i64, _>("current_execution_id") as u64,
            created_at: row.get("created_at"),
        })
    }

    async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Option<ExecutionInfo> {
        let row = sqlx::query(
            r#"
            SELECT status, output, started_at, completed_at
            FROM executions
            WHERE instance_id = $1 AND execution_id = $2
            "#
        )
        .bind(instance)
        .bind(execution_id as i64)
        .fetch_optional(&self.pool)
        .await
        .ok()??;

        Some(ExecutionInfo {
            instance_id: instance.to_string(),
            execution_id,
            status: row.get("status"),
            output: row.get("output"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
        })
    }

    async fn get_queue_depths(&self) -> QueueDepths {
        let orch = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM orchestrator_queue")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let worker = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM worker_queue")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let timer = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM timer_queue")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        QueueDepths {
            orchestrator: orch as usize,
            worker: worker as usize,
            timer: timer as usize,
        }
    }

    async fn get_system_metrics(&self) -> SystemMetrics {
        let total_instances = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM instances")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let running = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT instance_id) FROM executions WHERE status = 'Running'"
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let completed = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT instance_id) FROM executions WHERE status = 'Completed'"
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let failed = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT instance_id) FROM executions WHERE status = 'Failed'"
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_events = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM history")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        SystemMetrics {
            total_instances: total_instances as usize,
            running_instances: running as usize,
            completed_instances: completed as usize,
            failed_instances: failed as usize,
            total_events: total_events as usize,
        }
    }
}
```

---

## Cargo Dependencies

Add to `Cargo.toml`:

```toml
[dependencies]
# Existing dependencies...

# PostgreSQL support
sqlx = { version = "0.7", features = ["runtime-tokio-native-tls", "postgres", "json", "chrono", "uuid"] }
chrono = "0.4"
uuid = { version = "1.0", features = ["v4", "serde"] }
```

---

## Testing Strategy

### 1. Unit Tests

Port all SQLite provider tests to PostgreSQL:

```rust
// tests/postgres_provider_test.rs

use duroxide::providers::postgres::PostgresProvider;
use testcontainers::{clients, images::postgres::Postgres, Docker};

#[tokio::test]
async fn test_postgres_provider_basic() {
    let docker = clients::Cli::default();
    let postgres = docker.run(Postgres::default());
    
    let database_url = format!(
        "postgresql://postgres:postgres@localhost:{}/postgres",
        postgres.get_host_port_ipv4(5432)
    );
    
    let provider = PostgresProvider::new(&database_url).await.unwrap();
    provider.run_migrations().await.unwrap();
    
    // Run same tests as SQLite...
}
```

### 2. Integration Tests with Testcontainers

Use Docker to spin up PostgreSQL for tests:

```toml
[dev-dependencies]
testcontainers = "0.15"
```

### 3. Azure-Specific Tests

```rust
// tests/postgres_azure_integration_test.rs

#[tokio::test]
#[ignore] // Only run with --ignored when Azure credentials available
async fn test_azure_postgres_connection() {
    let database_url = std::env::var("AZURE_POSTGRES_URL")
        .expect("AZURE_POSTGRES_URL not set");
    
    let provider = PostgresProvider::new(&database_url).await.unwrap();
    // Test SSL, auth, connection pooling...
}
```

### 4. Performance Benchmarks

Compare with SQLite:

```rust
#[bench]
fn bench_postgres_ack_throughput(b: &mut Bencher) {
    // Measure ack_orchestration_item throughput
}
```

---

## Deployment Guide

### Azure PostgreSQL Flexible Server Setup

#### 1. Create Server via Azure CLI

```bash
# Create resource group
az group create --name duroxide-rg --location eastus

# Create PostgreSQL Flexible Server
az postgres flexible-server create \
  --resource-group duroxide-rg \
  --name duroxide-postgres \
  --location eastus \
  --admin-user duradmin \
  --admin-password 'YourSecurePassword123!' \
  --sku-name Standard_B1ms \
  --tier Burstable \
  --version 14 \
  --storage-size 32 \
  --public-access 0.0.0.0

# Create database
az postgres flexible-server db create \
  --resource-group duroxide-rg \
  --server-name duroxide-postgres \
  --database-name duroxide

# Configure firewall (allow Azure services)
az postgres flexible-server firewall-rule create \
  --resource-group duroxide-rg \
  --name duroxide-postgres \
  --rule-name AllowAzureServices \
  --start-ip-address 0.0.0.0 \
  --end-ip-address 0.0.0.0
```

#### 2. Connection String

```bash
postgresql://duradmin:YourSecurePassword123!@duroxide-postgres.postgres.database.azure.com:5432/duroxide?sslmode=require
```

#### 3. Run Migrations

```rust
use duroxide::providers::postgres::PostgresProvider;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let provider = PostgresProvider::new(&database_url).await?;
    
    provider.run_migrations().await?;
    
    println!("Database initialized!");
    Ok(())
}
```

#### 4. Configure for Production

**Recommended Settings:**

- **Tier**: General Purpose (for production)
- **vCores**: 2-4 (adjust based on load)
- **Storage**: 32-128 GB (auto-grows)
- **Backup**: 7-day retention (default)
- **HA**: Enable zone-redundant HA
- **SSL**: Enforce SSL connections
- **Connection Pooling**: Use PgBouncer or built-in pooling

**Environment Variables:**

```bash
export DATABASE_URL="postgresql://user:pass@server.postgres.database.azure.com:5432/duroxide?sslmode=require"
export POSTGRES_MAX_CONNECTIONS=50
export POSTGRES_LOCK_TIMEOUT_SECS=30
```

---

## Performance Tuning

### PostgreSQL Configuration

Optimize for Duroxide workload:

```sql
-- Connection pooling
ALTER SYSTEM SET max_connections = 100;

-- Query performance
ALTER SYSTEM SET shared_buffers = '256MB';
ALTER SYSTEM SET effective_cache_size = '1GB';
ALTER SYSTEM SET work_mem = '16MB';

-- Write performance
ALTER SYSTEM SET wal_buffers = '16MB';
ALTER SYSTEM SET checkpoint_completion_target = 0.9;

-- Lock timeout (prevent deadlocks)
ALTER SYSTEM SET lock_timeout = '30s';
```

### Index Optimization

Monitor query performance and add indexes as needed:

```sql
-- Slow query log
ALTER SYSTEM SET log_min_duration_statement = 1000; -- Log queries > 1s

-- Analyze index usage
SELECT schemaname, tablename, indexname, idx_scan, idx_tup_read
FROM pg_stat_user_indexes
ORDER BY idx_scan ASC;

-- Add custom indexes based on workload
CREATE INDEX CONCURRENTLY idx_custom ON table(column) WHERE condition;
```

### Connection Pooling Best Practices

```rust
let provider = PostgresProvider::new_with_config(
    &database_url,
    50,  // max_connections: tune based on workload
    Duration::from_secs(30),  // lock_timeout
).await?;
```

**Guidelines:**
- **Workers**: 1-2 connections per worker thread
- **Dispatchers**: 5-10 connections per dispatcher
- **Total**: Don't exceed PostgreSQL max_connections - 10 (reserve for admin)

---

## Migration from SQLite

### Step-by-Step Migration

#### 1. Export SQLite Data

```rust
// export_sqlite_to_postgres.rs

use duroxide::providers::{sqlite::SqliteProvider, postgres::PostgresProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sqlite = SqliteProvider::new("sqlite:data.db", None).await?;
    let postgres = PostgresProvider::new(&std::env::var("POSTGRES_URL")?).await?;
    
    postgres.run_migrations().await?;
    
    // Migrate instances
    for instance in sqlite.list_instances().await {
        let history = sqlite.read(&instance).await;
        if !history.is_empty() {
            // Determine execution_id
            let exec_id = sqlite.latest_execution_id(&instance).await.unwrap_or(1);
            
            // Append to PostgreSQL
            postgres.append_with_execution(&instance, exec_id, history).await?;
        }
    }
    
    println!("Migration complete!");
    Ok(())
}
```

#### 2. Validate Migration

```bash
# Count records in both databases
sqlite3 data.db "SELECT COUNT(*) FROM history;"
psql $POSTGRES_URL -c "SELECT COUNT(*) FROM history;"

# Compare specific instances
sqlite3 data.db "SELECT * FROM history WHERE instance_id = 'test' ORDER BY event_id;"
psql $POSTGRES_URL -c "SELECT * FROM history WHERE instance_id = 'test' ORDER BY event_id;"
```

#### 3. Switch Provider

```rust
// Before
let provider = SqliteProvider::new("sqlite:data.db", None).await?;

// After
let provider = PostgresProvider::new(&std::env::var("DATABASE_URL")?).await?;
```

---

## Cost Estimation (Azure)

### Flexible Server Pricing (as of 2024)

| Tier | vCores | RAM | Storage | Monthly Cost |
|------|--------|-----|---------|--------------|
| **Burstable B1ms** | 1 | 2 GB | 32 GB | ~$15/month |
| **Burstable B2s** | 2 | 4 GB | 64 GB | ~$40/month |
| **General Purpose D2s v3** | 2 | 8 GB | 128 GB | ~$150/month |
| **General Purpose D4s v3** | 4 | 16 GB | 256 GB | ~$300/month |

**Additional Costs:**
- Storage: ~$0.12/GB/month
- Backup: First 100 GB free, then ~$0.10/GB/month
- Zone-redundant HA: +$100/month (doubles compute cost)

**Recommendations:**
- **Development/Testing**: Burstable B1ms
- **Small Production**: Burstable B2s
- **Medium Production**: General Purpose D2s v3
- **High Throughput**: General Purpose D4s v3 + HA

---

## Implementation Timeline

### Week 1: Core Implementation
- [x] Set up PostgreSQL schema
- [ ] Implement `PostgresProvider` struct
- [ ] Implement `fetch_orchestration_item()`
- [ ] Implement `ack_orchestration_item()`
- [ ] Implement queue operations
- [ ] Add migrations support

### Week 2: Testing & Refinement
- [ ] Port all SQLite provider tests
- [ ] Set up testcontainers integration
- [ ] Test ACID guarantees
- [ ] Test concurrent access
- [ ] Performance benchmarking
- [ ] Optimize indexes

### Week 3: Deployment & Documentation
- [ ] Azure deployment scripts
- [ ] Connection pooling tuning
- [ ] Migration tools (SQLite → PostgreSQL)
- [ ] Update provider implementation guide
- [ ] Create deployment guide
- [ ] Example configurations

---

## Open Questions & Decisions

### 1. Should we support connection string from Azure Key Vault?

**Proposal**: Add helper for Azure-managed identity:

```rust
impl PostgresProvider {
    pub async fn new_from_azure_identity(
        server: &str,
        database: &str,
    ) -> Result<Self, String> {
        // Use Azure DefaultAzureCredential to get access token
        // Build connection string with token auth
        todo!()
    }
}
```

**Decision**: Implement in Phase 2 (post-MVP)

### 2. Partitioning Strategy for Large Deployments?

For massive scale (millions of instances), consider table partitioning:

```sql
-- Partition history by instance_id hash
CREATE TABLE history_partition_0 PARTITION OF history
    FOR VALUES WITH (MODULUS 10, REMAINDER 0);
```

**Decision**: Document pattern, implement if needed

### 3. Read Replicas for Query Offloading?

Azure supports read replicas. Should provider support read-only operations on replicas?

**Decision**: Out of scope for MVP (writes dominate workload)

---

## Success Criteria

Provider is complete when:

- [ ] All Provider trait methods implemented
- [ ] All SQLite provider tests pass with PostgreSQL
- [ ] ACID guarantees verified under concurrent load
- [ ] Successfully deployed to Azure PostgreSQL Flexible Server
- [ ] Performance >= SQLite for typical workloads
- [ ] Documentation complete (this plan + deployment guide)
- [ ] Migration path from SQLite validated
- [ ] Example configurations provided

---

## References

- [SQLite Provider Implementation](../src/providers/sqlite.rs)
- [Provider Implementation Guide](./provider-implementation-guide.md)
- [Azure PostgreSQL Documentation](https://learn.microsoft.com/en-us/azure/postgresql/)
- [PostgreSQL SKIP LOCKED](https://www.postgresql.org/docs/current/sql-select.html#SQL-FOR-UPDATE-SHARE)
- [sqlx Documentation](https://docs.rs/sqlx/latest/sqlx/)

---

**Next Steps:** Begin implementation with Week 1 tasks. Start with schema migration and core Provider trait methods.

