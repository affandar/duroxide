# Provider Interface Minimization Analysis

**Goal:** Reduce Provider trait to essential methods only  
**Date:** 2025-10-10

---

## Current Provider Interface (18 methods)

### Hot Path (Called Frequently)

1. ✅ **`fetch_orchestration_item()`** - ESSENTIAL
   - Called continuously by orchestration dispatcher
   - Cannot be replaced

2. ✅ **`ack_orchestration_item()`** - ESSENTIAL
   - Called after every orchestration turn
   - Atomic commit boundary
   - Cannot be replaced

3. ✅ **`dequeue_worker_peek_lock()`** - ESSENTIAL
   - Called continuously by work dispatcher
   - Cannot be replaced

4. ✅ **`ack_worker()`** - ESSENTIAL
   - Called after every activity execution
   - Cannot be replaced

---

### Control Plane (Called Occasionally)

5. ✅ **`enqueue_orchestrator_work()`** - ESSENTIAL
   - Used by Client (raise_event, cancel_instance, start)
   - Used by Runtime (dispatch completions)
   - Cannot be replaced without breaking Client API

6. ⚠️ **`enqueue_worker_work()`** - ESSENTIAL (but rarely called standalone)
   - Called from dispatch.rs for activity scheduling
   - Could be absorbed into ack_orchestration_item
   - Keep for now (useful for testing)

7. ⚠️ **`abandon_orchestration_item()`** - NEEDED for robustness
   - Called on ack failures (database busy, etc.)
   - Enables automatic retry
   - Could theoretically rely on lock expiration alone
   - Keep for explicit retry control

---

### History Access (Called by Client)

8. ✅ **`read()`** - ESSENTIAL for Client
   - Used by get_orchestration_status()
   - Used by wait_for_orchestration()
   - Cannot be replaced

9. 🟡 **`read_with_execution()`** - OPTIONAL (has default)
   - Only used for debugging/testing specific executions
   - Default implementation delegates to read()
   - **CANDIDATE FOR REMOVAL** (or keep as optional)

10. 🟡 **`latest_execution_id()`** - OPTIONAL (has default)
    - Used by Runtime to track execution IDs
    - Default implementation uses read()
    - **CANDIDATE FOR REMOVAL** (or keep as optional)

11. ⚠️ **`append_with_execution()`** - USED but could be private
    - Called internally by ack_orchestration_item
    - Not called standalone in hot path
    - **CANDIDATE: Make it required but remove from public interface**

---

### Timer Queue (Conditional)

12. ✅ **`supports_delayed_visibility()`** - ESSENTIAL (selector)
    - Determines timer implementation
    - Must keep

13. 🟡 **`enqueue_timer_work()`** - CONDITIONAL
    - Only called if supports_delayed_visibility=true
    - Already has default panic implementation
    - Keep as-is

14. 🟡 **`dequeue_timer_peek_lock()`** - CONDITIONAL
    - Only called if supports_delayed_visibility=true
    - Already has default None implementation
    - Keep as-is

15. 🟡 **`ack_timer()`** - CONDITIONAL
    - Only called if supports_delayed_visibility=true
    - Already has default panic implementation
    - Keep as-is

---

### Management/Debug APIs (Rarely Used)

16. 🔴 **`create_new_execution()`** - DEPRECATED
    - Used to create events (violates design principles!)
    - Runtime now handles via ExecutionMetadata
    - **STRONG CANDIDATE FOR REMOVAL**

17. 🟡 **`list_instances()`** - OPTIONAL (has default)
    - Only for testing/debugging
    - Default returns empty Vec
    - Keep as optional

18. 🟡 **`list_executions()`** - OPTIONAL (has default)
    - Only for testing/debugging
    - Default returns vec![1]
    - Keep as optional

---

## Analysis by Usage Pattern

### Actually Called by Runtime (Core Loop)

```
CRITICAL PATH:
- fetch_orchestration_item()       [orchestration dispatcher - continuous]
- ack_orchestration_item()          [after every turn]
- dequeue_worker_peek_lock()        [work dispatcher - continuous]
- ack_worker()                      [after every activity]
- enqueue_orchestrator_work()       [dispatch completions]
- abandon_orchestration_item()      [error recovery]

CONDITIONAL (if supports_delayed_visibility):
- dequeue_timer_peek_lock()
- ack_timer()

CONTROL PLANE:
- enqueue_orchestrator_work()       [Client API: start, raise_event, cancel]
- read()                            [Client API: get_status, wait]
```

