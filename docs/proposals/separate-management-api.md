# Separate Management & Observability API Proposal

**Core Insight:** Management operations (list instances, debugging, metrics) are fundamentally different from runtime operations (fetch, ack, enqueue).

**Proposed:** Split into two separate traits/APIs

---

## Problem with Current Design

### Provider Trait Mixes Concerns

```rust
trait Provider {
    // RUNTIME OPERATIONS (hot path, performance-critical)
    fetch_orchestration_item()      // Called 1000s of times/sec
    ack_orchestration_item()         // Called 1000s of times/sec
    dequeue_worker_peek_lock()       // Called 1000s of times/sec
    
    // MANAGEMENT OPERATIONS (cold path, occasional)
    list_instances()                 // Called once for admin dashboard
    list_executions()                // Called once for debugging
    read_with_execution()            // Called once for inspection
    latest_execution_id()            // Rarely used
}
```

**Issues:**
- 🔴 Mixed performance characteristics (hot vs cold path)
- 🔴 Mixed concerns (operations vs observability)
- 🔴 Provider implementers must implement management features
- 🔴 Can't swap management implementation independently

---

## Proposed Design: Separate Traits

### Core Provider (Runtime Operations Only)

```rust
/// Core runtime operations for durable orchestration execution.
/// 
/// This trait contains ONLY methods required for the runtime to function.
/// Hot path, performance-critical operations.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    // ===== Orchestration Processing =====
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        timer_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), String>;
    async fn abandon_orchestration_item(&self, lock_token: &str, delay_ms: Option<u64>) -> Result<(), String>;
    
    // ===== History Access =====
    async fn read(&self, instance: &str) -> Vec<Event>;
    
    // ===== Worker Queue =====
    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)>;
    async fn ack_worker(&self, token: &str) -> Result<(), String>;
    
    // ===== Orchestrator Queue =====
    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
    
    // ===== Timer Support =====
    fn supports_delayed_visibility(&self) -> bool { false }
    async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String> { 
        Err("Not supported".to_string()) 
    }
    async fn dequeue_timer_peek_lock(&self) -> Option<(WorkItem, String)> { None }
    async fn ack_timer(&self, token: &str) -> Result<(), String> { 
        Err("Not supported".to_string()) 
    }
}
```

**Total: 11 required methods + 3 conditional**

---

### Management API (Observability & Admin Tools)

```rust
/// Management and observability API for Duroxide.
/// 
/// Separate from Provider - these operations are for:
/// - Admin dashboards
/// - Debugging tools
/// - Monitoring systems
/// - CLI utilities
/// 
/// NOT used by runtime hot path. Can have different implementation,
/// performance characteristics, and even data source than Provider.
#[async_trait::async_trait]
pub trait ManagementAPI: Send + Sync {
    // ===== Instance Listing & Discovery =====
    
    /// List all known instance IDs.
    /// 
    /// Used by admin dashboards to show all workflows.
    async fn list_instances(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())  // Default: not supported
    }
    
    /// List all instance IDs matching a filter.
    /// 
    /// # Filters
    /// - status: "Running", "Completed", "Failed"
    /// - orchestration: orchestration name
    /// - created_after: timestamp
    async fn list_instances_filtered(&self, filter: InstanceFilter) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    
    // ===== Execution Inspection =====
    
    /// List all execution IDs for an instance.
    /// 
    /// For instances with ContinueAsNew, returns [1, 2, 3, ...].
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String> {
        Ok(Vec::new())
    }
    
    /// Read history for a specific execution.
    /// 
    /// Used for debugging specific execution in multi-execution instances.
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String> {
        Err("Not supported".to_string())
    }
    
    /// Get metadata for a specific execution.
    async fn get_execution_metadata(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, String> {
        Err("Not supported".to_string())
    }
    
    // ===== Instance Metadata =====
    
    /// Get comprehensive instance information.
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    
    // ===== Metrics & Statistics =====
    
    /// Get system-wide metrics.
    async fn get_metrics(&self) -> Result<SystemMetrics, String> {
        Ok(SystemMetrics::default())
    }
    
    /// Get queue depths.
    async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
        Ok(QueueDepths::default())
    }
    
    // ===== Operational Tools =====
    
    /// Purge completed instances older than duration.
    /// 
    /// Used for cleanup/archival.
    async fn purge_completed(&self, older_than: std::time::Duration) -> Result<usize, String> {
        Ok(0)
    }
    
    /// Export instance history for archival.
    async fn export_instance(&self, instance: &str) -> Result<InstanceExport, String> {
        Err("Not supported".to_string())
    }
}

// Supporting types
#[derive(Debug, Clone)]
pub struct InstanceFilter {
    pub status: Option<String>,
    pub orchestration: Option<String>,
    pub created_after: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct InstanceInfo {
    pub instance_id: String,
    pub orchestration_name: String,
    pub orchestration_version: String,
    pub current_execution_id: u64,
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone)]
pub struct ExecutionInfo {
    pub execution_id: u64,
    pub status: String,
    pub output: Option<String>,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    pub event_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SystemMetrics {
    pub total_instances: u64,
    pub running_instances: u64,
    pub completed_instances: u64,
    pub failed_instances: u64,
    pub total_executions: u64,
    pub total_events: u64,
}

#[derive(Debug, Clone, Default)]
pub struct QueueDepths {
    pub orchestrator_queue: usize,
    pub worker_queue: usize,
    pub timer_queue: usize,
}

#[derive(Debug, Clone)]
pub struct InstanceExport {
    pub instance: InstanceInfo,
    pub executions: Vec<ExecutionInfo>,
    pub history: Vec<Event>,
}
```

