# Cross-Crate Registry Pattern

**Version:** 1.0  
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
│   ├── names.rs              # Name constants with documentation
│   ├── types.rs              # Input/output types
│   ├── registry.rs           # Registry builders
│   ├── inventory.rs          # Discovery API (optional)
│   ├── orchestrations/
│   │   ├── mod.rs
│   │   ├── provision_postgres.rs
│   │   └── deploy_webapp.rs
│   └── activities/
│       ├── mod.rs
│       ├── provision_vm.rs
│       └── configure_firewall.rs
└── README.md
```

### 2. Define Name Constants

Create `src/names.rs` with const strings for all orchestrations and activities:

```rust
//! Name constants for orchestrations and activities

/// Orchestration names
pub mod orchestrations {
    /// Provision an Azure PostgreSQL database
    /// 
    /// **Input:** [`crate::types::ProvisionPostgresInput`]  
    /// **Output:** [`crate::types::ProvisionPostgresOutput`]  
    /// **Activities used:**
    /// - [`super::activities::PROVISION_VM`]
    /// - [`super::activities::CONFIGURE_FIREWALL`]
    pub const PROVISION_POSTGRES: &str = "duroxide-azure-arm::orchestration::provision-postgres";
    
    /// Deploy an Azure Web App
    pub const DEPLOY_WEBAPP: &str = "duroxide-azure-arm::orchestration::deploy-webapp";
}

/// Activity names
pub mod activities {
    /// Provision an Azure VM
    /// 
    /// **Input:** [`crate::types::ProvisionVMInput`]  
    /// **Output:** [`crate::types::ProvisionVMOutput`]  
    /// **Idempotent:** Yes
    pub const PROVISION_VM: &str = "duroxide-azure-arm::activity::provision-vm";
    
    /// Configure Azure firewall rules
    /// 
    /// **Input:** [`crate::types::FirewallConfig`]  
    /// **Output:** [`crate::types::FirewallRuleId`]  
    /// **Idempotent:** Yes
    pub const CONFIGURE_FIREWALL: &str = "duroxide-azure-arm::activity::configure-firewall";
}
```

**Requirements:**
- Use your crate name as the prefix (e.g., `duroxide-azure-arm`)
- Use `::orchestration::` for orchestrations
- Use `::activity::` for activities
- Use kebab-case for names (e.g., `provision-postgres`, not `ProvisionPostgres`)
- Document input/output types in doc comments
- Document whether activities are idempotent
- List dependencies (which activities an orchestration uses)

### 3. Define Strongly-Typed Inputs and Outputs

Create `src/types.rs` with serde-enabled types:

```rust
//! Input and output types for orchestrations and activities

use serde::{Deserialize, Serialize};

// Orchestration types

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

