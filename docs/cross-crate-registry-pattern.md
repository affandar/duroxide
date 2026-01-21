# Cross-Crate Registry Pattern

**Version:** 1.1  
**Status:** Specification

---

## Overview

This document defines the convention for organizing, exporting, and importing orchestrations and activities across Duroxide library crates. Following this pattern enables modular composition of workflows from multiple domain-specific libraries.

**Naming Convention:** `{crate-name}::{type}::{name}`

Example:
- `duroxide-azure-arm::orchestration::provision-postgres`
- `duroxide-azure-arm::activity::provision-vm`

---

## Part 1: For Library Builders

*Instructions for creating Duroxide library crates (e.g., `duroxide-azure-arm`, `duroxide-aws-ec2`)*

### 1. Project Structure

Organize your crate as follows:

```
duroxide-azure-arm/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── names.rs              # Orchestration name constants (centralized)
│   ├── types.rs              # Orchestration input/output types
│   ├── activity_types.rs     # Activity input/output types
│   ├── registry.rs           # Registry builders
│   ├── inventory.rs          # Discovery API (optional)
│   ├── orchestrations/
│   │   ├── mod.rs
│   │   ├── provision_postgres.rs
│   │   └── deploy_webapp.rs
│   └── activities/
│       ├── mod.rs
│       ├── provision_vm.rs      # Contains NAME + activity fn
│       └── configure_firewall.rs
└── README.md
```

### 2. Define Orchestration Name Constants

Create `src/names.rs` with const strings for **orchestrations only**:

```rust
//! Name constants for orchestrations
//!
//! Note: Activity names are co-located with their implementations
//! in the activities/ module for better IDE navigation (F12 support).

/// Orchestration names
pub mod orchestrations {
    /// Provision an Azure PostgreSQL database
    /// 
    /// **Input:** [`crate::types::ProvisionPostgresInput`]  
    /// **Output:** [`crate::types::ProvisionPostgresOutput`]  
    /// **Activities used:**
    /// - [`crate::activities::provision_vm::NAME`]
    /// - [`crate::activities::configure_firewall::NAME`]
    pub const PROVISION_POSTGRES: &str = "duroxide-azure-arm::orchestration::provision-postgres";
    
    /// Deploy an Azure Web App
    pub const DEPLOY_WEBAPP: &str = "duroxide-azure-arm::orchestration::deploy-webapp";
}
```

**Why centralized orchestration names?**
- Orchestrations are referenced externally (client.start_orchestration)
- Having them in one file makes discovery easy
- Consumers import `names::orchestrations::PROVISION_POSTGRES`

### 3. Define Activity Names (Co-located with Implementation)

**Activity names live in each activity file**, not a centralized names file. This enables IDE navigation (F12 jumps to implementation):

```rust
// src/activities/provision_vm.rs

use duroxide::ActivityContext;
use crate::activity_types::{ProvisionVMInput, ProvisionVMOutput};

/// Activity name for registration and scheduling
/// 
/// **Input:** [`ProvisionVMInput`]  
/// **Output:** [`ProvisionVMOutput`]  
/// **Idempotent:** Yes
pub const NAME: &str = "duroxide-azure-arm::activity::provision-vm";

pub async fn activity(
    ctx: ActivityContext,
    input: ProvisionVMInput,
) -> Result<ProvisionVMOutput, String> {
    ctx.trace_info(format!("Provisioning VM: {}", input.name));
    
    // Implementation...
    
    Ok(ProvisionVMOutput {
        vm_id: "vm-123".to_string(),
        ip_address: "10.0.0.4".to_string(),
    })
}
```

**Benefits:**
- F12 in IDE → jumps directly to implementation
- Name can't get out of sync with implementation
- Doc comments on NAME are right next to the code

**Requirements:**
- Use your crate name as the prefix (e.g., `duroxide-azure-arm`)
- Use `::activity::` for activities
- Use kebab-case for names (e.g., `provision-vm`, not `ProvisionVM`)

### 4. Define Strongly-Typed Inputs and Outputs

Create **separate files** for orchestration and activity types:

