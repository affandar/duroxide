/// Stress test: Spin up N orchestrations in parallel and measure throughput
/// 
/// This test creates multiple orchestration instances that each perform
/// fan-out/fan-in work, measuring total time and throughput to establish
/// baseline performance before multi-threaded dispatcher improvements.

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, OrchestrationRegistry};
use duroxide::{Client, OrchestrationContext};
use std::sync::Arc;
use std::time::Instant;
use tracing::info;

/// Simple orchestration that fans out to N activities and waits for all
async fn fanout_orchestration(
    ctx: OrchestrationContext,
    input: String,
) -> Result<String, String> {
    let config: FanoutConfig = serde_json::from_str(&input)
        .map_err(|e| format!("Invalid input: {}", e))?;

    ctx.trace_info(format!("Starting fanout with {} tasks", config.task_count));

    // Fan-out: schedule all activities in parallel
    let mut futures = Vec::new();
    for i in 0..config.task_count {
        let task_input = format!("task-{}", i);
        futures.push(ctx.schedule_activity("ProcessTask", task_input));
    }

    // Fan-in: wait for all to complete
    let results = ctx.join(futures).await;

    let success_count = results
        .iter()
        .filter(|r| matches!(r, duroxide::DurableOutput::Activity(Ok(_))))
        .count();

    ctx.trace_info(format!(
        "Fanout completed: {}/{} succeeded",
        success_count, config.task_count
    ));

    Ok(format!(
        "Completed {} tasks ({} succeeded)",
        config.task_count, success_count
    ))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FanoutConfig {
    task_count: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StressTestConfig {
    /// Maximum number of concurrent orchestrations
    max_concurrent: usize,
    /// Duration to run the test (seconds)
    duration_secs: u64,
    /// Number of tasks each orchestration fans out to
    tasks_per_instance: usize,
    /// Simulated activity execution time (ms)
    activity_delay_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,duroxide=debug".into()),
        )
        .init();

    // Test configuration
    let config = StressTestConfig {
        max_concurrent: 20,
        duration_secs: 30,
        tasks_per_instance: 5,
        activity_delay_ms: 10,
    };

    info!("=== Duroxide Parallel Orchestration Stress Test ===");
    info!("Max concurrent: {}", config.max_concurrent);
    info!("Duration: {}s", config.duration_secs);
    info!("Tasks per instance: {}", config.tasks_per_instance);
    info!("Activity delay: {}ms", config.activity_delay_ms);

    // Create storage provider
    let store = Arc::new(SqliteProvider::new_in_memory().await?);

    // Register activities
    let delay_ms = config.activity_delay_ms;
    let activities = ActivityRegistry::builder()
        .register("ProcessTask", move |input: String| {
            let delay = delay_ms;
            async move {
                // Simulate work
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                Ok(format!("processed: {}", input))
            }
        })
        .build();

    // Register orchestrations
    let orchestrations = OrchestrationRegistry::builder()
        .register("FanoutWorkflow", fanout_orchestration)
        .build();

    // Start runtime
    info!("Starting runtime...");
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    )
    .await;

    // Create client (wrap in Arc for sharing across tasks)
    let client = Arc::new(Client::new(store.clone()));

    // Continuous orchestration pump
    info!("Starting continuous orchestration pump...");
    let start_time = Instant::now();
    let end_time = start_time + std::time::Duration::from_secs(config.duration_secs);
    
    let launched = Arc::new(tokio::sync::Mutex::new(0_usize));
    let completed = Arc::new(tokio::sync::Mutex::new(0_usize));
    let failed = Arc::new(tokio::sync::Mutex::new(0_usize));
    let active = Arc::new(tokio::sync::Mutex::new(0_usize));
    
    let input = serde_json::to_string(&FanoutConfig {
        task_count: config.tasks_per_instance,
    })?;

    let mut instance_id = 0_usize;
    
    loop {
        let now = Instant::now();
        if now >= end_time {
            info!("Duration elapsed, stopping pump...");
            break;
        }

        // Check if we can launch more orchestrations
        let current_active = *active.lock().await;
        if current_active >= config.max_concurrent {
            // Wait a bit before checking again
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            continue;
        }

        // Launch new orchestration
        instance_id += 1;
        let instance = format!("stress-test-{}", instance_id);
        
        *active.lock().await += 1;
        *launched.lock().await += 1;
        
        let client_clone = Arc::clone(&client);
        let input_clone = input.clone();
        let completed_clone = Arc::clone(&completed);
        let failed_clone = Arc::clone(&failed);
        let active_clone = Arc::clone(&active);
        
        tokio::spawn(async move {
            // Start orchestration
            let start_result = client_clone
                .start_orchestration(&instance, "FanoutWorkflow", input_clone)
                .await;
            
            if let Err(e) = start_result {
                tracing::error!("Failed to start {}: {}", instance, e);
                *failed_clone.lock().await += 1;
                *active_clone.lock().await -= 1;
                return;
            }

            // Wait for completion
            match client_clone
                .wait_for_orchestration(&instance, std::time::Duration::from_secs(60))
                .await
            {
                Ok(duroxide::OrchestrationStatus::Completed { .. }) => {
                    *completed_clone.lock().await += 1;
                }
                Ok(duroxide::OrchestrationStatus::Failed { error }) => {
                    tracing::warn!("Orchestration {} failed: {}", instance, error);
                    *failed_clone.lock().await += 1;
                }
                Err(e) => {
                    tracing::warn!("Wait error for {}: {:?}", instance, e);
                    *failed_clone.lock().await += 1;
                }
                _ => {
                    *failed_clone.lock().await += 1;
                }
            }
            
            *active_clone.lock().await -= 1;
        });

        // Small delay between launches to avoid hammering
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }

    // Wait for all active orchestrations to complete
    info!("Waiting for active orchestrations to complete...");
    let mut wait_iterations = 0;
    loop {
        let current_active = *active.lock().await;
        if current_active == 0 {
            break;
        }
        
        if wait_iterations % 100 == 0 {
            info!("Still waiting for {} active orchestrations...", current_active);
        }
        
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        wait_iterations += 1;
        
        // Timeout after 2 minutes
        if wait_iterations > 1200 {
            info!("Timeout waiting for orchestrations to complete");
            break;
        }
    }

    let total_time = start_time.elapsed();
    let final_launched = *launched.lock().await;
    let final_completed = *completed.lock().await;
    let final_failed = *failed.lock().await;

    // Report results
    info!("=== Results ===");
    info!("Total time: {:?}", total_time);
    info!("Launched: {}", final_launched);
    info!("Completed: {}", final_completed);
    info!("Failed: {}", final_failed);
    info!("Throughput: {:.2} orchestrations/sec", 
        final_completed as f64 / total_time.as_secs_f64());
    
    if final_completed > 0 {
        info!("Average time per orchestration: {:.2}ms", 
            total_time.as_millis() as f64 / final_completed as f64);
    }
    
    let total_activities = final_completed * config.tasks_per_instance;
    info!("Activity throughput: {:.2} activities/sec",
        total_activities as f64 / total_time.as_secs_f64());

    // Shutdown
    rt.shutdown(None).await;

    Ok(())
}

