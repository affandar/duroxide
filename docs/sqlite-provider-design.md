# SQLite Provider Design for Duroxide

## Status: Implemented ✅

The SQLite provider has been successfully implemented with full transactional support.

## Executive Summary

SQLite is an excellent choice for a transactional Provider because:
- **ACID compliance** with full transactional support
- **Single-file deployment** with zero configuration
- **Excellent performance** for local/embedded scenarios
- **Battle-tested reliability** used in millions of applications
- **Built-in concurrency** with proper locking mechanisms

## Schema Design

### Core Tables

```sql
-- Instance metadata and current state
CREATE TABLE instances (
    instance_id TEXT PRIMARY KEY,
    orchestration_name TEXT NOT NULL,
    orchestration_version TEXT NOT NULL,
    current_execution_id INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'Running', -- Running, Completed, Failed, ContinuedAsNew
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Multi-execution support for ContinueAsNew
CREATE TABLE executions (
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'Running',
    started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    PRIMARY KEY (instance_id, execution_id),
    FOREIGN KEY (instance_id) REFERENCES instances(instance_id)
);

-- Event history (append-only)
CREATE TABLE history (
    instance_id TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    sequence_num INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL, -- JSON serialized Event
    timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (instance_id, execution_id, sequence_num),
    FOREIGN KEY (instance_id, execution_id) REFERENCES executions(instance_id, execution_id)
);

-- Orchestrator queue with visibility support
CREATE TABLE orchestrator_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    work_item TEXT NOT NULL, -- JSON serialized WorkItem
    visible_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    lock_token TEXT,
    locked_until TIMESTAMP,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_visible (visible_at, lock_token),
    INDEX idx_instance (instance_id)
);

-- Worker queue for activity execution
CREATE TABLE worker_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item TEXT NOT NULL, -- JSON serialized WorkItem
    lock_token TEXT,
    locked_until TIMESTAMP,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_available (lock_token, id)
);

-- Timer queue (if we implement native delayed visibility)
CREATE TABLE timer_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item TEXT NOT NULL, -- JSON serialized WorkItem
    fire_at TIMESTAMP NOT NULL,
    lock_token TEXT,
    locked_until TIMESTAMP,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_fire_time (fire_at, lock_token)
);
```

### Key Design Decisions

1. **Separate queues** for orchestrator, worker, and timer to avoid contention
2. **Lock tokens with expiry** for crash recovery
3. **Visibility timestamps** for delayed message processing
4. **Normalized schema** for efficient queries and updates
5. **JSON storage** for flexible event/work item serialization

## Implementation Plan

### Phase 1: Core Provider Structure

```rust
use sqlx::{SqlitePool, Transaction, Sqlite};
use serde_json;
use std::time::{SystemTime, Duration};
use crate::providers::{Provider, WorkItem, OrchestrationItem};
use crate::Event;

pub struct SqliteProvider {
    pool: SqlitePool,
    lock_timeout: Duration,
    history_cap: usize,
}

impl SqliteProvider {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePool::connect(database_url).await?;
        
        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await?;
        
        Ok(Self {
            pool,
            lock_timeout: Duration::from_secs(30),
            history_cap: 1024,
        })
    }
    
    async fn with_transaction<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Transaction<'_, Sqlite>) -> Result<R, sqlx::Error>,
    {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        match f(&mut tx) {
            Ok(result) => {
                tx.commit().await.map_err(|e| e.to_string())?;
                Ok(result)
            }
            Err(e) => {
                tx.rollback().await.ok();
                Err(e.to_string())
            }
        }
    }
}
```

### Phase 2: Core Atomic Methods

```rust
#[async_trait::async_trait]
impl Provider for SqliteProvider {
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        let mut tx = self.pool.begin().await.ok()?;
        
        // Get next available instance with work
        let row = sqlx::query!(
            r#"
            SELECT DISTINCT instance_id
            FROM orchestrator_queue
            WHERE visible_at <= datetime('now')
              AND (lock_token IS NULL OR locked_until < datetime('now'))
            ORDER BY visible_at
            LIMIT 1
            "#
        )
        .fetch_optional(&mut tx)
        .await
        .ok()??;
        
        let instance_id = row.instance_id;
        let lock_token = generate_lock_token();
        let locked_until = SystemTime::now() + self.lock_timeout;
        
        // Lock all messages for this instance
        sqlx::query!(
            r#"
            UPDATE orchestrator_queue
            SET lock_token = ?1, locked_until = ?2
            WHERE instance_id = ?3
              AND visible_at <= datetime('now')
              AND (lock_token IS NULL OR locked_until < datetime('now'))
            "#,
            lock_token,
            locked_until,
            instance_id
        )
        .execute(&mut tx)
        .await
        .ok()?;
        
        // Fetch locked messages
        let messages = sqlx::query!(
            r#"
            SELECT id, work_item
            FROM orchestrator_queue
            WHERE lock_token = ?1
            ORDER BY id
            "#,
            lock_token
        )
        .fetch_all(&mut tx)
        .await
        .ok()?;
        
        // Deserialize work items
        let work_items: Vec<WorkItem> = messages
            .iter()
            .filter_map(|r| serde_json::from_str(&r.work_item).ok())
            .collect();
        
        // Load history
        let history = self.read_history_in_tx(&mut tx, &instance_id).await.ok()?;
        
        tx.commit().await.ok()?;
        
        Some(OrchestrationItem {
            instance: instance_id,
            messages: work_items,
            history,
            lock_token,
        })
    }
    
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
    ) -> Result<(), String> {
        self.with_transaction(|tx| async move {
            // 1. Delete acknowledged messages
            sqlx::query!(
                "DELETE FROM orchestrator_queue WHERE lock_token = ?",
                lock_token
            )
            .execute(tx)
            .await?;
            
            // 2. Get instance from the lock
            let instance = self.get_instance_from_lock(tx, lock_token).await?;
            
            // 3. Append history
            self.append_history_in_tx(tx, &instance, history_delta).await?;
            
            // 4. Enqueue worker items
            for item in worker_items {
                self.enqueue_worker_in_tx(tx, item).await?;
            }
            
            // 5. Enqueue orchestrator items (may include TimerFired with delayed visibility)
            for item in orchestrator_items {
                self.enqueue_orchestrator_in_tx(tx, item).await?;
            }
            
            Ok(())
        }).await
    }
}
```