**`src/types.rs`** - Orchestration types:
```rust
//! Input and output types for orchestrations

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionPostgresInput {
    pub database_name: String,
    pub resource_group: String,
    pub sku: String,
    pub admin_username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionPostgresOutput {
    pub server_id: String,
    pub connection_string: String,
    pub admin_password: String,
}
```

**`src/activity_types.rs`** - Activity types:
```rust
//! Input and output types for activities

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionVMInput {
    pub name: String,
    pub resource_group: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionVMOutput {
    pub vm_id: String,
    pub ip_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallConfig {
    pub resource_group: String,
    pub rules: Vec<FirewallRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallRule {
    pub name: String,
    pub port: u16,
    pub protocol: String,
}
```

**Requirements:**
- All types must implement `Serialize` and `Deserialize`
- Use descriptive struct names
- Document fields with doc comments
- Version types when breaking changes occur (e.g., `ProvisionPostgresInputV2`)

### 5. Implement Orchestrations (Typed)

Create orchestration functions in `src/orchestrations/` using typed signatures:

```rust
// src/orchestrations/provision_postgres.rs

use duroxide::OrchestrationContext;
use crate::activities;
use crate::types::{ProvisionPostgresInput, ProvisionPostgresOutput};
use crate::activity_types::ProvisionVMInput;

pub async fn provision_postgres_orchestration(
    ctx: OrchestrationContext,
    input: ProvisionPostgresInput,
) -> Result<ProvisionPostgresOutput, String> {
    ctx.trace_info(format!("Provisioning PostgreSQL: {}", input.database_name));
    
    // Call activities using NAME from the activity module
    let vm_input = ProvisionVMInput {
        name: format!("{}-vm", input.database_name),
        resource_group: input.resource_group.clone(),
    };
    
    let vm_output = ctx
        .schedule_activity_typed(activities::provision_vm::NAME, vm_input)
        .await?;
    
    // Build output
    Ok(ProvisionPostgresOutput {
        server_id: vm_output.vm_id,
        connection_string: format!("postgresql://{}:5432/{}", vm_output.ip_address, input.database_name),
        admin_password: "generated".to_string(),
    })
}
```

**Requirements:**
- Accept typed input, return `Result<TypedOutput, String>`
- Use activity NAME constants from the activities module
- Add trace logging for observability

### 6. Implement Activities (Typed)

Activities have their NAME co-located with implementation:

```rust
// src/activities/provision_vm.rs

use duroxide::ActivityContext;
use crate::activity_types::{ProvisionVMInput, ProvisionVMOutput};

/// Activity name for registration and scheduling
pub const NAME: &str = "duroxide-azure-arm::activity::provision-vm";

pub async fn activity(
    ctx: ActivityContext,
    input: ProvisionVMInput,
) -> Result<ProvisionVMOutput, String> {
    ctx.trace_info(format!("Provisioning VM: {}", input.name));
    
    // Check idempotency - does resource already exist?
    if let Some(existing) = get_existing_vm(&input.name).await? {
        ctx.trace_info("VM already exists, returning existing");
        return Ok(existing);
    }
    
    // Create the resource
    let vm = create_vm(&input).await?;
    
    ctx.trace_info("VM provisioned successfully");
    
    Ok(ProvisionVMOutput {
        vm_id: vm.id,
        ip_address: vm.ip,
    })
}
```

**Requirements:**
- Define `pub const NAME: &str` at the top of each activity file
- Accept `ActivityContext` as first parameter, then typed input
- Return `Result<TypedOutput, String>`
- Make activities idempotent when possible
- Use `ctx.trace_*()` for logging with automatic correlation IDs

### 7. Create Registry Builders

Create `src/registry.rs` using `register_typed()`:

```rust
//! Registry builders for exporting orchestrations and activities

use duroxide::OrchestrationRegistry;
use duroxide::runtime::registry::ActivityRegistry;
use crate::names::orchestrations;
use crate::activities;

/// Create an OrchestrationRegistry with all orchestrations from this crate.
///
/// Consumers can merge this into their own registry using `.merge()`.
pub fn create_orchestration_registry() -> OrchestrationRegistry {
    OrchestrationRegistry::builder()
        .register_typed(
            orchestrations::PROVISION_POSTGRES,
            crate::orchestrations::provision_postgres::provision_postgres_orchestration,
        )
        .register_typed(
            orchestrations::DEPLOY_WEBAPP,
            crate::orchestrations::deploy_webapp::deploy_webapp_orchestration,
        )
        .build()
}

/// Create an ActivityRegistry with all activities from this crate.
///
/// Consumers can merge this into their own registry using `.merge()`.
pub fn create_activity_registry() -> ActivityRegistry {
    ActivityRegistry::builder()
        // Use NAME from each activity module
        .register_typed(
            activities::provision_vm::NAME,
            activities::provision_vm::activity,
        )
        .register_typed(
            activities::configure_firewall::NAME,
            activities::configure_firewall::activity,
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_orchestration_registry_can_be_created() {
        let _registry = create_orchestration_registry();
    }
    
    #[test]
    fn test_activity_registry_can_be_created() {
        let _registry = create_activity_registry();
    }
}
```

**Key points:**
- Use `register_typed()` for automatic serde - no manual JSON serialization!
- Reference activity names via `activities::module_name::NAME`
- Reference orchestration names via `orchestrations::CONSTANT`

### 8. Create lib.rs Exports

Export all modules in `src/lib.rs`:

```rust
//! Azure ARM Orchestrations and Activities for Duroxide
//!
//! This crate provides orchestrations and activities for provisioning and managing
//! Azure resources using Duroxide's durable orchestration framework.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use duroxide_azure_arm::registry::{create_orchestration_registry, create_activity_registry};
//! use duroxide_azure_arm::names::orchestrations;
//! use std::sync::Arc;
//!
//! // Load all orchestrations and activities
//! let orchestrations = create_orchestration_registry();
//! let activities = Arc::new(create_activity_registry());
//!
//! // Use with Duroxide runtime
//! // Runtime::start_with_store(store, activities, orchestrations).await;
//! ```
//!
//! # Available Orchestrations
//!
//! - [`names::orchestrations::PROVISION_POSTGRES`] - Provision PostgreSQL database
//! - [`names::orchestrations::DEPLOY_WEBAPP`] - Deploy web application
//!
//! # Available Activities
//!
//! - [`activities::provision_vm::NAME`] - Provision Azure VM
//! - [`activities::configure_firewall::NAME`] - Configure firewall rules

pub mod names;
pub mod types;
pub mod activity_types;
pub mod orchestrations;
pub mod activities;
pub mod registry;

// Optional: runtime discovery
pub mod inventory;

// Re-export commonly used types for convenience
pub use types::*;
pub use activity_types::*;
```

### 9. Activities Module Structure

Organize `src/activities/mod.rs`:

```rust
//! Activities for Azure ARM operations
//!
//! Each activity module exports:
//! - `NAME`: The activity name constant for registration
//! - `activity`: The async activity function

pub mod provision_vm;
pub mod configure_firewall;

// Submodules for related activities
pub mod storage;
pub mod networking;
```

### 10. Optional: Add Inventory/Discovery

Create `src/inventory.rs` for runtime discovery:

```rust
//! Inventory of all orchestrations and activities for discovery

use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub input_type: &'static str,
    pub output_type: &'static str,
    pub activities_used: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub input_type: &'static str,
    pub output_type: &'static str,
    pub idempotent: bool,
}

pub fn list_orchestrations() -> Vec<OrchestrationInfo> {
    vec![
        OrchestrationInfo {
            name: crate::names::orchestrations::PROVISION_POSTGRES,
            description: "Provision an Azure PostgreSQL database",
            input_type: "ProvisionPostgresInput",
            output_type: "ProvisionPostgresOutput",
            activities_used: vec![
                crate::activities::provision_vm::NAME,
                crate::activities::configure_firewall::NAME,
            ],
        },
    ]
}

pub fn list_activities() -> Vec<ActivityInfo> {
    vec![
        ActivityInfo {
            name: crate::activities::provision_vm::NAME,
            description: "Provision an Azure VM",
            input_type: "ProvisionVMInput",
            output_type: "ProvisionVMOutput",
            idempotent: true,
        },
    ]
}
```

### 11. Document in README

Your crate's `README.md` should include:

```markdown
# duroxide-azure-arm