// Activity types

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionVMInput {
    pub name: String,
    pub resource_group: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionVMOutput {
    pub vm_id: String,
    pub ip_address: String,
    pub admin_password: String,
}
```

**Requirements:**
- All types must implement `Serialize` and `Deserialize`
- Use descriptive struct names
- Document fields with doc comments
- Version types when breaking changes occur (e.g., `ProvisionPostgresInputV2`)

### 4. Implement Orchestrations

Create orchestration functions in `src/orchestrations/`:

```rust
// src/orchestrations/provision_postgres.rs

use duroxide::OrchestrationContext;
use crate::names::activities;
use crate::types::*;

pub async fn provision_postgres_orchestration(
    ctx: OrchestrationContext,
    input: String,
) -> Result<String, String> {
    // 1. Deserialize input
    let input: ProvisionPostgresInput = serde_json::from_str(&input)
        .map_err(|e| format!("Invalid input: {}", e))?;
    
    ctx.trace_info(format!("Provisioning PostgreSQL: {}", input.database_name));
    
    // 2. Call activities using name constants
    let vm_input = ProvisionVMInput {
        name: format!("{}-vm", input.database_name),
        resource_group: input.resource_group.clone(),
    };
    
    let vm_result = ctx
        .schedule_activity(activities::PROVISION_VM, serde_json::to_string(&vm_input).unwrap())
        .into_activity()
        .await?;
    
    let vm_output: ProvisionVMOutput = serde_json::from_str(&vm_result)
        .map_err(|e| format!("Failed to parse VM output: {}", e))?;
    
    // 3. Build and serialize output
    let output = ProvisionPostgresOutput {
        server_id: vm_output.vm_id,
        connection_string: format!("postgresql://{}:5432/{}", vm_output.ip_address, input.database_name),
        admin_password: vm_output.admin_password,
    };
    
    serde_json::to_string(&output).map_err(|e| format!("Failed to serialize output: {}", e))
}
```

**Requirements:**
- Accept `String` input, return `Result<String, String>`
- Deserialize input to strongly-typed struct
- Serialize output from strongly-typed struct
- Use name constants from `crate::names` when calling activities
- Add trace logging for observability

### 5. Implement Activities

Create activity functions in `src/activities/`:

```rust
// src/activities/provision_vm.rs

use duroxide::ActivityContext;
use crate::types::*;

pub async fn provision_vm_activity(ctx: ActivityContext, input: String) -> Result<String, String> {
    let input: ProvisionVMInput = serde_json::from_str(&input)
        .map_err(|e| format!("Invalid input: {}", e))?;
    
    ctx.trace_info(format!("Provisioning VM: {}", input.name));
    
    // Implement your activity logic
    // Best practice: Make idempotent
    
    let output = ProvisionVMOutput {
        vm_id: create_or_get_vm(&input).await?,
        ip_address: "10.0.0.4".to_string(),
        admin_password: generate_password(),
    };
    
    ctx.trace_info("VM provisioned successfully");
    
    serde_json::to_string(&output).map_err(|e| format!("Failed to serialize output: {}", e))
}
```

**Requirements:**
- Accept `ActivityContext` as first parameter, then `String` input
- Return `Result<String, String>`
- Deserialize input, serialize output
- Make activities idempotent when possible
- Handle errors gracefully
- Activities can use any async operations (sleep, HTTP, database, etc.)
- Use `ctx.trace_*()` for logging with automatic correlation IDs

### 6. Create Registry Builders

Create `src/registry.rs` with a function that returns a complete registry:

```rust
//! Registry builders for exporting orchestrations and activities

use duroxide::OrchestrationRegistry;
use duroxide::runtime::registry::ActivityRegistry;
use crate::names::{orchestrations, activities};

/// Create an OrchestrationRegistry with all orchestrations from this crate.
///
/// Consumers can merge this into their own registry using `.merge()`.
///
/// # Example
///
/// ```rust
/// let orchestrations = duroxide_azure_arm::registry::create_orchestration_registry();
/// ```
pub fn create_orchestration_registry() -> OrchestrationRegistry {
    OrchestrationRegistry::builder()
        .register(
            orchestrations::PROVISION_POSTGRES,
            crate::orchestrations::provision_postgres::provision_postgres_orchestration,
        )
        .register(
            orchestrations::DEPLOY_WEBAPP,
            crate::orchestrations::deploy_webapp::deploy_webapp_orchestration,
        )
        .build()
}

/// Create an ActivityRegistry with all activities from this crate.
///
/// Consumers can merge this into their own registry using `.merge()`.
///
/// # Example
///
/// ```rust
/// let activities = duroxide_azure_arm::registry::create_activity_registry();
/// ```
pub fn create_activity_registry() -> ActivityRegistry {
    ActivityRegistry::builder()
        .register(
            activities::PROVISION_VM,
            crate::activities::provision_vm::provision_vm_activity,
        )
        .register(
            activities::CONFIGURE_FIREWALL,
            crate::activities::configure_firewall::configure_firewall_activity,
        )
        .build()
}
```

**Requirements:**
- Provide `create_orchestration_registry()` that returns a complete `OrchestrationRegistry`
- Provide `create_activity_registry()` that returns a complete `ActivityRegistry`
- **No need for separate `extend_*` functions** - consumers use `.merge()` instead

### 7. Create lib.rs Exports

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
//! - [`names::activities::PROVISION_VM`] - Provision Azure VM
//! - [`names::activities::CONFIGURE_FIREWALL`] - Configure firewall rules

pub mod names;
pub mod types;
pub mod orchestrations;
pub mod activities;
pub mod registry;

// Optional: runtime discovery
pub mod inventory;

// Re-export commonly used types for convenience
pub use types::*;
```

### 8. Optional: Add Inventory/Discovery

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
                crate::names::activities::PROVISION_VM,
                crate::names::activities::CONFIGURE_FIREWALL,
            ],
        },
        // ... more orchestrations
    ]
}

