use duroxide::*;
use duroxide::providers::fs::FsHistoryStore;
use duroxide::runtime::registry::{ActivityRegistry, OrchestrationRegistry};
use duroxide::runtime;
use std::sync::Arc;

/// This example demonstrates the CORRECT way to handle delays and timeouts.
/// 
/// ⚠️ CRITICAL MISTAKES TO AVOID:
/// 1. Using activities for delays instead of timers
/// 2. Calling .await directly on schedule methods (missing .into_*())
/// 
/// This example shows:
/// 1. ✅ CORRECT: Using timers for delays with .into_timer().await
/// 2. ✅ CORRECT: Using timers for timeouts with select2
/// 3. ❌ What NOT to do (commented out)

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up the runtime with filesystem store
    let temp_dir = tempfile::tempdir()?;
    let store = Arc::new(FsHistoryStore::new(temp_dir.path(), true));
    
    // Register activities - these should be pure business logic without delays
    let activities = ActivityRegistry::builder()
        .register("ProcessData", |input: String| async move {
            println!("Processing data: {}", input);
            // ✅ GOOD: Pure business logic, no delays
            Ok(format!("Processed: {}", input))
        })
        .register("SlowOperation", |input: String| async move {
            println!("Starting slow operation: {}", input);
            // ❌ DON'T DO THIS: tokio::time::sleep(Duration::from_secs(3)).await;
            // Instead, use timers in the orchestration!
            Ok(format!("Slow result: {}", input))
        })
        .build();

    // Orchestration showing CORRECT timer usage
    let delay_orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("Starting delay example orchestration");
        
        // ✅ CORRECT: Use timer for delay
        ctx.trace_info("Waiting 2 seconds...");
        ctx.schedule_timer(2000).into_timer().await;  // MUST use .into_timer().await!
        // ❌ WRONG: ctx.schedule_timer(2000).await;  // Missing .into_timer()!
        ctx.trace_info("Timer fired! Processing data...");
        
        // Process some data after the delay
        let result = ctx.schedule_activity("ProcessData", input)
            .into_activity()  // MUST use .into_activity().await!
            .await?;
        // ❌ WRONG: ctx.schedule_activity("ProcessData", input).await;  // Missing .into_activity()!
        
        ctx.trace_info("Processing complete!");
        Ok(format!("Delayed result: {}", result))
    };

    // Orchestration showing CORRECT timeout usage
    let timeout_orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("Starting timeout example orchestration");
        
        // ✅ CORRECT: Use timer for timeout with select2
        let work = ctx.schedule_activity("SlowOperation", input.clone());
        let timeout = ctx.schedule_timer(5000); // 5 second timeout
        
        ctx.trace_info("Racing work against timeout...");
        let (winner_index, result) = ctx.select2(work, timeout).await;
        
        match winner_index {
            0 => {
                // Work completed first
                match result {
                    DurableOutput::Activity(Ok(value)) => {
                        ctx.trace_info("Work completed within timeout");
                        Ok(format!("Success: {}", value))
                    }
                    DurableOutput::Activity(Err(e)) => {
                        ctx.trace_info("Work failed");
                        Err(format!("Work failed: {}", e))
                    }
                    _ => unreachable!(),
                }
            }
            1 => {
                // Timeout occurred first
                ctx.trace_info("Operation timed out");
                Err("Operation timed out after 5 seconds".to_string())
            }
            _ => unreachable!(),
        }
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("DelayExample", delay_orchestration)
        .register("TimeoutExample", timeout_orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(
        store,
        Arc::new(activities),
        orchestrations,
    ).await;

    println!("🚀 Running delay example...");
    
    // Run the delay example
    let delay_instance = format!("delay-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    rt.clone().start_orchestration(&delay_instance, "DelayExample", "test data").await?;
    match rt.wait_for_orchestration(&delay_instance, std::time::Duration::from_secs(15)).await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Delay example completed: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Delay example failed: {}", error);
        }
        _ => println!("⏳ Delay example still running..."),
    }

    println!("\n🚀 Running timeout example...");
    
    // Run the timeout example  
    let timeout_instance = format!("timeout-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    rt.clone().start_orchestration(&timeout_instance, "TimeoutExample", "test data").await?;
    match rt.wait_for_orchestration(&timeout_instance, std::time::Duration::from_secs(15)).await
        .map_err(|e| format!("Wait error: {:?}", e))?
    {
        OrchestrationStatus::Completed { output } => {
            println!("✅ Timeout example completed: {}", output);
        }
        OrchestrationStatus::Failed { error } => {
            println!("❌ Timeout example failed: {}", error);
        }
        _ => println!("⏳ Timeout example still running..."),
    }

    rt.shutdown().await;
    
    println!("\n📚 Key Takeaways:");
    println!("✅ Use ctx.schedule_timer(ms).into_timer().await for delays");
    println!("✅ Use ctx.schedule_activity(name, input).into_activity().await for work");
    println!("✅ Use ctx.select2(work, timeout) for timeout patterns");
    println!("❌ Never call .await directly on schedule methods (missing .into_*()!)");
    println!("❌ Never put tokio::time::sleep() in activities");
    println!("❌ Activities should be pure business logic only");
    
    Ok(())
}
