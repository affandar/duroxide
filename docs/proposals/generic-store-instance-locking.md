# Instance Locking in Generic Event Store Design

**Critical Question:** How do we maintain instance-level locking with a generic message queue?

---

## Current Design: Instance-Level Batching

### How It Works Now

```rust
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    // 1. Find first available message
    let instance_id = SELECT instance_id FROM orchestrator_queue WHERE unlocked LIMIT 1
    
    // 2. Lock ALL messages for that instance
    UPDATE orchestrator_queue SET lock_token = ?
    WHERE instance_id = instance_id AND unlocked
    
    // 3. Fetch all locked messages
    let messages = SELECT * FROM orchestrator_queue WHERE lock_token = ?
    
    // Result: Batch of messages for ONE instance, all locked together
}
```

**Key Properties:**
- ✅ Instance-level atomicity (all messages for instance processed together)
- ✅ Prevents concurrent processing of same instance
- ✅ Batching is efficient (fewer round-trips)

**Why This Matters:**
- Messages for an instance must be processed in order
- Can't have two dispatchers processing the same instance concurrently
- Batching reduces orchestration turns (one turn handles multiple completions)

---

## Challenge with Generic Queue

### Naive Generic Approach (Broken)

```rust
trait MessageQueue {
    async fn dequeue(&self, queue: &str) -> Option<(Vec<u8>, LockToken)>;
    // ↑ Returns ONE message, not a batch
}

// Problem: How to lock multiple messages for same instance?
```

**Issues:**
1. ❌ Can't batch messages by instance (returns one message)
2. ❌ Can't prevent concurrent processing of instance (no instance lock)
3. ❌ Multiple dequeues could interleave messages from different instances

**Example Failure:**
```
Dispatcher 1: dequeue() → Message A for instance "order-1"
Dispatcher 2: dequeue() → Message B for instance "order-1"  // ❌ CONCURRENT!
Both process order-1 simultaneously → RACE CONDITION
```

---

## Solution Options

### Option 1: Add Instance-Level Locking to Generic Interface

**Extend MessageQueue with instance batching:**

```rust
trait MessageQueue {
    // Single-message dequeue (for worker/timer queues - no batching needed)
    async fn dequeue(&self, queue: &str) -> Option<(Vec<u8>, LockToken)>;
    
    // Batch dequeue by routing key (for orchestrator queue - instance batching)
    async fn dequeue_batch(
        &self,
        queue: &str,
        routing_key_extractor: fn(&[u8]) -> Option<String>,
    ) -> Option<(Vec<Vec<u8>>, LockToken)>;
    
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String>;
    async fn abandon(&self, lock_token: &LockToken, delay_ms: Option<u64>) -> Result<(), String>;
}

// Usage by adapter:
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    let (message_batch, lock_token) = self.provider.dequeue_batch(
        "orchestrator",
        |msg_bytes| {
            // Extract instance_id from message
            let envelope: MessageEnvelope = bincode::deserialize(msg_bytes).ok()?;
            Some(envelope.instance_id)
        }
    ).await?;
    
    // All messages in batch belong to same instance
    // ...
}
```

**Implementation:**
```rust
async fn dequeue_batch(&self, queue: &str, extract_key: fn(&[u8]) -> Option<String>) -> Option<(Vec<Vec<u8>>, LockToken)> {
    let tx = begin_transaction();
    
    // 1. Find first available message
    let first = SELECT id, message_data FROM message_queue
                WHERE queue_name = ? AND unlocked
                ORDER BY id LIMIT 1;
    
    let routing_key = extract_key(&first.message_data)?;
    
    // 2. Lock ALL messages with same routing key
    let lock_token = generate_uuid();
    UPDATE message_queue SET lock_token = ?, locked_until = now() + 30s
    WHERE queue_name = ? AND routing_key_extracted = ?;
    
    // Problem: Can't extract routing key in SQL!
    // Need to store routing key explicitly
}
```

**Problem:** SQL can't call Rust function to extract routing key.

**Solution:** Store routing key in database:

```sql
CREATE TABLE message_queue (
    id INTEGER PRIMARY KEY,
    queue_name TEXT NOT NULL,
    routing_key TEXT,          -- ← Store instance_id explicitly
    message_data BLOB NOT NULL,
    visible_at INTEGER,
    lock_token TEXT,
    locked_until INTEGER
);

CREATE INDEX idx_queue_routing ON message_queue(queue_name, routing_key, visible_at, lock_token);
```

```rust
async fn enqueue(&self, queue: &str, routing_key: Option<&str>, message: Vec<u8>, ...) {
    INSERT INTO message_queue (queue_name, routing_key, message_data, ...)
    VALUES (?, ?, ?, ...)
}

async fn dequeue_batch(&self, queue: &str) -> Option<(String, Vec<Vec<u8>>, LockToken)> {
    // 1. Find first available routing key
    let routing_key = SELECT routing_key FROM message_queue
                      WHERE queue_name = ? AND unlocked
                      ORDER BY id LIMIT 1;
    
    // 2. Lock all messages for that routing key
    let lock_token = generate_uuid();
    UPDATE message_queue SET lock_token = ?, locked_until = ?
    WHERE queue_name = ? AND routing_key = ? AND unlocked;
    
    // 3. Fetch all locked messages
    let messages = SELECT message_data FROM message_queue WHERE lock_token = ?;
    
    Some((routing_key, messages, lock_token))
}
```

**Pros:**
- ✅ Maintains instance-level batching
- ✅ Generic (routing_key can be anything)
- ✅ Efficient (SQL-level filtering)

**Cons:**
- ⚠️ Routing key must be stored explicitly (8-16 extra bytes per message)
- ⚠️ Provider must know about routing concept (less generic)

---

### Option 2: Runtime-Level Instance Locking (Coordinator Pattern)

**Move instance locking OUT of Provider, into Runtime:**

```rust
// Generic Provider (no batching, no instance knowledge)
trait MessageQueue {
    async fn dequeue(&self, queue: &str) -> Option<(Vec<u8>, LockToken)>;
    // Returns ONE message
}

// Runtime maintains instance locks
struct Runtime {
    provider: Arc<dyn Provider>,
    instance_locks: Mutex<HashMap<String, InstanceLock>>,  // In-memory locks
}

struct InstanceLock {
    locked_by: WorkerId,
    locked_until: Instant,
}

impl Runtime {
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        loop {
            // 1. Dequeue one message
            let (message_bytes, lock_token) = self.provider.dequeue("orchestrator").await?;
            let envelope: MessageEnvelope = bincode::deserialize(&message_bytes).ok()?;
            let instance = envelope.instance_id;
            
            // 2. Try to acquire instance lock (in-memory)
            let mut locks = self.instance_locks.lock().await;
            if let Some(existing_lock) = locks.get(&instance) {
                if existing_lock.locked_until > Instant::now() {
                    // Instance is locked by another dispatcher
                    // Put message back (abandon with small delay)
                    self.provider.abandon(&lock_token, Some(100)).await;
                    continue;  // Try next message
                }
            }
            
            // Acquire lock
            locks.insert(instance.clone(), InstanceLock {
                locked_by: current_worker_id(),
                locked_until: Instant::now() + Duration::from_secs(30),
            });
            drop(locks);
            
            // 3. Fetch additional messages for this instance (batching)
            let mut messages = vec![envelope];
            while let Some((msg_bytes, token)) = self.provider.dequeue("orchestrator").await {
                let env: MessageEnvelope = bincode::deserialize(&msg_bytes).ok()?;
                if env.instance_id == instance {
                    messages.push(env);
                    // Ack immediately (we own the instance lock)
                    self.provider.ack(&token).await;
                } else {
                    // Different instance - put back
                    self.provider.abandon(&token, None).await;
                    break;
                }
            }
            
            // 4. Load history and return
            let partition = self.provider.latest_partition(&instance).await.unwrap_or(1);
            let history = self.provider.read_events(&instance, partition).await;
            
            return Some(OrchestrationItem {
                instance,
                history,
                messages: messages.into_iter().map(|e| e.payload).collect(),
                lock_token,
            });
        }
    }
}
```

