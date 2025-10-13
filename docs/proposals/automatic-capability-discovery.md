# Automatic Capability Discovery

**Key Insight:** Client automatically discovers if Provider implements ManagementCapability trait.

---

## Design: Automatic Trait Discovery

### Core Concept

```rust
// Base Provider trait
trait Provider: Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
trait ManagementCapability: Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        // Try to downcast to ManagementCapability
        self.provider.as_ref()
            .downcast_ref::<dyn ManagementCapability>()
            .ok_or_else(|| "Management features not available".to_string())
    }
}
```

**Problem:** Trait object downcasting doesn't work with `dyn Trait` - you can't downcast `Arc<dyn Provider>` to `Arc<dyn ManagementCapability>`.

---

## Solution: Type Erasure with Any Trait

### Option 1: Any Trait Downcasting

```rust
use std::any::Any;

// Base Provider trait with Any
pub trait Provider: Any + Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
pub trait ManagementCapability: Any + Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        // Try to downcast to ManagementCapability
        self.provider.as_ref()
            .downcast_ref::<dyn ManagementCapability>()
            .ok_or_else(|| "Management features not available".to_string())
    }
    
    // Check if management capability is available
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }
```

**Problem:** Still doesn't work - `dyn Trait` objects can't be downcast to other `dyn Trait` types.

---

## Solution: Concrete Type Downcasting

### Option 2: Concrete Type Discovery

```rust
use std::any::Any;

// Base Provider trait with Any
pub trait Provider: Any + Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
pub trait ManagementCapability: Any + Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        // Try to downcast to concrete types that implement ManagementCapability
        if let Some(sqlite) = self.provider.as_ref().downcast_ref::<SqliteProvider>() {
            Ok(sqlite as &dyn ManagementCapability)
        } else {
            Err("Management features not available".to_string())
        }
    }
    
    // Check if management capability is available
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }
```

**Pros:**
- ✅ Automatic discovery
- ✅ No explicit capability passing
- ✅ Works with concrete types

**Cons:**
- ⚠️ Requires listing all concrete types
- ⚠️ Not extensible for new providers

---

## Solution: Trait Object with Type Erasure

### Option 3: Type Erasure Pattern

```rust
use std::any::Any;

// Base Provider trait with Any
pub trait Provider: Any + Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
pub trait ManagementCapability: Any + Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        // Try to downcast to concrete types that implement ManagementCapability
        if let Some(sqlite) = self.provider.as_ref().downcast_ref::<SqliteProvider>() {
            Ok(sqlite as &dyn ManagementCapability)
        } else if let Some(postgres) = self.provider.as_ref().downcast_ref::<PostgresProvider>() {
            Ok(postgres as &dyn ManagementCapability)
        } else {
            Err("Management features not available".to_string())
        }
    }
    
    // Check if management capability is available
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }

impl Provider for PostgresProvider { /* runtime methods */ }
impl ManagementCapability for PostgresProvider { /* management methods */ }
```

---

## Solution: Trait Object with Dynamic Dispatch

### Option 4: Dynamic Dispatch Pattern

```rust
use std::any::Any;

// Base Provider trait with Any
pub trait Provider: Any + Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
    
    // Capability detection method
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        None
    }
}

// Management capability trait
pub trait ManagementCapability: Any + Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        self.provider.as_management_capability()
            .ok_or_else(|| "Management features not available".to_string())
    }
    
    // Check if management capability is available
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }
}

// Implementation
impl Provider for SqliteProvider {
    // ... runtime methods
    
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        Some(self as &dyn ManagementCapability)
    }
}

impl ManagementCapability for SqliteProvider {
    // ... management methods
}
```

**Pros:**
- ✅ Automatic discovery
- ✅ Extensible (each provider implements capability detection)
- ✅ No concrete type listing

**Cons:**
- ⚠️ Requires adding method to Provider trait
- ⚠️ Slightly more complex

---

## Solution: Trait Object with Type Erasure (Recommended)

### Option 5: Type Erasure with Dynamic Dispatch