Azure ARM orchestrations and activities for Duroxide.

## Available Orchestrations

- `duroxide-azure-arm::orchestration::provision-postgres` - Provision PostgreSQL database
- `duroxide-azure-arm::orchestration::deploy-webapp` - Deploy web application

## Available Activities

- `duroxide-azure-arm::activity::provision-vm` - Provision Azure VM (idempotent)
- `duroxide-azure-arm::activity::configure-firewall` - Configure firewall rules (idempotent)

## Usage

See examples below for importing into your Duroxide application.
```

### 12. Cargo.toml Configuration

```toml
[package]
name = "duroxide-azure-arm"
version = "0.1.0"
edition = "2021"
description = "Azure ARM orchestrations and activities for Duroxide"

[dependencies]
duroxide = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

# Add your cloud SDK dependencies here
# azure_mgmt_compute = "0.1"
```

---

## Part 2: For Library Consumers

*Instructions for using Duroxide library crates in your applications*

### Pattern 1: Import Everything from One Crate

Simplest approach - load all orchestrations and activities from a single library:

```rust
use duroxide::runtime::Runtime;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide_azure_arm::registry::{create_orchestration_registry, create_activity_registry};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
    
    // Import pre-built registries
    let orchestrations = create_orchestration_registry();
    let activities = Arc::new(create_activity_registry());
    
    let runtime = Runtime::start_with_store(store, activities, orchestrations).await;
    
    // Your orchestrations and activities are now available
    Ok(())
}
```

### Pattern 2: Compose Multiple Crates

Combine orchestrations and activities from multiple libraries using `.merge()`:

```rust
use duroxide::{OrchestrationRegistry, Runtime};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
    
    // Compose orchestrations from multiple crates
    let orchestrations = OrchestrationRegistry::builder()
        .merge(duroxide_azure_arm::registry::create_orchestration_registry())
        .merge(duroxide_aws_ec2::registry::create_orchestration_registry())
        .merge(my_workflows::registry::create_orchestration_registry())
        .build();
    
    // Compose activities from multiple crates
    let activities = ActivityRegistry::builder()
        .merge(duroxide_azure_arm::registry::create_activity_registry())
        .merge(duroxide_aws_ec2::registry::create_activity_registry())
        .merge(my_activities::registry::create_activity_registry())
        .build();
    
    let runtime = Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;
    
    Ok(())
}
```

### Pattern 3: Selective Import

Import only specific orchestrations and activities you need:

```rust
use duroxide::{OrchestrationRegistry, Client};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide_azure_arm::names::orchestrations;
use duroxide_azure_arm::{orchestrations as orch_impl, activities};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
    
    // Import only specific items
    let orchestrations = OrchestrationRegistry::builder()
        .register_typed(
            orchestrations::PROVISION_POSTGRES,
            orch_impl::provision_postgres::provision_postgres_orchestration,
        )
        // Don't import DEPLOY_WEBAPP - we don't need it
        .build();
    
    let activities = Arc::new(
        ActivityRegistry::builder()
            .register_typed(
                activities::provision_vm::NAME,
                activities::provision_vm::activity,
            )
            .register_typed(
                activities::configure_firewall::NAME,
                activities::configure_firewall::activity,
            )
            .build()
    );
    
    let runtime = Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    
    Ok(())
}
```

### Pattern 4: Using Orchestrations

Start orchestrations using name constants with typed input:

```rust
use duroxide::Client;
use duroxide_azure_arm::names::orchestrations;
use duroxide_azure_arm::types::ProvisionPostgresInput;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(store);
    
    // Create strongly-typed input
    let input = ProvisionPostgresInput {
        database_name: "my-production-db".to_string(),
        resource_group: "production-rg".to_string(),
        sku: "Standard_D2s_v3".to_string(),
        admin_username: "dbadmin".to_string(),
    };
    
    // Start orchestration with typed input
    client.start_orchestration_typed(
        "postgres-prod-1",                      // instance ID
        orchestrations::PROVISION_POSTGRES,     // orchestration name
        input,                                  // typed input (auto-serialized)
    ).await?;
    
    // Wait for completion with typed output
    let output: ProvisionPostgresOutput = client.wait_for_orchestration_typed(
        "postgres-prod-1",
        std::time::Duration::from_secs(300),
    ).await?;
    
    println!("Connection string: {}", output.connection_string);
    
    Ok(())
}
```

### Pattern 5: Cross-Crate Orchestration Composition

Build orchestrations that use orchestrations from other crates:

```rust
// In your application crate

