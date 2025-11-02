//! Example demonstrating registry discovery
//!
//! This example shows how clients can discover which orchestrations and activities
//! are registered in a running runtime.
//!
//! Run with:
//! ```
//! cargo run --example registry_discovery
//! ```

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::{ActivityRegistry, OrchestrationRegistry};
use duroxide::runtime::Runtime;
use duroxide::{ActivityContext, Client, OrchestrationContext};
use std::sync::Arc;

// Sample orchestrations
async fn hello_orchestration(_ctx: OrchestrationContext, input: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", input))
}

async fn goodbye_orchestration(_ctx: OrchestrationContext, input: String) -> Result<String, String> {
    Ok(format!("Goodbye, {}!", input))
}

// Sample activities
async fn greet_activity(_ctx: ActivityContext, name: String) -> Result<String, String> {
    Ok(format!("Greetings, {}!", name))
}

async fn process_activity(_ctx: ActivityContext, data: String) -> Result<String, String> {
    Ok(format!("Processed: {}", data))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Registry Discovery Example ===\n");

    // Create a SQLite provider (in-memory for this example)
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await?);

    // Register orchestrations
    let orch_registry = OrchestrationRegistry::builder()
        .register("HelloOrchestration", hello_orchestration)
        .register("GoodbyeOrchestration", goodbye_orchestration)
        .build();

    // Register activities
    let activity_registry = Arc::new(
        ActivityRegistry::builder()
            .register("GreetActivity", greet_activity)
            .register("ProcessActivity", process_activity)
            .build(),
    );

    // Start runtime - this automatically registers the orchestrations and activities
    let runtime = Runtime::start_with_store(store.clone(), activity_registry, orch_registry).await;

    // Create a client to interact with the runtime
    let client = Client::new(store.clone());

    // Check if management capabilities are available
    if !client.has_management_capability() {
        println!("Management capabilities not available");
        runtime.shutdown(None).await;
        return Ok(());
    }

    println!("✓ Management capabilities available\n");

    // Get the registry snapshot
    let snapshot = client.get_registry_snapshot().await?;

    // Display registered orchestrations
    println!("📋 Registered Orchestrations ({}):", snapshot.orchestrations.len());
    for orch in &snapshot.orchestrations {
        println!("  • {} (versions: {:?})", orch.name, orch.versions);
        println!(
            "    Last registered: {}",
            orch.registered_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    println!();

    // Display registered activities
    println!("🔧 Registered Activities ({}):", snapshot.activities.len());
    for activity in &snapshot.activities {
        println!("  • {}", activity.name);
        println!(
            "    Last registered: {}",
            activity.registered_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    println!("\n=== Example Complete ===");

    // Shutdown runtime
    runtime.shutdown(None).await;

    Ok(())
}