**Pros:**
- ✅ Provider is completely generic (no routing_key concept)
- ✅ Instance batching maintained
- ✅ Flexible (runtime can change batching strategy)

**Cons:**
- ❌ Complex runtime logic (multiple dequeues, abandons)
- ❌ In-memory locks don't survive runtime crashes
- ❌ Less efficient (multiple roundtrips)
- ❌ Lock coordination issues across multiple runtime instances

**Verdict:** Workable for single-runtime deployments, problematic for distributed

---

### Option 3: Two-Level Queue Design (Hybrid)

**Separate concerns: routing vs locking**

```rust
trait MessageQueue {
    // Low-level: enqueue with routing metadata
    async fn enqueue(&self, queue: &str, routing_key: Option<&str>, message: Vec<u8>, ...) -> Result<(), String>;
    
    // High-level: dequeue batch by routing key
    async fn dequeue_batch(&self, queue: &str) -> Option<RoutedBatch>;
    
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String>;
}

struct RoutedBatch {
    routing_key: Option<String>,  // Some("order-123") for orchestrator, None for worker
    messages: Vec<Vec<u8>>,
    lock_token: LockToken,
}

// Provider implementation:
async fn dequeue_batch(&self, queue: &str) -> Option<RoutedBatch> {
    if queue == "orchestrator" {
        // Batch by routing_key (instance-level locking)
        self.dequeue_batch_by_routing_key(queue).await
    } else {
        // No batching (worker/timer queues process one at a time)
        let (message, token) = self.dequeue_single(queue).await?;
        Some(RoutedBatch {
            routing_key: None,
            messages: vec![message],
            lock_token: token,
        })
    }
}
```

**Pros:**
- ✅ Generic interface (routing_key is optional, queue-dependent)
- ✅ Instance batching when needed (orchestrator queue)
- ✅ Simple for worker/timer queues (no batching)
- ✅ Provider-level locking (survives crashes)

**Cons:**
- ⚠️ Slightly more complex than pure generic
- ⚠️ Provider must store routing_key (extra field)

---

### Option 4: Entity-Level Queue Partitioning

**Store separate queue per instance (avoid batching complexity):**

```rust
trait MessageQueue {
    // Enqueue to entity-specific queue
    async fn enqueue_for_entity(&self, queue: &str, entity: &str, message: Vec<u8>, ...) -> Result<(), String>;
    
    // Dequeue from any entity (provider picks)
    async fn dequeue_any_entity(&self, queue: &str) -> Option<EntityBatch>;
    
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String>;
}

struct EntityBatch {
    entity: String,           // instance_id
    messages: Vec<Vec<u8>>,   // All messages for this entity
    lock_token: LockToken,
}
```

**Implementation:**

```sql
-- Separate queue per entity
CREATE TABLE entity_queues (
    entity_id TEXT NOT NULL,
    queue_name TEXT NOT NULL,
    message_id INTEGER NOT NULL,
    message_data BLOB NOT NULL,
    visible_at INTEGER,
    lock_token TEXT,
    locked_until INTEGER,
    PRIMARY KEY (entity_id, queue_name, message_id)
);

-- Index to find next entity with work
CREATE INDEX idx_entity_queue_next ON entity_queues(queue_name, visible_at, lock_token);
```

```rust
async fn dequeue_any_entity(&self, queue: &str) -> Option<EntityBatch> {
    let tx = begin_transaction();
    
    // Find first entity with unlocked messages
    let entity_id = SELECT DISTINCT entity_id FROM entity_queues
                    WHERE queue_name = ? AND visible_at <= ? AND unlocked
                    ORDER BY visible_at LIMIT 1;
    
    // Lock ALL messages for this entity
    let lock_token = generate_uuid();
    UPDATE entity_queues SET lock_token = ?, locked_until = ?
    WHERE entity_id = ? AND queue_name = ? AND unlocked;
    
    // Fetch all messages
    let messages = SELECT message_data FROM entity_queues 
                   WHERE lock_token = ?
                   ORDER BY message_id;
    
    commit_transaction();
    Some(EntityBatch { entity: entity_id, messages, lock_token })
}
```

