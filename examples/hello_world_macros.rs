//! Hello World with Macros Example
//!
//! Demonstrates the new macro-based API for duroxide.
//!
//! Run with: `cargo run --example hello_world_macros --features macros`

#[cfg(feature = "macros")]
use duroxide::prelude::*;

// Simple activity using macro
#[cfg(feature = "macros")]
#[activity]
async fn greet(name: String) -> Result<String, String> {
    Ok(format!("Hello, {}!", name))
}

// Simple orchestration using macro
#[cfg(feature = "macros")]
#[orchestration]
async fn hello_world(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    // For now, use old-style call (Phase 1 - macros just annotate)
    let greeting = ctx.schedule_activity("greet", name)
        .into_activity()
        .await?;
    
    Ok(greeting)
}

#[cfg(feature = "macros")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    println!("🚀 Hello World with Macros (Phase 1)\n");
    println!("Note: Phase 1 just has basic annotations.");
    println!("Full macro features coming in later phases!\n");
    
    // For Phase 1: Use manual registration (discovery comes later)
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("hello_macros.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // Manual registration for Phase 1
    let activities = ActivityRegistry::builder()
        .register("greet", greet)
        .build();
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("hello_world", hello_world)
        .build();
    
    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    
    let client = Client::new(store);
    
    client.start_orchestration("hello-1", "hello_world", "Rust Developer").await?;
    
    match client.wait_for_orchestration("hello-1", Duration::from_secs(10)).await
        .map_err(|e| format!("Wait error: {:?}", e))? {
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

