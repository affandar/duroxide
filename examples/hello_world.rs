//! Hello World Example - Start here to learn Duroxide basics
//!
//! This example demonstrates:
//! - Setting up a basic orchestration with activities
//! - Using the SQLite provider for persistence
//! - Running orchestrations with the in-process runtime
//!
//! Run with: `cargo run --example hello_world`

use duroxide::providers::sqlite::SqliteHistoryStore;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for better output
    tracing_subscriber::fmt::init();

    // Create a temporary SQLite database for persistence
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("hello_world.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteHistoryStore::new(&db_url).await?);

    // Register a simple activity that greets users
    let activities = ActivityRegistry::builder()
        .register("Greet", |name: String| async move {
            Ok(format!("Hello, {}!", name))
        })
        .build();

    // Define our orchestration
    let orchestration = |ctx: OrchestrationContext, name: String| async move {
        ctx.trace_info("Starting greeting orchestration");
        
        // Schedule and await the greeting activity
        let greeting = ctx
            .schedule_activity("Greet", name)
            .into_activity()
            .await?;
        
        ctx.trace_info(format!("Greeting completed: {}", greeting));
        Ok(greeting)
    };

    // Register the orchestration
    let orchestrations = OrchestrationRegistry::builder()
        .register("HelloWorld", orchestration)
        .build();

    // Start the runtime
    let rt = runtime::Runtime::start_with_store(
        store,
        Arc::new(activities),
        orchestrations,
    ).await;

    // Start an orchestration instance
    let instance_id = "hello-instance-1";
    let _handle = rt
        .clone()
        .start_orchestration(instance_id, "HelloWorld", "Rust Developer")
        .await?;

    // Wait for completion
    match rt
        .wait_for_orchestration(instance_id, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        runtime::OrchestrationStatus::Completed { output } => {
            println!("✅ Orchestration completed successfully!");
            println!("Result: {}", output);
        }
        runtime::OrchestrationStatus::Failed { error } => {
            println!("❌ Orchestration failed: {}", error);
        }
        _ => {
            println!("⏳ Orchestration still running or in unexpected state");
        }
    }

    rt.shutdown().await;
    Ok(())
}