**Pros:**
- ✅ Natural entity-level batching (built into schema)
- ✅ Instance locking is implicit (per-entity partitioning)
- ✅ Clean generic interface

**Cons:**
- ⚠️ Different schema (entity-partitioned vs global queue)
- ⚠️ Provider must partition queues by entity (not fully generic)
- ⚠️ More complex schema

---

## Recommended Approach: Option 3 (Hybrid with Routing Key)

### Detailed Design

#### Generic Interface with Routing Support

```rust
/// Message queue with optional routing for batching.
#[async_trait::async_trait]
pub trait MessageQueue: Send + Sync {
    /// Enqueue a message with optional routing key.
    /// 
    /// # Routing Key
    /// 
    /// - `Some(key)`: Messages with same key can be batched together
    /// - `None`: Messages processed independently (no batching)
    /// 
    /// **Use cases:**
    /// - Orchestrator queue: routing_key = instance_id (batch by instance)
    /// - Worker queue: routing_key = None (process independently)
    /// - Timer queue: routing_key = None (process independently)
    /// 
    /// # Example
    /// 
    /// ```ignore
    /// // Orchestrator message (batch by instance)
    /// queue.enqueue("orchestrator", Some("order-123"), message_bytes, None).await;
    /// 
    /// // Worker message (no batching)
    /// queue.enqueue("worker", None, message_bytes, None).await;
    /// ```
    async fn enqueue(
        &self,
        queue: &str,
        routing_key: Option<&str>,
        message: Vec<u8>,
        visible_after_ms: Option<u64>,
    ) -> Result<(), String>;
    
    /// Dequeue a batch of messages with optional routing.
    /// 
    /// # Behavior
    /// 
    /// If routing keys are used in this queue:
    /// - Returns ALL messages for ONE routing key
    /// - Locks ALL those messages together (instance-level lock)
    /// 
    /// If routing keys are not used:
    /// - Returns single message
    /// 
    /// # Return Value
    /// 
    /// - `routing_key`: The key for batched messages (Some for orchestrator, None for worker/timer)
    /// - `messages`: One or more messages (batch for orchestrator, single for worker)
    /// - `lock_token`: Token to ack/abandon the batch
    async fn dequeue_batch(
        &self,
        queue: &str,
    ) -> Option<MessageBatch>;
    
    /// Acknowledge message(s) - deletes from queue.
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String>;
    
    /// Abandon message(s) - release lock for retry.
    async fn abandon(&self, lock_token: &LockToken, delay_ms: Option<u64>) -> Result<(), String>;
}

/// Batch of messages dequeued together.
#[derive(Debug)]
pub struct MessageBatch {
    /// Routing key (Some if batched by key, None if single message)
    pub routing_key: Option<String>,
    
    /// Messages in the batch (1+ messages)
    pub messages: Vec<Vec<u8>>,
    
    /// Lock token for this batch
    pub lock_token: LockToken,
}
```

#### Database Schema

```sql
CREATE TABLE message_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_name TEXT NOT NULL,
    routing_key TEXT,              -- NULL for worker/timer, instance_id for orchestrator
    message_data BLOB NOT NULL,
    visible_at INTEGER NOT NULL,
    lock_token TEXT,
    locked_until INTEGER,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Composite index for efficient batching
CREATE INDEX idx_queue_batch ON message_queue(queue_name, routing_key, visible_at, lock_token);

