# Client Separation Architecture

**Key Insight:** Design from the user's perspective (clients), then determine what Provider needs.

---

## Current Architecture (Single Client)

```
User Code
    ↓
  Client (mixed concerns)
    ├─ start_orchestration()     [control plane]
    ├─ raise_event()              [control plane]
    ├─ cancel_instance()          [control plane]
    ├─ wait_for_orchestration()   [control plane]
    └─ (no management methods yet)
    ↓
  Provider (mixed methods)
    ├─ enqueue_orchestrator_work  [runtime]
    ├─ read                       [both!]
    ├─ list_instances             [management]
    └─ list_executions            [management]
```

**Problems:**
- Client mixes control plane with (potential) observability
- Provider mixes runtime hot-path with cold-path queries
- No clear API for admin/debugging tools

---

## Proposed Architecture (Separated Clients)

```
User Code (Control Plane)          User Code (Observability)
    ↓                                   ↓
RuntimeClient                      ManagementClient
    ├─ start_orchestration              ├─ list_instances
    ├─ raise_event                      ├─ get_instance_info
    ├─ cancel_instance                  ├─ list_executions
    └─ wait_for_orchestration           ├─ read_execution
    ↓                                   ├─ get_metrics
                                        └─ get_queue_depths
                                        ↓
    ↓                                   ↓
Provider (runtime hot-path)        ManagementProvider (cold-path queries)
    ├─ fetch_orchestration_item         ├─ list_instances
    ├─ ack_orchestration_item           ├─ list_executions
    ├─ enqueue_orchestrator_work        ├─ read_execution
    ├─ dequeue_worker_peek_lock         ├─ get_instance_metadata
    ├─ ack_worker                       ├─ get_execution_metadata
    └─ read (for status checks)         ├─ get_metrics
                                        └─ get_queue_depths
```

---

## Design Option 1: Separate Provider Traits

### RuntimeClient + ManagementClient → Provider + ManagementProvider

```rust
// ===== USER-FACING APIS =====

/// Client for runtime control-plane operations.
pub struct RuntimeClient {
    provider: Arc<dyn Provider>,
}

impl RuntimeClient {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane operations
    pub async fn start_orchestration(&self, instance: &str, orchestration: &str, input: String) -> Result<(), String> {
        let work_item = WorkItem::StartOrchestration { ... };
        self.provider.enqueue_orchestrator_work(work_item, None).await
    }
    
    pub async fn raise_event(&self, instance: &str, event_name: &str, data: String) -> Result<(), String> {
        let work_item = WorkItem::ExternalRaised { ... };
        self.provider.enqueue_orchestrator_work(work_item, None).await
    }
    
    pub async fn cancel_instance(&self, instance: &str, reason: String) -> Result<(), String> {
        let work_item = WorkItem::CancelInstance { ... };
        self.provider.enqueue_orchestrator_work(work_item, None).await
    }
    
    pub async fn wait_for_orchestration(&self, instance: &str, timeout: Duration) -> Result<OrchestrationStatus, WaitError> {
        // Uses provider.read() to poll for status
        // ...
    }
    
    pub async fn get_orchestration_status(&self, instance: &str) -> OrchestrationStatus {
        // Uses provider.read()
        let history = self.provider.read(instance).await;
        // Scan for terminal events
        // ...
    }
}

/// Client for management and observability operations.
pub struct ManagementClient {
    provider: Arc<dyn ManagementProvider>,
}

impl ManagementClient {
    pub fn new(provider: Arc<dyn ManagementProvider>) -> Self {
        Self { provider }
    }
    
    // Observability operations
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.provider.list_instances().await
    }
    
    pub async fn list_running_instances(&self) -> Result<Vec<String>, String> {
        self.provider.list_instances_by_status("Running").await
    }
    
    pub async fn get_instance_details(&self, instance: &str) -> Result<InstanceInfo, String> {
        self.provider.get_instance_info(instance).await
    }
    
    pub async fn get_execution_history(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String> {
        self.provider.read_execution(instance, execution_id).await
    }
    
    pub async fn get_system_health(&self) -> Result<SystemMetrics, String> {
        self.provider.get_system_metrics().await
    }
    
    pub async fn get_queue_status(&self) -> Result<QueueDepths, String> {
        self.provider.get_queue_depths().await
    }
}

// ===== PROVIDER LAYER =====

/// Core provider for runtime operations (hot-path).
trait Provider: Send + Sync {
    // 11 methods (runtime critical)
    fetch_orchestration_item, ack_orchestration_item, ...
    read  // Used by RuntimeClient.get_status()
}

/// Management provider for observability (cold-path).
trait ManagementProvider: Send + Sync {
    // 8 methods (all optional with defaults)
    list_instances, list_executions, read_execution, ...
}

// Implementation
impl Provider for SqliteProvider { ... }
impl ManagementProvider for SqliteProvider { ... }
```