use duroxide::OrchestrationContext;
use duroxide_azure_arm::names::orchestrations as azure;
use duroxide_aws_ec2::names::orchestrations as aws;

async fn deploy_multi_cloud_app(
    ctx: OrchestrationContext,
    input: DeployMultiCloudInput,
) -> Result<DeployMultiCloudOutput, String> {
    // Deploy database on Azure
    let db_result = ctx
        .schedule_sub_orchestration_typed(
            azure::PROVISION_POSTGRES,
            "db-instance",
            db_config,
        )
        .await?;
    
    // Deploy compute on AWS
    let compute_result = ctx
        .schedule_sub_orchestration_typed(
            aws::CREATE_EC2_CLUSTER,
            "app-cluster",
            cluster_config,
        )
        .await?;
    
    Ok(DeployMultiCloudOutput {
        db_connection: db_result.connection_string,
        cluster_endpoint: compute_result.endpoint,
    })
}

// Register your custom orchestration alongside imported ones using .merge()
let orchestrations = OrchestrationRegistry::builder()
    .merge(duroxide_azure_arm::registry::create_orchestration_registry())
    .merge(duroxide_aws_ec2::registry::create_orchestration_registry())
    .register_typed("my-app::orchestration::deploy-multi-cloud", deploy_multi_cloud_app)
    .build();
```

### Pattern 6: Discovery at Runtime

Query available orchestrations and activities:

```rust
use duroxide_azure_arm::inventory;

// List all available orchestrations
for orch in inventory::list_orchestrations() {
    println!("{}", orch.name);
    println!("  Description: {}", orch.description);
    println!("  Input: {}", orch.input_type);
    println!("  Output: {}", orch.output_type);
    println!("  Uses activities: {:?}", orch.activities_used);
}

// List all available activities
for activity in inventory::list_activities() {
    println!("{}", activity.name);
    println!("  Idempotent: {}", activity.idempotent);
}
```

---

## Naming Best Practices

### DO ✅

```rust
// Good: Descriptive, kebab-case, prefixed with crate
"duroxide-azure-arm::orchestration::provision-postgres"
"duroxide-azure-arm::activity::configure-firewall"
"duroxide-aws-ec2::orchestration::create-vpc"

// Good: Hierarchical organization
"duroxide-stripe::orchestration::process-payment"
"duroxide-stripe::activity::create-charge"
"duroxide-stripe::activity::refund-charge"

// Good: Consistent prefix for ALL items in a crate
// All activities from duroxide-azure-arm use "duroxide-azure-arm::activity::"
```

### DON'T ❌

```rust
// Bad: No crate prefix (collision risk)
"provision-postgres"

// Bad: CamelCase or PascalCase
"duroxide-azure-arm::orchestration::ProvisionPostgres"

// Bad: Inconsistent separators
"duroxide-azure-arm/orchestration/provision-postgres"
"duroxide-azure-arm.orchestration.provision-postgres"

// Bad: Generic names
"duroxide-azure-arm::orchestration::workflow1"

