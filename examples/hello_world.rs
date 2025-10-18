//! Hello World Example - Start here to learn Duroxide basics
//!
//! This example demonstrates:
//! - Setting up activities with #[activity(typed)]
//! - Creating orchestrations with #[orchestration]
//! - Using durable!() for type-safe activity calls
//! - Auto-discovery with Runtime::builder()
//!
//! For the low-level API without macros, see: low_level_api.rs
//!
//! Run with: `cargo run --example hello_world`

use duroxide::prelude::*;

// Simple activity using macros
#[activity(typed)]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

// Orchestration using macros
#[orchestration]
async fn hello_world(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    durable_trace_info!("Starting greeting orchestration");
    
    // Type-safe activity call with durable!() macro
    let greeting = durable!(greet(name)).await?;
    
    durable_trace_info!("Greeting completed: {}", greeting);
    
    Ok(greeting)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for better output
    tracing_subscriber::fmt::init();
    
    // Create a temporary SQLite database for persistence
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("hello_world.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // Start the runtime with auto-discovery
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    // Create a client bound to the same provider
    let client = Client::new(store);
    
    // Start an orchestration using generated client helper
    let instance_id = "hello-instance-1";
    hello_world::start(&client, instance_id, "Rust Developer".to_string()).await?;
    
    // Wait for completion with typed output
    match hello_world::wait(&client, instance_id, Duration::from_secs(10)).await? {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Orchestration completed successfully!");
            println!("Result: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Orchestration failed: {}", error);
        }
        _ => {
            println!("⏳ Orchestration still running or in unexpected state");
        }
    }
    
    rt.shutdown().await;
    
    println!("\n📝 This example uses the macro-based API.");
    println!("For the low-level API, see: cargo run --example low_level_api");
    
    Ok(())
}