**Usage:**
```rust
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);

// Control plane
let runtime_client = RuntimeClient::new(store.clone());
runtime_client.start_orchestration("order-1", "ProcessOrder", "{}").await?;

// Observability
let mgmt_client = ManagementClient::new(store.clone());
let instances = mgmt_client.list_all_instances().await?;
let metrics = mgmt_client.get_system_health().await?;
```

**Pros:**
- ✅ Clear API separation (control vs observability)
- ✅ Provider traits separated (hot vs cold path)
- ✅ Can use different storage for management (read replica)

**Cons:**
- ⚠️ Two client types (more API surface)
- ⚠️ Need to create both clients
- ⚠️ RuntimeClient.get_status() uses Provider.read() (some overlap)

---

## Design Option 2: Single Provider, Separate Clients

### RuntimeClient + ManagementClient → Unified Provider

```rust
// ===== USER-FACING APIS =====

/// Client for runtime control-plane operations.
pub struct RuntimeClient {
    provider: Arc<dyn Provider>,
}

impl RuntimeClient {
    // Same as Option 1
    // Uses: enqueue_orchestrator_work, read
}

/// Client for management and observability operations.
pub struct ManagementClient {
    provider: Arc<dyn Provider>,  // Same Provider!
}

impl ManagementClient {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        // Directly calls provider management methods
        self.provider.list_instances().await
    }
    
    pub async fn get_instance_details(&self, instance: &str) -> Result<InstanceInfo, String> {
        // Option A: Provider has this method
        self.provider.get_instance_info(instance).await
        
        // Option B: Client builds from Provider primitives
        let history = self.provider.read(instance).await;
        let executions = self.provider.list_executions(instance).await?;
        // Build InstanceInfo from primitives
        Ok(InstanceInfo { ... })
    }
}

// ===== PROVIDER LAYER =====

/// Unified provider (has both runtime and management methods).
trait Provider: Send + Sync {
    // Runtime operations (11 methods, required)
    fetch_orchestration_item, ack, enqueue, ...
    
    // Management operations (8 methods, optional with defaults)
    list_instances, list_executions, get_metrics, ...
}
```

**Pros:**
- ✅ Simpler provider interface (one trait)
- ✅ Clear client separation
- ✅ ManagementClient can build on Provider primitives

**Cons:**
- ⚠️ Provider still mixes hot and cold path
- ⚠️ Can't use different storage for management

---

## Design Option 3: Layered Clients (Recommended)

### RuntimeClient + ManagementClient with Capability Detection