### Never Called by Runtime (Only Tests/Debugging)

```
- list_instances()        [has default - empty Vec]
- list_executions()       [has default - vec![1]]
- read_with_execution()   [has default - delegates to read()]
```

### Questionable (Used but could be redesigned)

```
- append_with_execution()     [called by ack, could be private]
- latest_execution_id()       [runtime caches this anyway]
- enqueue_worker_work()       [only called from dispatch helpers]
- create_new_execution()      [DEPRECATED - violates design]
```

---

## Minimization Proposals

### Option 1: Aggressive Minimization (8 core methods)

**Remove entirely:**
- ❌ `create_new_execution()` - Runtime handles via metadata
- ❌ `list_instances()` - Not essential, providers can offer as extension
- ❌ `list_executions()` - Not essential, providers can offer as extension
- ❌ `read_with_execution()` - Not essential, providers can offer as extension
- ❌ `latest_execution_id()` - Runtime can track in memory
- ❌ `append_with_execution()` - Make it impl detail of ack

**Keep minimal interface:**
```rust
trait Provider {
    // Core orchestration ops
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    async fn ack_orchestration_item(..., metadata: ExecutionMetadata) -> Result<(), String>;
    async fn abandon_orchestration_item(lock_token: &str, delay_ms: Option<u64>) -> Result<(), String>;
    
    // History
    async fn read(&self, instance: &str) -> Vec<Event>;
    
    // Worker queue
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)>;
    async fn ack_worker(&self, token: &str) -> Result<(), String>;
    
    // Orchestrator queue
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
    
    // Timer support (conditional)
    fn supports_delayed_visibility(&self) -> bool { false }
    // + 3 timer methods (conditional)
}
```

**Result:** 8 required + 3 conditional = **11 total methods** (down from 18)

---

### Option 2: Moderate Minimization (Keep useful optionals)

**Remove:**
- ❌ `create_new_execution()` - Truly deprecated, violates design
- ❌ `append_with_execution()` - Make private to ack implementation

**Keep but mark as optional:**
- 🟡 `list_instances()` - Useful for debugging
- 🟡 `list_executions()` - Useful for ContinueAsNew debugging
- 🟡 `read_with_execution()` - Useful for multi-execution debugging
- 🟡 `latest_execution_id()` - Useful optimization point

**Keep with better defaults:**
- 🟡 `enqueue_worker_work()` - Rarely standalone, but useful for testing

**Result:** 8 required + 4 optional + 3 conditional = **15 total methods** (down from 18)

---

### Option 3: Radical Simplification (Generic Event Store)

**Redesign as pure event store + queue:**

```rust
trait Provider {
    // Generic event log (no orchestration knowledge)
    async fn append_events(&self, entity: &str, partition: u64, events: Vec<Event>) -> Result<(), String>;
    async fn read_events(&self, entity: &str, partition: u64) -> Vec<Event>;
    async fn read_latest_partition(&self, entity: &str) -> Vec<Event>;
    
    // Generic queue with peek-lock
    async fn enqueue(&self, queue: QueueType, item: Vec<u8>, visible_after_ms: Option<u64>) -> Result<(), String>;
    async fn dequeue(&self, queue: QueueType) -> Option<(Vec<u8>, LockToken)>;
    async fn ack(&self, lock_token: LockToken) -> Result<(), String>;
    async fn abandon(&self, lock_token: LockToken, delay_ms: Option<u64>) -> Result<(), String>;
    
    // Atomic batch operations
    async fn atomic_commit(&self, ops: Vec<Operation>) -> Result<(), String>;
}

enum QueueType { Orchestrator, Worker, Timer }
enum Operation {
    AppendEvents { entity: String, partition: u64, events: Vec<Event> },
    Enqueue { queue: QueueType, item: Vec<u8> },
    UpdateMetadata { entity: String, partition: u64, metadata: HashMap<String, String> },
}
```

