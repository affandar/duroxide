# Lightweight File System Provider Design

**Status:** Proposal  
**Author:** Design Document  
**Target:** Extremely lightweight provider for development, testing, and embedded use cases

---

## 1. Goals and Non-Goals

### Goals

1. **Extremely lightweight** - No external dependencies beyond std/tokio
2. **Zero configuration** - Just point at a directory and go
3. **Portable** - Works on any POSIX or Windows filesystem
4. **Atomic operations** - Must uphold all atomicity requirements from the Provider trait
5. **Single-process safe** - Correct behavior with concurrent dispatchers in same process
6. **Crash recovery** - Locks expire, orphaned work items recovered

### Non-Goals

1. **Multi-process coordination** - Not designed for multiple processes accessing same directory
2. **High performance** - SQLite will always be faster for production use
3. **Large scale** - Not optimized for millions of instances
4. **Distributed systems** - No network filesystem support

---

## 2. Directory Structure

```
<root>/
├── instances/
│   └── <instance_id>/
│       ├── meta.json           # Instance metadata
│       ├── lock.json           # Instance-level lock (if locked)
│       └── executions/
│           └── <execution_id>/
│               └── history.json   # Append-only event history
│
├── queues/
│   ├── orchestrator/
│   │   └── <uuid>.json         # Individual queue messages
│   └── worker/
│       └── <uuid>.json         # Individual queue messages
│
└── locks/
    └── global.lock             # Optional: Global write lock for atomic operations
```

### Alternative: Single-File-Per-Instance (Simpler)

```
<root>/
├── instances/
│   └── <instance_id>.json      # All instance state in one file
│
├── orchestrator_queue/
│   └── <seq>_<instance_id>.json
│
├── worker_queue/
│   └── <seq>_<uuid>.json
│
└── .lock                       # flock() on this file for atomicity
```

---

## 3. Atomicity Strategy

### Option A: File Rename Atomicity (Recommended)

POSIX and Windows both guarantee that `rename()` is atomic within the same filesystem. This is the foundation of our atomicity:

```rust
// Atomic write pattern
fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;  // Atomic on same filesystem
    Ok(())
}
```

**For complex operations** (like `ack_orchestration_item`), we:
1. Compute all changes
2. Write all new files to `.tmp` versions
3. Rename all files atomically (best effort - see failure handling below)
4. Delete old files

### Option B: Global Lock File (Simpler but Slower)

Use a global lock file with `flock()`:

```rust
use std::fs::File;
use fs2::FileExt;  // or use libc directly

fn with_global_lock<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    let lock_path = root.join(".lock");
    let lock = File::create(&lock_path).unwrap();
    lock.lock_exclusive().unwrap();  // Blocks until acquired
    let result = f();
    lock.unlock().unwrap();
    result
}
```

**Trade-off:** Simpler implementation but serializes all operations.

### Recommended: Hybrid Approach

Use **Option A (rename atomicity)** for most operations, with **per-instance locks** for orchestration processing:

```rust
// Per-instance lock via lock file
fn acquire_instance_lock(instance_dir: &Path, lock_timeout: Duration) -> Option<InstanceLock> {
    let lock_path = instance_dir.join("lock.json");
    let now = SystemTime::now();
    
    // Check if existing lock is expired
    if let Ok(content) = fs::read_to_string(&lock_path) {
        let lock: LockInfo = serde_json::from_str(&content)?;
        if lock.expires_at > now {
            return None;  // Lock still valid
        }
    }
    
    // Write our lock (atomic rename)
    let lock_info = LockInfo {
        token: Uuid::new_v4().to_string(),
        locked_at: now,
        expires_at: now + lock_timeout,
    };
    atomic_write(&lock_path, &serde_json::to_vec(&lock_info)?)?;
    
    // Read back to verify we won (handles race conditions)
    let verify: LockInfo = serde_json::from_str(&fs::read_to_string(&lock_path)?)?;
    if verify.token == lock_info.token {
        Some(InstanceLock { token: lock_info.token, path: lock_path })
    } else {
        None  // Another process won the race
    }
}
```

---

## 4. Data Structures

### Instance Metadata (`meta.json`)

```json
{
  "instance_id": "order-123",
  "orchestration_name": "ProcessOrder",
  "orchestration_version": "1.0.0",
  "current_execution_id": 1,
  "parent_instance_id": null,
  "created_at": 1706500000000,
  "updated_at": 1706500100000
}
```

### Execution State (`executions/<id>/state.json`)