// Bad: Inconsistent crate prefixes within same crate
"duroxide-azure-arm::activity::provision-vm"
"azure-activities::activity::configure-firewall"  // Different prefix!
```

---

## Testing Your Library

### Unit Tests

Test your orchestrations and activities:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use duroxide::Client;
    use duroxide::providers::sqlite::SqliteProvider;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_provision_postgres_orchestration() {
        let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
        let orchestrations = crate::registry::create_orchestration_registry();
        let activities = Arc::new(crate::registry::create_activity_registry());
        
        let runtime = Runtime::start_with_store(
            store.clone(),
            activities,
            orchestrations,
        ).await;
        
        let client = Client::new(store);
        
        let input = ProvisionPostgresInput {
            database_name: "test-db".to_string(),
            resource_group: "test-rg".to_string(),
            sku: "Basic".to_string(),
            admin_username: "admin".to_string(),
        };
        
        client.start_orchestration_typed(
            "test-instance",
            crate::names::orchestrations::PROVISION_POSTGRES,
            input,
        ).await.unwrap();
        
        // Wait and verify
        let output: ProvisionPostgresOutput = client.wait_for_orchestration_typed(
            "test-instance",
            std::time::Duration::from_secs(10),
        ).await.unwrap();
        
        assert!(!output.connection_string.is_empty());
        
        runtime.shutdown(None).await;
    }
}
```

---

## Versioning Strategy

### Semantic Versioning

Follow semver for your library crate:

- **MAJOR**: Breaking changes to orchestration/activity signatures
- **MINOR**: New orchestrations or activities added
- **PATCH**: Bug fixes, internal improvements

### Type Versioning

When making breaking changes to input/output types:

```rust
// v1
pub struct ProvisionPostgresInput {
    pub database_name: String,
}

// v2 (breaking change)
pub struct ProvisionPostgresInputV2 {
    pub database_name: String,
    pub region: String,  // New required field
}

// Register both versions
pub const PROVISION_POSTGRES_V1: &str = "duroxide-azure-arm::orchestration::provision-postgres@v1";
pub const PROVISION_POSTGRES_V2: &str = "duroxide-azure-arm::orchestration::provision-postgres@v2";
```

Or use Duroxide's built-in versioning:

```rust
// Register with explicit version
builder.register_versioned_typed(
    "duroxide-azure-arm::orchestration::provision-postgres",
    "2.0.0",
    provision_postgres_v2_orchestration,
)
```

---

## Publishing Checklist

Before publishing your Duroxide library crate:

- [ ] All orchestration and activity names follow `{crate}::{type}::{name}` pattern
- [ ] Orchestration names centralized in `names.rs`
- [ ] Activity names co-located with implementations (each file has `pub const NAME`)
- [ ] Input/output types are strongly-typed with serde
- [ ] Registry uses `register_typed()` for type-safe registration
- [ ] Documentation includes input/output types for each orchestration/activity
- [ ] Activities document whether they're idempotent
- [ ] Unit tests verify orchestrations work end-to-end
- [ ] README lists all available orchestrations and activities
- [ ] Cargo.toml has correct duroxide dependency
- [ ] All names use consistent crate prefix

---

## Common Patterns

### Pattern: Configuration Injection

Pass configuration to activities through dependency injection:

```rust
// Library provides a factory
pub fn create_activity_registry_with_config(azure_config: AzureConfig) -> ActivityRegistry {
    let config = Arc::new(azure_config);
    
    ActivityRegistry::builder()
        .register_typed(activities::provision_vm::NAME, {
            let config = config.clone();
            move |ctx: ActivityContext, input: ProvisionVMInput| {
                let config = config.clone();
                async move {
                    ctx.trace_info("Provisioning VM with Azure SDK");
                    let client = AzureClient::new(&config);
                    // Use client...
                    Ok(ProvisionVMOutput { /* ... */ })
                }
            }
        })
        .build()
}

// Consumer configures
let config = AzureConfig::from_env();
let activities = duroxide_azure_arm::registry::create_activity_registry_with_config(config);
```

### Pattern: Error Types

Define domain-specific error types:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AzureError {
    ResourceNotFound { resource_id: String },
    QuotaExceeded { resource_type: String },
    AuthenticationFailed,
}

