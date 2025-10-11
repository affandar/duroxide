# Capability-Based Management Design

**Key Insight:** Make ManagementProvider an optional extension trait that "lights up" ManagementClient when available.

---

## Design: Optional Trait Extension

### Core Concept

```rust
// Base Provider trait (runtime only)
trait Provider: Send + Sync {
    // 11 runtime methods
    fetch_orchestration_item, ack_orchestration_item, ...
    read  // ← Used by Client.get_status()
}

// Optional extension trait
trait ManagementProvider: Provider + Send + Sync {
    // 8 management methods
    list_instances, list_executions, get_instance_info, ...
}

// Client with capability detection
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    pub async fn get_status(...) { }
    
    // Management (requires ManagementProvider capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.try_management()?.list_instances().await
    }
    
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        self.try_management()?.get_instance_info(instance).await
    }
    
    // Capability detection
    fn try_management(&self) -> Result<&dyn ManagementProvider, String> {
        self.provider.as_ref()
            .downcast_ref::<dyn ManagementProvider>()
            .ok_or_else(|| "Management features not available".to_string())
    }
}
```

---

## Implementation Options

### Option 1: Trait Object Downcasting

```rust
// Base Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    async fn ack_orchestration_item(...) -> Result<(), String>;
    async fn read(&self, instance: &str) -> Vec<Event>;
    // ... other runtime methods
}

// Extension trait (inherits from Provider)
#[async_trait]
pub trait ManagementProvider: Provider + Send + Sync {
    // Management methods
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String>;
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    // ... other management methods
}

// Client with capability detection
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(&self, instance: &str, orchestration: &str, input: String) -> Result<(), String> {
        let item = WorkItem::StartOrchestration { ... };
        self.provider.enqueue_orchestrator_work(item, None).await
    }
    
    // Management (requires ManagementProvider capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.try_management()?.list_instances().await
    }
    
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        self.try_management()?.get_instance_info(instance).await
    }
    
    // Capability detection helper
    fn try_management(&self) -> Result<&dyn ManagementProvider, String> {
        // Try to downcast to ManagementProvider
        self.provider.as_ref()
            .downcast_ref::<dyn ManagementProvider>()
            .ok_or_else(|| "Management features not available - provider doesn't implement ManagementProvider".to_string())
    }
    
    // Check if management features are available
    pub fn has_management_capability(&self) -> bool {
        self.try_management().is_ok()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementProvider for SqliteProvider { /* management methods */ }
```

**Problem:** Trait object downcasting doesn't work with `dyn Trait` - you can't downcast `Arc<dyn Provider>` to `Arc<dyn ManagementProvider>`.

---

### Option 2: Capability Enum (Recommended)

```rust
// Base Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    async fn ack_orchestration_item(...) -> Result<(), String>;
    async fn read(&self, instance: &str) -> Vec<Event>;
    // ... other runtime methods
}

// Management capability trait
#[async_trait]
pub trait ManagementCapability: Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String>;
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    // ... other management methods
}

// Provider with optional management
pub struct Client {
    provider: Arc<dyn Provider>,
    management: Option<Arc<dyn ManagementCapability>>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { 
            provider,
            management: None,
        }
    }
    
    pub fn with_management(provider: Arc<dyn Provider>, management: Arc<dyn ManagementCapability>) -> Self {
        Self { provider, management: Some(management) }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(&self, instance: &str, orchestration: &str, input: String) -> Result<(), String> {
        let item = WorkItem::StartOrchestration { ... };
        self.provider.enqueue_orchestrator_work(item, None).await
    }
    
    // Management (requires ManagementCapability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        match &self.management {
            Some(mgmt) => mgmt.list_instances().await,
            None => Err("Management features not available".to_string()),
        }
    }
    
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String> {
        match &self.management {
            Some(mgmt) => mgmt.get_instance_info(instance).await,
            None => Err("Management features not available".to_string()),
        }
    }
    
    // Check capability
    pub fn has_management_capability(&self) -> bool {
        self.management.is_some()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }

// Convenience factory
impl Client {
    pub fn from_provider_with_management<P>(provider: Arc<P>) -> Self 
    where 
        P: Provider + ManagementCapability + 'static,
    {
        let management: Arc<dyn ManagementCapability> = provider.clone();
        Self::with_management(provider, management)
    }
}
```

