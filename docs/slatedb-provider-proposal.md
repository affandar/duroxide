# SlateDB Provider Implementation Proposal

**Version:** 1.0  
**Date:** 2025-11-23  
**Status:** Proposal  
**Author:** AI Assistant  

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Background & Motivation](#2-background--motivation)
3. [SlateDB Overview](#3-slatedb-overview)
4. [Architecture Design](#4-architecture-design)
5. [Key-Value Schema Design](#5-key-value-schema-design)
6. [Implementation Details](#6-implementation-details)
7. [Transaction Strategy](#7-transaction-strategy)
8. [Performance Considerations](#8-performance-considerations)
9. [Testing Strategy](#9-testing-strategy)
10. [Implementation Roadmap](#10-implementation-roadmap)
11. [Risk Analysis](#11-risk-analysis)
12. [Success Criteria](#12-success-criteria)
13. [References](#13-references)

---

## 1. Executive Summary

This proposal outlines the design and implementation of a **SlateDB-backed Provider** for Duroxide, enabling cloud-native, distributed orchestration execution with object storage backends (S3, GCS, Azure Blob Storage, etc.).

### Key Benefits

- **Cloud-native architecture**: Direct integration with object storage without intermediate databases
- **Horizontal scalability**: Multiple workers across regions can share state via object storage
- **Cost-efficient**: Leverage inexpensive object storage instead of managed databases
- **Embedded deployment**: No separate database process or infrastructure to manage
- **Multi-region capable**: Built-in support for object storage replication and failover
- **Serverless-friendly**: Ideal for AWS Lambda, Cloud Functions, and similar environments

### Project Scope

- **Estimated Effort**: 4-5 weeks (full-time equivalent)
- **Complexity**: Medium-High (key-value schema mapping, custom locking)
- **Dependencies**: SlateDB 0.2+, standard Rust async ecosystem
- **Deliverable**: Production-ready provider passing all 45 validation tests

---

## 2. Background & Motivation

### 2.1 Current State

Duroxide currently supports a **SQLite provider** (`SqliteProvider`) which offers:
- ✅ Full ACID transactions
- ✅ SQL-based queries and indexes
- ✅ Mature, battle-tested storage engine
- ❌ Limited to single-node deployments (WAL mode helps but doesn't enable true distributed access)
- ❌ Requires file system access (challenging in serverless environments)
- ❌ No native cloud storage integration

### 2.2 Use Cases for SlateDB Provider

1. **Multi-Region Workflows**: Orchestrations that need to survive regional failures
2. **Serverless Deployments**: Lambda/Cloud Functions without persistent file systems
3. **Elastic Workloads**: Auto-scaling workers that share state via object storage
4. **Cost Optimization**: Replace expensive managed databases with cheap object storage
5. **Data Locality**: Process data where it lives (near S3 buckets, etc.)

### 2.3 Why SlateDB?

SlateDB is a Rust-native, embedded key-value store specifically designed for cloud storage:
- **LSM Tree Architecture**: Optimized for append-heavy workloads (perfect for event logs)
- **Object Storage Backend**: First-class support for S3, GCS, Azure Blob
- **Async/Await**: Built on Tokio, natural fit for Duroxide
- **Transactions**: Supports batch writes for atomic operations
- **Active Development**: Modern, well-maintained project

---

## 3. SlateDB Overview

### 3.1 Core Characteristics

```
┌─────────────────────────────────────────────────────────┐
│                    Application                          │
│                  (Duroxide Runtime)                     │
└────────────────┬────────────────────────────────────────┘
                 │
                 │ SlateDB API (Rust)
                 ▼
┌─────────────────────────────────────────────────────────┐
│                     SlateDB                             │
│  ┌────────────┐  ┌──────────┐  ┌──────────────────┐    │
│  │MemTable   │→ │WAL (S3)  │→ │SSTable (S3)      │    │
│  │(in-memory)│  │(durable) │  │(sorted, immut.)  │    │
│  └────────────┘  └──────────┘  └──────────────────┘    │
│                                                         │
│  Compaction, Bloom Filters, Block Cache                │
└────────────────┬────────────────────────────────────────┘
                 │
                 │ Object Storage Protocol
                 ▼
┌─────────────────────────────────────────────────────────┐
│          Object Storage (S3, GCS, Azure)                │
│  - Immutable objects (SSTables)                         │
│  - Write-Ahead Log (durability)                         │
│  - Versioning & lifecycle policies                      │
└─────────────────────────────────────────────────────────┘
```

### 3.2 Relevant SlateDB Features

| Feature | Description | Duroxide Usage |
|---------|-------------|----------------|
| `put(key, value)` | Write a key-value pair | Store events, metadata, queue items |
| `get(key)` | Read a value by key | Fetch instance metadata, locks |
| `range(start..end)` | Scan keys in range | Fetch queue items by timestamp |
| `WriteBatch` | Atomic multi-operation commit | Implement atomic `ack_orchestration_item` |
| `compare_and_swap` | Optimistic locking primitive | Instance-level lock acquisition |
| `delete(key)` | Remove a key | Delete acknowledged messages |

### 3.3 Constraints & Limitations

**What SlateDB Provides:**
- ✅ Key-value operations (put, get, delete)
- ✅ Range scans (lexicographic ordering)
- ✅ Batch writes (atomic commits)
- ✅ Compare-and-swap (CAS) for optimistic locking

**What SlateDB Does NOT Provide:**
- ❌ SQL queries or relational operations
- ❌ Secondary indexes (must build manually via key patterns)
- ❌ Row-level pessimistic locks (must implement via CAS)
- ❌ Complex transaction semantics (no savepoints, etc.)

**Implications for Implementation:**
- Must design key schema carefully for efficient range scans
- Must implement queue semantics using sorted keys
- Must build custom locking mechanisms using CAS primitives
- Must handle consistency at application level

---

## 4. Architecture Design

### 4.1 High-Level Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Duroxide Runtime                         │
│  ┌──────────────────────┐    ┌─────────────────────────┐   │
│  │ Orchestration        │    │ Worker                  │   │
│  │ Dispatcher           │    │ Dispatcher              │   │
│  └──────────┬───────────┘    └──────────┬──────────────┘   │
│             │                            │                  │
│             │  Provider Trait Interface  │                  │
└─────────────┼────────────────────────────┼──────────────────┘
              │                            │
              ▼                            ▼
┌─────────────────────────────────────────────────────────────┐
│                   SlateDBProvider                           │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Key Encoding/Decoding Layer                          │   │
│  │  - Hierarchical key prefixes                         │   │
│  │  - Zero-padded integers for sorting                  │   │
│  │  - UUID generation for uniqueness                    │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Transaction Coordinator                              │   │
│  │  - WriteBatch accumulation                           │   │
│  │  - CAS-based optimistic locking                      │   │
│  │  - Conflict detection & retry                        │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Queue Manager                                        │   │
│  │  - Orchestrator queue (time-sorted)                  │   │
│  │  - Worker queue (FIFO)                               │   │
│  │  - Lock management & expiration                      │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ History Manager                                      │   │
│  │  - Append-only event log                             │   │
│  │  - Multi-execution support                           │   │
│  │  - Efficient range scans                             │   │
│  └──────────────────────────────────────────────────────┘   │
└────────────────────────┬────────────────────────────────────┘
                         │ SlateDB API
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                        SlateDB                              │
│  (LSM Tree + WAL + Compaction + Bloom Filters)              │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│              Object Storage (S3/GCS/Azure)                  │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 Component Responsibilities

#### 4.2.1 Key Encoding Layer

**Purpose**: Map relational concepts to hierarchical key-value schema.

**Responsibilities**:
- Encode/decode keys with zero-padded integers for sorting
- Generate UUIDs for uniqueness
- Provide type-safe key construction methods
- Handle key prefix patterns for range scans

**Example**:
```rust
pub struct KeyEncoder;

impl KeyEncoder {
    pub fn instance_metadata(instance_id: &str) -> String {
        format!("meta:inst:{}", instance_id)
    }
    
    pub fn history_event(instance_id: &str, exec_id: u64, event_id: u64) -> String {
        format!("hist:{}:{}:{:016}", instance_id, exec_id, event_id)
    }
    
    pub fn orchestrator_queue_item(visible_at_ms: i64) -> String {
        format!("qo:{:016}:{}", visible_at_ms, uuid::Uuid::new_v4())
    }
}
```

#### 4.2.2 Transaction Coordinator

**Purpose**: Ensure atomic operations across multiple key-value pairs.

**Responsibilities**:
- Accumulate operations in `WriteBatch`
- Coordinate CAS-based locking
- Retry on conflicts
- Validate preconditions before commit

**Example**:
```rust
pub struct TxnCoordinator<'a> {
    db: &'a SlateDB,
    batch: WriteBatch,
    locks_held: Vec<String>,
}

impl<'a> TxnCoordinator<'a> {
    pub fn put(&mut self, key: String, value: Vec<u8>) {
        self.batch.put(key, value);
    }
    
    pub fn delete(&mut self, key: String) {
        self.batch.delete(key);
    }
    
    pub async fn commit(self) -> Result<(), ProviderError> {
        self.db.write(self.batch).await?;
        // Release locks...
        Ok(())
    }
}
```

#### 4.2.3 Queue Manager

**Purpose**: Implement peek-lock semantics for orchestrator and worker queues.

**Responsibilities**:
- Enqueue items with visibility timestamps
- Fetch items via range scans
- Lock items with token and expiration
- Handle lock renewal
- Expire stale locks

#### 4.2.4 History Manager

**Purpose**: Manage append-only event log with multi-execution support.

**Responsibilities**:
- Append events to specific execution
- Read history for latest or specific execution
- Enforce event ID uniqueness
- Support efficient range scans

---

## 5. Key-Value Schema Design

### 5.1 Key Encoding Patterns

All keys use hierarchical prefixes for efficient range scans. Numeric values are zero-padded to 16 digits for lexicographic sorting.

#### 5.1.1 Instance Metadata

```
Key:   "meta:inst:{instance_id}"
Value: {
  "orchestration_name": "ProcessOrder",
  "orchestration_version": "1.0.0",
  "current_execution_id": 2,
  "created_at": 1700000000000,
  "updated_at": 1700000100000
}
```

**Purpose**: Store instance-level metadata (orchestration name, version, current execution).

**Access Patterns**:
- Direct get by instance ID
- No range scans needed

#### 5.1.2 Execution Metadata

```
Key:   "exec:{instance_id}:{execution_id:016}"
Value: {
  "status": "Completed",
  "output": "Order processed successfully",
  "started_at": 1700000000000,
  "completed_at": 1700000100000
}
```

**Purpose**: Store execution-level metadata (status, output, timestamps).

**Access Patterns**:
- Direct get by instance + execution ID
- Range scan for all executions: `"exec:{instance}:"..`

#### 5.1.3 History Events (Append-Only)

```
Key:   "hist:{instance_id}:{execution_id:016}:{event_id:016}"
Value: {
  "event_type": "ActivityScheduled",
  "event_data": { ... Event enum as JSON ... },
  "created_at": 1700000000123
}
```

**Purpose**: Append-only event log for orchestration replay.

**Access Patterns**:
- Range scan for all events in execution: `"hist:{instance}:{exec}:"..`
- Check existence for duplicate detection
- Never update or delete (append-only)

**Example Keys**:
```
hist:order-123:0000000000000001:0000000000000001  → OrchestrationStarted
hist:order-123:0000000000000001:0000000000000002  → ActivityScheduled
hist:order-123:0000000000000001:0000000000000003  → ActivityCompleted
hist:order-123:0000000000000002:0000000000000001  → OrchestrationStarted (ContinueAsNew)
```

#### 5.1.4 Instance Locks

```
Key:   "lock:inst:{instance_id}"
Value: {
  "lock_token": "550e8400-e29b-41d4-a716-446655440000",
  "locked_until_ms": 1700000030000,
  "locked_at_ms": 1700000000000
}
```

**Purpose**: Prevent concurrent processing of same instance (critical requirement).

**Access Patterns**:
- Get by instance ID to check lock status
- CAS to acquire lock atomically
- Delete on successful ack
- Check expiration on fetch

**Lock Lifecycle**:
1. `fetch_orchestration_item`: CAS to acquire lock
2. Processing: Lock held, prevents other dispatchers
3. `ack_orchestration_item`: Delete lock (release)
4. `abandon_orchestration_item`: Delete lock (release)
5. Expiration: Lock becomes available after `locked_until_ms`

#### 5.1.5 Orchestrator Queue (Time-Sorted)

```
Key:   "qo:{visible_at_ms:016}:{uuid}"
Value: {
  "instance_id": "order-123",
  "work_item": { ... WorkItem enum as JSON ... },
  "lock_token": "550e8400-...",  // null if unlocked
  "locked_until_ms": 1700000030000  // null if unlocked
}
```

**Purpose**: Queue of orchestration work items, sorted by visibility time.

**Access Patterns**:
- Range scan from current time: `"qo:{now:016}:"..` (gets all visible items)
- Scan stops at first invisible item (queue is sorted)
- Lock items by updating value in-place
- Delete on ack

**Special Handling for TimerFired**:
```
// Regular message (visible immediately)
qo:0000170000000000:uuid-1  → { instance: "order-123", work_item: StartOrchestration }

// Timer message (visible in future)
qo:0000170000030000:uuid-2  → { instance: "order-123", work_item: TimerFired }
```

**Queue Item Lifecycle**:
1. Enqueue: Insert with `visible_at_ms` = now (or future for timers)
2. Fetch: Range scan, check visibility, check lock expiration
3. Lock: Update value with `lock_token` and `locked_until_ms`
4. Ack: Delete key
5. Abandon: Update value, clear `lock_token`

#### 5.1.6 Worker Queue (FIFO)

```
Key:   "qw:{enqueued_at_ms:016}:{uuid}"
Value: {
  "work_item": { ... WorkItem enum as JSON ... },
  "lock_token": "550e8400-...",  // null if unlocked
  "locked_until_ms": 1700000030000  // null if unlocked
}
```

**Purpose**: FIFO queue of activity execution requests.

**Access Patterns**:
- Range scan from beginning: `"qw:"..` (FIFO order)
- Lock by updating value
- Delete on ack

#### 5.1.7 Worker Lock Index

```
Key:   "lockidx:worker:{lock_token}"
Value: "qw:0000170000000000:uuid-xyz"  // Points to queue key
```

**Purpose**: Fast lookup of queue key by lock token (for ack/renew operations).

**Access Patterns**:
- Direct get by lock token
- Created when locking worker item
- Deleted when acking worker item
- Enables O(1) ack without scanning queue

### 5.2 Key Design Rationale

**Why Zero-Padding?**
- Lexicographic sorting: `"hist:...:0000000000000002"` < `"hist:...:0000000000000010"`
- Without padding: `"hist:...:2"` > `"hist:...:10"` (string comparison)

**Why UUIDs?**
- Prevent key collisions when multiple items have same timestamp
- Enable distributed ID generation (no coordination needed)
- Unique across all dispatchers and regions

**Why Hierarchical Prefixes?**
- Enable efficient range scans: `"hist:order-123:1:"..` gets all events for execution
- Group related data together in key space
- Leverage SlateDB's LSM tree locality

---

## 6. Implementation Details

### 6.1 Core Provider Methods

#### 6.1.1 `fetch_orchestration_item()`

**High-Level Flow**:
```
1. Scan orchestrator queue for visible messages
2. For each message, check if instance is locked
3. Try to acquire instance lock with CAS
4. If successful, lock all messages for that instance
5. Load instance metadata and history
6. Return OrchestrationItem with lock token
```

**Pseudo-Code**:
```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
) -> Result<Option<OrchestrationItem>, ProviderError> {
    let now_ms = Self::now_millis();
    
    // 1. Scan orchestrator queue for visible items
    let start_key = format!("qo:0000000000000000:");
    let end_key = format!("qo:{:016}:~", now_ms);
    let mut scan = self.db.range(start_key..end_key).await?;
    
    while let Some((key, value)) = scan.next().await {
        let queue_item: QueueItem = serde_json::from_slice(&value)?;
        
        // Check if item is locked and lock hasn't expired
        if let Some(locked_until) = queue_item.locked_until_ms {
            if locked_until > now_ms {
                continue; // Locked by another dispatcher
            }
        }
        
        let instance_id = &queue_item.instance_id;
        
        // 2. Check if instance is locked
        let lock_key = format!("lock:inst:{}", instance_id);
        let existing_lock = self.db.get(&lock_key).await?;
        
        if let Some(lock_bytes) = existing_lock {
            let lock_info: LockInfo = serde_json::from_slice(&lock_bytes)?;
            if lock_info.locked_until_ms > now_ms {
                continue; // Instance locked, try next
            }
        }
        
        // 3. Try to acquire instance lock with CAS
        let lock_token = uuid::Uuid::new_v4().to_string();
        let new_lock = LockInfo {
            lock_token: lock_token.clone(),
            locked_until_ms: now_ms + lock_timeout.as_millis() as i64,
            locked_at_ms: now_ms,
        };
        
        let cas_succeeded = self.db.compare_and_swap(
            &lock_key,
            existing_lock.as_deref(),
            Some(&serde_json::to_vec(&new_lock)?),
        ).await?;
        
        if !cas_succeeded {
            continue; // Another dispatcher won the race
        }
        
        // 4. Lock acquired! Collect all visible messages for this instance
        let mut messages = Vec::new();
        let mut locked_keys = Vec::new();
        
        let instance_scan_start = format!("qo:0000000000000000:");
        let instance_scan_end = format!("qo:{:016}:~", now_ms);
        let mut instance_scan = self.db.range(instance_scan_start..instance_scan_end).await?;
        
        while let Some((msg_key, msg_value)) = instance_scan.next().await {
            let msg_item: QueueItem = serde_json::from_slice(&msg_value)?;
            
            if msg_item.instance_id != *instance_id {
                continue; // Different instance
            }
            
            // Lock this message
            let mut locked_msg = msg_item.clone();
            locked_msg.lock_token = Some(lock_token.clone());
            locked_msg.locked_until_ms = Some(now_ms + lock_timeout.as_millis() as i64);
            
            self.db.put(&msg_key, &serde_json::to_vec(&locked_msg)?).await?;
            
            messages.push(locked_msg.work_item);
            locked_keys.push(msg_key);
        }
        
        // 5. Load instance metadata
        let meta_key = format!("meta:inst:{}", instance_id);
        let metadata = if let Some(meta_bytes) = self.db.get(&meta_key).await? {
            serde_json::from_slice::<InstanceMetadata>(&meta_bytes)?
        } else {
            // New instance, derive from messages
            self.derive_metadata_from_messages(&messages)?
        };
        
        // 6. Load history for current execution
        let exec_id = metadata.current_execution_id;
        let hist_start = format!("hist:{}:{}:0000000000000000", instance_id, exec_id);
        let hist_end = format!("hist:{}:{}:9999999999999999", instance_id, exec_id);
        let mut history = Vec::new();
        
        let mut hist_scan = self.db.range(hist_start..hist_end).await?;
        while let Some((_key, value)) = hist_scan.next().await {
            let event: Event = serde_json::from_slice(&value)?;
            history.push(event);
        }
        
        // 7. Return OrchestrationItem
        return Ok(Some(OrchestrationItem {
            instance: instance_id.clone(),
            orchestration_name: metadata.orchestration_name,
            execution_id: exec_id,
            version: metadata.orchestration_version.unwrap_or_else(|| "unknown".to_string()),
            history,
            messages,
            lock_token,
        }));
    }
    
    Ok(None) // No work available
}
```

**Key Design Decisions**:
- Use CAS for atomic lock acquisition (prevents race conditions)
- Scan queue once for instance detection, then again to lock all messages
- Store locked message keys in-memory for later deletion
- Handle missing metadata gracefully (derive from messages)

#### 6.1.2 `ack_orchestration_item()`

**High-Level Flow**:
```
1. Validate lock token (check instance lock)
2. Start WriteBatch for atomic commit
3. Delete instance lock (release)
4. Create/update execution metadata
5. Append history events
6. Enqueue worker items
7. Enqueue orchestrator items (with TimerFired delay)
8. Delete locked orchestrator messages
9. Commit WriteBatch atomically
```

**Pseudo-Code**:
```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
) -> Result<(), ProviderError> {
    // 1. Validate lock token
    let lock_scan = self.db.range("lock:inst:".."lock:inst:~").await?;
    let mut instance_id: Option<String> = None;
    
    for (key, value) in lock_scan {
        let lock_info: LockInfo = serde_json::from_slice(&value)?;
        if lock_info.lock_token == lock_token {
            if lock_info.locked_until_ms <= Self::now_millis() {
                return Err(ProviderError::permanent(
                    "ack_orchestration_item",
                    "Lock token expired",
                ));
            }
            instance_id = Some(key.strip_prefix("lock:inst:").unwrap().to_string());
            break;
        }
    }
    
    let instance_id = instance_id.ok_or_else(|| {
        ProviderError::permanent("ack_orchestration_item", "Invalid lock token")
    })?;
    
    // 2. Start WriteBatch
    let mut batch = self.db.write_batch();
    
    // 3. Delete instance lock
    batch.delete(format!("lock:inst:{}", instance_id));
    
    // 4. Create/update execution metadata
    let exec_key = format!("exec:{}:{:016}", instance_id, execution_id);
    let exec_meta = ExecutionMeta {
        status: metadata.status.clone().unwrap_or_else(|| "Running".to_string()),
        output: metadata.output.clone(),
        started_at: Self::now_millis(),
        completed_at: if metadata.status.is_some() {
            Some(Self::now_millis())
        } else {
            None
        },
    };
    batch.put(exec_key, serde_json::to_vec(&exec_meta)?);
    
    // Update instance metadata if provided
    if let (Some(name), Some(version)) = (&metadata.orchestration_name, &metadata.orchestration_version) {
        let meta_key = format!("meta:inst:{}", instance_id);
        let instance_meta = InstanceMetadata {
            orchestration_name: name.clone(),
            orchestration_version: Some(version.clone()),
            current_execution_id: execution_id,
            created_at: Self::now_millis(),
            updated_at: Self::now_millis(),
        };
        batch.put(meta_key, serde_json::to_vec(&instance_meta)?);
    }
    
    // 5. Append history events
    for event in history_delta {
        let event_key = format!(
            "hist:{}:{:016}:{:016}",
            instance_id,
            execution_id,
            event.event_id()
        );
        
        // Check for duplicate (should not happen, but validate)
        if self.db.get(&event_key).await?.is_some() {
            return Err(ProviderError::permanent(
                "ack_orchestration_item",
                "Duplicate event_id detected",
            ));
        }
        
        batch.put(event_key, serde_json::to_vec(&event)?);
    }
    
    // 6. Enqueue worker items
    for item in worker_items {
        let queue_key = format!("qw:{:016}:{}", Self::now_millis(), uuid::Uuid::new_v4());
        let queue_item = WorkerQueueItem {
            work_item: item,
            lock_token: None,
            locked_until_ms: None,
        };
        batch.put(queue_key, serde_json::to_vec(&queue_item)?);
    }
    
    // 7. Enqueue orchestrator items (with TimerFired delay)
    for item in orchestrator_items {
        let visible_at = match &item {
            WorkItem::TimerFired { fire_at_ms, .. } => *fire_at_ms as i64,
            _ => Self::now_millis(),
        };
        
        let instance = self.extract_instance(&item)?;
        let queue_key = format!("qo:{:016}:{}", visible_at, uuid::Uuid::new_v4());
        let queue_item = OrchQueueItem {
            instance_id: instance,
            work_item: item,
            lock_token: None,
            locked_until_ms: None,
        };
        batch.put(queue_key, serde_json::to_vec(&queue_item)?);
    }
    
    // 8. Delete locked orchestrator messages
    // Note: In fetch_orchestration_item, we stored locked_keys somewhere
    // For simplicity, we can scan for messages with our lock_token
    let orch_scan = self.db.range("qo:".."qo:~").await?;
    for (key, value) in orch_scan {
        let queue_item: OrchQueueItem = serde_json::from_slice(&value)?;
        if queue_item.lock_token.as_deref() == Some(lock_token) {
            batch.delete(key);
        }
    }
    
    // 9. Commit atomically
    self.db.write(batch).await?;
    
    Ok(())
}
```

**Atomicity Guarantee**:
All operations in the `WriteBatch` commit atomically. If any operation fails, the entire batch is rolled back. This ensures:
- History events are not saved without enqueueing worker items
- Worker items are not enqueued without history events
- Locks are not released if commit fails
- Instance metadata is consistent with execution state

#### 6.1.3 Other Core Methods

**`abandon_orchestration_item()`**:
```rust
async fn abandon_orchestration_item(
    &self,
    lock_token: &str,
    delay: Option<Duration>,
) -> Result<(), ProviderError> {
    // 1. Find and validate lock token
    let lock_scan = self.db.range("lock:inst:".."lock:inst:~").await?;
    let mut instance_id: Option<String> = None;
    
    for (key, value) in lock_scan {
        let lock_info: LockInfo = serde_json::from_slice(&value)?;
        if lock_info.lock_token == lock_token {
            instance_id = Some(key.strip_prefix("lock:inst:").unwrap().to_string());
            break;
        }
    }
    
    let instance_id = instance_id.ok_or_else(|| {
        ProviderError::permanent("abandon_orchestration_item", "Invalid lock token")
    })?;
    
    // 2. Start WriteBatch
    let mut batch = self.db.write_batch();
    
    // 3. Unlock all messages with this lock_token
    let visible_at = if let Some(delay) = delay {
        Self::now_millis() + delay.as_millis() as i64
    } else {
        Self::now_millis()
    };
    
    let orch_scan = self.db.range("qo:".."qo:~").await?;
    for (key, value) in orch_scan {
        let mut queue_item: OrchQueueItem = serde_json::from_slice(&value)?;
        if queue_item.lock_token.as_deref() == Some(lock_token) {
            queue_item.lock_token = None;
            queue_item.locked_until_ms = None;
            // Note: Changing visible_at requires re-keying (delete + insert)
            batch.delete(key.clone());
            let new_key = format!("qo:{:016}:{}", visible_at, uuid::Uuid::new_v4());
            batch.put(new_key, serde_json::to_vec(&queue_item)?);
        }
    }
    
    // 4. Delete instance lock
    batch.delete(format!("lock:inst:{}", instance_id));
    
    // 5. Commit
    self.db.write(batch).await?;
    
    Ok(())
}
```

**`read()`**:
```rust
async fn read(&self, instance: &str) -> Result<Vec<Event>, ProviderError> {
    // Get latest execution ID
    let meta_key = format!("meta:inst:{}", instance);
    let exec_id = if let Some(meta_bytes) = self.db.get(&meta_key).await? {
        let meta: InstanceMetadata = serde_json::from_slice(&meta_bytes)?;
        meta.current_execution_id
    } else {
        return Ok(Vec::new()); // Instance doesn't exist
    };
    
    // Read history for that execution
    self.read_with_execution(instance, exec_id).await
}
```

**`read_with_execution()`**:
```rust
async fn read_with_execution(
    &self,
    instance: &str,
    execution_id: u64,
) -> Result<Vec<Event>, ProviderError> {
    let start_key = format!("hist:{}:{:016}:0000000000000000", instance, execution_id);
    let end_key = format!("hist:{}:{:016}:9999999999999999", instance, execution_id);
    
    let mut history = Vec::new();
    let mut scan = self.db.range(start_key..end_key).await?;
    
    while let Some((_key, value)) = scan.next().await {
        let event: Event = serde_json::from_slice(&value)?;
        history.push(event);
    }
    
    Ok(history)
}
```

### 6.2 Queue Operations

**`fetch_work_item()`**:
```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
) -> Result<Option<(WorkItem, String)>, ProviderError> {
    let mut scan = self.db.range("qw:".."qw:~").await?;
    let now_ms = Self::now_millis();
    
    while let Some((key, value)) = scan.next().await {
        let mut queue_item: WorkerQueueItem = serde_json::from_slice(&value)?;
        
        // Check if locked and not expired
        if let Some(locked_until) = queue_item.locked_until_ms {
            if locked_until > now_ms {
                continue; // Still locked
            }
        }
        
        // Lock it
        let lock_token = uuid::Uuid::new_v4().to_string();
        queue_item.lock_token = Some(lock_token.clone());
        queue_item.locked_until_ms = Some(now_ms + lock_timeout.as_millis() as i64);
        
        self.db.put(&key, &serde_json::to_vec(&queue_item)?).await?;
        
        // Create lock index for fast ack
        let lock_idx_key = format!("lockidx:worker:{}", lock_token);
        self.db.put(&lock_idx_key, key.as_bytes()).await?;
        
        return Ok(Some((queue_item.work_item, lock_token)));
    }
    
    Ok(None) // No work available
}
```

**`ack_work_item()`**:
```rust
async fn ack_work_item(
    &self,
    token: &str,
    completion: WorkItem,
) -> Result<(), ProviderError> {
    // 1. Find queue key via lock index
    let lock_idx_key = format!("lockidx:worker:{}", token);
    let queue_key = self.db.get(&lock_idx_key).await?
        .ok_or_else(|| ProviderError::permanent("ack_work_item", "Invalid lock token"))?;
    let queue_key = String::from_utf8(queue_key)?;
    
    // 2. Start WriteBatch for atomic ack + enqueue
    let mut batch = self.db.write_batch();
    
    // 3. Delete from worker queue
    batch.delete(&queue_key);
    
    // 4. Delete lock index
    batch.delete(&lock_idx_key);
    
    // 5. Enqueue completion to orchestrator queue
    let instance = self.extract_instance(&completion)?;
    let orch_key = format!("qo:{:016}:{}", Self::now_millis(), uuid::Uuid::new_v4());
    let orch_item = OrchQueueItem {
        instance_id: instance,
        work_item: completion,
        lock_token: None,
        locked_until_ms: None,
    };
    batch.put(orch_key, serde_json::to_vec(&orch_item)?);
    
    // 6. Commit atomically
    self.db.write(batch).await?;
    
    Ok(())
}
```

### 6.3 Lock Renewal

**`renew_work_item_lock()`**:
```rust
async fn renew_work_item_lock(
    &self,
    token: &str,
    extend_for: Duration,
) -> Result<(), ProviderError> {
    // 1. Find queue key via lock index
    let lock_idx_key = format!("lockidx:worker:{}", token);
    let queue_key = self.db.get(&lock_idx_key).await?
        .ok_or_else(|| ProviderError::permanent("renew_work_item_lock", "Invalid lock token"))?;
    let queue_key = String::from_utf8(queue_key)?;
    
    // 2. Get current queue item
    let value = self.db.get(&queue_key).await?
        .ok_or_else(|| ProviderError::permanent("renew_work_item_lock", "Item not found"))?;
    let mut queue_item: WorkerQueueItem = serde_json::from_slice(&value)?;
    
    // 3. Validate lock token matches
    if queue_item.lock_token.as_deref() != Some(token) {
        return Err(ProviderError::permanent("renew_work_item_lock", "Lock token mismatch"));
    }
    
    // 4. Check if lock already expired
    let now_ms = Self::now_millis();
    if let Some(locked_until) = queue_item.locked_until_ms {
        if locked_until <= now_ms {
            return Err(ProviderError::permanent("renew_work_item_lock", "Lock already expired"));
        }
    }
    
    // 5. Extend lock
    queue_item.locked_until_ms = Some(now_ms + extend_for.as_millis() as i64);
    self.db.put(&queue_key, &serde_json::to_vec(&queue_item)?).await?;
    
    Ok(())
}
```

---

## 7. Transaction Strategy

### 7.1 SlateDB Transaction Model

SlateDB provides **write batching** and **compare-and-swap (CAS)** primitives:

| Primitive | Semantics | Use Case |
|-----------|-----------|----------|
| `WriteBatch` | Atomic multi-key commit | Bundle multiple operations |
| `compare_and_swap` | Optimistic lock acquisition | Instance locks |
| Snapshot reads | Read consistency during batch | Validate state before commit |

**What SlateDB Guarantees:**
- ✅ All operations in a `WriteBatch` commit atomically
- ✅ CAS succeeds only if expected value matches
- ✅ Reads see consistent snapshot (isolation)

**What SlateDB Does NOT Guarantee:**
- ❌ No multi-version concurrency control (MVCC) across batches
- ❌ No serializable transaction isolation
- ❌ No automatic conflict resolution or retry

### 7.2 Duroxide Atomicity Requirements

Per the provider implementation guide, `ack_orchestration_item()` must atomically:

1. Validate lock token
2. Delete instance lock
3. Create/update execution metadata
4. Append history events
5. Update execution status/output
6. Enqueue worker items
7. Enqueue orchestrator items
8. Delete acknowledged messages

**Implementation Strategy:**

```rust
async fn ack_orchestration_item(...) -> Result<(), ProviderError> {
    // Phase 1: Read & Validate (outside batch)
    let lock_info = self.validate_lock_token(lock_token).await?;
    let instance_id = lock_info.instance_id;
    
    // Phase 2: Accumulate all operations in WriteBatch
    let mut batch = self.db.write_batch();
    
    batch.delete(format!("lock:inst:{}", instance_id));
    // ... add all other operations ...
    
    // Phase 3: Atomic commit
    self.db.write(batch).await?;  // ALL or NOTHING
    
    Ok(())
}
```

**Conflict Handling:**

If another dispatcher modifies state concurrently:
1. Lock validation fails → Return error immediately
2. CAS fails during lock acquisition → Return None (try next instance)
3. Batch write fails → Entire operation rolls back, lock remains held

### 7.3 Consistency Model

**Consistency Guarantees:**
- **Read-your-writes**: After commit, subsequent reads see new state
- **Atomic ack**: Either all operations succeed or all fail
- **Lock integrity**: Instance locks prevent concurrent processing

**Trade-offs:**
- No cross-batch transactions (but not needed for Duroxide)
- Retries needed for CAS conflicts (acceptable for lock acquisition)
- Object storage eventual consistency (mitigated by SlateDB's WAL)

---

## 8. Performance Considerations

### 8.1 Expected Performance Characteristics

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| `put(key, value)` | O(1) + flush | Write to memtable, async flush to S3 |
| `get(key)` | O(log N) | Check memtable → bloom filter → SSTable |
| `range(start..end)` | O(N + log M) | Merge-sort from memtable + SSTables |
| `WriteBatch` commit | O(K) | K operations, atomic WAL write |
| `compare_and_swap` | O(1) + consensus | Read-check-write, potential retry |

### 8.2 Optimization Strategies

#### 8.2.1 Read Path Optimization

**Caching:**
```rust
use moka::future::Cache;

pub struct SlateDBProvider {
    db: SlateDB,
    metadata_cache: Cache<String, InstanceMetadata>,
    lock_cache: Cache<String, LockInfo>,
}

impl SlateDBProvider {
    async fn get_instance_metadata(&self, instance_id: &str) -> Result<InstanceMetadata, ProviderError> {
        // Check cache first
        if let Some(meta) = self.metadata_cache.get(instance_id).await {
            return Ok(meta);
        }
        
        // Cache miss, read from SlateDB
        let key = format!("meta:inst:{}", instance_id);
        let value = self.db.get(&key).await?;
        let meta: InstanceMetadata = serde_json::from_slice(&value)?;
        
        // Cache for 30 seconds
        self.metadata_cache.insert(instance_id.to_string(), meta.clone()).await;
        
        Ok(meta)
    }
}
```

**Bloom Filters:**
- SlateDB automatically uses bloom filters to skip SSTables
- Helps avoid unnecessary S3 reads for non-existent keys
- Most effective for `get()` operations

#### 8.2.2 Write Path Optimization

**Batch Size Tuning:**
```rust
// Configure SlateDB to buffer writes
let config = SlateDBConfig {
    memtable_size: 64 * 1024 * 1024,  // 64MB before flush
    compaction_schedule: CompactionSchedule::Tiered,
    write_buffer_size: 4 * 1024 * 1024,  // 4MB write buffer
};
```

**Compaction Strategy:**
- Use tiered compaction for write-heavy workloads
- Schedule background compaction during low-traffic periods
- Monitor SSTable count and file sizes

#### 8.2.3 Queue Scan Optimization

**Limit Scan Range:**
```rust
// Instead of scanning entire queue
let scan = self.db.range("qo:".."qo:~");

// Limit to next 100 items (early termination)
let scan = self.db.range("qo:".."qo:~").take(100);
```

**Timestamp-Based Pruning:**
```rust
// Only scan up to current time (for orchestrator queue)
let now_ms = Self::now_millis();
let start_key = "qo:0000000000000000:";
let end_key = format!("qo:{:016}:~", now_ms);
let scan = self.db.range(start_key..end_key);
```

### 8.3 Object Storage Considerations

**Latency Impact:**
- S3 typical latency: 10-50ms per request
- SlateDB mitigates via memtable caching (in-memory writes)
- WAL writes are async (non-blocking for reads)

**Cost Optimization:**
- PUT/POST requests: ~$0.005 per 1,000 requests
- GET requests: ~$0.0004 per 1,000 requests
- Storage: ~$0.023 per GB-month
- Minimize small writes via batching
- Use lifecycle policies to archive old history

**Throughput:**
- S3 can handle 3,500 PUTs/sec and 5,500 GETs/sec per prefix
- Use key prefix sharding if needed: `qo:shard0:...`, `qo:shard1:...`
- SlateDB handles this internally via SSTable distribution

### 8.4 Performance Benchmarks (Projected)

| Metric | SQLite (File) | SlateDB (S3) | Notes |
|--------|---------------|--------------|-------|
| `fetch_orchestration_item` | 1-5ms | 10-50ms | Higher latency due to S3 |
| `ack_orchestration_item` | 5-20ms | 20-100ms | Batch write + S3 flush |
| `read()` (100 events) | 2-10ms | 15-80ms | Range scan + S3 reads |
| Throughput (orch/sec) | 500-1000 | 50-200 | Limited by S3 latency |

**Optimization Targets:**
- Reduce S3 round-trips via caching
- Batch multiple orchestrations per fetch (if possible)
- Use CloudFront or CDN for read-heavy workloads

---

## 9. Testing Strategy

### 9.1 Validation Test Suite (45 Tests)

Per the provider implementation guide, use the built-in validation suite:

```rust
// tests/slatedb_provider_validations.rs

use duroxide::provider_validations::*;
use std::sync::Arc;
use std::time::Duration;

struct SlateDBProviderFactory;

#[async_trait::async_trait]
impl ProviderFactory for SlateDBProviderFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        let tmp_dir = tempfile::tempdir().unwrap();
        let db_path = tmp_dir.path().join("test-db");
        Arc::new(SlateDBProvider::new(&db_path).await.unwrap())
    }
    
    fn lock_timeout(&self) -> Duration {
        Duration::from_secs(1)
    }
}

// Run all 45 validation tests
#[tokio::test]
async fn test_atomicity_failure_rollback() {
    test_atomicity_failure_rollback(&SlateDBProviderFactory).await;
}

#[tokio::test]
async fn test_exclusive_instance_lock() {
    test_exclusive_instance_lock(&SlateDBProviderFactory).await;
}

// ... 43 more tests
```

**Test Categories:**
1. **Atomicity Tests (4)**: Verify atomic ack behavior
2. **Error Handling Tests (5)**: Invalid tokens, duplicates, corruption
3. **Instance Locking Tests (11)**: Lock acquisition, expiration, isolation
4. **Lock Expiration Tests (9)**: Timeout, renewal, concurrent access
5. **Multi-Execution Tests (5)**: ContinueAsNew, execution isolation
6. **Queue Semantics Tests (5)**: FIFO, peek-lock, atomicity
7. **Instance Creation Tests (4)**: Metadata management, NULL versions
8. **Management Tests (7)**: Optional ProviderAdmin methods

### 9.2 Stress Testing

Use provider stress test infrastructure:

```rust
use duroxide::provider_stress_tests::parallel_orchestrations::{
    ProviderStressFactory, run_parallel_orchestrations_test
};

struct SlateDBStressFactory;

#[async_trait::async_trait]
impl ProviderStressFactory for SlateDBStressFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        Arc::new(SlateDBProvider::new("s3://test-bucket/stress-db").await.unwrap())
    }
}

#[tokio::test]
async fn stress_test_parallel_orchestrations() {
    let factory = SlateDBStressFactory;
    let result = run_parallel_orchestrations_test(&factory).await.unwrap();
    
    assert!(result.success_rate() > 99.0, "Success rate too low: {}", result.success_rate());
    println!("Throughput: {} orch/sec", result.throughput());
}
```

### 9.3 Object Storage Integration Tests

**Local Testing (MinIO):**
```bash
# Start MinIO locally
docker run -p 9000:9000 -p 9001:9001 \
  minio/minio server /data --console-address ":9001"

# Run tests against MinIO
export AWS_ENDPOINT_URL=http://localhost:9000
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
cargo test --test slatedb_integration -- --nocapture
```

**S3 Integration Tests:**
```rust
#[tokio::test]
#[ignore] // Only run in CI with credentials
async fn test_real_s3_provider() {
    let bucket = std::env::var("TEST_S3_BUCKET").unwrap();
    let provider = SlateDBProvider::new(&format!("s3://{}/test-db", bucket))
        .await
        .unwrap();
    
    // Run validation tests against real S3
    // ...
}
```

### 9.4 Performance Benchmarking

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_fetch_orchestration_item(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let provider = runtime.block_on(async {
        SlateDBProvider::new("s3://test-bucket/bench-db").await.unwrap()
    });
    
    c.bench_function("fetch_orchestration_item", |b| {
        b.to_async(&runtime).iter(|| async {
            provider.fetch_orchestration_item(Duration::from_secs(30)).await
        });
    });
}

criterion_group!(benches, bench_fetch_orchestration_item);
criterion_main!(benches);
```

---

## 10. Implementation Roadmap

### Phase 1: Foundation (Week 1)

**Goals**: Basic infrastructure and key encoding.

**Tasks**:
- [ ] Set up SlateDB dependency and basic provider struct
- [ ] Implement key encoding utilities (`KeyEncoder`)
- [ ] Implement basic serialization/deserialization
- [ ] Write unit tests for key encoding
- [ ] Set up local MinIO for testing

**Deliverables**:
- `src/providers/slatedb/mod.rs` (provider skeleton)
- `src/providers/slatedb/schema.rs` (key encoding)
- Basic unit tests passing

### Phase 2: Core Operations (Week 2)

**Goals**: Implement history and metadata operations.

**Tasks**:
- [ ] Implement `read()` and `read_with_execution()`
- [ ] Implement `append_with_execution()`
- [ ] Implement instance metadata management
- [ ] Implement execution metadata management
- [ ] Run basic validation tests (history tests)

**Deliverables**:
- History read/write working
- Metadata operations working
- 10+ validation tests passing

### Phase 3: Queue Operations (Week 3)

**Goals**: Implement worker and orchestrator queues.

**Tasks**:
- [ ] Implement `enqueue_for_worker()` and `enqueue_for_orchestrator()`
- [ ] Implement `fetch_work_item()` with peek-lock
- [ ] Implement `ack_work_item()` with atomic enqueue
- [ ] Implement `renew_work_item_lock()`
- [ ] Handle lock expiration in queue scans
- [ ] Run queue validation tests

**Deliverables**:
- Queue operations working
- Worker lock renewal working
- 20+ validation tests passing

### Phase 4: Orchestration Dispatcher (Week 4)

**Goals**: Implement orchestration fetch/ack with instance locks.

**Tasks**:
- [ ] Implement instance-level locking with CAS
- [ ] Implement `fetch_orchestration_item()` with lock acquisition
- [ ] Implement `ack_orchestration_item()` with atomic batch writes
- [ ] Implement `abandon_orchestration_item()`
- [ ] Handle TimerFired delayed visibility
- [ ] Run atomicity and locking validation tests

**Deliverables**:
- Full orchestration dispatch cycle working
- Instance locks preventing concurrent processing
- 40+ validation tests passing

### Phase 5: Optimization & Polish (Week 5)

**Goals**: Performance tuning and production readiness.

**Tasks**:
- [ ] Add metadata caching layer
- [ ] Optimize range scan limits
- [ ] Tune SlateDB configuration (memtable size, compaction)
- [ ] Run stress tests and measure performance
- [ ] Fix any remaining validation test failures
- [ ] Write documentation and examples
- [ ] Test with real S3 (not just MinIO)

**Deliverables**:
- All 45 validation tests passing
- Stress tests passing with acceptable performance
- Documentation complete
- Ready for production evaluation

### Phase 6 (Optional): Management APIs

**Goals**: Implement ProviderAdmin trait.

**Tasks**:
- [ ] Implement `list_instances()` and `list_executions()`
- [ ] Implement `get_instance_info()` and `get_execution_info()`
- [ ] Implement `get_system_metrics()` and `get_queue_depths()`
- [ ] Add secondary indexes for efficient status queries
- [ ] Run management validation tests

**Deliverables**:
- Full ProviderAdmin implementation
- Client management APIs working

---

## 11. Risk Analysis

### 11.1 Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| **Object storage latency too high** | Medium | High | Cache hot keys, batch operations, benchmark early |
| **SlateDB lacks needed features** | Low | Critical | POC first, verify CAS and WriteBatch work |
| **CAS contention under load** | Medium | Medium | Implement exponential backoff, optimize lock holding time |
| **Eventually consistent reads** | Low | High | Use SlateDB WAL for consistency, test thoroughly |
| **Key-value schema limitations** | Medium | Medium | Design schema carefully upfront, validate with tests |
| **Debugging complexity** | High | Low | Add extensive logging, build debugging tools |

### 11.2 Performance Risks

| Risk | Mitigation |
|------|------------|
| Throughput < 50 orch/sec | Cache aggressively, batch operations, consider read replicas |
| High tail latency (P99 > 500ms) | Monitor S3 latency, use CloudFront, set realistic expectations |
| Expensive S3 API costs | Batch writes, use lifecycle policies, estimate costs early |

### 11.3 Operational Risks

| Risk | Mitigation |
|------|------------|
| S3 outage impacts availability | Multi-region replication, fallback to SQLite for critical paths |
| Key design mistakes (hard to migrate) | Extensive validation testing, document schema carefully |
| Lock expiration tuning difficult | Make lock timeout configurable, provide monitoring tools |

### 11.4 Mitigation Strategy Summary

1. **Build POC first** (1-2 days): Verify SlateDB CAS and WriteBatch work as expected
2. **Test early and often**: Run validation tests from day 1, don't wait until end
3. **Benchmark continuously**: Track performance metrics at each phase
4. **Document schema extensively**: Key design is critical and hard to change
5. **Plan for failure**: Design graceful degradation (fallback to SQLite if needed)

---

## 12. Success Criteria

### 12.1 Functional Requirements

- [ ] **Pass all 45 validation tests**: Full correctness per provider guide
- [ ] **Support atomic ack**: All operations in `ack_orchestration_item` are atomic
- [ ] **Instance-level locking**: Prevent concurrent processing of same instance
- [ ] **Multi-execution support**: Handle ContinueAsNew correctly
- [ ] **Lock renewal**: Support long-running activities (> 30 seconds)
- [ ] **TimerFired delayed visibility**: Timers fire at correct logical time
- [ ] **Error handling**: Return appropriate ProviderError (retryable vs permanent)

### 12.2 Performance Requirements

- [ ] **Throughput**: ≥50 orchestrations/second (single dispatcher)
- [ ] **Latency (P50)**: ≤100ms for `fetch_orchestration_item`
- [ ] **Latency (P99)**: ≤500ms for `fetch_orchestration_item`
- [ ] **Stress test**: Handle 100+ concurrent orchestrations without failures
- [ ] **Lock contention**: ≥90% success rate on first CAS attempt

### 12.3 Production Readiness

- [ ] **Documentation**: Complete schema documentation and deployment guide
- [ ] **Examples**: Sample code for common configurations (S3, GCS, Azure)
- [ ] **Monitoring**: Emit metrics for key operations (latency, throughput, errors)
- [ ] **Error messages**: Clear, actionable error messages for common failures
- [ ] **Testing**: Integration tests with real S3 (not just MinIO)

### 12.4 Nice-to-Have (Stretch Goals)

- [ ] **ProviderAdmin implementation**: Management APIs for observability
- [ ] **Multi-region replication**: Active-active or active-passive setup
- [ ] **Cost estimation tool**: Predict S3 costs based on workload
- [ ] **Migration tool**: Convert SQLite database to SlateDB

---

## 13. References

### 13.1 Duroxide Documentation

- **Provider Implementation Guide**: `docs/provider-implementation-guide.md`
- **Provider Testing Guide**: `docs/provider-testing-guide.md`
- **Observability Guide**: `docs/observability-guide.md`
- **Architecture Overview**: `docs/architecture.md`

### 13.2 SlateDB Resources

- **SlateDB Repository**: https://github.com/slatedb/slatedb
- **SlateDB Documentation**: https://docs.slatedb.io/
- **LSM Tree Overview**: https://www.igvita.com/2012/02/06/sstable-and-log-structured-storage-leveldb/

### 13.3 Object Storage Documentation

- **AWS S3**: https://aws.amazon.com/s3/
- **Google Cloud Storage**: https://cloud.google.com/storage
- **Azure Blob Storage**: https://azure.microsoft.com/en-us/services/storage/blobs/
- **MinIO (S3-compatible)**: https://min.io/

### 13.4 Related Papers

- **Bigtable Paper** (LSM Tree foundation): https://research.google/pubs/pub27898/
- **Dynamo Paper** (Eventual consistency): https://www.allthingsdistributed.com/files/amazon-dynamo-sosp2007.pdf

---

## Appendix A: Key Encoding Reference

```rust
// Instance Metadata
format!("meta:inst:{}", instance_id)
// Example: meta:inst:order-123

// Execution Metadata
format!("exec:{}:{:016}", instance_id, execution_id)
// Example: exec:order-123:0000000000000001

// History Event
format!("hist:{}:{:016}:{:016}", instance_id, execution_id, event_id)
// Example: hist:order-123:0000000000000001:0000000000000003

// Instance Lock
format!("lock:inst:{}", instance_id)
// Example: lock:inst:order-123

// Orchestrator Queue Item
format!("qo:{:016}:{}", visible_at_ms, uuid)
// Example: qo:0000170000000000:550e8400-e29b-41d4-a716-446655440000

// Worker Queue Item
format!("qw:{:016}:{}", enqueued_at_ms, uuid)
// Example: qw:0000170000000000:550e8400-e29b-41d4-a716-446655440000

// Worker Lock Index
format!("lockidx:worker:{}", lock_token)
// Example: lockidx:worker:550e8400-e29b-41d4-a716-446655440000
```

---

## Appendix B: Configuration Example

```rust
use duroxide::providers::slatedb::SlateDBProvider;
use duroxide::runtime::{Runtime, RuntimeOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure SlateDB provider with S3 backend
    let provider = SlateDBProvider::builder()
        .backend("s3://my-bucket/duroxide-prod")
        .memtable_size(64 * 1024 * 1024)  // 64MB
        .compaction_schedule(CompactionSchedule::Tiered)
        .build()
        .await?;
    
    // Configure runtime with appropriate lock timeouts
    let options = RuntimeOptions {
        orchestrator_lock_timeout: Duration::from_secs(5),
        worker_lock_timeout: Duration::from_secs(300),  // 5 minutes for long activities
        worker_lock_renewal_buffer: Duration::from_secs(30),
        ..Default::default()
    };
    
    // Start runtime
    let runtime = Runtime::start_with_store_and_options(
        Arc::new(provider),
        activities,
        orchestrations,
        options,
    ).await;
    
    // ... use runtime ...
    
    runtime.shutdown(None).await;
    Ok(())
}
```

---

## Appendix C: Debugging Tools

```rust
impl SlateDBProvider {
    /// Dump all keys with given prefix (for debugging)
    pub async fn debug_dump_prefix(&self, prefix: &str) -> Vec<(String, String)> {
        let mut results = Vec::new();
        let scan = self.db.range(prefix..(prefix.to_string() + "~")).await.unwrap();
        
        for (key, value) in scan {
            let key_str = String::from_utf8_lossy(&key).to_string();
            let value_str = String::from_utf8_lossy(&value).to_string();
            results.push((key_str, value_str));
        }
        
        results
    }
    
    /// Get statistics about key distribution
    pub async fn debug_key_stats(&self) -> KeyStats {
        let mut stats = KeyStats::default();
        
        // Count by prefix
        for prefix in &["meta:", "exec:", "hist:", "qo:", "qw:", "lock:"] {
            let count = self.db.range(*prefix..((*prefix).to_string() + "~"))
                .await
                .unwrap()
                .count();
            stats.counts.insert(prefix.to_string(), count);
        }
        
        stats
    }
}
```

---

**End of Proposal**
