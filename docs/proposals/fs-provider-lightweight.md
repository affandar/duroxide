# Lightweight Filesystem Provider Proposal

**Status:** Draft  
**Author:** GitHub Copilot  
**Date:** 2026-01-31

## Executive Summary

This proposal describes an extremely lightweight filesystem-based provider for Duroxide that prioritizes minimal code size, low memory footprint, and transactional safety. The design assumes **a single runtime process** owns the data directory exclusively, which dramatically simplifies locking and transaction handling.

**Key simplification:** All locking is in-memory. On restart, locks reset automatically. No file-based locks needed for internal operations.

Ideal for:
- Embedded/edge deployments
- Development and testing
- Single-node deployments with low throughput
- Environments where SQLite is too heavy

---

## Goals

| Goal | Priority |
|------|----------|
| Minimal code size (~400-600 LOC) | 🔴 Critical |
| Low memory footprint | 🔴 Critical |
| Transactional safety (no data loss) | 🔴 Critical |
| Simple implementation | 🔴 Critical |
| Zero external dependencies beyond std | 🟡 High |
| Reasonable performance | 🟢 Nice to have |

## Non-Goals

- Multi-runtime / multi-process access
- High-throughput scenarios (use SQLite/PostgreSQL)
- Multi-node distributed deployments
- Advanced query capabilities
- Garbage collection / auto-pruning

---

## Design Overview

### Single-Runtime Guarantee

The provider acquires an **exclusive file lock** on `<root>/runtime.lock` at construction time:

```rust
impl FsProvider {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        
        // Acquire exclusive runtime lock - fails if another runtime is running
        let lock_file = File::create(root.join("runtime.lock"))?;
        flock(&lock_file, LOCK_EX | LOCK_NB)
            .map_err(|_| io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another runtime is already using this data directory"
            ))?;
        
        Ok(Self {
            root,
            _runtime_lock: lock_file,  // Held for lifetime of provider
            // ... in-memory lock state ...
        })
    }
}
```

**On crash:** OS releases the file lock → next runtime can acquire it.

### Directory Structure

```
<root>/
├── runtime.lock            # Exclusive lock file (held by running process)
├── instances/
│   └── <instance_id>/
│       ├── meta.json       # Instance metadata
│       └── exec_<n>/
│           ├── history.jsonl   # Append-only event log (one JSON per line)
│           └── status.json     # Execution status/output
└── queues/
    ├── orchestrator/
    │   └── <ulid>.json     # WorkItem files
    └── worker/
        └── <ulid>.json     # WorkItem files
```

Note: No per-instance lock files needed! All locking is in-memory.

### Key Design Decisions

#### 1. In-Memory Locking (The Big Simplification)

Since only one runtime can run at a time, all lock state lives in memory:

```rust
pub struct FsProvider {
    root: PathBuf,
    _runtime_lock: File,  // Keeps runtime.lock held
    
    // In-memory lock state - resets on restart
    locked_instances: Mutex<HashMap<String, LockState>>,
    locked_worker_items: Mutex<HashMap<String, PathBuf>>,  // lock_token -> file path
}

struct LockState {
    lock_token: String,
    locked_messages: Vec<PathBuf>,
}
```

**Why this works:**
- Single runtime = no concurrent access = no need for file locks
- On crash: process dies → all in-memory state gone → restart with clean slate
- On restart: no stale locks to clean up, just re-acquire from fresh state

#### 2. File-per-Message Queues

Each WorkItem is a separate file.

Because queues are **not wiped on restart**, we use two naming strategies:

1) **Idempotent (deterministic) filenames** for *derived* items that recovery may recreate
2) **Unique (ULID/UUID) filenames** for *external inputs* where duplicates are tolerated (at-least-once)

```
# Derived (deterministic): safe to "ensure present" during recovery
timer.inst-1.exec-1.id-42.fireat-1706745600000.json
can.inst-1.exec-1.json

# External (unique): supports at-least-once enqueue
01HQXYZ...ABC.json
01HQXYZ...DEF_v1706745600000.json  # With visibility timestamp
```

**Why files?**
- Atomic message addition (write temp → rename)
- Atomic message deletion (unlink)
- No corruption on crash
- Simple visibility delay via filename suffix