## Advantages Over Filesystem Provider

### 1. **True ACID Transactions**
```sql
BEGIN TRANSACTION;
-- All operations succeed or fail together
DELETE FROM orchestrator_queue WHERE lock_token = ?;
INSERT INTO history ...;
INSERT INTO worker_queue ...;
INSERT INTO timer_queue ...;
INSERT INTO orchestrator_queue ...;
COMMIT;
```

### 2. **Built-in Concurrency Control**
- Row-level locking prevents race conditions
- Atomic compare-and-swap operations
- No manual file locking needed

### 3. **Efficient Queries**
```sql
-- Get next available work with single query
SELECT * FROM orchestrator_queue 
WHERE visible_at <= datetime('now') 
  AND lock_token IS NULL
ORDER BY visible_at 
LIMIT 1;
```

### 4. **Automatic Crash Recovery**
```sql
-- Auto-abandon expired locks
UPDATE orchestrator_queue 
SET lock_token = NULL 
WHERE locked_until < datetime('now');
```

### 5. **Native Delayed Visibility**
```sql
-- Schedule future work
INSERT INTO timer_queue (work_item, fire_at) 
VALUES (?, datetime('now', '+5 minutes'));
```

## Performance Characteristics

### Expected Performance
- **Enqueue**: ~1-2ms per operation
- **Dequeue**: ~2-5ms including lock acquisition
- **History append**: ~1-3ms for small batches
- **Transaction overhead**: ~0.5-1ms

### Optimization Options
1. **Write-Ahead Logging** (WAL mode) for better concurrency
2. **Connection pooling** to reduce overhead
3. **Prepared statements** for repeated queries
4. **Batch operations** where possible

## Migration Path

### 1. **Side-by-Side Testing**
```rust
// Feature flag for provider selection
let provider: Arc<dyn Provider> = match config.provider {
    ProviderKind::Filesystem => Arc::new(FsProvider::new(path)),
    ProviderKind::Sqlite => Arc::new(SqliteProvider::new(url, None).await?),
    ProviderKind::InMemory => Arc::new(InMemoryProvider::default()),
};
```

### 2. **Data Migration Tool**
```rust
pub async fn migrate_fs_to_sqlite(fs_path: &Path, sqlite_url: &str) -> Result<()> {
    let fs_store = FsProvider::new(fs_path, false);
    let sqlite_store = SqliteProvider::new(sqlite_url, None).await?;
    
    for instance in fs_store.list_instances().await {
        let history = fs_store.read(&instance).await;
        sqlite_store.import_instance(&instance, history).await?;
    }
    
    Ok(())
}
```

## Implementation Timeline

### Week 1: Foundation
- Set up SQLite dependencies (sqlx)
- Create schema and migrations
- Implement basic CRUD operations

### Week 2: Core Methods
- Implement `fetch_orchestration_item`
- Implement `ack_orchestration_item`
- Add queue operations

### Week 3: Advanced Features
- Multi-execution support
- Timer queue with delays
- Lock timeout handling

### Week 4: Production Ready
- Performance optimization
- Monitoring and metrics
- Migration tooling

## Risks and Mitigations

### 1. **SQLite Limitations**
- **Risk**: Write concurrency limits
- **Mitigation**: Use WAL mode, consider sharding by instance

### 2. **Lock Timeout Tuning**
- **Risk**: Too short = unnecessary retries, too long = slow recovery
- **Mitigation**: Make configurable, add metrics

### 3. **History Growth**
- **Risk**: Unbounded history size
- **Mitigation**: Implement history capping, archival strategy

## Conclusion

SQLite provides an ideal solution for Duroxide's transactional needs:
- **Solves all concurrency issues** in the filesystem provider
- **Minimal operational overhead** compared to full databases
- **Production-ready** for single-node deployments
- **Easy migration path** from existing providers

The implementation is straightforward with modern Rust SQLite libraries, and the benefits far outweigh the development effort.