**Benefits:**
- Truly generic (not Duroxide-specific)
- Minimal surface area (7 methods)
- Clean abstraction
- Reusable for any event-sourced system

**Drawbacks:**
- Large refactor
- Loses some optimizations
- More work for provider implementers

---

## Specific Redundancies Identified

### 1. OrchestrationItem Contains Too Much Metadata

```rust
// Current
pub struct OrchestrationItem {
    pub instance: String,
    pub orchestration_name: String,  // ← Can derive from history
    pub execution_id: u64,           // ← Can derive from history
    pub version: String,             // ← Can derive from history
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
}

// Minimal
pub struct OrchestrationItem {
    pub instance: String,
    pub history: Vec<Event>,          // Runtime derives metadata from history
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
}
```

**Benefit:** Provider doesn't need to track orchestration_name, version, execution_id

**Cost:** Runtime does O(1) scan of history to extract metadata

---

### 2. ExecutionMetadata is a Workaround

**Current design:**
- Runtime computes metadata (status, output, next_execution_id)
- Provider stores it

**Alternative: Generic key-value metadata**
```rust
// Instead of ExecutionMetadata struct
type Metadata = HashMap<String, String>;

// Runtime provides
let metadata = hashmap!{
    "status" => "Completed",
    "output" => "result",
    "create_partition" => "2",
};

provider.ack(..., metadata);

// Provider stores blindly
for (key, value) in metadata {
    UPDATE metadata SET value WHERE key
}
```

**Benefit:** Even more generic, extensible without trait changes

**Cost:** Type safety lost (strings vs structured data)

---

### 3. Three Separate Ack Methods (Could Be One)

**Current:**
```rust
async fn ack_worker(&self, token: &str) -> Result<(), String>;
async fn ack_timer(&self, token: &str) -> Result<(), String>;
async fn ack_orchestration_item(&self, token: &str, ...) -> Result<(), String>;
```

**Alternative: Unified ack**
```rust
async fn ack(&self, token: &str, changes: Option<Changes>) -> Result<(), String>;

struct Changes {
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    timer_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
}

// Worker ack: ack(token, None)
// Orchestration ack: ack(token, Some(changes))
```

**Benefit:** Fewer methods, simpler trait

**Cost:** Less type safety (None vs full Changes)

---

### 4. Enqueue Methods Could Be Unified

**Current:**
```rust
async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String>;
async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String>;
async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
```

**Alternative:**
```rust
async fn enqueue(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;

// Provider routes based on WorkItem variant:
match item {
    WorkItem::ActivityExecute { .. } => enqueue_to_worker_queue(),
    WorkItem::TimerSchedule { .. } => enqueue_to_timer_queue(),
    _ => enqueue_to_orchestrator_queue(),
}
```

**Benefit:** Single enqueue method, provider determines routing

**Cost:** Provider must pattern-match on WorkItem (but already does this)

---

## Recommendation: Two-Phase Approach

### Phase 1: Quick Wins (Low Risk)

**Remove immediately:**
1. ❌ `create_new_execution()` - Already deprecated, violates design
   - Runtime handles via ExecutionMetadata.create_next_execution
   - No code calls this anymore

**Make private/internal:**
2. 🔒 `append_with_execution()` - Implementation detail
   - Only called from within ack_orchestration_item
   - Providers can keep it private

**Result:** 18 → 16 methods

---

### Phase 2: Simplify Optional Methods (Medium Risk)

**Consolidate metadata from OrchestrationItem:**
3. 🔄 Remove `orchestration_name`, `version`, `execution_id` from OrchestrationItem
   - Runtime derives from history (first OrchestrationStarted event)
   - Reduces provider responsibility

**Simplify history:**
4. 🔄 Make `read_with_execution()` a Provider extension trait
5. 🔄 Make `latest_execution_id()` a Provider extension trait
6. 🔄 Make `list_instances()` a Provider extension trait
7. 🔄 Make `list_executions()` a Provider extension trait