**Important:** Queue state is **not fully derivable** from history in Duroxide.

- Some queue items are **external inputs** (e.g., `StartOrchestration`, `ExternalRaised`, `CancelInstance`) and are **not** represented in history until they are actually processed.
- Some queue items are **completions** produced by workers/children and may exist in the orchestrator queue **before** the corresponding history event is appended.

Because of that, startup recovery must be **reconciling and additive** (recreate missing *derived* items), not “delete all queues and rebuild from history”.

#### 3. No File-Based Instance Locks Needed!

With single-runtime guarantee, instance locking is just a HashMap check:

```rust
fn try_lock_instance(&self, instance_id: &str, lock_token: &str, messages: Vec<PathBuf>) -> bool {
    let mut locked = self.locked_instances.lock().unwrap();
    if locked.contains_key(instance_id) {
        return false;  // Already locked by another turn in progress
    }
    locked.insert(instance_id.to_string(), LockState { 
        lock_token: lock_token.to_string(), 
        locked_messages: messages 
    });
    true
}

fn release_instance_lock(&self, lock_token: &str) {
    let mut locked = self.locked_instances.lock().unwrap();
    locked.retain(|_, state| state.lock_token != lock_token);
}
```

**On restart:** HashMap is empty → all instances available → clean slate.

#### 4. Append-Only History with JSONL

Events are appended as newline-delimited JSON:

```jsonl
{"OrchestrationStarted":{"event_id":1,"timestamp":"2026-01-31T12:00:00Z",...}}
{"ActivityScheduled":{"event_id":2,...}}
{"ActivityCompleted":{"event_id":3,...}}
```

**Why JSONL?**
- Append-only is just `write()` + `fsync()`
- No corruption on crash mid-write (partial line discarded on read)
- No need to parse entire file to append
- Streaming reads for large histories

#### 5. Atomic Writes via Rename

All writes follow the pattern:

```rust
// Write to temp file
let temp = format!("{}.tmp.{}", path, process_id);
write_all(&temp, data)?;
fsync(&temp)?;

// Atomic rename
rename(&temp, path)?;
fsync(parent_dir)?;
```

This guarantees either the old or new file exists, never a corrupted state.

#### 6. History-First Transaction Model

**Core Principle:** History is the source of truth. Everything else is derived state.

```
ack_orchestration_item():
  1. Append history_delta + fsync    ← COMMIT POINT (if this succeeds, turn is committed)
  2. Update status.json              ← Derived (can reconstruct from history)
  3. Update meta.json                ← Derived (can reconstruct from history)  
    4. Enqueue new items               ← Mostly derived (see notes below)
    5. Delete processed messages       ← Provider bookkeeping (queue cleanup)
  6. Release in-memory lock
```

**Crash at any point after step 1?** History is durable.

However, “reconstruct everything” only works if the runtime can recover:
- **Derived outbox items** that the runtime would have enqueued from scheduling events (timers, CAN follow-up, etc.)
- Without losing **inbox items** that are not in history yet (starts, external events, cancellations, completions)

**Startup Recovery:**

