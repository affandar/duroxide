//! Parallel Orchestrations Stress Test Binary
//!
//! This binary runs the parallel orchestrations stress test scenario
//! across multiple providers and concurrency configurations.
//!
//! Usage:
//!   cargo run --release --bin parallel_orchestrations [DURATION_SECS]
//!
//! Examples:
//!   cargo run --release --bin parallel_orchestrations       # Default 30 seconds
//!   cargo run --release --bin parallel_orchestrations 60    # Run for 60 seconds
//!   cargo run --release --bin parallel_orchestrations 5     # Quick 5 second test

use duroxide_stress_tests::parallel_orchestrations;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Parse duration from command line args
    let duration_secs = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<u64>().ok())
        .unwrap_or(30);

    // Run the test suite
    parallel_orchestrations::run_test_suite(duration_secs).await?;

    Ok(())
}