```rust
// ===== USER-FACING APIS =====

/// Client for runtime control-plane operations.
/// 
/// Minimal interface for starting orchestrations, signaling, and status checks.
pub struct RuntimeClient {
    provider: Arc<dyn Provider>,
}

impl RuntimeClient {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    /// Start an orchestration instance.
    pub async fn start_orchestration(&self, instance: &str, orchestration: &str, input: String) -> Result<(), String>;
    
    /// Raise an external event.
    pub async fn raise_event(&self, instance: &str, event_name: &str, data: String) -> Result<(), String>;
    
    /// Request cancellation.
    pub async fn cancel_instance(&self, instance: &str, reason: String) -> Result<(), String>;
    
    /// Get current status (uses Provider.read()).
    pub async fn get_status(&self, instance: &str) -> OrchestrationStatus;
    
    /// Wait for completion.
    pub async fn wait_for_completion(&self, instance: &str, timeout: Duration) -> Result<OrchestrationStatus, WaitError>;
    
    /// Get management client (if provider supports it).
    pub fn management(&self) -> Option<ManagementClient> {
        // Try to cast provider to ManagementProvider
        // Return Some(ManagementClient) if supported, None otherwise
    }
}

/// Client for management and observability operations.
/// 
/// Rich interface for admin dashboards, debugging, and monitoring.
pub struct ManagementClient {
    provider: Arc<dyn ManagementProvider>,
}

impl ManagementClient {
    pub fn new(provider: Arc<dyn ManagementProvider>) -> Self {
        Self { provider }
    }
    
    /// List all orchestration instances.
    pub async fn list_instances(&self) -> Result<Vec<String>, String>;
    
    /// List instances by status.
    pub async fn list_running_instances(&self) -> Result<Vec<String>, String>;
    pub async fn list_completed_instances(&self) -> Result<Vec<String>, String>;
    pub async fn list_failed_instances(&self) -> Result<Vec<String>, String>;
    
    /// Get detailed instance information.
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    
    /// Get execution history.
    pub async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String>;
    pub async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, String>;
    pub async fn read_execution_history(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String>;
    
    /// Get system metrics.
    pub async fn get_metrics(&self) -> Result<SystemMetrics, String>;
    pub async fn get_queue_depths(&self) -> Result<QueueDepths, String>;
    
    /// Get runtime client for control operations.
    pub fn runtime(&self, provider: Arc<dyn Provider>) -> RuntimeClient {
        RuntimeClient::new(provider)
    }
}

// ===== PROVIDER LAYER =====

/// Core provider for runtime operations.
trait Provider: Send + Sync {
    // 11 core methods (runtime hot-path)
}

/// Management provider for observability.
/// 
/// Separate trait - providers can choose to implement or not.
/// Can have different implementation than Provider (read replica, analytics DB).
trait ManagementProvider: Send + Sync {
    // 8 management methods (all with defaults)
}

// SqliteProvider implements both
impl Provider for SqliteProvider { }
impl ManagementProvider for SqliteProvider { }
```

**Usage:**
```rust
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);

// Control plane
let client = RuntimeClient::new(store.clone());
client.start_orchestration("order-1", "ProcessOrder", "{}").await?;
let status = client.get_status("order-1").await;

// Management/observability
let mgmt = ManagementClient::new(store.clone());
let all_instances = mgmt.list_instances().await?;
let metrics = mgmt.get_metrics().await?;
println!("System health: {} running, {} completed", 
    metrics.running_instances, metrics.completed_instances);
```

---

## Analysis: What Each Client Needs from Provider

### RuntimeClient Requirements

**Operations it performs:**
1. Start orchestrations → `Provider.enqueue_orchestrator_work(StartOrchestration)`
2. Raise events → `Provider.enqueue_orchestrator_work(ExternalRaised)`
3. Cancel instances → `Provider.enqueue_orchestrator_work(CancelInstance)`
4. Get status → `Provider.read(instance)` then scan for terminal events
5. Wait for completion → Repeatedly call `Provider.read()` until terminal

**Provider methods needed:**
- ✅ `enqueue_orchestrator_work(WorkItem, delay_ms)`
- ✅ `read(instance) → Vec<Event>`

**That's it! Just 2 methods.**

### ManagementClient Requirements

**Operations it performs:**
1. List instances → Query instance registry
2. List executions → Query execution table
3. Read execution history → Query history for specific execution
4. Get instance metadata → Query instance + execution tables
5. Get metrics → Aggregate queries
6. Get queue depths → Count unlocked messages