```rust
impl FsProvider {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        // ... acquire runtime lock ...
        
        // RECOVERY: Reconstruct all derived state from history
        Self::recover(&root)?;
        
        Ok(Self { /* ... */ })
    }
    
    fn recover(root: &Path) -> io::Result<()> {
        // 1. Scan all instances and reconcile *derived* queue state from history
        //
        // NOTE: Do NOT delete all queue files here.
        // The orchestrator/worker queues contain durable inputs (StartOrchestration,
        // ExternalRaised, CancelInstance) and in-flight completions that are not yet in history.
        // Deleting them would cause data loss and/or duplicate work.
        for instance_dir in fs::read_dir(root.join("instances"))? {
            let instance_id = instance_dir?.file_name();
            Self::recover_instance(root, &instance_id)?;
        }
        
        Ok(())
    }
    
    fn recover_instance(root: &Path, instance_id: &OsStr) -> io::Result<()> {
        // Read history for latest execution
        let (exec_id, history) = Self::read_latest_history(root, instance_id)?;
        
        // Check if terminal state
        if Self::is_terminal(&history) {
            return Ok(());  // Nothing to recover
        }
        
        // Reconcile derived work from history.
        //
        // Key constraints from the runtime:
        // - Activities are at-least-once. If users make activities idempotent (recommended),
        //   enqueueing a duplicate ActivityExecute after a crash is not a correctness problem
        //   (it is an efficiency/side-effects concern).
        // - HOWEVER: activities can be cancelled via lock stealing (deleted from worker queue)
        //   without a corresponding history event. A history-only rebuild can incorrectly
        //   resurrect work the runtime intended to cancel (e.g., select losers), which changes
        //   cancellation semantics.
        //
        // Therefore recovery policy choices:
        // - If you require cancellation to be strong (preferred), you must avoid resurrecting
        //   cancelled activities; that implies either persisting cancellation in history OR doing
        //   conservative queue-aware reconciliation.
        // - If you accept best-effort cancellation, you may rebuild worker items purely from
        //   history (at-least-once), knowing some cancelled work might re-run after a crash.
        let plan = Self::compute_recovery_plan_from_history(&history);
        Self::apply_recovery_plan(root, instance_id, exec_id, plan)?;
        
        Ok(())
    }
}

### What Can / Can’t Be Reconstructed From History

| Work / State | Can we reconstruct from history alone? | Notes |
|---|---:|---|
| `status.json` / terminal output | Yes | Terminal events are in history. |
| `meta.json` basics (name/version/parent) | Mostly | From `OrchestrationStarted` events (or first turn metadata). |
| `TimerFired` work items | Yes (with care) | Derive from `TimerCreated` without matching `TimerFired` *event*; duplicates are deduped by replay engine. |
| `ContinueAsNew` follow-up work item | Yes (with care) | Derive when latest execution ends with `OrchestrationContinuedAsNew` and next execution hasn’t started yet. |
| `ActivityExecute` work items | It depends | If you accept at-least-once activity execution and require idempotent activities, you can regenerate from `ActivityScheduled` minus completions. If you require strong cancellation semantics for select losers/terminal cleanup, you must not resurrect cancelled activities; without a cancellation event in history, that requires conservative reconciliation. |
| External inputs (`StartOrchestration`, `ExternalRaised`, `CancelInstance`) | **No** | These are not in history until processed; deleting queues loses them. |
| Worker/child completions sitting in orchestrator queue | **No** | They aren’t in history yet; deleting queues loses them. |
```

**Crash Safety Analysis (Simplified):**

| Crash After | History State | On Restart |
|-------------|---------------|------------|
| Step 1 (history) | ✅ Committed | Recovery reconciles missing *derived* messages (without deleting queues) |
| Step 2-5 | ✅ Committed | Recovery cleans temp files and reconciles derived messages |
| Before step 1 | ❌ Not committed | Original messages still in queue, retry turn |

**Why This Is Better:**

| Aspect | Old Approach (Idempotent Ops) | New Approach (History-First) |
|--------|------------------------------|------------------------------|
| Normal operation | Check every op for idempotency | Just write, no checks |
| Queue filenames | Content hash (complex) | Deterministic names for derived items; unique names for external inputs |
| Crash handling | Each operation must be idempotent | Recovery pass re-adds missing derived items without wiping queues |
| Code complexity | Higher (idempotency logic everywhere) | Lower (recovery in one place) |
| Startup cost | None | O(instances) scan, but only once |

---

## Core Data Structures

### Provider State (In-Memory)

```rust
pub struct FsProvider {
    root: PathBuf,
    _runtime_lock: File,  // Held for provider lifetime
    
    // Lock state - all in memory, resets on restart
    locked_instances: Mutex<HashMap<String, LockState>>,
    locked_worker_items: Mutex<HashMap<String, PathBuf>>,
}

struct LockState {
    lock_token: String,
    locked_messages: Vec<PathBuf>,
}
```

### Instance Metadata (`meta.json`)

```json
{
  "instance_id": "order-123",
  "orchestration_name": "ProcessOrder",
  "orchestration_version": "1.0.0",
  "current_execution_id": 1,
  "parent_instance_id": null,
  "created_at": "2026-01-31T12:00:00Z"
}
```

### Execution Status (`status.json`)

```json
{
  "status": "Running",
  "output": null
}
```

Or when completed:

```json
{
  "status": "Completed",
  "output": "{\"result\": \"success\"}"
}
```

