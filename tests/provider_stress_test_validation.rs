//! Validation test for the provider stress test infrastructure.
//!
//! This test demonstrates how external provider implementations can use the
//! stress test infrastructure with a minimal test provider.

use duroxide::provider_stress_tests::parallel_orchestrations::{
    ProviderStressFactory, run_parallel_orchestrations_test, run_parallel_orchestrations_test_with_config,
};
use duroxide::provider_stress_tests::StressTestConfig;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::Provider;
use std::sync::Arc;

/// Simple test factory for in-memory SQLite provider
struct InMemorySqliteFactory;

#[async_trait::async_trait]
impl ProviderStressFactory for InMemorySqliteFactory {
    async fn create_provider(&self) -> Arc<dyn Provider> {
        Arc::new(
            SqliteProvider::new_in_memory()
                .await
                .expect("Failed to create in-memory SQLite provider"),
        )
    }

    // Use a shorter test duration for fast validation
    fn stress_test_config(&self) -> StressTestConfig {
        StressTestConfig {
            max_concurrent: 10,
            duration_secs: 2, // Short test for validation
            tasks_per_instance: 3,
            activity_delay_ms: 5,
            orch_concurrency: 2,
            worker_concurrency: 2,
        }
    }
}

#[tokio::test]
async fn test_stress_infrastructure_with_sqlite() {
    // Initialize logging for the test
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    let factory = InMemorySqliteFactory;
    let result = run_parallel_orchestrations_test(&factory)
        .await
        .expect("Stress test failed");

    // Validate results
    assert!(
        result.success_rate() > 95.0,
        "Success rate too low: {:.2}%",
        result.success_rate()
    );
    assert!(result.completed > 0, "No orchestrations completed");
    assert!(
        result.orch_throughput > 0.1,
        "Throughput too low: {:.2} orch/sec",
        result.orch_throughput
    );

    println!("\n=== Stress Test Results ===");
    println!("Success rate: {:.2}%", result.success_rate());
    println!("Completed: {}", result.completed);
    println!("Throughput: {:.2} orch/sec", result.orch_throughput);
    println!(
        "Activity throughput: {:.2} activities/sec",
        result.activity_throughput
    );
    println!("Average latency: {:.2}ms", result.avg_latency_ms);
}

#[tokio::test]
async fn test_stress_infrastructure_with_custom_config() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let factory = InMemorySqliteFactory;

    // Test with minimal config (very short)
    let config = StressTestConfig {
        max_concurrent: 5,
        duration_secs: 1,
        tasks_per_instance: 2,
        activity_delay_ms: 5,
        orch_concurrency: 1,
        worker_concurrency: 1,
    };

    let result = run_parallel_orchestrations_test_with_config(&factory, config)
        .await
        .expect("Stress test failed");

    assert!(
        result.success_rate() > 90.0,
        "Success rate too low: {:.2}%",
        result.success_rate()
    );
    assert!(result.completed > 0, "No orchestrations completed");

    println!("\n=== Custom Config Test Results ===");
    println!("Completed: {}", result.completed);
    println!("Success rate: {:.2}%", result.success_rate());
}

#[tokio::test]
async fn test_multiple_configurations_comparison() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("error")
        .with_test_writer()
        .try_init();

    let factory = InMemorySqliteFactory;
    let mut results = Vec::new();

    // Test different concurrency settings (short duration for test)
    for (orch, worker) in [(1, 1), (2, 2)] {
        let config = StressTestConfig {
            max_concurrent: 10,
            duration_secs: 1,
            tasks_per_instance: 3,
            activity_delay_ms: 5,
            orch_concurrency: orch,
            worker_concurrency: worker,
        };

        let result = run_parallel_orchestrations_test_with_config(&factory, config)
            .await
            .expect("Stress test failed");

        results.push((
            "InMemorySQLite".to_string(),
            format!("{}/{}", orch, worker),
            result,
        ));
    }

    // Validate all tests succeeded
    for (provider, config, result) in &results {
        assert!(
            result.success_rate() > 90.0,
            "{} {}: Success rate too low: {:.2}%",
            provider,
            config,
            result.success_rate()
        );
    }

    // Print comparison
    duroxide::provider_stress_tests::print_comparison_table(&results);
}
