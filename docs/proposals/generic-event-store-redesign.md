# Generic Event Store Provider Redesign

**Goal:** Transform Provider from Duroxide-specific trait to generic event store abstraction  
**Effort:** ~20-40 hours  
**Risk:** High (breaking change)  
**Benefit:** Perfect abstraction, reusable for any event-sourced system

---

## Executive Summary

Current Provider is tightly coupled to Duroxide orchestration concepts (instance, execution_id, WorkItem types, etc.). This redesign would create a **truly generic** event store + message queue abstraction that:

- ✅ Has no Duroxide-specific knowledge
- ✅ Can be used by any event-sourced system
- ✅ Minimal interface (5-7 core methods)
- ✅ Perfect separation of concerns

The runtime would provide an **adapter layer** that translates Duroxide concepts to/from generic storage primitives.

---

## Proposed Generic Interface

### Core Abstraction: EventStore + MessageQueue + AtomicOps

```rust
use serde::{Serialize, de::DeserializeOwned};

/// Generic event log abstraction.
/// 
/// Events are stored in partitions (e.g., orchestration executions).
/// Entity = instance ID, Partition = execution ID
#[async_trait::async_trait]
pub trait EventLog: Send + Sync {
    /// Append events to a specific partition.
    /// 
    /// # Parameters
    /// - `entity`: Entity identifier (e.g., "order-123")
    /// - `partition`: Partition within entity (e.g., execution_id=1)
    /// - `events`: Events to append (must preserve order)
    /// 
    /// # Guarantees
    /// - Events appended in order
    /// - Duplicate detection via (entity, partition, event_id)
    /// - Returns error if storage fails
    async fn append_events(
        &self,
        entity: &str,
        partition: u64,
        events: Vec<Event>,
    ) -> Result<(), String>;
    
    /// Read all events for a specific partition.
    /// 
    /// # Returns
    /// - Events in order (by event_id ascending)
    /// - Empty Vec if partition doesn't exist
    async fn read_events(
        &self,
        entity: &str,
        partition: u64,
    ) -> Vec<Event>;
    
    /// Get the latest (highest) partition ID for an entity.
    /// 
    /// # Returns
    /// - None if entity doesn't exist
    /// - Some(partition_id) otherwise
    async fn latest_partition(&self, entity: &str) -> Option<u64>;
}

/// Generic message queue with peek-lock semantics.
#[async_trait::async_trait]
pub trait MessageQueue: Send + Sync {
    /// Enqueue a message to a named queue.
    /// 
    /// # Parameters
    /// - `queue`: Queue name (e.g., "orchestrator", "worker", "timer")
    /// - `message`: Serialized message (opaque bytes)
    /// - `visible_after_ms`: Optional delay before message is visible
    /// 
    /// # Routing
    /// Messages can be routed to any queue. Suggested queues:
    /// - "orchestrator" - For orchestration work
    /// - "worker" - For activity execution
    /// - "timer" - For scheduled work
    /// 
    /// Providers can use any queue naming scheme.
    async fn enqueue(
        &self,
        queue: &str,
        message: Vec<u8>,
        visible_after_ms: Option<u64>,
    ) -> Result<(), String>;
    
    /// Dequeue a message with peek-lock semantics.
    /// 
    /// # Parameters
    /// - `queue`: Queue name to dequeue from
    /// 
    /// # Returns
    /// - Some((message, lock_token)) - Message locked for processing
    /// - None - No visible messages available
    /// 
    /// # Peek-Lock Semantics
    /// - Message stays in queue (not deleted)
    /// - Lock token identifies this lock
    /// - Lock expires after timeout (provider-specific)
    /// - If lock expires before ack, message becomes available again
    async fn dequeue(
        &self,
        queue: &str,
    ) -> Option<(Vec<u8>, LockToken)>;
    
    /// Acknowledge successful processing (delete message).
    /// 
    /// # Parameters
    /// - `lock_token`: Token from dequeue operation
    /// 
    /// # Behavior
    /// - Delete message from queue
    /// - Idempotent (Ok if already deleted)
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String>;
    
    /// Abandon processing (release lock for retry).
    /// 
    /// # Parameters
    /// - `lock_token`: Token from dequeue operation
    /// - `delay_ms`: Optional delay before message becomes visible again
    /// 
    /// # Behavior
    /// - Clear lock (make message available)
    /// - Optionally delay visibility for backoff
    async fn abandon(&self, lock_token: &LockToken, delay_ms: Option<u64>) -> Result<(), String>;
}

/// Generic key-value metadata store for partition state.
#[async_trait::async_trait]
pub trait MetadataStore: Send + Sync {
    /// Update metadata for a partition.
    /// 
    /// # Parameters
    /// - `entity`: Entity identifier
    /// - `partition`: Partition within entity
    /// - `metadata`: Key-value pairs to store
    /// 
    /// # Behavior
    /// - Upsert (update existing or insert new)
    /// - Atomic with respect to other metadata operations
    async fn update_metadata(
        &self,
        entity: &str,
        partition: u64,
        metadata: &HashMap<String, String>,
    ) -> Result<(), String>;
    
    /// Read metadata for a partition.
    async fn read_metadata(
        &self,
        entity: &str,
        partition: u64,
    ) -> HashMap<String, String>;
}

/// Atomic transaction coordinator.
/// 
/// This is the key abstraction that enables atomic orchestration turns.
#[async_trait::async_trait]
pub trait AtomicOps: Send + Sync {
    /// Execute multiple operations atomically.
    /// 
    /// # Guarantees
    /// - All operations succeed or all fail (no partial commits)
    /// - Isolation (other transactions don't see partial state)
    /// - Durability (committed changes survive crashes)
    /// 
    /// # Parameters
    /// - `operations`: List of operations to execute atomically
    /// 
    /// # Returns
    /// - Ok(()) - All operations committed
    /// - Err(msg) - Transaction rolled back
    async fn atomic_batch(&self, operations: Vec<Operation>) -> Result<(), String>;
}

/// Atomic operation types.
#[derive(Clone, Debug)]
pub enum Operation {
    /// Append events to partition
    AppendEvents {
        entity: String,
        partition: u64,
        events: Vec<Event>,
    },
    
    /// Update partition metadata
    UpdateMetadata {
        entity: String,
        partition: u64,
        metadata: HashMap<String, String>,
    },
    
    /// Enqueue message to queue
    Enqueue {
        queue: String,
        message: Vec<u8>,
        visible_after_ms: Option<u64>,
    },
    
    /// Delete messages by lock token
    DeleteMessages {
        lock_token: LockToken,
    },
}

/// Lock token (opaque to runtime).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LockToken(pub String);

/// Unified Provider trait.
pub trait Provider: EventLog + MessageQueue + MetadataStore + AtomicOps {}

// Blanket impl - any type that implements the 4 sub-traits is a Provider
impl<T: EventLog + MessageQueue + MetadataStore + AtomicOps> Provider for T {}
```