```rust
// Core trait (required)
#[async_trait]
trait Provider {
    // 8 essential methods only
}

// Extension trait (optional debugging features)
#[async_trait]
trait ProviderExt: Provider {
    async fn read_with_execution(...) { ... }
    async fn latest_execution_id(...) { ... }
    async fn list_instances() { ... }
    async fn list_executions(...) { ... }
}
```

**Result:** 8 required + 4 optional = **12 total** (down from 16)

---

### Phase 3: Radical Redesign (High Risk, High Reward)

**Redesign as generic storage primitives:**

```rust
trait EventStore {
    // Append-only log
    async fn append(&self, entity: &str, partition: u64, events: Vec<Event>) -> Result<(), String>;
    async fn read(&self, entity: &str, partition: u64) -> Vec<Event>;
    async fn latest_partition(&self, entity: &str) -> u64;
}

trait MessageQueue {
    // Peek-lock queue
    async fn enqueue(&self, queue: &str, message: Vec<u8>, visible_after_ms: Option<u64>) -> Result<(), String>;
    async fn dequeue(&self, queue: &str) -> Option<(Vec<u8>, LockToken)>;
    async fn ack(&self, token: LockToken) -> Result<(), String>;
    async fn abandon(&self, token: LockToken, delay_ms: Option<u64>) -> Result<(), String>;
}

trait Provider: EventStore + MessageQueue {
    // Atomic batch (ties it together)
    async fn atomic_batch(&self, operations: Vec<Operation>) -> Result<(), String>;
}

enum Operation {
    AppendEvents { entity: String, partition: u64, events: Vec<Event> },
    UpdateMetadata { entity: String, partition: u64, metadata: HashMap<String, String> },
    Enqueue { queue: String, message: Vec<u8>, visible_after_ms: Option<u64> },
    DeleteMessages { lock_token: LockToken },
}
```

**Benefits:**
- Truly generic (works for any event-sourced system)
- Clean separation (events vs queues vs atomicity)
- Provider has zero orchestration knowledge
- Minimal interface (5 methods total)

**Drawbacks:**
- Breaking change (all providers need rewrite)
- Runtime layer becomes more complex
- May lose optimizations (batching, indexes)

**Result:** **5 core methods** (most minimal possible)

---

## Concrete Simplification Options

### Quick Win #1: Remove create_new_execution

```diff
trait Provider {
-   async fn create_new_execution(...) -> Result<u64, String>;
}
```

**Impact:** 
- No runtime code calls this
- ExecutionMetadata.create_next_execution replaces it
- Removal is safe

**Effort:** Delete method, update docs

---

### Quick Win #2: Simplify OrchestrationItem

```diff
pub struct OrchestrationItem {
    pub instance: String,
-   pub orchestration_name: String,
-   pub execution_id: u64,
-   pub version: String,
    pub history: Vec<Event>,
    pub messages: Vec<WorkItem>,
    pub lock_token: String,
}
```

**Runtime derives metadata:**
```rust
let first_start = item.history.iter()
    .find_map(|e| match e {
        Event::OrchestrationStarted { name, version, .. } => Some((name, version)),
        _ => None
    })?;
```

**Impact:**
- Provider doesn't track orchestration metadata
- Runtime does O(n) scan (but history is small, cached)
- Cleaner provider interface

**Effort:** Update fetch_orchestration_item implementations

---

### Medium Win #3: Unified Enqueue

```diff
trait Provider {
-   async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String>;
-   async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String>;
-   async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
+   async fn enqueue(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
}
```

**Provider routes by type:**
```rust
async fn enqueue(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
    match &item {
        WorkItem::ActivityExecute { .. } => self.enqueue_to_worker(&item),
        WorkItem::TimerSchedule { .. } => self.enqueue_to_timer(&item, delay_ms),
        _ => self.enqueue_to_orchestrator(&item, delay_ms),
    }
}
```

**Impact:**
- 3 methods → 1 method
- Provider still needs routing logic (but already has it)

**Effort:** Update all call sites (mechanical)

---

### Medium Win #4: Unified Dequeue

```diff
trait Provider {
-   async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)>;
-   async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)>;
+   async fn dequeue(&self, queue: QueueType) -> Option<(WorkItem, String)>;
}

enum QueueType { Worker, Timer }
```

**Impact:**
- 2 methods → 1 method
- More explicit queue selection

