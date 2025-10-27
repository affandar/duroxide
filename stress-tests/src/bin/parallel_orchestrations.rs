//! Parallel Orchestrations Stress Test Binary
//!
//! This binary runs the parallel orchestrations stress test scenario
//! across multiple providers and concurrency configurations.

use duroxide_stress_tests::parallel_orchestrations;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Run the test suite
    parallel_orchestrations::run_test_suite().await?;

    Ok(())
}