---

## Runtime Adapter Layer

The runtime would provide an adapter that translates between Duroxide concepts and generic storage:

```rust
/// Runtime's adapter for Provider interactions.
/// 
/// This layer knows about Duroxide concepts (WorkItem, OrchestrationItem, etc.)
/// but the Provider only knows about generic events/messages.
pub struct ProviderAdapter<P: Provider> {
    provider: Arc<P>,
}

impl<P: Provider> ProviderAdapter<P> {
    /// Fetch orchestration work (Duroxide-specific logic).
    pub async fn fetch_orchestration_item(&self) -> Option<DuroxideOrchestrationItem> {
        // 1. Dequeue from "orchestrator" queue
        let (message_bytes, lock_token) = self.provider.dequeue("orchestrator").await?;
        
        // 2. Deserialize to get instance and messages
        let batch: OrchestrationBatch = bincode::deserialize(&message_bytes).ok()?;
        
        // 3. Get latest partition (execution_id) for this entity
        let partition = self.provider.latest_partition(&batch.instance).await.unwrap_or(1);
        
        // 4. Load history from event log
        let history = self.provider.read_events(&batch.instance, partition).await;
        
        // 5. Extract metadata from first event
        let (orchestration_name, version) = history.iter()
            .find_map(|e| match e {
                Event::OrchestrationStarted { name, version, .. } => Some((name.clone(), version.clone())),
                _ => None
            })
            .unwrap_or_else(|| ("Unknown".to_string(), "1.0.0".to_string()));
        
        Some(DuroxideOrchestrationItem {
            instance: batch.instance,
            orchestration_name,
            execution_id: partition,
            version,
            history,
            messages: batch.messages,
            lock_token,
        })
    }
    
    /// Acknowledge orchestration turn (Duroxide-specific logic).
    pub async fn ack_orchestration_item(
        &self,
        instance: &str,
        execution_id: u64,
        lock_token: LockToken,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        timer_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), String> {
        let mut operations = Vec::new();
        
        // 1. Append events
        if !history_delta.is_empty() {
            operations.push(Operation::AppendEvents {
                entity: instance.to_string(),
                partition: execution_id,
                events: history_delta,
            });
        }
        
        // 2. Update metadata
        if metadata.status.is_some() {
            let mut meta_map = HashMap::new();
            if let Some(status) = &metadata.status {
                meta_map.insert("status".to_string(), status.clone());
            }
            if let Some(output) = &metadata.output {
                meta_map.insert("output".to_string(), output.clone());
            }
            meta_map.insert("completed_at".to_string(), current_timestamp_iso());
            
            operations.push(Operation::UpdateMetadata {
                entity: instance.to_string(),
                partition: execution_id,
                metadata: meta_map,
            });
        }
        
        // 3. Create next partition
        if metadata.create_next_execution {
            if let Some(next_id) = metadata.next_execution_id {
                let mut next_meta = HashMap::new();
                next_meta.insert("status".to_string(), "Running".to_string());
                next_meta.insert("started_at".to_string(), current_timestamp_iso());
                
                operations.push(Operation::UpdateMetadata {
                    entity: instance.to_string(),
                    partition: next_id,
                    metadata: next_meta,
                });
                
                // Update current partition pointer
                next_meta.clear();
                next_meta.insert("current_partition".to_string(), next_id.to_string());
                operations.push(Operation::UpdateMetadata {
                    entity: instance.to_string(),
                    partition: 0,  // Entity-level metadata
                    metadata: next_meta,
                });
            }
        }
        
        // 4. Enqueue worker items
        for item in worker_items {
            operations.push(Operation::Enqueue {
                queue: "worker".to_string(),
                message: bincode::serialize(&item).unwrap(),
                visible_after_ms: None,
            });
        }
        
        // 5. Enqueue timer items
        for item in timer_items {
            if let WorkItem::TimerSchedule { fire_at_ms, .. } = &item {
                let delay = fire_at_ms.saturating_sub(current_timestamp_ms());
                operations.push(Operation::Enqueue {
                    queue: "timer".to_string(),
                    message: bincode::serialize(&item).unwrap(),
                    visible_after_ms: Some(delay),
                });
            }
        }
        
        // 6. Enqueue orchestrator items
        for item in orchestrator_items {
            operations.push(Operation::Enqueue {
                queue: "orchestrator".to_string(),
                message: bincode::serialize(&item).unwrap(),
                visible_after_ms: None,
            });
        }
        
        // 7. Delete acknowledged messages
        operations.push(Operation::DeleteMessages { lock_token });
        
        // Execute all atomically
        self.provider.atomic_batch(operations).await
    }
}
```

