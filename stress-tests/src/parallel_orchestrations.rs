//! Parallel Orchestrations stress test scenario
//!
//! This scenario tests fan-out/fan-in orchestration patterns with concurrent instance execution.

use crate::{create_default_activities, create_default_orchestrations, print_comparison_table, run_stress_test};
use duroxide::provider_stress_tests::StressTestConfig;
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;
use tracing::info;

/// Create an in-memory SQLite provider
pub async fn create_in_memory_provider() -> Result<Arc<SqliteProvider>, Box<dyn std::error::Error>> {
    Ok(Arc::new(SqliteProvider::new_in_memory().await?))
}

/// Create a file-based SQLite provider
pub async fn create_file_provider() -> Result<(Arc<SqliteProvider>, String), Box<dyn std::error::Error>> {
    let db_path = format!("/tmp/duroxide_stress_{}.db", std::process::id());
    std::fs::File::create(&db_path)?;
    let provider = Arc::new(SqliteProvider::new(&format!("sqlite:{}", db_path), None).await?);
    Ok((provider, db_path))
}

/// Run the parallel orchestrations stress test across multiple providers and configurations
pub async fn run_test_suite(duration_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
    info!("=== Duroxide Parallel Orchestration Stress Test Suite ===");
    info!("Duration: {} seconds per test", duration_secs);

    let concurrency_combos = vec![(1, 1), (2, 2)];

    let mut results = Vec::new();

    // Test in-memory SQLite
    info!("\n--- Testing In-Memory SQLite Provider ---");
    for (orch_conc, worker_conc) in &concurrency_combos {
        let config = StressTestConfig {
            max_concurrent: 20,
            duration_secs,
            tasks_per_instance: 5,
            activity_delay_ms: 10,
            orch_concurrency: *orch_conc,
            worker_concurrency: *worker_conc,
        };

        match run_stress_test(
            config.clone(),
            create_in_memory_provider().await?,
            create_default_activities(config.activity_delay_ms),
            create_default_orchestrations(),
        )
        .await
        {
            Ok(result) => {
                results.push((
                    "In-Memory SQLite".to_string(),
                    format!("{}/{}", orch_conc, worker_conc),
                    result,
                ));
                info!("✓ Test completed");
            }
            Err(e) => {
                info!("✗ Test failed: {}", e);
            }
        }
    }

    // Test file-based SQLite
    info!("\n--- Testing File-Based SQLite Provider ---");
    for (orch_conc, worker_conc) in &concurrency_combos {
        let config = StressTestConfig {
            max_concurrent: 20,
            duration_secs,
            tasks_per_instance: 5,
            activity_delay_ms: 10,
            orch_concurrency: *orch_conc,
            worker_concurrency: *worker_conc,
        };

        match create_file_provider().await {
            Ok((provider, db_path)) => {
                match run_stress_test(
                    config.clone(),
                    provider,
                    create_default_activities(config.activity_delay_ms),
                    create_default_orchestrations(),
                )
                .await
                {
                    Ok(result) => {
                        results.push((
                            "File SQLite".to_string(),
                            format!("{}/{}", orch_conc, worker_conc),
                            result,
                        ));
                        info!("✓ Test completed");

                        // Cleanup
                        if let Err(e) = std::fs::remove_file(&db_path) {
                            tracing::warn!("Failed to remove temp DB file {}: {}", db_path, e);
                        }
                    }
                    Err(e) => {
                        info!("✗ Test failed: {}", e);
                    }
                }
            }
            Err(e) => {
                info!("✗ Failed to create file provider: {}", e);
            }
        }
    }

    // Print comparison table
    print_comparison_table(&results);

    Ok(())
}