**Provider methods needed:**
- ✅ `list_instances() → Vec<String>`
- ✅ `list_executions(instance) → Vec<u64>`
- ✅ `read_execution(instance, exec_id) → Vec<Event>`
- ✅ `get_instance_metadata(instance) → InstanceMetadata`
- ✅ `get_execution_metadata(instance, exec_id) → ExecutionMetadata`
- ✅ `get_metrics() → SystemMetrics`
- ✅ `get_queue_depths() → QueueDepths`

**None of these are used by Runtime! Pure management.**

---

## Key Insight: Provider.read() is Shared

Both clients need `read()`:
- RuntimeClient: For `get_status()` and `wait_for_completion()`
- ManagementClient: For `read_execution()` (specific execution)

**Question:** Should `read()` be in Provider or ManagementProvider?

### Option A: read() in Provider

```rust
trait Provider {
    async fn read(&self, instance: &str) -> Vec<Event>;  // Latest execution
    // ... other runtime methods
}

trait ManagementProvider {
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Vec<Event>;
    // Delegates to Provider.read() for latest execution
}
```

**Pro:** RuntimeClient has direct access  
**Con:** Provider has one non-runtime method

### Option B: read() in ManagementProvider

```rust
trait Provider {
    // Pure runtime operations only (no read)
}

trait ManagementProvider {
    async fn read(&self, instance: &str) -> Vec<Event>;  // Latest execution
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Vec<Event>;  // Specific
}

// RuntimeClient needs both
struct RuntimeClient {
    provider: Arc<dyn Provider>,
    reader: Arc<dyn ManagementProvider>,  // For read()
}
```

**Pro:** Provider is pure runtime  
**Con:** RuntimeClient depends on both traits

### Option C: Shared Reader Trait

```rust
trait HistoryReader: Send + Sync {
    async fn read(&self, instance: &str) -> Vec<Event>;
}

trait Provider: HistoryReader {
    // Runtime operations
}

trait ManagementProvider: HistoryReader {
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Vec<Event>;
    // Other management methods
}

// RuntimeClient uses HistoryReader
struct RuntimeClient {
    provider: Arc<dyn Provider>,  // Provider: HistoryReader, so has read()
}

// ManagementClient uses ManagementProvider
struct ManagementClient {
    provider: Arc<dyn ManagementProvider>,  // ManagementProvider: HistoryReader
}
```

**Pro:** Clear abstraction (HistoryReader is shared)  
**Con:** More traits

---

## Recommended Design: Option A (read in Provider)

### Final Architecture

```rust
// ===== TRAITS =====

/// Core provider for runtime operations.
#[async_trait]
pub trait Provider: Send + Sync {
    // Orchestration processing
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    async fn ack_orchestration_item(...) -> Result<(), String>;
    async fn abandon_orchestration_item(&self, lock_token: &str, delay_ms: Option<u64>) -> Result<(), String>;
    
    // History (used by both Runtime and RuntimeClient)
    async fn read(&self, instance: &str) -> Vec<Event>;
    
    // Worker queue
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)>;
    async fn ack_worker(&self, token: &str) -> Result<(), String>;
    
    // Orchestrator queue
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
    
    // Timer support (conditional)
    fn supports_delayed_visibility(&self) -> bool { false }
    async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String>;
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)>;
    async fn ack_timer(&self, token: &str) -> Result<(), String>;
}

/// Management provider for observability and administrative operations.
#[async_trait]
pub trait ManagementProvider: Send + Sync {
    // Instance discovery
    async fn list_instances(&self) -> Result<Vec<String>, String> { Ok(Vec::new()) }
    async fn list_instances_by_status(&self, status: &str) -> Result<Vec<String>, String> { Ok(Vec::new()) }
    
    // Execution inspection
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String> { Ok(vec![1]) }
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String>;
    
    // Metadata access
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    async fn get_execution_info(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, String>;
    
    // Metrics
    async fn get_system_metrics(&self) -> Result<SystemMetrics, String> { Ok(SystemMetrics::default()) }
    async fn get_queue_depths(&self) -> Result<QueueDepths, String> { Ok(QueueDepths::default()) }
}

// ===== CLIENTS =====

/// Client for runtime control-plane operations.
pub struct RuntimeClient {
    provider: Arc<dyn Provider>,
}

/// Client for management and observability operations.
pub struct ManagementClient {
    provider: Arc<dyn ManagementProvider>,
}
```