---

## Detailed Implementation Plan

### Phase 1: Design New Traits (Week 1)

#### Step 1.1: Define Core Traits

**File:** `src/providers/generic/mod.rs` (new)

```rust
// Create new module for generic abstractions
pub mod generic;

// Define 4 sub-traits:
pub trait EventLog { ... }
pub trait MessageQueue { ... }
pub trait MetadataStore { ... }
pub trait AtomicOps { ... }

// Provider is the combination
pub trait Provider: EventLog + MessageQueue + MetadataStore + AtomicOps {}
```

#### Step 1.2: Define Supporting Types

```rust
/// Opaque lock token
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LockToken(pub String);

/// Atomic operation enum
pub enum Operation {
    AppendEvents { entity: String, partition: u64, events: Vec<Event> },
    UpdateMetadata { entity: String, partition: u64, metadata: HashMap<String, String> },
    Enqueue { queue: String, message: Vec<u8>, visible_after_ms: Option<u64> },
    DeleteMessages { lock_token: LockToken },
}

/// Serialization trait for messages
pub trait Message: Serialize + DeserializeOwned {}
impl<T: Serialize + DeserializeOwned> Message for T {}
```

#### Step 1.3: Write Documentation

Add comprehensive docs similar to current Provider trait:
- Each trait's purpose
- Implementation patterns
- SQL examples
- Redis examples
- Common pitfalls

**Effort:** 3-4 hours

---

### Phase 2: Create Runtime Adapter Layer (Week 1-2)

#### Step 2.1: Define Adapter

**File:** `src/runtime/provider_adapter.rs` (new)

```rust
/// Adapter that translates Duroxide concepts to generic Provider operations.
pub struct ProviderAdapter<P: Provider> {
    provider: Arc<P>,
}

impl<P: Provider> ProviderAdapter<P> {
    pub fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }
    
    // Implement Duroxide-specific operations using generic Provider
    pub async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> { ... }
    pub async fn ack_orchestration_item(...) -> Result<(), String> { ... }
    pub async fn enqueue_worker_work(...) -> Result<(), String> { ... }
    // ... etc
}
```

#### Step 2.2: Message Batching Strategy

**Problem:** Current fetch returns batch of WorkItems for an instance.  
**Solution:** Store routing metadata with messages.