pub fn list_activities() -> Vec<ActivityInfo> {
    vec![
        ActivityInfo {
            name: crate::names::activities::PROVISION_VM,
            description: "Provision an Azure VM",
            input_type: "ProvisionVMInput",
            output_type: "ProvisionVMOutput",
            idempotent: true,
        },
        // ... more activities
    ]
}
```

### 9. Document in README

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

### 10. Cargo.toml Configuration

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
use duroxide_azure_arm::names::{orchestrations, activities};
use duroxide_azure_arm::orchestrations;
use duroxide_azure_arm::activities;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(SqliteProvider::new("sqlite:./data.db", None).await?);
    
    // Import only specific items
    let orchestrations = OrchestrationRegistry::builder()
        .register(
            orchestrations::PROVISION_POSTGRES,
            orchestrations::provision_postgres::provision_postgres_orchestration,
        )
        // Don't import DEPLOY_WEBAPP - we don't need it
        .build();
    
    let activities = Arc::new(
        ActivityRegistry::builder()
            .register(
                activities::PROVISION_VM,
                activities::provision_vm::provision_vm_activity,
            )
            .register(
                activities::CONFIGURE_FIREWALL,
                activities::configure_firewall::configure_firewall_activity,
            )
            .build()
    );
    
    let runtime = Runtime::start_with_store(store.clone(), activities, orchestrations).await;
    
    Ok(())
}
```

### Pattern 4: Using Orchestrations

Start orchestrations using name constants:

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
    
    // Start orchestration with name constant
    client.start_orchestration(
        "postgres-prod-1",                      // instance ID
        orchestrations::PROVISION_POSTGRES,     // orchestration name
        serde_json::to_string(&input)?,         // serialized input
    ).await?;
    
    // Wait for completion
    let result = client.wait_for_orchestration(
        "postgres-prod-1",
        std::time::Duration::from_secs(300),
    ).await?;
    
    // Deserialize result
    if let OrchestrationStatus::Completed { output } = result {
        let output: ProvisionPostgresOutput = serde_json::from_str(&output)?;
        println!("Connection string: {}", output.connection_string);
    }
    
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
    input: String,
) -> Result<String, String> {
    // Deploy database on Azure
    let db_result = ctx
        .schedule_sub_orchestration(
            azure::PROVISION_POSTGRES,
            "db-instance",
            db_config,
        )
        .into_sub_orchestration()
        .await?;
    
    // Deploy compute on AWS
    let compute_result = ctx
        .schedule_sub_orchestration(
            aws::CREATE_EC2_CLUSTER,
            "app-cluster",
            cluster_config,
        )
        .into_sub_orchestration()
        .await?;
    
    Ok(format!("Deployed: DB={}, Compute={}", db_result, compute_result))
}

