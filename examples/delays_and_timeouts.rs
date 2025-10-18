//! Delays and Timeouts Example - IMPORTANT: Read this for timer best practices!
//!
//! This example demonstrates:
//! - ✅ CORRECT: Use ctx.schedule_timer() for orchestration delays
//! - ✅ CORRECT: Use ctx.select2() for timeout patterns  
//! - ✅ CORRECT: Activities can use tokio::time::sleep for their work
//! - ❌ WRONG: Don't create activities that only sleep
//!
//! Now with macros for cleaner code!
//!
//! Run with: `cargo run --example delays_and_timeouts`

use duroxide::prelude::*;
use duroxide::DurableOutput;

#[activity(typed)]
async fn process_data(input: String) -> Result<String, String> {
    println!("Processing data: {}", input);
    Ok(format!("Processed: {}", input))
}

#[activity(typed)]
async fn slow_operation(input: String) -> Result<String, String> {
    println!("Starting slow operation: {}", input);
    // ✅ Activities can use tokio::time::sleep as part of their work
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!("Slow operation completed");
    Ok(format!("Slow result: {}", input))
}

// ✅ CORRECT: Orchestration with timer for delay
#[orchestration]
async fn delay_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    durable_trace_info!("Starting delay orchestration");
    
    let result1 = durable!(process_data(input.clone())).await?;
    durable_trace_info!("First step complete: {}", result1);
    
    // ✅ CORRECT: Use ctx.schedule_timer() for orchestration-level delays
    durable_trace_info!("Waiting 2 seconds before next step...");
    ctx.schedule_timer(2000).into_timer().await;
    durable_trace_info!("Delay complete");
    
    let result2 = durable!(process_data(result1)).await?;
    
    Ok(result2)
}

// ✅ CORRECT: Timeout pattern with select2
#[orchestration]
async fn timeout_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    durable_trace_info!("Starting timeout orchestration");
    
    // Race between slow operation and timeout
    // Note: select2 requires DurableFuture, so we schedule but don't call durable!() here
    let operation_future = ctx.schedule_activity("slow_operation", 
        serde_json::to_string(&input).unwrap());
    let timeout_future = ctx.schedule_timer(3000);
    
    let (winner_index, result) = ctx.select2(operation_future, timeout_future).await;
    
    match (winner_index, result) {
        (0, DurableOutput::Activity(Ok(result_json))) => {
            // Operation completed first
            let result: String = serde_json::from_str(&result_json).unwrap();
            durable_trace_info!("✅ Operation completed: {}", result);
            Ok(result)
        }
        (0, DurableOutput::Activity(Err(error))) => {
            durable_trace_error!("Operation failed: {}", error);
            Err(error)
        }
        (1, DurableOutput::Timer) => {
            // Timeout occurred
            durable_trace_warn!("⏱️  Operation timed out");
            Err("Operation timed out after 3 seconds".to_string())
        }
        _ => unreachable!(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("delays_and_timeouts.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url).await?);
    
    // Auto-discovery
    let rt = Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await;
    
    let client = Client::new(store);
    
    println!("🚀 Example 1: Delay Orchestration\n");
    
    delay_orchestration::start(&client, "delay-1", "test data".to_string()).await?;
    
    match delay_orchestration::wait(&client, "delay-1", Duration::from_secs(10)).await? {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Delay orchestration completed: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Failed: {}", error);
        }
        _ => {}
    }
    
    println!("\n🚀 Example 2: Timeout Pattern\n");
    
    timeout_orchestration::start(&client, "timeout-1", "test data".to_string()).await?;
    
    match timeout_orchestration::wait(&client, "timeout-1", Duration::from_secs(10)).await? {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Timeout orchestration completed: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Timeout orchestration failed (expected): {}", error);
        }
        _ => {}
    }
    
    rt.shutdown().await;
    
    println!("\n📚 Key Takeaways:");
    println!("  ✅ Use ctx.schedule_timer() for orchestration delays");
    println!("  ✅ Use ctx.select2() for timeout patterns");
    println!("  ✅ Activities can use tokio::time::sleep for their work");
    println!("  ❌ Don't create activities that ONLY sleep");
    
    Ok(())
}