```rust
/// Metadata stored with each queue message
#[derive(Serialize, Deserialize)]
struct MessageEnvelope {
    entity: String,           // instance_id
    partition: u64,           // execution_id
    sequence: u64,            // message sequence number
    payload: Vec<u8>,         // WorkItem serialized
    created_at_ms: u64,
}

/// Adapter batching logic
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    // 1. Dequeue from "orchestrator" queue
    let (message_bytes, lock_token) = self.provider.dequeue("orchestrator").await?;
    let envelope: MessageEnvelope = bincode::deserialize(&message_bytes).ok()?;
    let instance = envelope.entity.clone();
    
    // 2. Fetch ALL messages for this instance (batch)
    // Option A: Dequeue until different instance (peek-based batching)
    // Option B: Store instance→messages mapping (simpler, used here)
    
    // For simplicity, assume one message per fetch (change hot path to not batch)
    let messages = vec![bincode::deserialize(&envelope.payload).ok()?];
    
    // 3. Load history
    let partition = self.provider.latest_partition(&instance).await.unwrap_or(1);
    let history = self.provider.read_events(&instance, partition).await;
    
    // 4. Extract metadata from history
    let (orch_name, version) = extract_metadata_from_history(&history);
    
    Some(OrchestrationItem {
        instance,
        orchestration_name: orch_name,
        execution_id: partition,
        version,
        history,
        messages,
        lock_token,
    })
}
```

**Alternative: Instance-Batched Queue**

Store pending work per instance, fetch batches:

```rust
// Provider stores: entity → Vec<MessageEnvelope>
// fetch_orchestration_item fetches entire batch for one entity
// ack clears all messages for that entity
```

#### Step 2.3: Metadata Mapping

**Map Duroxide concepts to generic metadata:**

```rust
// Duroxide → Generic mapping
Instance metadata:
  orchestration_name → metadata["orch_name"]
  orchestration_version → metadata["orch_version"]
  current_execution_id → metadata["current_partition"]

Execution metadata:
  status → metadata["status"]
  output → metadata["output"]
  started_at → metadata["started_at"]
  completed_at → metadata["completed_at"]
```

**Effort:** 8-12 hours

---

### Phase 3: Implement Generic SqliteProvider (Week 2)

#### Step 3.1: Database Schema

**Generic schema (no Duroxide knowledge):**

```sql
-- Entity registry (optional, for listing)
CREATE TABLE entities (
    entity_id TEXT PRIMARY KEY,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Event log (append-only)
CREATE TABLE events (
    entity_id TEXT NOT NULL,
    partition_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    event_data BLOB NOT NULL,
    event_type TEXT,  -- For indexing (extracted from Event discriminant)
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (entity_id, partition_id, event_id)
);

-- Partition metadata (generic key-value)
CREATE TABLE partition_metadata (
    entity_id TEXT NOT NULL,
    partition_id INTEGER NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (entity_id, partition_id, key)
);

-- Generic message queue
CREATE TABLE message_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_name TEXT NOT NULL,
    message_data BLOB NOT NULL,
    visible_at INTEGER NOT NULL,  -- Milliseconds since epoch
    lock_token TEXT,
    locked_until INTEGER,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Indexes
CREATE INDEX idx_events_lookup ON events(entity_id, partition_id, event_id);
CREATE INDEX idx_queue_visible ON message_queue(queue_name, visible_at, lock_token);
CREATE INDEX idx_queue_lock ON message_queue(lock_token);
CREATE INDEX idx_metadata_lookup ON partition_metadata(entity_id, partition_id, key);
```

**Notice:**
- No `instances` table (entity-level metadata in partition_metadata with partition_id=0)
- No `executions` table (partition metadata in partition_metadata)
- No `orchestrator_queue`, `worker_queue`, `timer_queue` (unified message_queue)
- No Duroxide-specific columns

#### Step 3.2: Implement EventLog