**Pros:**
- ✅ Clear capability separation
- ✅ Compile-time safety
- ✅ No trait object downcasting issues

**Cons:**
- ⚠️ Requires passing both provider and management capability

---

### Option 3: Trait Object with Type Erasure (Advanced)

```rust
// Base Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
#[async_trait]
pub trait ManagementCapability: Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Combined trait object
pub struct Client {
    provider: Arc<dyn Provider>,
    management: Option<Arc<dyn ManagementCapability>>,
}

impl Client {
    // Factory that automatically detects capabilities
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        // Try to extract management capability from provider
        let management = if let Some(mgmt) = provider.as_ref().as_any().downcast_ref::<SqliteProvider>() {
            Some(Arc::new(mgmt.clone()) as Arc<dyn ManagementCapability>)
        } else {
            None
        };
        
        Self { provider, management }
    }
    
    // Management methods with capability detection
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        match &self.management {
            Some(mgmt) => mgmt.list_instances().await,
            None => Err("Management features not available".to_string()),
        }
    }
}

// Add Any trait to Provider for downcasting
pub trait Provider: Any + Send + Sync {
    // ... methods
}

impl Provider for SqliteProvider {
    // ... implementation
}
```

**Problem:** Requires `Any` trait and type-specific downcasting.

---

### Option 4: Builder Pattern (Cleanest)

```rust
// Base Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
#[async_trait]
pub trait ManagementCapability: Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client builder
pub struct ClientBuilder {
    provider: Option<Arc<dyn Provider>>,
    management: Option<Arc<dyn ManagementCapability>>,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self { provider: None, management: None }
    }
    
    pub fn with_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.provider = Some(provider);
        self
    }
    
    pub fn with_management(mut self, management: Arc<dyn ManagementCapability>) -> Self {
        self.management = Some(management);
        self
    }
    
    pub fn build(self) -> Result<Client, String> {
        let provider = self.provider.ok_or("Provider required")?;
        Ok(Client { provider, management: self.management })
    }
}

// Client
pub struct Client {
    provider: Arc<dyn Provider>,
    management: Option<Arc<dyn ManagementCapability>>,
}

impl Client {
    // Control plane (always available)
    pub async fn start_orchestration(&self, instance: &str, orchestration: &str, input: String) -> Result<(), String> {
        let item = WorkItem::StartOrchestration { ... };
        self.provider.enqueue_orchestrator_work(item, None).await
    }
    
    // Management (requires ManagementCapability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        match &self.management {
            Some(mgmt) => mgmt.list_instances().await,
            None => Err("Management features not available".to_string()),
        }
    }
    
    pub fn has_management_capability(&self) -> bool {
        self.management.is_some()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }

// Usage
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);

// Basic client
let client = ClientBuilder::new()
    .with_provider(store.clone())
    .build()?;

// Rich client
let mgmt_capability: Arc<dyn ManagementCapability> = store.clone();
let client = ClientBuilder::new()
    .with_provider(store.clone())
    .with_management(mgmt_capability)
    .build()?;
```

---

## My Recommendation: Option 2 (Capability Enum)

### Why Option 2?

1. **Clear Separation**: Provider = runtime, ManagementCapability = observability
2. **Compile-Time Safety**: No runtime downcasting
3. **Explicit Capabilities**: Clear what features are available
4. **Easy to Extend**: Can add more capabilities later

### Implementation Plan

