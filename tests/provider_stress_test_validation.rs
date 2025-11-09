//! Minimal validation test for the provider stress test infrastructure.
//!
//! This test just ensures the stress test infrastructure doesn't break.
//! It runs a single orchestration to validate the plumbing works.

use duroxide::provider_stress_tests::parallel_orchestrations::{
    ProviderStressFactory, run_parallel_orchestrations_test_with_config,
};
use duroxide::provider_stress_tests::StressTestConfig;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::Provider;
use std::sync::Arc;

/// Minimal test factory for in-memory SQLite provider
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
}

#[tokio::test]
async fn test_stress_infrastructure_minimal() {
    // Initialize logging for the test
    let _ = tracing_subscriber::fmt()
        .with_env_filter("error")
        .with_test_writer()
        .try_init();

    let factory = InMemorySqliteFactory;

    // Minimal config: just run 1 orchestration to validate infrastructure
    let config = StressTestConfig {
        max_concurrent: 1,
        duration_secs: 1,
        tasks_per_instance: 2,
        activity_delay_ms: 5,
        orch_concurrency: 1,
        worker_concurrency: 1,
    };

    let result = run_parallel_orchestrations_test_with_config(&factory, config)
        .await
        .expect("Stress test infrastructure is broken");

    // Just validate that at least one orchestration completed successfully
    assert!(result.completed > 0, "No orchestrations completed - infrastructure broken");
    assert_eq!(result.failed, 0, "Orchestration failed - infrastructure broken");
}