```rust
impl EventLog for GenericSqliteProvider {
    async fn append_events(&self, entity: &str, partition: u64, events: Vec<Event>) -> Result<(), String> {
        let mut conn = self.pool.acquire().await.map_err(|e| e.to_string())?;
        
        for event in events {
            if event.event_id() == 0 {
                return Err("event_id must be set".to_string());
            }
            
            let event_data = bincode::serialize(&event).map_err(|e| e.to_string())?;
            let event_type = discriminant_name(&event);
            
            sqlx::query(
                "INSERT INTO events (entity_id, partition_id, event_id, event_data, event_type) 
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING"
            )
            .bind(entity)
            .bind(partition as i64)
            .bind(event.event_id() as i64)
            .bind(event_data)
            .bind(event_type)
            .execute(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
        }
        
        Ok(())
    }
    
    async fn read_events(&self, entity: &str, partition: u64) -> Vec<Event> {
        let mut conn = self.pool.acquire().await.ok()?;
        
        let rows = sqlx::query("SELECT event_data FROM events WHERE entity_id = ? AND partition_id = ? ORDER BY event_id")
            .bind(entity)
            .bind(partition as i64)
            .fetch_all(&mut *conn)
            .await
            .unwrap_or_default();
        
        rows.into_iter()
            .filter_map(|row| {
                let bytes: Vec<u8> = row.try_get("event_data").ok()?;
                bincode::deserialize(&bytes).ok()
            })
            .collect()
    }
    
    async fn latest_partition(&self, entity: &str) -> Option<u64> {
        let mut conn = self.pool.acquire().await.ok()?;
        
        let partition: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(partition_id) FROM events WHERE entity_id = ?"
        )
        .bind(entity)
        .fetch_optional(&mut *conn)
        .await
        .ok()?;
        
        partition.map(|p| p as u64)
    }
}
```

#### Step 3.3: Implement MessageQueue

```rust
impl MessageQueue for GenericSqliteProvider {
    async fn enqueue(&self, queue: &str, message: Vec<u8>, visible_after_ms: Option<u64>) -> Result<(), String> {
        let visible_at = current_timestamp_ms() + visible_after_ms.unwrap_or(0);
        
        sqlx::query(
            "INSERT INTO message_queue (queue_name, message_data, visible_at) VALUES (?, ?, ?)"
        )
        .bind(queue)
        .bind(message)
        .bind(visible_at as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        
        Ok(())
    }
    
    async fn dequeue(&self, queue: &str) -> Option<(Vec<u8>, LockToken)> {
        let mut tx = self.pool.begin().await.ok()?;
        let now = current_timestamp_ms();
        
        // Find next visible message
        let row = sqlx::query(
            "SELECT id, message_data FROM message_queue 
             WHERE queue_name = ? AND visible_at <= ? AND (lock_token IS NULL OR locked_until <= ?)
             ORDER BY id LIMIT 1"
        )
        .bind(queue)
        .bind(now as i64)
        .bind(now as i64)
        .fetch_optional(&mut *tx)
        .await
        .ok()??;
        
        let id: i64 = row.try_get("id").ok()?;
        let message_data: Vec<u8> = row.try_get("message_data").ok()?;
        
        // Lock it
        let lock_token = LockToken(uuid::Uuid::new_v4().to_string());
        let locked_until = now + 30_000;  // 30 second lock
        
        sqlx::query("UPDATE message_queue SET lock_token = ?, locked_until = ? WHERE id = ?")
            .bind(&lock_token.0)
            .bind(locked_until as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .ok()?;
        
        tx.commit().await.ok()?;
        Some((message_data, lock_token))
    }
    
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String> {
        sqlx::query("DELETE FROM message_queue WHERE lock_token = ?")
            .bind(&lock_token.0)
            .execute(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    
    async fn abandon(&self, lock_token: &LockToken, delay_ms: Option<u64>) -> Result<(), String> {
        let visible_at = current_timestamp_ms() + delay_ms.unwrap_or(0);
        
        sqlx::query(
            "UPDATE message_queue SET lock_token = NULL, locked_until = NULL, visible_at = ? WHERE lock_token = ?"
        )
        .bind(visible_at as i64)
        .bind(&lock_token.0)
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        
        Ok(())
    }
}
```

#### Step 3.4: Implement MetadataStore

```rust
impl MetadataStore for GenericSqliteProvider {
    async fn update_metadata(&self, entity: &str, partition: u64, metadata: &HashMap<String, String>) -> Result<(), String> {
        let mut conn = self.pool.acquire().await.map_err(|e| e.to_string())?;
        
        for (key, value) in metadata {
            sqlx::query(
                "INSERT INTO partition_metadata (entity_id, partition_id, key, value, updated_at)
                 VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                 ON CONFLICT (entity_id, partition_id, key) DO UPDATE SET value = ?, updated_at = CURRENT_TIMESTAMP"
            )
            .bind(entity)
            .bind(partition as i64)
            .bind(key)
            .bind(value)
            .bind(value)
            .execute(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
        }
        
        Ok(())
    }
    
    async fn read_metadata(&self, entity: &str, partition: u64) -> HashMap<String, String> {
        let mut conn = self.pool.acquire().await.ok()?;
        
        let rows = sqlx::query("SELECT key, value FROM partition_metadata WHERE entity_id = ? AND partition_id = ?")
            .bind(entity)
            .bind(partition as i64)
            .fetch_all(&mut *conn)
            .await
            .unwrap_or_default();
        
        rows.into_iter()
            .filter_map(|row| {
                let key: String = row.try_get("key").ok()?;
                let value: String = row.try_get("value").ok()?;
                Some((key, value))
            })
            .collect()
    }
}
```

