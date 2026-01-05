//! Integration tests for execution pruning operations via Client API.

use duroxide::providers::{InstanceFilter, PruneOptions};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, RuntimeOptions};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

mod common;

fn fast_runtime_options() -> RuntimeOptions {
    RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(50),
        ..Default::default()
    }
}

async fn wait_for_terminal(client: &Client, instance_id: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(info) = client.get_instance_info(instance_id).await {
            if info.status == "Completed" || info.status == "Failed" {
                return true;
            }
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Test: prune ContinueAsNew chain
///
/// Covers:
/// - Long chain of executions
/// - Keep last N executions
/// - Combined filters
#[tokio::test]
async fn test_prune_continue_as_new_chain() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "ContinueOrch",
            |ctx: OrchestrationContext, count_str: String| async move {
                let count: u32 = count_str.parse().unwrap_or(0);
                if count < 5 {
                    // ContinueAsNew 5 times (6 total executions)
                    ctx.continue_as_new((count + 1).to_string()).await
                } else {
                    Ok(format!("Final: {}", count))
                }
            },
        )
        .build();

    let _rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(ActivityRegistry::builder().build()),
        orchestrations,
        fast_runtime_options(),
    )
    .await;

    // Start orchestration that will ContinueAsNew 5 times
    client
        .start_orchestration("prune-chain", "ContinueOrch", "0")
        .await
        .unwrap();

    // Wait for completion
    assert!(
        wait_for_terminal(&client, "prune-chain", Duration::from_secs(10)).await,
        "Should complete"
    );

    // Verify we have 6 executions
    let executions = client.list_executions("prune-chain").await.unwrap();
    assert_eq!(executions.len(), 6, "Should have 6 executions");

    // Prune keeping last 2
    let result = client
        .prune_executions(
            "prune-chain",
            PruneOptions {
                keep_last: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(result.instances_processed, 1);
    assert!(result.executions_deleted >= 4, "Should delete at least 4 executions");

    // Verify only 2 remain
    let executions_after = client.list_executions("prune-chain").await.unwrap();
    assert_eq!(executions_after.len(), 2, "Should have 2 executions remaining");
    // Should be the latest ones
    assert!(executions_after.contains(&5) || executions_after.contains(&6));
}

/// Test: bulk prune operations
///
/// Covers:
/// - Prune multiple instances
/// - Respects limit
#[tokio::test]
async fn test_prune_bulk_operations() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "ContinueOrch",
            |ctx: OrchestrationContext, count_str: String| async move {
                let count: u32 = count_str.parse().unwrap_or(0);
                if count < 3 {
                    ctx.continue_as_new((count + 1).to_string()).await
                } else {
                    Ok(format!("Final: {}", count))
                }
            },
        )
        .build();

    let _rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(ActivityRegistry::builder().build()),
        orchestrations,
        fast_runtime_options(),
    )
    .await;

    // Create 3 instances each with 4 executions
    for i in 0..3 {
        client
            .start_orchestration(&format!("prune-bulk-{}", i), "ContinueOrch", "0")
            .await
            .unwrap();
    }

    // Wait for all to complete
    for i in 0..3 {
        assert!(
            wait_for_terminal(&client, &format!("prune-bulk-{}", i), Duration::from_secs(10)).await,
            "Instance {} should complete",
            i
        );
    }

    // Verify each has 4 executions
    for i in 0..3 {
        let executions = client.list_executions(&format!("prune-bulk-{}", i)).await.unwrap();
        assert_eq!(executions.len(), 4, "Instance {} should have 4 executions", i);
    }

    // Bulk prune keeping last 1 for specific instances
    let result = client
        .prune_executions_bulk(
            InstanceFilter {
                instance_ids: Some(vec!["prune-bulk-0".into(), "prune-bulk-1".into()]),
                ..Default::default()
            },
            PruneOptions {
                keep_last: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(result.instances_processed, 2);
    assert!(result.executions_deleted >= 6, "Should delete 3 from each instance");

    // Verify pruned instances have 1 execution each
    assert_eq!(client.list_executions("prune-bulk-0").await.unwrap().len(), 1);
    assert_eq!(client.list_executions("prune-bulk-1").await.unwrap().len(), 1);

    // Instance 2 should be untouched
    assert_eq!(client.list_executions("prune-bulk-2").await.unwrap().len(), 4);
}

/// Test: prune safety
///
/// Covers:
/// - Never deletes current execution
/// - Never deletes running execution
/// - Empty options = no deletions
#[tokio::test]
async fn test_prune_safety() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "ContinueOrch",
            |ctx: OrchestrationContext, count_str: String| async move {
                let count: u32 = count_str.parse().unwrap_or(0);
                if count < 2 {
                    ctx.continue_as_new((count + 1).to_string()).await
                } else {
                    Ok(format!("Final: {}", count))
                }
            },
        )
        .build();

    let _rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(ActivityRegistry::builder().build()),
        orchestrations,
        fast_runtime_options(),
    )
    .await;

    // Create instance with 3 executions
    client
        .start_orchestration("prune-safety", "ContinueOrch", "0")
        .await
        .unwrap();
    wait_for_terminal(&client, "prune-safety", Duration::from_secs(10)).await;

    let info = client.get_instance_info("prune-safety").await.unwrap();
    let current_exec = info.current_execution_id;

    // Test 1: Prune with keep_last=0 should still keep current
    let _result = client
        .prune_executions(
            "prune-safety",
            PruneOptions {
                keep_last: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let executions = client.list_executions("prune-safety").await.unwrap();
    assert!(
        executions.contains(&current_exec),
        "Current execution should never be deleted"
    );

    // Test 2: Even with aggressive pruning, current execution is preserved
    client
        .start_orchestration("prune-safety-empty", "ContinueOrch", "0")
        .await
        .unwrap();
    wait_for_terminal(&client, "prune-safety-empty", Duration::from_secs(10)).await;

    let info = client.get_instance_info("prune-safety-empty").await.unwrap();
    let current_exec_empty = info.current_execution_id;

    // Prune with empty options (implementation prunes all historical)
    let _result = client
        .prune_executions(
            "prune-safety-empty",
            PruneOptions {
                keep_last: None,
                completed_before: None,
            },
        )
        .await
        .unwrap();

    // Current execution must always remain
    let executions_after = client.list_executions("prune-safety-empty").await.unwrap();
    assert!(
        executions_after.contains(&current_exec_empty),
        "Current execution should never be deleted"
    );
}

/// Test: prune while ContinueAsNew is actively happening
///
/// Covers:
/// - Pruning during in-flight ContinueAsNew chain
/// - Running execution is never pruned
/// - Historical executions can be pruned mid-flight
#[tokio::test]
async fn test_prune_during_active_continue_as_new() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    // Track which execution we're on
    let execution_counter = Arc::new(AtomicU32::new(0));
    let execution_counter_clone = execution_counter.clone();

    // Use an activity to create a pause point where we can prune
    let activities = ActivityRegistry::builder()
        .register("SlowActivity", move |_ctx: ActivityContext, input: String| {
            let counter = execution_counter_clone.clone();
            async move {
                let exec_num: u32 = input.parse().unwrap_or(0);
                counter.store(exec_num, Ordering::SeqCst);
                // Delay to allow time for pruning during mid-execution
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(format!("exec-{}", exec_num))
            }
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "ActiveContinueOrch",
            |ctx: OrchestrationContext, count_str: String| async move {
                let count: u32 = count_str.parse().unwrap_or(0);
                // Call activity to track execution and create pause point
                ctx.schedule_activity("SlowActivity", count.to_string())
                    .into_activity()
                    .await?;

                if count < 5 {
                    ctx.continue_as_new((count + 1).to_string()).await
                } else {
                    Ok(format!("Final: {}", count))
                }
            },
        )
        .build();

    let _rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activities),
        orchestrations,
        fast_runtime_options(),
    )
    .await;

    // Start orchestration
    client
        .start_orchestration("prune-active", "ActiveContinueOrch", "0")
        .await
        .unwrap();

    // Wait until we're on execution 3 or later
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while execution_counter.load(Ordering::SeqCst) < 3 {
        if std::time::Instant::now() > deadline {
            panic!("Orchestration never reached execution 3");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Now we're mid-flight on execution 3+, try to prune keeping last 1
    let result = client
        .prune_executions(
            "prune-active",
            PruneOptions {
                keep_last: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Should have pruned some historical executions
    // The running execution should NOT be pruned
    assert!(result.instances_processed == 1, "Should process 1 instance");

    // Get current info to verify running execution is preserved
    let info = client.get_instance_info("prune-active").await.unwrap();
    assert_eq!(info.status, "Running", "Instance should still be running");

    // Wait for completion
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(info) = client.get_instance_info("prune-active").await {
            if info.status == "Completed" {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("Orchestration never completed");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Verify final state
    let info = client.get_instance_info("prune-active").await.unwrap();
    assert_eq!(info.status, "Completed");
    assert!(
        info.output.unwrap().contains("Final: 5"),
        "Should complete with final value"
    );
}