// Activities can return rich errors (serialized to String)
pub async fn activity(
    ctx: ActivityContext,
    input: ProvisionVMInput,
) -> Result<ProvisionVMOutput, String> {
    // ... on error:
    ctx.trace_error("Quota exceeded for VM provisioning");
    Err(serde_json::to_string(&AzureError::QuotaExceeded {
        resource_type: "VM".to_string(),
    }).unwrap())
}
```

### Pattern: Idempotency

Make activities idempotent by checking existing state:

```rust
pub async fn activity(
    ctx: ActivityContext,
    input: ProvisionVMInput,
) -> Result<ProvisionVMOutput, String> {
    // Check if VM already exists (idempotency)
    if let Some(existing_vm) = azure_client.get_vm(&input.name).await? {
        ctx.trace_info("VM already exists, returning existing");
        return Ok(ProvisionVMOutput {
            vm_id: existing_vm.id,
            ip_address: existing_vm.ip,
        });
    }
    
    // Create only if it doesn't exist
    ctx.trace_info(format!("Creating new VM: {}", input.name));
    let vm = azure_client.create_vm(input).await?;
    Ok(ProvisionVMOutput {
        vm_id: vm.id,
        ip_address: vm.ip,
    })
}
```

---

## FAQ

### Q: Can orchestration names collide across crates?

**A:** No, if you follow the naming convention. Each crate prefixes names with its own crate name (e.g., `duroxide-azure-arm::`, `duroxide-aws-ec2::`).

### Q: Why are activity names in the activity files, not centralized?

**A:** For IDE navigation. When you F12 on `activities::provision_vm::NAME` in the registry, you jump directly to the implementation file. Centralized names require an extra hop.

### Q: Should I use sub-orchestrations or activities for complex operations?

**A:** 
- **Activity**: Single-purpose operation (provision one VM, send one email)
- **Sub-orchestration**: Multi-step workflow that benefits from durability (deploy entire application stack)

### Q: How do I handle secrets and credentials?

**A:** Pass through configuration or environment variables. Never hardcode in orchestrations or activities.

```rust
// Good: Configuration through dependency injection
let activities = create_activity_registry_with_config(AzureConfig {
    subscription_id: env::var("AZURE_SUBSCRIPTION_ID")?,
    tenant_id: env::var("AZURE_TENANT_ID")?,
});
```

### Q: Can consumers override my orchestrations?

**A:** Yes, they can register their own implementation with the same name (last registration wins):

```rust
let orchestrations = OrchestrationRegistry::builder()
    .merge(duroxide_azure_arm::registry::create_orchestration_registry())
    .register_typed(
        duroxide_azure_arm::names::orchestrations::PROVISION_POSTGRES,
        my_custom_implementation,  // Overrides the library version
    )
    .build();
```

---

## Example Library Crates

### Suggested Crate Names

- `duroxide-azure-arm` - Azure Resource Manager
- `duroxide-aws-ec2` - AWS EC2 compute
- `duroxide-aws-s3` - AWS S3 storage
- `duroxide-gcp-compute` - Google Cloud Compute
- `duroxide-stripe` - Stripe payment processing
- `duroxide-sendgrid` - SendGrid email
- `duroxide-twilio` - Twilio messaging
- `duroxide-slack` - Slack notifications
- `duroxide-github` - GitHub API operations
- `duroxide-k8s` - Kubernetes orchestrations

---

## Summary

### For Library Builders:

1. Create `names.rs` with orchestration constants (centralized for external reference)
2. Put activity `NAME` constants in each activity file (for IDE navigation)
3. Create `types.rs` for orchestration types, `activity_types.rs` for activity types
4. Use `register_typed()` in registry for automatic serde
5. Implement typed orchestrations and activities
6. Document all exports with input/output types
7. Test thoroughly

**Key points:**
- Orchestration names → centralized in `names.rs`
- Activity names → co-located with implementation (`pub const NAME`)
- Use `register_typed()` → no manual JSON serialization

### For Library Consumers:

1. Add library to `Cargo.toml`
2. Use `.merge()` to compose registries from multiple crates:
   ```rust
   OrchestrationRegistry::builder()
       .merge(lib1::create_orchestration_registry())
       .merge(lib2::create_orchestration_registry())
       .build()
   ```
3. Use name constants when starting orchestrations
4. Use typed methods for automatic serde

**Key features:**
- `.merge()` - Combine registries from multiple crates
- `register_typed()` / `start_orchestration_typed()` - Type-safe with auto-serde
- Activity names via `activities::module::NAME` - IDE navigation works

**This pattern enables a rich ecosystem of reusable Duroxide workflows!**