### Queue Item Files

Each file contains a single `WorkItem` as JSON:

```json
{
  "StartOrchestration": {
    "instance": "order-123",
    "orchestration": "ProcessOrder",
    "input": "{}",
    "version": null,
    "parent_instance": null,
    "parent_id": null,
    "execution_id": 1
  }
}
```

---

## API Implementation

### Provider Construction

```rust
pub struct FsProvider {
    root: PathBuf,
    _runtime_lock: File,  // Exclusive lock on runtime.lock
    
    // In-memory lock state (resets on restart)
    locked_instances: Mutex<HashMap<String, LockState>>,
    locked_worker_items: Mutex<HashMap<String, PathBuf>>,
}

struct LockState {
    lock_token: String,
    locked_messages: Vec<PathBuf>,
}

impl FsProvider {
    /// Create a new filesystem provider.
    /// 
    /// Performs crash recovery on startup:
    /// 1. Acquires exclusive runtime lock (fails if another runtime is running)
    /// 2. Cleans up partial temp files (safe to delete)
    /// 3. Reconciles *derived* queue items from history (without wiping queues)
    /// 
    /// # Errors
    /// Returns error if another runtime is already using this directory.
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("instances"))?;
        fs::create_dir_all(root.join("queues/orchestrator"))?;
        fs::create_dir_all(root.join("queues/worker"))?;
        
        // Acquire exclusive runtime lock
        let lock_file = File::create(root.join("runtime.lock"))?;
        Self::try_flock(&lock_file)?;
        
        // ═══════════════════════════════════════════════════════════
        // CRASH RECOVERY: Reconstruct derived state from history
        // ═══════════════════════════════════════════════════════════
        Self::recover(&root)?;
        
        Ok(Self {
            root,
            _runtime_lock: lock_file,
            locked_instances: Mutex::new(HashMap::new()),
            locked_worker_items: Mutex::new(HashMap::new()),
        })
    }
    
    /// Crash recovery: reconstruct queue state from history
    fn recover(root: &Path) -> io::Result<()> {
        // 1. Clean up safe-to-delete temp files (from interrupted atomic rename flows)
        Self::cleanup_temp_files(&root.join("queues/orchestrator"))?;
        Self::cleanup_temp_files(&root.join("queues/worker"))?;
        Self::cleanup_temp_files(&root.join("instances"))?;
        
        // 2. Scan all instances and reconcile derived state
        let instances_dir = root.join("instances");
        if !instances_dir.exists() {
            return Ok(());
        }
        
        for entry in fs::read_dir(&instances_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                Self::recover_instance(root, &entry.file_name())?;
            }
        }
        
        Ok(())
    }
    
    fn recover_instance(root: &Path, instance_id: &OsStr) -> io::Result<()> {
        // Implementation shown in section 6 above
        // Reads history, computes pending work, re-enqueues items
        todo!()
    }
    
    fn cleanup_temp_files(dir: &Path) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        // Best-effort: delete files that match our temp naming convention.
        // Any failure here should not prevent startup; worst case we leave junk behind.
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str())
                && name.contains(".tmp.")
            {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }
}
```

#### `fetch_orchestration_item`

```rust
async fn fetch_orchestration_item(
    &self,
    _lock_timeout: Duration,  // Unused - in-memory locks don't expire
    _poll_timeout: Duration,
) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError> {
    // 1. Find oldest visible message (no queue lock needed - single runtime!)
    let msg_path = self.find_oldest_visible("orchestrator")?;
    let Some(msg_path) = msg_path else {
        return Ok(None);
    };
    
    // 2. Read WorkItem, extract instance_id
    let work_item: WorkItem = self.read_json(&msg_path)?;
    let instance_id = work_item.instance_id();
    
    // 3. Check if instance is already locked (in-memory check)
    let lock_token = Uuid::new_v4().to_string();
    
    // 4. Collect ALL visible messages for this instance
    let messages = self.collect_instance_messages("orchestrator", &instance_id)?;
    
    // 5. Try to acquire in-memory instance lock
    if !self.try_lock_instance(&instance_id, &lock_token, messages.clone()) {
        return Ok(None);  // Instance busy, will retry
    }
    
    // 6. Load instance metadata and history (may not exist for new instances)
    let (meta, history) = self.load_instance_state(&instance_id)?;
    
    Ok(Some((
        OrchestrationItem {
            instance: instance_id,
            orchestration_name: meta.orchestration_name,
            execution_id: meta.current_execution_id,
            version: meta.orchestration_version,
            history,
            messages,
        },
        lock_token,
        0, // attempt_count (could track in a separate file)
    )))
}
```