---

## Module Structure

```
src/
├── lib.rs
│   └── pub use client::{RuntimeClient, ManagementClient};
│
├── client/
│   ├── mod.rs              # RuntimeClient (was Client)
│   └── management.rs       # ManagementClient (NEW)
│
└── providers/
    ├── mod.rs              # Provider trait
    ├── management.rs       # ManagementProvider trait (NEW)
    └── sqlite.rs           # Implements both
```

---

## What Gets Removed from Provider

**Move to ManagementProvider:**
- ❌ `list_instances()`
- ❌ `list_executions(instance)`
- ❌ `read_with_execution(instance, exec_id)` → becomes `ManagementProvider.read_execution()`
- ❌ `latest_execution_id(instance)`

**Remove entirely (deprecated):**
- ❌ `create_new_execution()` - Runtime handles via metadata

**Keep in Provider:**
- ✅ `read(instance)` - Used by RuntimeClient.get_status()
- ✅ All other runtime methods

---

## Provider Method Count

**Before:** 18 methods (mixed)

**After:**
- **Provider:** 11 methods (all runtime)
- **ManagementProvider:** 8 methods (all management)

**Total:** 19 methods, but clearly separated

---

## Client API Examples

### RuntimeClient (Control Plane)

```rust
let client = RuntimeClient::new(store);

// Start workflow
client.start_orchestration("order-123", "ProcessOrder", order_json).await?;

// Signal workflow
client.raise_event("order-123", "PaymentReceived", payment_json).await?;

// Check status
let status = client.get_status("order-123").await;
match status {
    OrchestrationStatus::Running => println!("Still processing"),
    OrchestrationStatus::Completed { output } => println!("Done: {}", output),
    _ => {}
}

// Wait for result
let result = client.wait_for_completion("order-123", Duration::from_secs(30)).await?;
```

### ManagementClient (Observability)

```rust
let mgmt = ManagementClient::new(store);

// Dashboard: List all instances
let instances = mgmt.list_instances().await?;
for instance in instances {
    let info = mgmt.get_instance_info(&instance).await?;
    println!("{}: {} ({})", instance, info.orchestration_name, info.status);
}

// Debug: Inspect specific execution
let executions = mgmt.list_executions("order-123").await?;
for exec_id in executions {
    let info = mgmt.get_execution_info("order-123", exec_id).await?;
    println!("Execution {}: {} events, status: {}", exec_id, info.event_count, info.status);
}

// Metrics: System health
let metrics = mgmt.get_system_metrics().await?;
println!("Running: {}, Completed: {}, Failed: {}", 
    metrics.running_instances, metrics.completed_instances, metrics.failed_instances);

// Queue monitoring
let queues = mgmt.get_queue_depths().await?;
println!("Backlog - Orch: {}, Worker: {}, Timer: {}", 
    queues.orchestrator_queue, queues.worker_queue, queues.timer_queue);
```

---

## My Recommendation

**✅ Go with Option A / Option 3 (Separate Traits + Clients)**

**Why:**
1. Clear separation: RuntimeClient=control, ManagementClient=observability
2. Provider traits separated: Provider=hot-path, ManagementProvider=cold-path
3. Both clients can coexist: `RuntimeClient` for app code, `ManagementClient` for admin tools
4. ManagementProvider is optional: Minimal providers don't need it
5. Future flexibility: Can use different storage for management

**API becomes:**
```rust
use duroxide::{RuntimeClient, ManagementClient};

// Control plane
let client = RuntimeClient::new(store.clone());

// Observability  
let mgmt = ManagementClient::new(store.clone());
```

**Clean, clear, extensible!** 

Want me to implement this? It's about 6 hours of work for a much better architecture.