```rust
use std::any::Any;

// Base Provider trait with Any
pub trait Provider: Any + Send + Sync {
    // Runtime methods only
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem>;
    // ... other runtime methods
}

// Management capability trait
pub trait ManagementCapability: Any + Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    // ... other management methods
}

// Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        // Try to downcast to concrete types that implement ManagementCapability
        if let Some(sqlite) = self.provider.as_ref().downcast_ref::<SqliteProvider>() {
            Ok(sqlite as &dyn ManagementCapability)
        } else if let Some(postgres) = self.provider.as_ref().downcast_ref::<PostgresProvider>() {
            Ok(postgres as &dyn ManagementCapability)
        } else {
            Err("Management features not available".to_string())
        }
    }
    
    // Check if management capability is available
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }
}

// Implementation
impl Provider for SqliteProvider { /* runtime methods */ }
impl ManagementCapability for SqliteProvider { /* management methods */ }

impl Provider for PostgresProvider { /* runtime methods */ }
impl ManagementCapability for PostgresProvider { /* management methods */ }
```

---

## My Recommendation: Option 4 (Dynamic Dispatch)

### Why Option 4?

1. **Automatic Discovery**: Client automatically detects capabilities
2. **Extensible**: Each provider implements capability detection
3. **No Concrete Type Listing**: No need to list all provider types
4. **Clean API**: Single `Client::new()` constructor

### Implementation Plan

```rust
// 1. Add capability detection to Provider trait
pub trait Provider: Any + Send + Sync {
    // ... existing runtime methods
    
    // Capability detection method
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        None
    }
}

// 2. Add ManagementCapability trait
pub trait ManagementCapability: Any + Send + Sync {
    async fn list_instances(&self) -> Result<Vec<String>, String>;
    async fn list_executions(&self, instance: &str) -> Result<Vec<u64>, String>;
    async fn get_instance_info(&self, instance: &str) -> Result<InstanceInfo, String>;
    async fn get_execution_info(&self, instance: &str, exec_id: u64) -> Result<ExecutionInfo, String>;
    async fn read_execution_history(&self, instance: &str, exec_id: u64) -> Result<Vec<Event>, String>;
    async fn get_system_metrics(&self) -> Result<SystemMetrics, String>;
    async fn get_queue_depths(&self) -> Result<QueueDepths, String>;
}

// 3. Update Client with automatic discovery
pub struct Client {
    provider: Arc<dyn Provider>,
}

impl Client {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
    
    // Control plane (always available)
    pub async fn start_orchestration(...) { }
    
    // Management (automatically discovers capability)
    pub async fn list_all_instances(&self) -> Result<Vec<String>, String> {
        self.discover_management()?.list_instances().await
    }
    
    // Automatic capability discovery
    fn discover_management(&self) -> Result<&dyn ManagementCapability, String> {
        self.provider.as_management_capability()
            .ok_or_else(|| "Management features not available".to_string())
    }
    
    // Check if management capability is available
    pub fn has_management_capability(&self) -> bool {
        self.discover_management().is_ok()
    }
}

// 4. Implement capability detection for SqliteProvider
impl Provider for SqliteProvider {
    // ... existing runtime methods
    
    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        Some(self as &dyn ManagementCapability)
    }
}

impl ManagementCapability for SqliteProvider {
    // ... management methods
}
```

### Usage Examples

```rust
let store = Arc::new(SqliteProvider::new("sqlite:./data.db").await?);

// Single constructor - automatically discovers capabilities
let client = Client::new(store);

// Control plane (always available)
client.start_orchestration("order-1", "ProcessOrder", "{}").await?;
let status = client.get_status("order-1").await;

// Management (automatically discovered)
if client.has_management_capability() {
    let instances = client.list_all_instances().await?;
    let metrics = client.get_system_metrics().await?;
} else {
    println!("Management features not available");
}
```

---

## Benefits

1. **Automatic Discovery**: No explicit capability passing
2. **Single Constructor**: Just `Client::new(provider)`
3. **Extensible**: Each provider implements capability detection
4. **Backward Compatible**: Current API stays the same
5. **Type Safe**: Compile-time guarantees

**This gives us automatic capability discovery with a clean API!**

Want me to implement this?