#### `ack_orchestration_item`

```rust
async fn ack_orchestration_item(
    &self,
    lock_token: String,
    execution_id: u64,
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
) -> Result<(), ProviderError> {
    // 1. Get locked state from in-memory map
    let lock_state = {
        let locked = self.locked_instances.lock().unwrap();
        locked.values()
            .find(|s| s.lock_token == lock_token)
            .cloned()
            .ok_or(ProviderError::invalid_lock_token())?
    };
    
    let instance_id = extract_instance_id(&lock_state.locked_messages[0])?;
    
    // ═══════════════════════════════════════════════════════════════
    // STEP 1: COMMIT POINT - Append history + fsync
    // If this succeeds, the turn is committed. Everything else is
    // derived state that will be reconstructed on startup if we crash.
    // ═══════════════════════════════════════════════════════════════
    self.append_history(&instance_id, execution_id, &history_delta)?;
    
    // ═══════════════════════════════════════════════════════════════
    // STEPS 2-6: Best-effort derived state updates
    // If we crash here, startup recovery will reconstruct from history.
    // ═══════════════════════════════════════════════════════════════
    
    // 2. Update execution status (derived from history's terminal events)
    if metadata.status.is_some() {
        let _ = self.write_status(&instance_id, execution_id, &metadata);
    }
    
    // 3. Update instance metadata
    let _ = self.update_instance_meta(&instance_id, execution_id, &metadata);
    
    // 4. Enqueue worker items (derived from ActivityScheduled events)
    for item in &worker_items {
        let _ = self.enqueue("worker", item, None);
    }
    
    // 5. Enqueue orchestrator items (derived from scheduled events)
    for item in &orchestrator_items {
        let visible_at = match item {
            WorkItem::TimerFired { fire_at_ms, .. } => Some(*fire_at_ms),
            _ => None,
        };
        let _ = self.enqueue("orchestrator", item, visible_at);
    }
    
    // 6. Delete processed messages
    for msg_path in &lock_state.locked_messages {
        let _ = std::fs::remove_file(msg_path);
    }
    
    // 7. Release in-memory instance lock
    self.release_instance_lock(&lock_token);
    
    Ok(())
}

/// Append-only history write - THE commit point
fn append_history(&self, instance: &str, exec_id: u64, events: &[Event]) -> io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    
    let dir = self.instance_exec_dir(instance, exec_id);
    fs::create_dir_all(&dir)?;
    
    let path = dir.join("history.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    
    for event in events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }
    
    file.sync_all()?;  // CRITICAL: fsync makes it durable
    Ok(())
}
```

#### `abandon_orchestration_item`

```rust
async fn abandon_orchestration_item(
    &self,
    lock_token: String,
) -> Result<(), ProviderError> {
    // Just release the in-memory lock - messages remain in queue
    self.release_instance_lock(&lock_token);
    Ok(())
}
```

#### `fetch_work_item`

```rust
async fn fetch_work_item(
    &self,
    _lock_timeout: Duration,
    _poll_timeout: Duration,
) -> Result<Option<(WorkItem, String, u32, ExecutionState)>, ProviderError> {
    // 1. Find oldest visible message (no file lock needed - single runtime!)
    let msg_path = self.find_oldest_visible("worker")?;
    let Some(msg_path) = msg_path else {
        return Ok(None);
    };
    
    // 2. Read the work item
    let work_item: WorkItem = self.read_json(&msg_path)?;
    let lock_token = Uuid::new_v4().to_string();
    
    // 3. Track in-memory that this item is being processed
    {
        let mut locked = self.locked_worker_items.lock().unwrap();
        locked.insert(lock_token.clone(), msg_path.clone());
    }
    
    Ok(Some((work_item, lock_token, 0, ExecutionState::Running)))
}
```

#### `ack_work_item`