-- Simple index for non-batched queues
CREATE INDEX idx_queue_simple ON message_queue(queue_name, visible_at, lock_token) WHERE routing_key IS NULL;
```

#### Implementation

```rust
impl MessageQueue for GenericSqliteProvider {
    async fn enqueue(
        &self,
        queue: &str,
        routing_key: Option<&str>,
        message: Vec<u8>,
        visible_after_ms: Option<u64>,
    ) -> Result<(), String> {
        let visible_at = current_timestamp_ms() + visible_after_ms.unwrap_or(0);
        
        sqlx::query(
            "INSERT INTO message_queue (queue_name, routing_key, message_data, visible_at, lock_token, locked_until)
             VALUES (?, ?, ?, ?, NULL, NULL)"
        )
        .bind(queue)
        .bind(routing_key)
        .bind(message)
        .bind(visible_at as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        
        Ok(())
    }
    
    async fn dequeue_batch(&self, queue: &str) -> Option<MessageBatch> {
        let mut tx = self.pool.begin().await.ok()?;
        let now = current_timestamp_ms();
        
        // Find first available message (might have routing_key or not)
        let first_row = sqlx::query(
            "SELECT id, routing_key, message_data FROM message_queue
             WHERE queue_name = ? 
               AND visible_at <= ?
               AND (lock_token IS NULL OR locked_until <= ?)
             ORDER BY id LIMIT 1"
        )
        .bind(queue)
        .bind(now as i64)
        .bind(now as i64)
        .fetch_optional(&mut *tx)
        .await
        .ok()??;
        
        let first_id: i64 = first_row.try_get("id").ok()?;
        let routing_key: Option<String> = first_row.try_get("routing_key").ok()?;
        
        let lock_token = LockToken(uuid::Uuid::new_v4().to_string());
        let locked_until = now + 30_000;
        
        if let Some(ref key) = routing_key {
            // Batch mode: lock all messages with same routing key
            sqlx::query(
                "UPDATE message_queue 
                 SET lock_token = ?, locked_until = ?
                 WHERE queue_name = ? AND routing_key = ?
                   AND visible_at <= ?
                   AND (lock_token IS NULL OR locked_until <= ?)"
            )
            .bind(&lock_token.0)
            .bind(locked_until as i64)
            .bind(queue)
            .bind(key)
            .bind(now as i64)
            .bind(now as i64)
            .execute(&mut *tx)
            .await
            .ok()?;
        } else {
            // Single mode: lock just this message
            sqlx::query(
                "UPDATE message_queue SET lock_token = ?, locked_until = ? WHERE id = ?"
            )
            .bind(&lock_token.0)
            .bind(locked_until as i64)
            .bind(first_id)
            .execute(&mut *tx)
            .await
            .ok()?;
        }
        
        // Fetch all locked messages
        let rows = sqlx::query("SELECT message_data FROM message_queue WHERE lock_token = ? ORDER BY id")
            .bind(&lock_token.0)
            .fetch_all(&mut *tx)
            .await
            .ok()?;
        
        let messages: Vec<Vec<u8>> = rows.into_iter()
            .filter_map(|row| row.try_get("message_data").ok())
            .collect();
        
        tx.commit().await.ok()?;
        
        Some(MessageBatch {
            routing_key,
            messages,
            lock_token,
        })
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
            "UPDATE message_queue SET lock_token = NULL, locked_until = NULL, visible_at = ?
             WHERE lock_token = ?"
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

#### Runtime Adapter Usage

```rust
impl ProviderAdapter {
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        let batch = self.provider.dequeue_batch("orchestrator").await?;
        
        let instance = batch.routing_key.expect("orchestrator queue must have routing key");
        
        // Deserialize messages
        let messages: Vec<WorkItem> = batch.messages.iter()
            .filter_map(|bytes| bincode::deserialize(bytes).ok())
            .collect();
        
        // Load history
        let partition = self.provider.latest_partition(&instance).await.unwrap_or(1);
        let history = self.provider.read_events(&instance, partition).await;
        
        // Extract metadata from history
        let (orch_name, version) = extract_from_history(&history);
        
        Some(OrchestrationItem {
            instance,
            orchestration_name: orch_name,
            execution_id: partition,
            version,
            history,
            messages,
            lock_token: batch.lock_token,
        })
    }
    
    async fn enqueue_orchestrator_work(&self, item: WorkItem) -> Result<(), String> {
        let instance = extract_instance(&item);  // Helper function
        let message_bytes = bincode::serialize(&item).unwrap();
        
        self.provider.enqueue(
            "orchestrator",
            Some(&instance),  // ← Routing key for batching
            message_bytes,
            None
        ).await
    }
    
    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        let message_bytes = bincode::serialize(&item).unwrap();
        
        self.provider.enqueue(
            "worker",
            None,  // ← No routing key (process independently)
            message_bytes,
            None
        ).await
    }
}
```

---

## Comparison of Options

| Approach | Provider Complexity | Runtime Complexity | Correctness | Performance |
|----------|--------------------|--------------------|-------------|-------------|
| **Option 1: Routing in Generic API** | Medium (store routing_key) | Low | ✅ Correct | ✅ Excellent |
| **Option 2: Runtime Locks** | Low (pure generic) | High (coordination) | ⚠️ Single runtime only | ⚠️ Multiple roundtrips |
| **Option 3: Hybrid (Recommended)** | Medium (routing_key) | Low-Medium | ✅ Correct | ✅ Excellent |
| **Option 4: Entity Queues** | High (partitioned schema) | Low | ✅ Correct | ✅ Good |

---

## Recommended Solution: Option 3 (Hybrid)

### Final Interface Design

```rust
#[async_trait::async_trait]
pub trait MessageQueue: Send + Sync {
    /// Enqueue a message with optional routing for batching.
    /// 
    /// # Routing Key
    /// 
    /// Messages with the same routing_key can be batched together during dequeue.
    /// This enables instance-level locking for orchestrator queues.
    /// 
    /// - Orchestrator queue: routing_key = Some(instance_id)
    /// - Worker queue: routing_key = None
    /// - Timer queue: routing_key = None
    async fn enqueue(
        &self,
        queue: &str,
        routing_key: Option<&str>,
        message: Vec<u8>,
        visible_after_ms: Option<u64>,
    ) -> Result<(), String>;
    
    /// Dequeue a batch of messages.
    /// 
    /// # Batching Behavior
    /// 
    /// - If queue uses routing keys: Returns ALL messages for ONE routing key
    /// - If queue doesn't use routing keys: Returns single message
    /// 
    /// The provider determines batching strategy based on whether messages have routing_key.
    async fn dequeue_batch(&self, queue: &str) -> Option<MessageBatch>;
    
    /// Acknowledge batch - deletes all messages in the batch.
    async fn ack(&self, lock_token: &LockToken) -> Result<(), String>;
    
    /// Abandon batch - releases lock for retry.
    async fn abandon(&self, lock_token: &LockToken, delay_ms: Option<u64>) -> Result<(), String>;
}

pub struct MessageBatch {
    /// Routing key if batched (Some for orchestrator), None for worker/timer
    pub routing_key: Option<String>,
    
    /// Messages in batch (1+ messages)
    pub messages: Vec<Vec<u8>>,
    
    /// Lock token for entire batch
    pub lock_token: LockToken,
}
```

### Why This Works

1. **Instance-level locking preserved:**
   - Orchestrator messages enqueued with routing_key = instance_id
   - dequeue_batch() returns all messages for one instance
   - Lock prevents concurrent processing

2. **Generic enough:**
   - Routing key is optional (generic batching concept)
   - Not Duroxide-specific (could batch by user_id, order_id, etc.)
   - Worker/timer queues don't use routing (routing_key = None)

3. **Efficient:**
   - Single SQL query locks all messages for instance
   - Batching reduces orchestration turns
   - Index supports fast routing key lookup

4. **Correct:**
   - Atomicity maintained (lock entire batch)
   - No race conditions (database-level locking)
   - Survives crashes (lock timeout in DB)

---

## Implementation Details

### Schema

```sql
CREATE TABLE message_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_name TEXT NOT NULL,
    routing_key TEXT,              -- For batching (instance_id, user_id, etc.)
    message_data BLOB NOT NULL,    -- Serialized message (opaque)
    visible_at INTEGER NOT NULL,   -- Milliseconds since epoch
    lock_token TEXT,               -- NULL = unlocked
    locked_until INTEGER,          -- Lock expiration
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Index for batched queues (with routing_key)
CREATE INDEX idx_queue_routed ON message_queue(queue_name, routing_key, visible_at, lock_token) 
WHERE routing_key IS NOT NULL;

-- Index for non-batched queues (without routing_key)
CREATE INDEX idx_queue_simple ON message_queue(queue_name, visible_at, lock_token) 
WHERE routing_key IS NULL;
```

### Dequeue Algorithm

```sql
-- Step 1: Find first available routing_key (or message if no routing)
WITH next_item AS (
    SELECT DISTINCT routing_key, MIN(id) as first_id
    FROM message_queue
    WHERE queue_name = 'orchestrator'
      AND visible_at <= ?
      AND (lock_token IS NULL OR locked_until <= ?)
    GROUP BY routing_key
    ORDER BY first_id LIMIT 1
)
-- Step 2: Lock all messages for that routing_key
UPDATE message_queue
SET lock_token = ?, locked_until = ?
WHERE queue_name = 'orchestrator'
  AND routing_key = (SELECT routing_key FROM next_item)
  AND (lock_token IS NULL OR locked_until <= ?);

-- Step 3: Fetch all locked messages
SELECT message_data FROM message_queue WHERE lock_token = ? ORDER BY id;
```

### Runtime Adapter

```rust
async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
    let batch = self.provider.dequeue_batch("orchestrator").await?;
    
    // Instance is the routing key
    let instance = batch.routing_key.expect("orchestrator queue must have routing_key");
    
    // Deserialize messages
    let messages: Vec<WorkItem> = batch.messages.iter()
        .filter_map(|bytes| bincode::deserialize(bytes).ok())
        .collect();
    
    // Load history and metadata
    let partition = self.provider.latest_partition(&instance).await.unwrap_or(1);
    let history = self.provider.read_events(&instance, partition).await;
    
    // Build OrchestrationItem
    Some(OrchestrationItem {
        instance,
        history,
        messages,
        lock_token: batch.lock_token,
        // ... extract orch_name, version from history
    })
}
```

---

## Verdict: Hybrid Approach (Option 3)

**Pros:**
- ✅ Instance-level locking works (database-enforced)
- ✅ Generic interface (routing_key is generic concept)
- ✅ Efficient (single query for batch)
- ✅ Correct (atomic, crash-safe)
- ✅ Flexible (routing can be anything, not just instance_id)

**Cons:**
- ⚠️ Slightly more complex than pure generic (routing_key parameter)
- ⚠️ 8-16 bytes overhead per message (routing_key storage)

**Trade-off:** Worth it for correctness and performance

---

## Alternative: Simplify Further (No Batching)

**If we're willing to give up batching:**

```rust
trait MessageQueue {
    async fn enqueue(&self, queue: &str, message: Vec<u8>, visible_after_ms: Option<u64>);
    async fn dequeue(&self, queue: &str) -> Option<(Vec<u8>, LockToken)>;
    async fn ack(&self, lock_token: &LockToken);
}

// Process ONE message per orchestration turn
// Instance "locking" via message ordering only
```

**Impact:**
- More orchestration turns (one message = one turn)
- Still correct (message ordering ensures instance consistency)
- Simpler provider interface
- May reduce performance (more turns = more DB roundtrips)

**Analysis:** Batching is an optimization, not required for correctness. Could work!

---

## My Recommendation

**Use Option 3 (Hybrid with Routing Key):**

1. Generic enough (routing_key is broadly applicable)
2. Preserves instance-level locking (critical for performance)
3. Backwards compatible in behavior (still batches)
4. Clean interface (4 methods for MessageQueue)

**Interface:**
```rust
trait MessageQueue {
    enqueue(queue, routing_key, message, delay_ms)
    dequeue_batch(queue) -> MessageBatch { routing_key, messages, lock_token }
    ack(lock_token)
    abandon(lock_token, delay_ms)
}
```

**This maintains correctness while achieving generic abstraction!**



