//! Interactive observability dashboard for duroxide.
//!
//! This CLI tool demonstrates how to consume and display duroxide metrics
//! and logs in real-time.
//!
//! Run with:
//! ```bash
//! cargo run --example metrics_cli --features observability
//! ```

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, LogFormat, ObservabilityConfig, RuntimeOptions};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("┌─────────────────────────────────────────────┐");
    println!("│   Duroxide Observability Dashboard         │");
    println!("└─────────────────────────────────────────────┘\n");

    // Configure observability
    let observability = ObservabilityConfig {
        metrics_enabled: false, // Set to true and provide endpoint for full metrics
        log_format: LogFormat::Compact,
        log_level: "info".to_string(),
        service_name: "duroxide-dashboard".to_string(),
        service_version: Some("1.0.0".to_string()),
        ..Default::default()
    };

    let options = RuntimeOptions {
        observability,
        orchestration_concurrency: 2,
        worker_concurrency: 2,
        ..Default::default()
    };

    // Create provider
    let store = Arc::new(SqliteProvider::new_in_memory().await?);

    // Register sample activities with varying characteristics
    let activities = ActivityRegistry::builder()
        .register("FastTask", |ctx: ActivityContext, _input: String| async move {
            ctx.trace_debug("Fast task executing");
            tokio::time::sleep(Duration::from_millis(10)).await;
            ctx.trace_debug("Fast task complete");
            Ok("fast_complete".to_string())
        })
        .register("SlowTask", |ctx: ActivityContext, _input: String| async move {
            ctx.trace_info("Slow task started");
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.trace_info("Slow task finished");
            Ok("slow_complete".to_string())
        })
        .register("FailingTask", |ctx: ActivityContext, input: String| async move {
            ctx.trace_info("Failing task invoked");
            if input == "fail" {
                ctx.trace_error("Failing task returning deliberate failure");
                Err("deliberate_failure".to_string())
            } else {
                ctx.trace_info("Failing task succeeded");
                Ok("success".to_string())
            }
        })
        .build();

    // Sample orchestrations
    let fast_orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.trace_info("Fast orchestration started");
        let result = ctx
            .schedule_activity("FastTask", "data".to_string())
            .into_activity()
            .await?;
        ctx.trace_info("Fast orchestration completed");
        Ok::<_, String>(result)
    };

    let slow_orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.trace_info("Slow orchestration started");

        let r1 = ctx.schedule_activity("SlowTask", "data".to_string());
        let r2 = ctx.schedule_activity("SlowTask", "data".to_string());

        let _results = ctx.join(vec![r1, r2]).await;

        ctx.trace_info("All tasks completed");
        Ok::<_, String>("done".to_string())
    };

    let failing_orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.trace_info("Orchestration with potential failure");

        match ctx
            .schedule_activity("FailingTask", "fail".to_string())
            .into_activity()
            .await
        {
            Ok(r) => Ok::<_, String>(r),
            Err(e) => {
                ctx.trace_error(format!("Activity failed: {}", e));
                Err(e)
            }
        }
    };

    let orchestrations = OrchestrationRegistry::builder()
        .register("FastWorkflow", fast_orch)
        .register("SlowWorkflow", slow_orch)
        .register("FailingWorkflow", failing_orch)
        .build();

    // Start runtime
    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(activities), orchestrations, options).await;

    let client = Client::new(store.clone());

    println!("Starting sample orchestrations...\n");
    println!("══════════════════════════════════════════════\n");

    // Run a mix of orchestrations
    for i in 1..=3 {
        client
            .start_orchestration(&format!("fast-{}", i), "FastWorkflow", "data")
            .await?;
    }

    for i in 1..=2 {
        client
            .start_orchestration(&format!("slow-{}", i), "SlowWorkflow", "data")
            .await?;
    }

    client.start_orchestration("fail-1", "FailingWorkflow", "data").await?;

    // Wait for completion
    println!("Waiting for orchestrations to complete...\n");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Display summary if management capabilities available
    if client.has_management_capability() {
        println!("\n══════════════════════════════════════════════");
        println!("             METRICS SUMMARY");
        println!("══════════════════════════════════════════════\n");

        let metrics = client.get_system_metrics().await?;
        println!("Orchestrations:");
        println!("  ✓ Completed: {}", metrics.completed_instances);
        println!("  ✗ Failed: {}", metrics.failed_instances);
        println!("  ⟳ Running: {}", metrics.running_instances);
        println!("  ∑ Total: {}", metrics.total_instances);

        let queues = client.get_queue_depths().await?;
        println!("\nQueue Depths:");
        println!("  Orchestrator: {}", queues.orchestrator_queue);
        println!("  Worker: {}", queues.worker_queue);
        println!("  Timer: {}", queues.timer_queue);

        println!("\nWith full metrics enabled, you would see:");
        println!("  • Activity success rates by name");
        println!("  • Average history sizes");
        println!("  • Turn count distributions");
        println!("  • Provider operation latencies");
        println!("  • Error breakdowns by type");

        println!("\n💡 Enable metrics by setting:");
        println!("   observability.metrics_enabled = true");
        println!("   observability.metrics_export_endpoint = Some(\"http://localhost:4317\")");
    } else {
        println!("\n📊 Management features not available for this provider");
    }

    // Shutdown
    rt.shutdown(None).await;

    println!("\n══════════════════════════════════════════════");
    println!("Dashboard demonstration complete!");
    println!("══════════════════════════════════════════════\n");

    Ok(())
}