**Effort:** Update dispatcher call sites

---

### Medium Win #5: Unified Ack

```diff
trait Provider {
-   async fn ack_worker(&self, token: &str) -> Result<(), String>;
-   async fn ack_timer(&self, token: &str) -> Result<(), String>;
-   async fn ack_orchestration_item(&self, token: &str, ..., metadata: ExecutionMetadata) -> Result<(), String>;
+   async fn ack(&self, token: &str, changes: Option<TurnChanges>) -> Result<(), String>;
}

struct TurnChanges {
    history_delta: Vec<Event>,
    worker_items: Vec<WorkItem>,
    timer_items: Vec<WorkItem>,
    orchestrator_items: Vec<WorkItem>,
    metadata: ExecutionMetadata,
}
```

**Impact:**
- 3 methods → 1 method
- Cleaner API

**Effort:** Update all call sites, adjust type signatures

---

## Recommended Immediate Actions

### 1. Remove `create_new_execution()` ✅ (Zero Risk)

Not called anywhere, violates design principles.

```diff
- async fn create_new_execution(...) -> Result<u64, String>;
```

**Effort:** 5 minutes  
**Risk:** None  
**Benefit:** Cleaner interface, better design

---

### 2. Move Optional Methods to Extension Trait ⚠️ (Low Risk)

```rust
// Core trait
#[async_trait]
pub trait Provider: Send + Sync {
    // 11 essential methods
}

// Optional debugging features
#[async_trait]
pub trait ProviderDebug: Provider {
    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event> {
        self.read(instance).await  // Default impl
    }
    async fn list_instances(&self) -> Vec<String> { Vec::new() }
    async fn list_executions(&self, instance: &str) -> Vec<u64> { vec![1] }
}

// Blanket impl
impl<T: Provider> ProviderDebug for T {}
```

**Benefit:** Clear separation of core vs debugging features

**Effort:** 30 minutes  
**Risk:** Low (backward compatible via blanket impl)

---

### 3. Simplify OrchestrationItem ⚠️ (Medium Risk)

Remove orchestration_name, version, execution_id fields.

**Effort:** 2 hours (update SQLite, tests, runtime)  
**Risk:** Medium (affects provider implementations)  
**Benefit:** Provider tracks less metadata

---

## Summary of Minimization Potential

| Approach | Methods | Risk | Effort | Benefit |
|----------|---------|------|--------|---------|
| **Current** | 18 | - | - | - |
| **Quick Win (remove create_new_execution)** | 17 | None | 5min | Clean up deprecated method |
| **Moderate (+ extension trait)** | 11 core + 4 optional | Low | 30min | Clear required vs optional |
| **Aggressive (remove optionals)** | 11 | Medium | 2h | Minimal interface |
| **Radical (generic store)** | 5 | High | 20h | Perfect abstraction |

---

## My Recommendation

**Do in order:**

1. ✅ **Remove `create_new_execution()`** - 5 minutes, zero risk
2. ⚠️ **Extension trait for debugging methods** - 30 minutes, low risk, clear benefits
3. 🤔 **Consider OrchestrationItem simplification** - Medium effort, good abstraction improvement
4. 🚫 **DON'T do radical redesign yet** - Save for v2.0 when breaking changes are acceptable

**Immediate action: Steps 1-2 give 80% of benefits with 20% of effort**

---

## Implementation Impact

### If We Do Steps 1-2 (Recommended)

**Before:**
```rust
trait Provider {
    // 18 methods, hard to know what's required
}
```

**After:**
```rust
// Core trait (11 required methods)
trait Provider {
    // fetch, ack_orchestration, abandon
    // read
    // dequeue_worker, ack_worker
    // enqueue_orchestrator
    // supports_delayed_visibility
    // + 3 timer methods (conditional)
}

// Optional debugging (4 methods)
trait ProviderDebug: Provider {
    // read_with_execution
    // list_instances, list_executions
    // latest_execution_id
}
```

**Benefits:**
- Clear what's required vs optional
- Smaller core interface
- Provider implementers focus on essentials first
- Backward compatible (blanket impl)

---

**Want me to implement the quick wins (remove create_new_execution + extension trait)?**