```rust
async fn ack_work_item(
    &self,
    lock_token: String,
    completion: Option<WorkItem>,  // ActivityCompleted or ActivityFailed
) -> Result<(), ProviderError> {
    // 1. Get the locked file path
    let msg_path = {
        let mut locked = self.locked_worker_items.lock().unwrap();
        locked.remove(&lock_token)
            .ok_or(ProviderError::invalid_lock_token())?
    };
    
    // 2. Enqueue completion to orchestrator queue (if provided)
    if let Some(completion) = completion {
        self.enqueue("orchestrator", &completion, None)?;
    }
    
    // 3. Delete the processed message
    std::fs::remove_file(&msg_path)?;
    
    Ok(())
}
```

---

## Memory Footprint Analysis

| Component | Memory Usage |
|-----------|-------------|
| `FsProvider` struct | ~100 bytes |
| `_runtime_lock` File handle | ~8 bytes |
| `locked_instances` HashMap (10 items) | ~1 KB |
| `locked_worker_items` HashMap (10 items) | ~500 bytes |
| Per-message processing buffer | ~4 KB |
| **Total steady-state** | **~6 KB** |

Compare to SQLite provider:
- SQLite connection pool: ~50 KB
- Statement cache: ~20 KB  
- WAL buffers: ~64 KB
- **Total: ~150 KB+**

---

## Code Size Estimate

| Component | Lines of Code |
|-----------|--------------|
| Core struct and construction | ~60 |
| Startup recovery (reconstruct from history) | ~100 |
| In-memory lock helpers | ~30 |
| `fetch_orchestration_item` | ~40 |
| `ack_orchestration_item` | ~50 |
| `abandon_orchestration_item` | ~5 |
| `fetch_work_item` / `ack_work_item` | ~40 |
| `read` / history helpers | ~40 |
| Queue operations (enqueue, find_oldest) | ~40 |
| File I/O helpers | ~30 |
| **Total** | **~435 LOC** |

---

## Limitations & Trade-offs

### 1. Single Runtime Only (By Design!)

Only one runtime process can use the data directory at a time. This is enforced by the `runtime.lock` file. For multi-process scenarios, use SQLite or PostgreSQL.

**This is a feature, not a bug!** Single runtime enables in-memory locking.

### 2. Startup Cost

Recovery scans all instances to reconstruct queue state. For large instance counts (10K+), this adds startup latency.

**Mitigation:** 
- Cache instance count is typically small for this provider's use case
- Could add optional skip-recovery flag for clean startups

### 3. Directory Scanning Overhead

Finding the oldest message requires `readdir()` on every fetch. For queues with many messages (1000+), this becomes slow.

**Mitigation:** For high throughput, use SQLite. This provider is designed for low-throughput scenarios.

### 4. Platform-Specific Runtime Lock

`flock()` behavior varies across platforms:
- Linux/macOS: Works reliably via `libc::flock()`
- Windows: Use `LockFileEx` via `fs2` crate or similar
- NFS: May not work reliably (use SQLite for network storage)

---

## Minimal Feature Set

### Implemented (Core Only)

| Feature | Status |
|---------|--------|
| `fetch_orchestration_item` | ✅ |
| `ack_orchestration_item` | ✅ |
| `abandon_orchestration_item` | ✅ |
| `fetch_work_item` | ✅ |
| `ack_work_item` | ✅ |
| `enqueue_for_orchestrator` | ✅ |
| `enqueue_for_worker` | ✅ |
| `read` (history) | ✅ |
| Delayed visibility (timers) | ✅ |
| In-memory instance locking | ✅ |
| Single-runtime enforcement | ✅ |

### Not Implemented (Use SQLite)

| Feature | Rationale |
|---------|-----------|
| `list_instances` | Directory listing is trivial if needed |
| `list_executions` | Same |
| Bulk deletion | Manual cleanup acceptable |
| Lock renewal | Not needed - in-memory locks |
| Attempt count tracking | Adds complexity |
| Management API | Out of scope |
| Multi-runtime support | Use SQLite |

---

## Implementation Plan

### Phase 1: Core Implementation (~1.5 days)

1. Directory structure creation
2. Runtime lock acquisition (flock)
3. Atomic write helpers (temp + rename)
4. JSONL append and read
5. In-memory lock structures

### Phase 2: Provider Trait (~1.5 days)