#### Step 3.5: Implement AtomicOps

```rust
impl AtomicOps for GenericSqliteProvider {
    async fn atomic_batch(&self, operations: Vec<Operation>) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        
        for op in operations {
            match op {
                Operation::AppendEvents { entity, partition, events } => {
                    for event in events {
                        let event_data = bincode::serialize(&event).map_err(|e| e.to_string())?;
                        let event_type = discriminant_name(&event);
                        
                        sqlx::query(
                            "INSERT INTO events (entity_id, partition_id, event_id, event_data, event_type)
                             VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
                        )
                        .bind(&entity)
                        .bind(partition as i64)
                        .bind(event.event_id() as i64)
                        .bind(event_data)
                        .bind(event_type)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                }
                
                Operation::UpdateMetadata { entity, partition, metadata } => {
                    for (key, value) in metadata {
                        sqlx::query(
                            "INSERT INTO partition_metadata (entity_id, partition_id, key, value)
                             VALUES (?, ?, ?, ?)
                             ON CONFLICT (entity_id, partition_id, key) DO UPDATE SET value = ?, updated_at = CURRENT_TIMESTAMP"
                        )
                        .bind(&entity)
                        .bind(partition as i64)
                        .bind(&key)
                        .bind(&value)
                        .bind(&value)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                }
                
                Operation::Enqueue { queue, message, visible_after_ms } => {
                    let visible_at = current_timestamp_ms() + visible_after_ms.unwrap_or(0);
                    
                    sqlx::query(
                        "INSERT INTO message_queue (queue_name, message_data, visible_at) VALUES (?, ?, ?)"
                    )
                    .bind(&queue)
                    .bind(&message)
                    .bind(visible_at as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                }
                
                Operation::DeleteMessages { lock_token } => {
                    sqlx::query("DELETE FROM message_queue WHERE lock_token = ?")
                        .bind(&lock_token.0)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
```

**Effort:** 8-12 hours

---

### Phase 3: Update Runtime to Use Adapter (Week 2-3)

#### Step 4.1: Replace Provider Usage

**Current:**
```rust
pub struct Runtime {
    history_store: Arc<dyn Provider>,  // Old Provider trait
}
```

**New:**
```rust
pub struct Runtime {
    provider_adapter: ProviderAdapter<dyn Provider>,  // Generic Provider via adapter
}

// Or keep old interface, wrap internally:
pub struct Runtime {
    history_store: Arc<dyn OldProvider>,  // Backward compat
    // Internally converted to adapter
}
```

#### Step 4.2: Update All Call Sites

Replace direct provider calls with adapter:

```rust
// Before
self.history_store.fetch_orchestration_item().await

// After
self.provider_adapter.fetch_orchestration_item().await
```

**Effort:** 4-6 hours (mechanical changes)

---

### Phase 4: Migration Strategy (Week 3)

#### Option A: Backward Compatibility Adapter

Keep both old and new Provider traits, provide adapter:

```rust
// Old trait (deprecated but still works)
pub trait LegacyProvider { ... }

// New trait (generic)
pub trait Provider: EventLog + MessageQueue + MetadataStore + AtomicOps {}

// Adapter: LegacyProvider → Provider
pub struct LegacyToGenericAdapter<P: LegacyProvider> {
    legacy: Arc<P>,
}

impl<P: LegacyProvider> EventLog for LegacyToGenericAdapter<P> {
    async fn append_events(...) {
        // Translate to legacy.append_with_execution(...)
    }
}

// Users can use either old or new Provider
```

**Benefit:** Zero breaking changes  
**Cost:** Maintain two traits temporarily

#### Option B: Clean Break (v2.0)

**Announce breaking change:**
- v1.x uses current Provider
- v2.0 uses generic Provider
- Provide migration guide
- Auto-migration tool

**Steps:**
1. Release v1.9 with deprecation warnings
2. Release v2.0 with new Provider
3. Maintain v1.x LTS for 6 months

**Effort:** 2-3 hours (communications + versioning)

---

### Phase 5: Testing (Week 3-4)

#### Step 5.1: Port Existing Tests

All tests in `tests/sqlite_provider_test.rs` must pass with new implementation.

**Changes needed:**
- Update to use generic trait
- Test with adapter layer
- Verify atomicity still works

#### Step 5.2: New Generic Provider Tests

Test the generic traits directly:

