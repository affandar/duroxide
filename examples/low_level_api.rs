//! Low-Level API Example
//!
//! This example demonstrates the traditional (lower-level) duroxide API
//! without macros. Useful for:
//! - Understanding how macros work under the hood
//! - Advanced use cases requiring manual control
//! - Reference implementation
//!
//! For most use cases, prefer the macro-based API (see hello_world_macros.rs)
//!
//! Run with: `cargo run --example low_level_api`

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    println!("🔧 Low-Level API Example\n");
    println!("This shows the traditional API without macros.\n");
    
    // 1. Create provider
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("low_level.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // 2. Register activities manually with closures
    let activities = ActivityRegistry::builder()
        .register("Greet", |name: String| async move {
            Ok(format!("Hello, {}!", name))
        })
        .register("ProcessData", |data: String| async move {
            Ok(format!("Processed: {}", data))
        })
        .build();
    
    // 3. Define orchestration as closure
    let orchestration = |ctx: OrchestrationContext, name: String| async move {
        // Manual trace
        ctx.trace_info("Starting orchestration");
        
        // Manual GUID and timestamp
        let guid = ctx.new_guid().await?;
        let timestamp = ctx.utcnow_ms().await?;
        
        ctx.trace_info(format!("Correlation ID: {}", guid));
        ctx.trace_info(format!("Timestamp: {}", timestamp));
        
        // Manual activity scheduling
        let greeting = ctx.schedule_activity("Greet", name)
            .into_activity()
            .await?;
        
        ctx.trace_info(format!("Greeting: {}", greeting));
        
        // Another activity
        let processed = ctx.schedule_activity("ProcessData", greeting)
            .into_activity()
            .await?;
        
        ctx.trace_info("Orchestration complete");
        
        Ok(processed)
    };
    
    // 4. Register orchestration manually
    let orchestrations = OrchestrationRegistry::builder()
        .register("HelloWorld", orchestration)
        .build();
    
    // 5. Start runtime (old way)
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations
    ).await;
    
    // 6. Create client and run
    let client = Client::new(store);
    
    client.start_orchestration("low-level-1", "HelloWorld", "World").await?;
    
    match client.wait_for_orchestration("low-level-1", std::time::Duration::from_secs(10)).await
        .map_err(|e| format!("Wait error: {:?}", e))? {
        runtime::OrchestrationStatus::Completed { output } => {
            println!("✅ Result: {}", output);
        }
        runtime::OrchestrationStatus::Failed { error } => {
            println!("❌ Failed: {}", error);
        }
        _ => {
            println!("⏳ Still running...");
        }
    }
    
    rt.shutdown().await;
    
    println!("\n📝 This is the low-level API.");
    println!("For the macro-based API, see: hello_world_macros.rs");
    
    Ok(())
}