**Effort:** 2-3 hours (design + implement + test)  
**Risk:** Low (additive, doesn't change Provider)  
**Benefit:** 
- Clean separation
- Can add features without touching Provider
- Can have different implementation (e.g., read-only replica)

---

### 3. **Unify Enqueue Methods** 🔄

```rust
// Current: 3 separate methods
async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String>;
async fn enqueue_timer_work(&self, item: WorkItem) -> Result<(), String>;
async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;

// Unified: 1 method
async fn enqueue(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String>;
```

Provider routes internally:
```rust
async fn enqueue(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
    match &item {
        WorkItem::ActivityExecute { .. } => self.enqueue_to_worker_queue(&item),
        WorkItem::TimerSchedule { .. } => self.enqueue_to_timer_queue(&item, delay_ms),
        _ => self.enqueue_to_orchestrator_queue(&item, delay_ms),
    }
}
```

**Effort:** 3 hours (update Runtime call sites)  
**Risk:** Low  
**Benefit:** 3 methods → 1 method, simpler interface

---

### 4. **Remove `latest_execution_id()` from Provider** 📦

Runtime already caches execution IDs:

```rust
pub struct Runtime {
    current_execution_ids: Mutex<HashMap<String, u64>>,  // Already tracks this!
}
```

**Move to ManagementAPI:**
```rust
// Remove from Provider
- async fn latest_execution_id(&self, instance: &str) -> Option<u64>;

// Add to ManagementAPI
+ async fn latest_execution_id(&self, instance: &str) -> Result<u64, String>;
```

**Effort:** 1 hour  
**Risk:** Low (runtime doesn't use it in hot path)  
**Benefit:** 1 fewer Provider method

---

## Recommended Approach: Management API Separation

### Phase 1: Create ManagementAPI Trait (2-3 hours)

**New file:** `src/providers/management.rs`

```rust
/// Management and observability operations for Duroxide providers.
/// 
/// Separate from Provider trait - these are administrative/debugging
/// operations, not runtime-critical operations.
#[async_trait::async_trait]
pub trait ManagementAPI: Send + Sync {
    // Instance discovery
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    async fn list_instances_filtered(&self, filter: InstanceFilter) -> Result<Vec<String>, String>;
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    
    // Execution inspection
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String>;
    async fn read_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, String>;
    async fn get_execution_metadata(&self, instance: &str, execution_id: u64) -> Result<ExecutionInfo, String>;
    
    // Metrics
    async fn get_metrics(&self) -> Result<SystemMetrics, String>;
    async fn get_queue_depths(&self) -> Result<QueueDepths, String>;
    
    // Operations
    async fn purge_completed(&self, older_than: Duration) -> Result<usize, String>;
    async fn export_instance(&self, instance: &str) -> Result<InstanceExport, String>;
}
```

### Phase 2: SqliteProvider Implements Both (1 hour)

```rust
impl ManagementAPI for SqliteProvider {
    async fn list_instances(&self) -> Result<Vec<String>, String> {
        let rows = sqlx::query("SELECT instance_id FROM instances ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        
        Ok(rows.into_iter()
            .filter_map(|row| row.try_get("instance_id").ok())
            .collect())
    }
    
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        let row = sqlx::query_as::<_, (String, String, String, i64, u64, u64)>(
            "SELECT i.orchestration_name, i.orchestration_version, i.current_execution_id,
                    e.status, i.created_at, i.updated_at
             FROM instances i
             LEFT JOIN executions e ON i.instance_id = e.instance_id AND i.current_execution_id = e.execution_id
             WHERE i.instance_id = ?"
        )
        .bind(instance)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        
        Ok(InstanceInfo {
            instance_id: instance.to_string(),
            orchestration_name: row.0,
            orchestration_version: row.1,
            current_execution_id: row.2 as u64,
            status: row.3,
            created_at: row.4,
            updated_at: row.5,
        })
    }
    
    async fn get_metrics(&self) -> Result<SystemMetrics, String> {
        let (total, running, completed, failed): (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT 
                COUNT(*) as total,
                SUM(CASE WHEN status = 'Running' THEN 1 ELSE 0 END) as running,
                SUM(CASE WHEN status = 'Completed' THEN 1 ELSE 0 END) as completed,
                SUM(CASE WHEN status = 'Failed' THEN 1 ELSE 0 END) as failed
             FROM executions"
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        
        Ok(SystemMetrics {
            total_instances: total as u64,
            running_instances: running as u64,
            completed_instances: completed as u64,
            failed_instances: failed as u64,
            total_executions: total as u64,
            total_events: 0,  // Could query history table
        })
    }
    
    async fn get_queue_depths(&self) -> Result<QueueDepths, String> {
        let orch: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orchestrator_queue WHERE lock_token IS NULL")
            .fetch_one(&self.pool).await.unwrap_or(0);
        let worker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_queue WHERE lock_token IS NULL")
            .fetch_one(&self.pool).await.unwrap_or(0);
        let timer: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM timer_queue WHERE lock_token IS NULL")
            .fetch_one(&self.pool).await.unwrap_or(0);
        
        Ok(QueueDepths {
            orchestrator_queue: orch as usize,
            worker_queue: worker as usize,
            timer_queue: timer as usize,
        })
    }
    
    // ... other methods
}
```

### Phase 3: Update Client to Use Both (1 hour)

```rust
/// Client for control-plane and management operations.
pub struct Client {
    store: Arc<dyn Provider>,
    management: Option<Arc<dyn ManagementAPI>>,  // Optional: not all providers support
}

impl Client {
    /// Create client with Provider only (no management features).
    pub fn new(store: Arc<dyn Provider>) -> Self {
        Self { store, management: None }
    }
    
    /// Create client with Provider and ManagementAPI.
    pub fn with_management(
        store: Arc<dyn Provider>,
        management: Arc<dyn ManagementAPI>,
    ) -> Self {
        Self { 
            store, 
            management: Some(management) 
        }
    }
    
    // Existing control-plane methods
    pub async fn start_orchestration(...) { }
    pub async fn raise_event(...) { }
    pub async fn cancel_instance(...) { }
    pub async fn get_orchestration_status(...) { }
    
    // New management methods (require management API)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.management.as_ref()
            .ok_or("Management API not configured")?
            .list_instances()
            .await
    }
    
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        self.management.as_ref()
            .ok_or("Management API not configured")?
            .get_instance_info(instance)
            .await
    }
    
    pub async fn get_system_metrics(&self) -> Result<SystemMetrics, String> {
        self.management.as_ref()
            .ok_or("Management API not configured")?
            .get_metrics()
            .await
    }
}
```

---

## Benefits of Separation

### 1. **Clear Responsibility Separation**

**Provider (Runtime):**
- Performance-critical
- Must be reliable
- Called 1000s of times/second
- Optimized for writes

**ManagementAPI (Observability):**
- Not performance-critical
- Can be eventually consistent
- Called occasionally (human-triggered)
- Optimized for reads/analytics

### 2. **Independent Implementation**

```rust
// Provider backed by primary database
let provider = Arc::new(SqliteProvider::new("sqlite:./primary.db").await?);

// ManagementAPI backed by read replica or analytics DB
let management = Arc::new(SqliteManagementAPI::new("sqlite:./analytics.db").await?);

// Or same implementation
let combined = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);
let client = Client::with_management(combined.clone(), combined.clone());
```

### 3. **Optional Features**

Not all providers need management features:

```rust
// Minimal provider (no management)
struct MinimalProvider { }
impl Provider for MinimalProvider { /* only core methods */ }

// Client works fine without management
let client = Client::new(Arc::new(MinimalProvider::new()));
client.start_orchestration(...).await;  // ✅ Works
// client.list_all_instances().await;   // ❌ Returns error (no management API)
```

### 4. **Different Data Sources**

```rust
// Hot path: Write-optimized primary DB
let provider = Arc::new(SqliteProvider::new("sqlite:./primary.db").await?);

// Observability: Read-optimized replica with pre-computed aggregates
let management = Arc::new(SqliteManagementAPI::new("sqlite:./read_replica.db").await?);

// Or even different tech
let management = Arc::new(ElasticsearchManagementAPI::new("http://localhost:9200"));
```

### 5. **Extensibility Without Breaking Changes**

Add new management features without touching Provider trait:

```rust
// Add new management method
trait ManagementAPI {
    // NEW: search instances by content
    async fn search_instances(&self, query: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    
    // NEW: get execution timeline
    async fn get_execution_timeline(&self, instance: &str) -> Result<Timeline, String> {
        Err("Not supported".to_string())
    }
}
```

Provider trait unchanged - no breaking changes!

---

## Comparison: Extension Trait vs Separate API

### Extension Trait Approach (ProviderDebug)

```rust
trait Provider { /* 11 core methods */ }
trait ProviderDebug: Provider { /* 4 debug methods */ }
impl<T: Provider> ProviderDebug for T {}  // Blanket impl
```

**Pros:**
- Simple (just split the trait)
- Automatic implementation (blanket impl)

**Cons:**
- Still coupled to Provider
- Can't have different implementation
- Mixes concerns (still in same module)

### Separate API Approach (ManagementAPI)

```rust
trait Provider { /* 11 core methods */ }
trait ManagementAPI { /* 10+ management methods */ }

// Provider impl
impl Provider for SqliteProvider { }

// Separate impl
impl ManagementAPI for SqliteProvider { }

// Or different impl
impl ManagementAPI for AnalyticsAPI { }
```

**Pros:**
- ✅ Complete separation of concerns
- ✅ Can have different implementations
- ✅ Can use different data sources
- ✅ Extensible without breaking Provider
- ✅ Optional (not all providers need it)

**Cons:**
- ⚠️ More interfaces to maintain
- ⚠️ Client needs to accept both (but optional)

---

## Proposed Structure

```
src/providers/
├── mod.rs              # Provider trait (runtime operations)
├── management.rs       # ManagementAPI trait (observability)
└── sqlite.rs           # Implements both Provider + ManagementAPI
```

### Provider Trait (Core Runtime)

**File:** `src/providers/mod.rs`

11 methods:
- fetch_orchestration_item
- ack_orchestration_item
- abandon_orchestration_item
- read
- dequeue_worker_peek_lock
- ack_worker
- enqueue_orchestrator_work
- supports_delayed_visibility
- enqueue_timer_work (conditional)
- dequeue_timer_peek_lock (conditional)
- ack_timer (conditional)

### ManagementAPI Trait (Admin/Debug)

**File:** `src/providers/management.rs`

10+ methods:
- list_instances
- list_instances_filtered
- get_instance_info
- list_executions
- read_execution
- get_execution_metadata
- get_metrics
- get_queue_depths
- purge_completed
- export_instance
- (extensible - add more later)

---

## Migration Plan

### Step 1: Create ManagementAPI Trait

**File:** `src/providers/management.rs`

```rust
pub mod management;

#[async_trait::async_trait]
pub trait ManagementAPI: Send + Sync {
    // Define all management methods with default impls
}
```

**Effort:** 1 hour

### Step 2: Remove Management Methods from Provider

```rust
// Remove from Provider trait in mod.rs
- async fn list_instances(&self) -> Vec<String>;
- async fn list_executions(&self, instance: &str) -> Vec<u64>;
- async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Vec<Event>;
- async fn latest_execution_id(&self, instance: &str) -> Option<u64>;
- async fn create_new_execution(...) -> Result<u64, String>;  // Also deprecated
```

**Effort:** 30 minutes

### Step 3: Implement ManagementAPI for SqliteProvider

```rust
impl ManagementAPI for SqliteProvider {
    // Implement all management methods
    // Move logic from old Provider methods
}
```

**Effort:** 1 hour

### Step 4: Update Client

```rust
pub struct Client {
    store: Arc<dyn Provider>,
    management: Option<Arc<dyn ManagementAPI>>,
}

// Update constructor, add new methods
```

**Effort:** 1 hour

### Step 5: Update Tests

```rust
// Tests that used list_instances() etc.
// Update to use ManagementAPI

let mgmt: Arc<dyn ManagementAPI> = store.clone();
let instances = mgmt.list_instances().await?;
```

**Effort:** 1 hour

**Total:** ~5-6 hours for complete migration

---

## Result After Cleanup

### Provider Trait (Runtime-Only)

```rust
trait Provider: Send + Sync {
    // 11 core methods for runtime operations
    // All hot-path, performance-critical
    // Required for runtime to function
}
```

**Characteristics:**
- Minimal interface
- Performance-critical
- No optional methods
- Clear contract

### ManagementAPI Trait (Admin Tools)

```rust
trait ManagementAPI: Send + Sync {
    // 10+ methods for observability
    // All cold-path, human-triggered
    // Extensible without breaking Provider
}
```

**Characteristics:**
- Rich interface
- Can add methods without breaking changes
- Optional (has default impls)
- Separate data source possible

---

## Comparison to Current State

| Aspect | Current Provider | After Split |
|--------|------------------|-------------|
| **Methods** | 18 mixed-purpose | 11 runtime + 10 management |
| **Concerns** | Mixed (runtime + debug) | Separated |
| **Required vs Optional** | Unclear | Very clear |
| **Extensibility** | Limited (breaks trait) | Good (ManagementAPI extensible) |
| **Implementation Flexibility** | Single impl | Can differ (replica, analytics DB) |
| **Performance Tuning** | Hard (mixed concerns) | Easy (optimize each separately) |

---

## My Strong Recommendation

✅ **Yes, create separate ManagementAPI**

**Why:**
1. Cleaner separation of concerns
2. Provider trait becomes minimal and focused
3. Management features can evolve independently
4. Can optimize differently (provider=writes, management=reads)
5. Can use different data sources (replica, analytics)
6. Not breaking (both traits can coexist)

**Do in this order:**
1. Create ManagementAPI trait (2 hours)
2. Move methods from Provider to ManagementAPI (1 hour)
3. SqliteProvider implements both (1 hour)
4. Update Client to optionally use ManagementAPI (1 hour)
5. Update tests (1 hour)

**Total: ~6 hours, huge improvement in design clarity**

**Want me to implement this? It's a much better design than extension trait!**