1. `fetch_orchestration_item`
2. `ack_orchestration_item`
3. `abandon_orchestration_item`
4. `fetch_work_item` / `ack_work_item`
5. `enqueue_*` methods

### Phase 3: Testing (~0.5 days)

1. Port provider validation tests
2. Crash recovery testing (kill -9 during ack)
3. Restart behavior validation

---

## Example Usage

```rust
use duroxide::{FsProvider, RuntimeBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create provider with data directory
    // Fails if another runtime is already using this directory
    let provider = FsProvider::new("./duroxide-data")?;
    
    // Build runtime (single-threaded friendly)
    let runtime = RuntimeBuilder::new()
        .with_provider(provider)
        .with_concurrency(1, 1)  // 1 orch, 1 worker
        .build()
        .await?;
    
    // ... register orchestrations and activities ...
    
    runtime.run().await
}
```

---

## Alternatives Considered

### 1. Single Queue File with Record Locking

Pros: Faster scanning, single file  
Cons: Complex corruption recovery, larger memory footprint

### 2. SQLite with Minimal Schema

Pros: Battle-tested, ACID  
Cons: Still ~150KB+ footprint, external dependency

### 3. Memory-Mapped Files

Pros: Very fast  
Cons: Complex, platform-specific, corruption risks

### 4. File-Based Peek-Lock (No Single-Runtime Assumption)

Pros: Supports multiple runtimes  
Cons: Much more complex, requires lock expiration, file-based lock state

---

## Conclusion

This proposal describes a filesystem provider that achieves:

- **~435 lines of code** (vs ~2000+ for SQLite provider)
- **~6 KB memory** (vs ~150 KB+ for SQLite)
- **History-first crash safety** - history is the commit point, everything else is reconstructed
- **Zero dependencies** beyond Rust std library
- **Trivial locking** via in-memory state (single-runtime guarantee)

**The key insight:** By making history the single source of truth and reconstructing all derived state (queues, metadata) on startup, we eliminate the need for complex idempotency checks during normal operation. The provider is simple, correct, and efficient.

The trade-off is limited to single-runtime deployments, making it ideal for embedded, edge, and development scenarios where simplicity trumps multi-process support.

---

## Appendix A: History-First Transaction Model Summary

| Aspect | Traditional DB Provider | This Provider |
|--------|------------------------|---------------|
| Commit point | DB transaction commit | `fsync(history.jsonl)` |
| Queue state | Persisted in DB | Ephemeral, reconstructed from history |
| Crash recovery | Relies on DB recovery | Explicit `recover()` on startup |
| Normal operation | Multi-step transaction | Single append + best-effort updates |
| Complexity | High (transactions, locks) | Low (append + reconstruct) |

---

## Appendix B: Single-Runtime Locking Summary

| Aspect | Traditional (Multi-Process) | This Design (Single-Runtime) |
|--------|----------------------------|------------------------------|
| Runtime lock | None | `flock()` on `runtime.lock` |
| Instance locks | File-based with expiration | In-memory HashMap |
| Worker item locks | File rename + expiration | In-memory HashMap |
| On crash | Stale lock cleanup needed | OS releases flock, HashMap gone |
| On restart | Parse lock files, check expiration | Start fresh, recover from history |
| Complexity | High | Very low |
| Code size | ~200 LOC for locks | ~30 LOC for locks |

---

## Appendix C: Atomic Operations Reference

### Append to JSONL (Crash-Safe)

```rust
fn append_event(path: &Path, event: &Event) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    Ok(())
}
```

### Atomic File Write (Rename Pattern)

```rust
fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    
    let mut file = File::create(&temp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    
    std::fs::rename(&temp, path)?;
    
    // Sync parent directory for durability
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    
    Ok(())
}
```

### Runtime Lock Acquisition

```rust
#[cfg(unix)]
fn acquire_runtime_lock(path: &Path) -> io::Result<File> {
    use std::os::unix::io::AsRawFd;
    
    let file = File::create(path)?;
    let fd = file.as_raw_fd();
    
    let result = unsafe {
        libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)
    };
    
    if result == 0 {
        Ok(file)  // Lock acquired, hold file for lifetime
    } else {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another runtime is already using this data directory"
        ))
    }
}
```