```json
{
  "execution_id": 1,
  "status": "Running",
  "output": null,
  "started_at": 1706500000000,
  "completed_at": null
}
```

### Event History (`executions/<id>/history.json`)

```json
{
  "events": [
    {
      "event_id": 1,
      "instance": "order-123",
      "execution_id": 1,
      "timestamp": null,
      "kind": {
        "OrchestrationStarted": {
          "name": "ProcessOrder",
          "version": "1.0.0",
          "input": "{}",
          "parent_instance": null,
          "parent_id": null
        }
      }
    }
  ]
}
```

### Queue Message

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "instance_id": "order-123",
  "work_item": { "ActivityCompleted": { ... } },
  "visible_at": 1706500000000,
  "lock_token": null,
  "locked_until": null,
  "attempt_count": 0,
  "created_at": 1706500000000
}
```

### Instance Lock (`lock.json`)

```json
{
  "token": "550e8400-e29b-41d4-a716-446655440001",
  "locked_at": 1706500000000,
  "expires_at": 1706500030000
}
```

---

## 5. Method Implementations

### `fetch_orchestration_item()`

```rust
async fn fetch_orchestration_item(
    &self,
    lock_timeout: Duration,
    _poll_timeout: Duration,  // Ignored - short polling
) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError> {
    let now = now_millis();
    
    // 1. Scan orchestrator queue for visible, unlocked messages
    let queue_dir = self.root.join("queues/orchestrator");
    let mut candidates: HashMap<String, Vec<QueueMessage>> = HashMap::new();
    
    for entry in fs::read_dir(&queue_dir)? {
        let path = entry?.path();
        let msg: QueueMessage = serde_json::from_str(&fs::read_to_string(&path)?)?;
        
        // Skip if not visible yet or locked
        if msg.visible_at > now {
            continue;
        }
        if let Some(until) = msg.locked_until {
            if until > now {
                continue;
            }
        }
        
        candidates.entry(msg.instance_id.clone())
            .or_default()
            .push(msg);
    }
    
    // 2. Find an instance we can lock
    for (instance_id, messages) in candidates {
        let instance_dir = self.root.join("instances").join(&instance_id);
        
        // Try to acquire instance lock
        let lock = match self.acquire_instance_lock(&instance_dir, lock_timeout) {
            Some(lock) => lock,
            None => continue,  // Someone else has the lock
        };
        
        // 3. Tag all visible messages with our lock token
        let mut tagged_messages = Vec::new();
        let mut max_attempt = 0;
        
        for mut msg in messages {
            msg.lock_token = Some(lock.token.clone());
            msg.locked_until = Some(now + lock_timeout.as_millis() as i64);
            msg.attempt_count += 1;
            max_attempt = max_attempt.max(msg.attempt_count);
            
            // Atomic update of queue file
            let path = queue_dir.join(format!("{}.json", msg.id));
            atomic_write(&path, &serde_json::to_vec(&msg)?)?;
            
            tagged_messages.push(msg.work_item);
        }
        
        // 4. Load instance metadata and history
        let meta_path = instance_dir.join("meta.json");
        let (orchestration_name, version, execution_id) = if meta_path.exists() {
            let meta: InstanceMeta = serde_json::from_str(&fs::read_to_string(&meta_path)?)?;
            (meta.orchestration_name, meta.orchestration_version, meta.current_execution_id)
        } else {
            // New instance - extract from StartOrchestration message
            extract_from_start_message(&tagged_messages)
        };
        
        let history = self.load_history(&instance_id, execution_id).await?;
        
        return Ok(Some((
            OrchestrationItem {
                instance: instance_id,
                orchestration_name,
                execution_id,
                version,
                history,
                messages: tagged_messages,
            },
            lock.token,
            max_attempt,
        )));
    }
    
    Ok(None)  // No work available
}
```

### `ack_orchestration_item()` - The Atomic Commit

```rust
async fn ack_orchestration_item(
    &self,
    lock_token: &str,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
    cancelled_activities: Vec<ScheduledActivityIdentifier>,
) -> Result<(), ProviderError> {
    let now = now_millis();
    
    // 1. Validate lock token
    let instance_id = self.get_instance_for_lock(lock_token)?
        .ok_or_else(|| ProviderError::permanent("ack", "Invalid lock token"))?;
    
    let instance_dir = self.root.join("instances").join(&instance_id);
    let lock_path = instance_dir.join("lock.json");
    
    let lock_info: LockInfo = serde_json::from_str(&fs::read_to_string(&lock_path)?)?;
    if lock_info.token != lock_token || lock_info.expires_at < now {
        return Err(ProviderError::permanent("ack", "Lock expired or invalid"));
    }
    
    // 2. Prepare all changes (compute first, write second)
    let mut pending_writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut pending_deletes: Vec<PathBuf> = Vec::new();
    
    // 2a. Update/create instance metadata
    let meta_path = instance_dir.join("meta.json");
    let meta = InstanceMeta {
        instance_id: instance_id.clone(),
        orchestration_name: metadata.orchestration_name.unwrap_or_default(),
        orchestration_version: metadata.orchestration_version,
        current_execution_id: execution_id,
        parent_instance_id: metadata.parent_instance_id,
        created_at: now,
        updated_at: now,
    };
    pending_writes.push((meta_path, serde_json::to_vec(&meta)?));
    
    // 2b. Create execution directory and append history
    let exec_dir = instance_dir.join("executions").join(execution_id.to_string());
    fs::create_dir_all(&exec_dir)?;
    
    let history_path = exec_dir.join("history.json");
    let mut history: HistoryFile = if history_path.exists() {
        serde_json::from_str(&fs::read_to_string(&history_path)?)?
    } else {
        HistoryFile { events: vec![] }
    };
    history.events.extend(history_delta);
    pending_writes.push((history_path, serde_json::to_vec(&history)?));
    
    // 2c. Update execution status
    if let Some(status) = &metadata.status {
        let state_path = exec_dir.join("state.json");
        let state = ExecutionState {
            execution_id,
            status: status.clone(),
            output: metadata.output.clone(),
            started_at: now,
            completed_at: Some(now),
        };
        pending_writes.push((state_path, serde_json::to_vec(&state)?));
    }
    
    // 2d. Enqueue worker items
    let worker_queue = self.root.join("queues/worker");
    for item in worker_items {
        let msg_id = Uuid::new_v4().to_string();
        let (inst, exec, act) = extract_activity_identity(&item);
        let msg = QueueMessage {
            id: msg_id.clone(),
            instance_id: inst,
            execution_id: Some(exec),
            activity_id: Some(act),
            work_item: item,
            visible_at: now,
            lock_token: None,
            locked_until: None,
            attempt_count: 0,
            created_at: now,
        };
        pending_writes.push((
            worker_queue.join(format!("{}.json", msg_id)),
            serde_json::to_vec(&msg)?,
        ));
    }
    
    // 2e. Enqueue orchestrator items (with delayed visibility for timers)
    let orch_queue = self.root.join("queues/orchestrator");
    for item in orchestrator_items {
        let msg_id = Uuid::new_v4().to_string();
        let visible_at = match &item {
            WorkItem::TimerFired { fire_at_ms, .. } => *fire_at_ms as i64,
            _ => now,
        };
        let msg = QueueMessage {
            id: msg_id.clone(),
            instance_id: extract_instance(&item),
            execution_id: None,
            activity_id: None,
            work_item: item,
            visible_at,
            lock_token: None,
            locked_until: None,
            attempt_count: 0,
            created_at: now,
        };
        pending_writes.push((
            orch_queue.join(format!("{}.json", msg_id)),
            serde_json::to_vec(&msg)?,
        ));
    }
    
    // 2f. Mark cancelled activities for deletion (AFTER enqueueing new ones)
    for cancelled in cancelled_activities {
        // Find matching worker queue entry
        for entry in fs::read_dir(&worker_queue)? {
            let path = entry?.path();
            let msg: QueueMessage = serde_json::from_str(&fs::read_to_string(&path)?)?;
            if msg.instance_id == cancelled.instance
                && msg.execution_id == Some(cancelled.execution_id)
                && msg.activity_id == Some(cancelled.activity_id)
            {
                pending_deletes.push(path);
                break;
            }
        }
    }
    
    // 2g. Delete processed messages from orchestrator queue
    for entry in fs::read_dir(&orch_queue)? {
        let path = entry?.path();
        let msg: QueueMessage = serde_json::from_str(&fs::read_to_string(&path)?)?;
        if msg.lock_token.as_deref() == Some(lock_token) {
            pending_deletes.push(path);
        }
    }
    
    // 3. Execute all writes atomically via rename
    for (path, data) in pending_writes {
        atomic_write(&path, &data)?;
    }
    
    // 4. Execute deletes
    for path in pending_deletes {
        let _ = fs::remove_file(&path);  // Ignore errors - idempotent
    }
    
    // 5. Release instance lock
    let _ = fs::remove_file(&lock_path);
    
    Ok(())
}
```

### Worker Queue Operations

```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    _poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError> {
    let now = now_millis();
    let queue_dir = self.root.join("queues/worker");
    
    // Find oldest visible, unlocked item (FIFO)
    let mut entries: Vec<_> = fs::read_dir(&queue_dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    
    for entry in entries {
        let path = entry.path();
        let mut msg: QueueMessage = serde_json::from_str(&fs::read_to_string(&path)?)?;
        
        // Skip if not visible or locked
        if msg.visible_at > now {
            continue;
        }
        if let Some(until) = msg.locked_until {
            if until > now {
                continue;
            }
        }
        
        // Lock the item
        let lock_token = Uuid::new_v4().to_string();
        msg.lock_token = Some(lock_token.clone());
        msg.locked_until = Some(now + lock_timeout.as_millis() as i64);
        msg.attempt_count += 1;
        
        atomic_write(&path, &serde_json::to_vec(&msg)?)?;
        
        return Ok(Some((msg.work_item, lock_token, msg.attempt_count)));
    }
    
    Ok(None)
}

async fn ack_work_item(
    &self,
    token: &str,
    completion: Option<WorkItem>,
) -> Result<(), ProviderError> {
    let now = now_millis();
    let worker_queue = self.root.join("queues/worker");
    let orch_queue = self.root.join("queues/orchestrator");
    
    // Find and delete the work item with this token
    let mut found = false;
    for entry in fs::read_dir(&worker_queue)? {
        let path = entry?.path();
        let msg: QueueMessage = serde_json::from_str(&fs::read_to_string(&path)?)?;
        
        if msg.lock_token.as_deref() == Some(token) {
            fs::remove_file(&path)?;
            found = true;
            break;
        }
    }
    
    if !found {
        return Err(ProviderError::permanent(
            "ack_work_item",
            "Lock token not found - activity was cancelled",
        ));
    }
    
    // Enqueue completion if provided
    if let Some(completion) = completion {
        let msg_id = Uuid::new_v4().to_string();
        let msg = QueueMessage {
            id: msg_id.clone(),
            instance_id: extract_instance(&completion),
            execution_id: None,
            activity_id: None,
            work_item: completion,
            visible_at: now,
            lock_token: None,
            locked_until: None,
            attempt_count: 0,
            created_at: now,
        };
        atomic_write(
            &orch_queue.join(format!("{}.json", msg_id)),
            &serde_json::to_vec(&msg)?,
        )?;
    }
    
    Ok(())
}
```

---

## 6. Atomicity Guarantees

### What IS Atomic

1. **Individual file writes** - Via rename pattern
2. **Lock acquisition** - Write + read-back verification
3. **Lock release** - Single file delete

### What is NOT Fully Atomic

The `ack_orchestration_item()` operation performs multiple file operations. If the process crashes mid-operation:

**Failure Scenarios and Recovery:**

| Failure Point | State | Recovery |
|---------------|-------|----------|
| Before any writes | Clean | Lock expires, messages refetched |
| After history write, before queue writes | Partial | Lock expires, messages refetched, duplicate events rejected |
| After queue writes, before message delete | Partial | Lock expires, messages refetched (idempotent) |
| After message delete, before lock release | Mostly complete | Lock file exists but lock expired, next fetch cleans up |

**Key Insight:** The runtime already handles duplicate events and idempotent replays. As long as:
1. Lock expiration allows recovery
2. Duplicate events are rejected
3. Queue items are eventually processed

The system will converge to correct state.

### Strengthening Atomicity (Optional)

For stricter atomicity, use a **write-ahead log**:

```rust
async fn ack_orchestration_item_with_wal(...) {
    // 1. Write all pending operations to WAL
    let wal_entry = WalEntry {
        token: lock_token,
        writes: pending_writes.clone(),
        deletes: pending_deletes.clone(),
    };
    atomic_write(&wal_path, &serde_json::to_vec(&wal_entry)?)?;
    
    // 2. Execute operations
    for (path, data) in pending_writes {
        atomic_write(&path, &data)?;
    }
    for path in pending_deletes {
        let _ = fs::remove_file(&path);
    }
    
    // 3. Delete WAL entry
    fs::remove_file(&wal_path)?;
}

// On startup, replay any incomplete WAL entries
async fn recover_from_wal(&self) {
    for entry in fs::read_dir(self.root.join("wal"))? {
        let wal: WalEntry = ...;
        // Re-execute operations (idempotent)
    }
}
```

---

## 7. Performance Considerations

### File System Operations per Method

| Method | Reads | Writes | Deletes |
|--------|-------|--------|---------|
| `fetch_orchestration_item` | O(Q) + O(H) | O(M) | 0 |
| `ack_orchestration_item` | 1 | O(H + W + O) | O(M + C) |
| `fetch_work_item` | O(Q) | 1 | 0 |
| `ack_work_item` | O(Q) | 0-1 | 1 |
| `read` | O(H) | 0 | 0 |

Where:
- Q = queue size
- H = history size
- M = messages in batch
- W = worker items
- O = orchestrator items
- C = cancelled activities

### Optimizations

1. **Index files** - Maintain `instances/_index.json` with instance list
2. **Queue ordering** - Use timestamp-prefixed filenames for natural FIFO
3. **Memory caching** - Cache frequently accessed metadata in memory
4. **Batch directory reads** - Use `readdir` batching where available

---

## 8. Comparison with SQLite Provider

| Aspect | File System | SQLite |
|--------|-------------|--------|
| Dependencies | None (std only) | sqlx, SQLite |
| Atomicity | Rename-based | ACID transactions |
| Concurrency | Per-instance locks | Database-level |
| Multi-process | Not supported | Supported (WAL mode) |
| Performance | Good for small scale | Better for large scale |
| Query capability | None | SQL queries |
| Setup complexity | Zero | Database file |
| Debugging | Human-readable JSON | Requires tooling |

---

## 9. Implementation Checklist

### Core Methods

- [ ] `fetch_orchestration_item()` - Instance lock + message tagging
- [ ] `ack_orchestration_item()` - Atomic commit via staged writes
- [ ] `abandon_orchestration_item()` - Release instance lock
- [ ] `renew_orchestration_item_lock()` - Extend lock timeout
- [ ] `read()` - Load history for latest execution
- [ ] `read_with_execution()` - Load history for specific execution
- [ ] `append_with_execution()` - Append events to history
- [ ] `enqueue_for_worker()` - Add to worker queue
- [ ] `fetch_work_item()` - Peek-lock from worker queue
- [ ] `ack_work_item()` - Delete + optional completion enqueue
- [ ] `abandon_work_item()` - Release work item lock
- [ ] `renew_work_item_lock()` - Extend work item lock
- [ ] `enqueue_for_orchestrator()` - Add to orchestrator queue

### Management Methods (ProviderAdmin)

- [ ] `list_instances()`
- [ ] `list_instances_by_status()`
- [ ] `list_executions()`
- [ ] `get_instance_info()`
- [ ] `get_execution_info()`
- [ ] `get_system_metrics()`
- [ ] `get_queue_depths()`
- [ ] `list_children()`
- [ ] `get_parent_id()`
- [ ] `delete_instances_atomic()`
- [ ] `prune_executions()`
- [ ] `prune_executions_bulk()`

### Testing

- [ ] Pass all provider validation tests
- [ ] Concurrent dispatcher stress tests
- [ ] Crash recovery tests
- [ ] Lock expiration tests

---

## 10. Recommended Structure

```rust
pub struct FileSystemProvider {
    root: PathBuf,
}

impl FileSystemProvider {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, io::Error> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("instances"))?;
        fs::create_dir_all(root.join("queues/orchestrator"))?;
        fs::create_dir_all(root.join("queues/worker"))?;
        Ok(Self { root })
    }
    
    pub fn new_temp() -> Result<Self, io::Error> {
        let dir = tempfile::tempdir()?;
        Self::new(dir.path())
    }
}
```

---

## 11. Open Questions

1. **File naming strategy** - UUID vs timestamp-prefixed vs sequential?
2. **Compression** - Worth compressing history files?
3. **Async file I/O** - Use tokio's spawn_blocking or dedicated thread pool?
4. **Lock file format** - JSON vs simpler format (just token + timestamp)?
5. **Index maintenance** - Eager vs lazy index updates?

---

## 12. Conclusion

This design provides a lightweight, zero-dependency provider suitable for:

- **Development** - Easy debugging with human-readable JSON
- **Testing** - No database setup required
- **Embedded use** - Minimal footprint
- **Learning** - Transparent internals

For production workloads with high concurrency or large scale, the SQLite provider remains the recommended choice due to its stronger atomicity guarantees and better performance characteristics.
