//! Example showing how to use trace_info_async for awaitable traces

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
    
    // Create the activity registry
    let activities = ActivityRegistry::builder()
        .register("ProcessData", |input: String| async move {
            Ok(format!("Processed: {}", input))
        })
        .build();
    
    // Create orchestration that uses only async traces
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        // Awaitable trace - returns the trace result
        let trace1 = ctx.trace_info_async("Starting orchestration")
            .into_activity()
            .await?;
        println!("Trace returned: {}", trace1);
        
        // Do some work
        let result = ctx.schedule_activity("ProcessData", input.clone())
            .into_activity()
            .await?;
        
        // Another awaitable trace
        let trace2 = ctx.trace_info_async(format!("Processed: {}", result))
            .into_activity()
            .await?;
        println!("Trace returned: {}", trace2);
        
        Ok(result)
    };
    
    // Register the orchestration
    let orchestrations = OrchestrationRegistry::builder()
        .register("TraceExample", orchestration)
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
        .start_orchestration("trace-example-1", "TraceExample", "test-data")
        .await?;
    
    // Wait for completion
    match client
        .wait_for_orchestration("trace-example-1", std::time::Duration::from_secs(5))
        .await
    {
        Ok(duroxide::OrchestrationStatus::Completed { output }) => {
            println!("Orchestration completed: {}", output);
        }
        Ok(duroxide::OrchestrationStatus::Failed { error }) => {
            eprintln!("Orchestration failed: {}", error);
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