```rust
#[tokio::test]
async fn test_event_log_generic() {
    let provider = GenericSqliteProvider::new_in_memory().await.unwrap();
    
    // Test generic event log (no Duroxide concepts)
    let events = vec![create_test_event(1), create_test_event(2)];
    provider.append_events("entity-1", 1, events).await.unwrap();
    
    let loaded = provider.read_events("entity-1", 1).await;
    assert_eq!(loaded.len(), 2);
}

#[tokio::test]
async fn test_message_queue_generic() {
    let provider = GenericSqliteProvider::new_in_memory().await.unwrap();
    
    // Test generic queue (no WorkItem knowledge)
    let message = b"test message".to_vec();
    provider.enqueue("test-queue", message.clone(), None).await.unwrap();
    
    let (dequeued, token) = provider.dequeue("test-queue").await.unwrap();
    assert_eq!(dequeued, message);
    
    provider.ack(&token).await.unwrap();
}
```

#### Step 5.3: Integration Tests

Verify Runtime works with generic Provider:

```rust
#[tokio::test]
async fn test_runtime_with_generic_provider() {
    let generic_provider = GenericSqliteProvider::new_in_memory().await.unwrap();
    
    // Runtime uses adapter internally
    let rt = Runtime::start_with_generic_provider(
        Arc::new(generic_provider),
        activities,
        orchestrations
    ).await;
    
    // Everything should work as before
    client.start_orchestration(...).await;
    // ...
}
```

**Effort:** 6-8 hours

---

## Implementation Checklist

### Week 1: Design & Prototyping
- [ ] Define EventLog trait
- [ ] Define MessageQueue trait
- [ ] Define MetadataStore trait
- [ ] Define AtomicOps trait
- [ ] Define Operation enum
- [ ] Define LockToken type
- [ ] Write comprehensive trait documentation
- [ ] Design ProviderAdapter interface

### Week 2: Implementation
- [ ] Implement GenericSqliteProvider (EventLog)
- [ ] Implement GenericSqliteProvider (MessageQueue)
- [ ] Implement GenericSqliteProvider (MetadataStore)
- [ ] Implement GenericSqliteProvider (AtomicOps)
- [ ] Create ProviderAdapter
- [ ] Implement adapter.fetch_orchestration_item()
- [ ] Implement adapter.ack_orchestration_item()
- [ ] Implement other adapter methods

### Week 3: Integration
- [ ] Update Runtime to use ProviderAdapter
- [ ] Update all call sites
- [ ] Update Client to work with generic Provider
- [ ] Decide on migration strategy (compat adapter vs clean break)
- [ ] Port existing tests
- [ ] Write new generic tests

### Week 4: Polish & Documentation
- [ ] Update all documentation
- [ ] Write migration guide
- [ ] Performance testing
- [ ] Benchmark comparison (old vs new)
- [ ] Fix any issues found
- [ ] Final integration testing

---

## Risk Mitigation

### Risk 1: Performance Regression

**Mitigation:**
- Benchmark before/after
- Profile hot paths
- Ensure indexes are equivalent
- Keep batching where beneficial

**Acceptance:** Within 10% of current performance

### Risk 2: Complexity in Adapter

**Mitigation:**
- Keep adapter logic simple and testable
- Document adapter thoroughly
- Unit test adapter separately
- Minimize state in adapter (stateless preferred)

### Risk 3: Breaking Existing Code

**Mitigation:**
- Provide backward compatibility adapter
- OR: Clear v2.0 release with migration guide
- Maintain v1.x LTS branch
- Automated migration tool

### Risk 4: Harder for Provider Implementers

**Mitigation:**
- Provide complete generic Provider implementation (SQLite)
- Write detailed documentation for generic traits
- Provide scaffolding/template
- Show how to implement for Redis, PostgreSQL

---

## Benefits Analysis

### Abstraction Quality

**Before:**
- Provider knows about orchestrations, executions, WorkItem types
- Tightly coupled to Duroxide
- Hard to reuse for other systems

**After:**
- Provider is pure storage (events + queues + metadata)
- Zero Duroxide knowledge
- Reusable for any event-sourced system

### Interface Simplicity

**Before:** 18 methods, mixture of concerns

**After:** 
- EventLog: 3 methods
- MessageQueue: 4 methods
- MetadataStore: 2 methods
- AtomicOps: 1 method
- **Total: 10 methods** (vs 18)

### Extensibility

**Generic metadata store** allows future features without trait changes:
- Add new metadata fields (just new keys in HashMap)
- Custom provider-specific optimizations
- Support for new event types (opaque to provider)

### Reusability

The generic Provider could power:
- Duroxide orchestrations
- Event-sourced aggregates (DDD)
- Saga frameworks
- CQRS systems
- Workflow engines
- State machines

---