// Register your custom orchestration alongside imported ones using .merge()
let orchestrations = OrchestrationRegistry::builder()
    .merge(duroxide_azure_arm::registry::create_orchestration_registry())
    .merge(duroxide_aws_ec2::registry::create_orchestration_registry())
    .register("my-app::orchestration::deploy-multi-cloud", deploy_multi_cloud_app)
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
        
        client.start_orchestration(
            "test-instance",
            crate::names::orchestrations::PROVISION_POSTGRES,
            test_input_json,
        ).await.unwrap();
        
        // Wait and verify
        let status = client.wait_for_orchestration(
            "test-instance",
            std::time::Duration::from_secs(10),
        ).await.unwrap();
        
        assert!(matches!(status, OrchestrationStatus::Completed { .. }));
        
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
builder.register_with_version(
    "duroxide-azure-arm::orchestration::provision-postgres",
    "2.0.0",
    provision_postgres_v2_orchestration,
)
```

---

## Publishing Checklist

Before publishing your Duroxide library crate:

- [ ] All orchestration and activity names follow the pattern
- [ ] Name constants defined in `names` module
- [ ] Input/output types are strongly-typed with serde
- [ ] Registry builders (`create_*` and `extend_*`) are provided
- [ ] Documentation includes input/output types for each orchestration/activity
- [ ] Activities document whether they're idempotent
- [ ] Unit tests verify orchestrations work end-to-end
- [ ] README lists all available orchestrations and activities
- [ ] Cargo.toml has correct duroxide dependency

---

## Common Patterns

### Pattern: Configuration Injection

Pass configuration to orchestrations through dependency injection:

```rust
// Library provides a factory
pub fn create_activity_registry_with_config(azure_config: AzureConfig) -> ActivityRegistry {
    ActivityRegistry::builder()
        .register(activities::PROVISION_VM, move |ctx: ActivityContext, input: String| {
            let config = azure_config.clone();
            async move {
                ctx.trace_info("Provisioning VM with Azure SDK");
                let client = AzureClient::new(config);
                // Use client...
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

// Activities return Result<String, String> but can serialize rich errors
pub async fn provision_vm_activity(ctx: ActivityContext, input: String) -> Result<String, String> {
    // ... on error:
    ctx.trace_error("Quota exceeded for VM provisioning");
    Err(serde_json::to_string(&AzureError::QuotaExceeded {
        resource_type: "VM".to_string(),
    }).unwrap())
}
```

### Pattern: Idempotency Tokens

Use instance IDs for idempotency:

```rust
pub async fn provision_vm_activity(ctx: ActivityContext, input: String) -> Result<String, String> {
    let input: ProvisionVMInput = serde_json::from_str(&input)?;
    
    // Check if VM already exists (idempotency)
    if let Some(existing_vm) = azure_client.get_vm(&input.name).await? {
        ctx.trace_info("VM already exists, returning existing");
        return Ok(serde_json::to_string(&existing_vm)?);
    }
    
    // Create only if it doesn't exist
    ctx.trace_info(format!("Creating new VM: {}", input.name));
    let vm = azure_client.create_vm(input).await?;
    Ok(serde_json::to_string(&vm)?)
}
```

---

## FAQ

### Q: Can orchestration names collide across crates?

**A:** No, if you follow the naming convention. Each crate prefixes names with its own crate name (e.g., `duroxide-azure-arm::`, `duroxide-aws-ec2::`).

### Q: Can I rename an orchestration after publishing?

**A:** You can add a new name and deprecate the old one, but avoid removing old names (breaks consumers). Use versioning instead.

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

**A:** Yes, they can register their own implementation with the same name:

```rust
// This will override the library's implementation
let orchestrations = OrchestrationRegistry::builder();
let orchestrations = duroxide_azure_arm::registry::extend_orchestration_registry(orchestrations);
let orchestrations = orchestrations.register(
    duroxide_azure_arm::names::orchestrations::PROVISION_POSTGRES,
    my_custom_implementation,  // Overrides the library version
);
let orchestrations = orchestrations.build();
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

1. Create `names.rs` with const strings following `{crate}::{type}::{name}` pattern
2. Create `types.rs` with strongly-typed serde structs
3. Implement orchestrations and activities
4. Create `registry.rs` with `create_orchestration_registry()` and `create_activity_registry()` functions
5. Document all exports with input/output types
6. Test thoroughly

**Key point:** Just provide `create_*()` functions that return complete registries. No need for `extend_*()` helpers - consumers use `.merge()`.

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
4. Deserialize outputs to strongly-typed structs

**Key features:**
- `.merge()` - Combine registries from multiple crates
- `.builder_from()` - Extend an existing registry
- `.register_versioned_typed()` - Type-safe versioned orchestrations

**This pattern enables a rich ecosystem of reusable Duroxide workflows!**
