//! Minimal test to verify basic orchestration flow

use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{OrchestrationContext, OrchestrationRegistry, Client};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("duroxide=info")
        .init();

    // Create an in-memory SQLite provider
    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    
    // Simple activity
    let activities = ActivityRegistry::builder()
        .register("Echo", |input: String| async move {
            Ok(format!("Echo: {}", input))
        })
        .build();
    
    // Orchestration with trace then echo
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        // First do a trace
        let trace_result = ctx.trace_info_async(format!("Processing: {}", input))
            .into_activity()
            .await?;
        println!("Trace returned: {}", trace_result);
        
        // Then do echo
        let echo_result = ctx.schedule_activity("Echo", input)
            .into_activity()
            .await?;
        
        Ok(echo_result)
    };
    
    // Register the orchestration
    let orchestrations = OrchestrationRegistry::builder()
        .register("Minimal", orchestration)
        .build();
    
    // Start the runtime
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    )
    .await;
    
    // Create a client
    let client = Client::new(store.clone());
    
    // Start an instance
    client
        .start_orchestration("minimal-1", "Minimal", "World")
        .await?;
    
    // Wait for completion
    match client
        .wait_for_orchestration("minimal-1", std::time::Duration::from_secs(1))
        .await
    {
        Ok(duroxide::OrchestrationStatus::Completed { output }) => {
            println!("Success: {}", output);
        }
        Ok(duroxide::OrchestrationStatus::Failed { error }) => {
            eprintln!("Failed: {}", error);
        }
        Err(e) => {
            eprintln!("Wait error: {:?}", e);
        }
        _ => {}
    }
    
    // Shutdown
    rt.shutdown().await;
    
    Ok(())
}
