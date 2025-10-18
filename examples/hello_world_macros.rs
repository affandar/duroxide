//! Hello World with Macros Example
//!
//! Demonstrates the new macro-based API for duroxide.
//!
//! Run with: `cargo run --example hello_world_macros --features macros`

#[cfg(feature = "macros")]
use duroxide::prelude::*;

// Simple activity using macro
#[cfg(feature = "macros")]
#[activity(typed)]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

// Simple orchestration using macro
#[cfg(feature = "macros")]
#[orchestration]
async fn hello_world(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    // Phase 3: Use durable!() macro!
    let greeting = durable!(greet(name)).await?;
    Ok(greeting)
}

#[cfg(feature = "macros")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    println!("🚀 Hello World with Macros (Phases 1-6 Complete!)\n");
    println!("Features:");
    
    // Phases 4-6: Use auto-discovery!
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("hello_macros.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // Phase 4: Auto-discovery!
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    // Phase 5: Use generated client helpers!
    hello_world::start(&client, "hello-1", "Rust Developer".to_string()).await?;
    
    // Phase 5: Type-safe wait!
    match hello_world::wait(&client, "hello-1", Duration::from_secs(10)).await? {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Result: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Failed: {}", error);
        }
        _ => {
            println!("⏳ Still running...");
        }
    }
    
    rt.shutdown().await;
    Ok(())
}

#[cfg(not(feature = "macros"))]
fn main() {
    eprintln!("This example requires the 'macros' feature.");
    eprintln!("Run with: cargo run --example hello_world_macros --features macros");
    std::process::exit(1);
}