## Alternative: Hybrid Approach

Keep some Duroxide-specific conveniences:

```rust
/// Generic core
pub trait GenericProvider: EventLog + MessageQueue + AtomicOps {}

/// Duroxide-specific extensions
pub trait DuroxideProvider: GenericProvider {
    /// Convenience: fetch with Duroxide types
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        // Default implementation using generic methods + adapter logic
    }
    
    /// Convenience: ack with Duroxide types
    async fn ack_orchestration_item(...) -> Result<(), String> {
        // Default implementation using atomic_batch
    }
}

// Blanket impl
impl<T: GenericProvider> DuroxideProvider for T {}
```

**Benefit:** Best of both worlds
- Core is generic (5-7 methods)
- Conveniences provided via default implementations
- Provider implementers can override for optimization

---

## Recommended Path Forward

### Immediate (This Month)
1. ✅ Remove `create_new_execution()` (5 min)
2. ✅ Create ProviderDebug extension trait (30 min)

### Near-Term (Next 2-3 Months)
3. 🔄 Design generic interface (1 week)
4. 🔄 Prototype GenericSqliteProvider (1 week)
5. 🔄 Build ProviderAdapter (1 week)
6. 🔄 Test and validate (1 week)

### v2.0 Release
7. 🚀 Switch to generic Provider as default
8. 🚀 Provide legacy adapter for compatibility
9. 🚀 Update all documentation
10. 🚀 Announce breaking change with migration guide

---

## Success Metrics

### Must Achieve
- ✅ All 186 tests still pass
- ✅ Performance within 10% of current
- ✅ Zero Duroxide concepts in Provider trait
- ✅ Can implement Provider without Duroxide knowledge

### Nice to Have
- ✅ Performance improvement (better indexes, less overhead)
- ✅ Simpler provider implementations
- ✅ Reusable for other event-sourced systems
- ✅ < 10 core trait methods

---

## Comparison to Current State

| Aspect | Current Provider | Generic Event Store |
|--------|------------------|---------------------|
| **Methods** | 18 | 10 (4 traits × ~2.5 avg) |
| **Duroxide Knowledge** | High (WorkItem, Execution, etc.) | Zero |
| **Abstraction Quality** | Leaky (inspects events, knows orch concepts) | Perfect (pure storage) |
| **Reusability** | Duroxide-only | Any event system |
| **Implementation Complexity** | Medium | Low (cleaner interface) |
| **Migration Effort** | N/A | High (breaking change) |
| **Runtime Complexity** | Low (direct calls) | Medium (adapter layer) |

---

## Detailed Migration Timeline

### Month 1: Design & Prototype
- Week 1: Design generic traits, write specs
- Week 2: Prototype GenericSqliteProvider
- Week 3: Build ProviderAdapter prototype
- Week 4: Validate with subset of tests

### Month 2: Implementation
- Week 1: Complete GenericSqliteProvider
- Week 2: Complete ProviderAdapter
- Week 3: Update Runtime to use adapter
- Week 4: Port all tests, fix issues

### Month 3: Polish & Release
- Week 1: Performance optimization
- Week 2: Documentation updates
- Week 3: Migration guide + tooling
- Week 4: Beta testing, v2.0 release

**Total:** 3 months for complete migration

---

## Code Size Estimate

| Component | Lines of Code |
|-----------|---------------|
| Generic trait definitions | ~300 |
| GenericSqliteProvider | ~600 |
| ProviderAdapter | ~400 |
| Runtime updates | ~200 |
| Test updates | ~300 |
| Documentation | ~800 |
| **Total new/modified** | **~2600 lines** |

---

## Open Questions

1. **Serialization:** Use bincode, serde_json, or pluggable codec?
2. **Metadata schema:** HashMap<String, String> or typed struct?
3. **Queue routing:** Provider decides or runtime specifies queue names?
4. **Lock tokens:** String or typed wrapper?
5. **Batching:** Keep instance batching or one-message-at-a-time?
6. **Backward compat:** Adapter or clean break?
7. **Multi-queue:** 3 tables or 1 table with queue_name column?

---

## Recommendation

**Start with 2-week spike:**
1. Design generic traits (2 days)
2. Prototype GenericSqliteProvider (3 days)
3. Build ProviderAdapter (3 days)
4. Run critical tests (2 days)

**Decision point:** If spike succeeds, commit to full migration. If issues found, iterate on design.

**My vote:** This redesign is worth doing for v2.0. It's the "right" abstraction and makes Duroxide significantly more elegant.

**Immediate action:** Do the quick wins first (remove create_new_execution, extension trait), then plan the generic redesign for v2.0.

---

**Want me to start with a working prototype of the generic interface?**