```rust
// 1. Keep current Provider trait (runtime only)
#[async_trait]
pub trait Provider: Send + Sync {
    // 11 runtime methods
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    async fn ack_orchestration_item(...) -> Result<(), String>;
    async fn read(&self, instance: &str) -> Vec<Event>;
    // ... other runtime methods
}

// 2. Add ManagementCapability trait
#[async_trait]
pub trait ManagementCapability: Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String>;
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    async fn get_execution_info(&self, instance: &str, exec_id: u64) -> Result<ExecutionInfo, String>;
    async fn read_execution_history(&self, instance: &str, exec_id: u64) -> Result<Vec<Event>, String>;
    async fn get_system_metrics(&self) -> Result<SystemMetrics, String>;
    async fn get_queue_depths(&self) -> Result<QueueDepths, String>;
}

// 3. Update Client to support capabilities
pub struct Client {
    provider: Arc<dyn Provider>,
    management: Option<Arc<dyn ManagementCapability>>,
}

impl Client {
    // Current methods stay the same
    pub async fn start_orchestration(...) { }
    pub async fn get_status(...) { }
    
    // Add rich management methods
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        match &self.management {
            Some(mgmt) => mgmt.list_instances().await,
            None => Err("Management features not available".to_string()),
        }
    }
    
    pub fn has_management_capability(&self) -> bool {
        self.management.is_some()
    }
}

// 4. Implement both traits for SqliteProvider
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }

// 5. Convenience factory
impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider, management: None }
    }
    
    pub fn with_management(provider: Arc<dyn Provider>, management: Arc<dyn ManagementCapability>) -> Self {
        Self { provider, management: Some(management) }
    }
    
    // Auto-detect capabilities for known providers
    pub fn from_sqlite_provider(provider: Arc<SqliteProvider>) -> Self {
        let management: Arc<dyn ManagementCapability> = provider.clone();
        Self::with_management(provider, management)
    }
}
```

### Usage Examples

```rust
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);

// Basic client (current API)
let client = Client::new(store.clone());
client.start_orchestration("order-1", "ProcessOrder", "{}").await?;
let status = client.get_status("order-1").await;

// Rich client (new capabilities)
let mgmt_capability: Arc<dyn ManagementCapability> = store.clone();
let client = Client::with_management(store, mgmt_capability);
let instances = client.list_all_instances().await?;
let metrics = client.get_system_metrics().await?;

// Auto-detection for known providers
let client = Client::from_sqlite_provider(store);
let has_mgmt = client.has_management_capability(); // true
```

### What Gets Added to Client

```rust
impl Client {
    // Rich management methods
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String>;
    pub async fn list_instances_by_status(&self, status: &str) -> Result<Vec<String>, String>;
    pub async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    pub async fn get_execution_info(&self, instance: &str, exec_id: u64) -> Result<ExecutionInfo, String>;
    pub async fn read_execution_history(&self, instance: &str, exec_id: u64) -> Result<Vec<Event>, String>;
    pub async fn get_system_metrics(&self) -> Result<SystemMetrics, String>;
    pub async fn get_queue_depths(&self) -> Result<QueueDepths, String>;
    
    // Capability detection
    pub fn has_management_capability(&self) -> bool;
    
    // Factory methods
    pub fn new(provider: Arc<dyn Provider>) -> Self;
    pub fn with_management(provider: Arc<dyn Provider>, management: Arc<dyn ManagementCapability>) -> Self;
    pub fn from_sqlite_provider(provider: Arc<SqliteProvider>) -> Self;
}
```

---

## Benefits of This Approach

1. **No Breaking Changes**: Current Client API stays the same
2. **Capability-Based**: Management features "light up" when available
3. **Clear Error Messages**: "Management features not available" when capability missing
4. **Extensible**: Can add more capabilities later (e.g., `AnalyticsCapability`)
5. **Type Safe**: No runtime downcasting, compile-time guarantees

**This gives us the best of both worlds: backward compatibility + rich capabilities!**

Want me to implement this?
