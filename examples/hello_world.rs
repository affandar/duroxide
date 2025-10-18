//! Hello World Example - Start here to learn Duroxide basics
//!
//! This example demonstrates:
//! - Setting up a basic orchestration with activities using macros
//! - Using the SQLite provider for persistence
//! - Running orchestrations with the in-process runtime
//! - Auto-discovery of activities and orchestrations
//!
//! Run with: `cargo run --example hello_world --features macros`

#[cfg(feature = "macros")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use duroxide::providers::sqlite::SqliteProvider;
    use duroxide::runtime::{self};
    use duroxide::{Client, OrchestrationContext};
    use std::sync::Arc;

    // Import macros directly
    use duroxide_macros::*;

    // Initialize tracing for better output
    tracing_subscriber::fmt::init();

    // Create a temporary SQLite database for persistence
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("hello_world.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);

    // Define activities using macros
    #[activity("examples::hello_world::greet")]
    async fn greet(name: String) -> Result<String, String> {
        Ok(format!("Hello, {}!", name))
    }

    // Define orchestration using macros
    #[orchestration("examples::hello_world::hello_world")]
    async fn hello_world(ctx: OrchestrationContext, name: String) -> Result<String, String> {
        ctx.trace_info("Starting greeting orchestration");

        // Schedule and await the greeting activity
        let greeting = ctx.schedule_activity("examples::hello_world::greet", name).into_activity().await?;

        ctx.trace_info(format!("Greeting completed: {}", greeting));
        Ok(greeting)
    }

    // Start the runtime with auto-discovery
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await?;

    // Create a client bound to the same provider
    let client = Client::new(store);

    // Start an orchestration instance
    let instance_id = "hello-instance-1";
    client
        .start_orchestration(instance_id, "examples::hello_world::hello_world", "Rust Developer")
        .await?;

    // Wait for completion
    match client
        .wait_for_orchestration(instance_id, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            println!("✅ Orchestration completed successfully!");
            println!("Result: {}", output);
        }
        duroxide::OrchestrationStatus::Failed { error } => {
            println!("❌ Orchestration failed: {}", error);
        }
        _ => {
            println!("⏳ Orchestration still running or in unexpected state");
        }
    }

    rt.shutdown().await;
    Ok(())
}

#[cfg(not(feature = "macros"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("This example requires the 'macros' feature to be enabled.");
    println!("Run with: cargo run --example hello_world --features macros");
    Ok(())
}
