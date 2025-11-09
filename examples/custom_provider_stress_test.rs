//! Example: How to use stress tests with a custom provider
//!
//! This example demonstrates how external provider implementations
//! can leverage the built-in stress test infrastructure.
//!
//! Run with:
//! ```bash
//! cargo run --example custom_provider_stress_test --features provider-test
//! ```

use duroxide::provider_stress_tests::parallel_orchestrations::{
    run_parallel_orchestrations_test, run_parallel_orchestrations_test_with_config, ProviderStressFactory,
};
use duroxide::provider_stress_tests::{print_comparison_table, StressTestConfig};
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::Provider;
use std::sync::Arc;

/// Example factory for in-memory SQLite provider
struct InMemorySqliteFactory;

#[async_trait::async_trait]
impl ProviderStressFactory for InMemorySqliteFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        Arc::new(
            SqliteProvider::new_in_memory()
                .await
                .expect("Failed to create provider"),
        )
    }

    // Optional: customize default config
    fn stress_test_config(&self) -> StressTestConfig {
        StressTestConfig {
            max_concurrent: 20,
            duration_secs: 5, // Short for example
            tasks_per_instance: 5,
            activity_delay_ms: 10,
            orch_concurrency: 2,
            worker_concurrency: 2,
        }
    }
}

/// Example factory for file-based SQLite provider
struct FileSqliteFactory;

#[async_trait::async_trait]
impl ProviderStressFactory for FileSqliteFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        let db_path = format!("/tmp/duroxide_stress_example_{}.db", std::process::id());
        Arc::new(
            SqliteProvider::new(&format!("sqlite:{}", db_path), None)
                .await
                .expect("Failed to create provider"),
        )
    }

    fn stress_test_config(&self) -> StressTestConfig {
        StressTestConfig {
            max_concurrent: 20,
            duration_secs: 5,
            tasks_per_instance: 5,
            activity_delay_ms: 10,
            orch_concurrency: 2,
            worker_concurrency: 2,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    println!("=== Custom Provider Stress Test Example ===\n");

    // Example 1: Simple stress test with default config
    println!("1. Running stress test with default config...\n");
    let factory = InMemorySqliteFactory;
    let result = run_parallel_orchestrations_test(&factory).await?;

    println!("\nResults:");
    println!("  Success rate: {:.2}%", result.success_rate());
    println!("  Completed: {}", result.completed);
    println!("  Throughput: {:.2} orch/sec", result.orch_throughput);
    println!(
        "  Activity throughput: {:.2} activities/sec",
        result.activity_throughput
    );
    println!("  Average latency: {:.2}ms", result.avg_latency_ms);

    // Example 2: Custom configuration
    println!("\n2. Running stress test with custom config...\n");
    let custom_config = StressTestConfig {
        max_concurrent: 10,
        duration_secs: 3,
        tasks_per_instance: 3,
        activity_delay_ms: 5,
        orch_concurrency: 1,
        worker_concurrency: 1,
    };

    let result = run_parallel_orchestrations_test_with_config(&factory, custom_config).await?;
    println!("\nResults with custom config:");
    println!("  Completed: {}", result.completed);
    println!("  Success rate: {:.2}%", result.success_rate());

    // Example 3: Comparing multiple configurations
    println!("\n3. Comparing multiple configurations...\n");
    let mut results = Vec::new();

    for (orch, worker) in [(1, 1), (2, 2)] {
        let config = StressTestConfig {
            max_concurrent: 15,
            duration_secs: 3,
            tasks_per_instance: 4,
            activity_delay_ms: 8,
            orch_concurrency: orch,
            worker_concurrency: worker,
        };

        let in_mem_result = run_parallel_orchestrations_test_with_config(&InMemorySqliteFactory, config.clone()).await?;
        results.push((
            "InMemory".to_string(),
            format!("{}/{}", orch, worker),
            in_mem_result,
        ));

        let file_result = run_parallel_orchestrations_test_with_config(&FileSqliteFactory, config).await?;
        results.push(("File".to_string(), format!("{}/{}", orch, worker), file_result));
    }

    print_comparison_table(&results);

    println!("\n=== Example Complete ===");
    println!("\nTo use this in your own tests:");
    println!("1. Implement ProviderStressFactory for your provider");
    println!("2. Call run_parallel_orchestrations_test(&factory)");
    println!("3. Assert on result.success_rate() and result.orch_throughput");

    Ok(())
}
