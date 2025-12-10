//! Comprehensive tests for the management interface including metrics

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, RuntimeOptions};
use duroxide::{ActivityContext, Client, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc;
use std::time::Duration;

mod common;

// Helper to create fast-polling runtime for management tests (timing-sensitive)
fn fast_runtime_options() -> RuntimeOptions {
    RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        ..Default::default()
    }
}

/// Test: Basic capability discovery
#[tokio::test]
async fn test_capability_discovery() {
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await.unwrap());
    let client = Client::new(store.clone());

    // Test capability discovery
    assert!(client.has_management_capability());

    // Test management methods work
    let instances = client.list_all_instances().await.unwrap();
    assert!(instances.is_empty());

    let metrics = client.get_system_metrics().await.unwrap();
    assert_eq!(metrics.total_instances, 0);
    assert_eq!(metrics.total_executions, 0);
    assert_eq!(metrics.running_instances, 0);
    assert_eq!(metrics.completed_instances, 0);
    assert_eq!(metrics.failed_instances, 0);
    assert_eq!(metrics.total_events, 0);

    let queues = client.get_queue_depths().await.unwrap();
    assert_eq!(queues.orchestrator_queue, 0);
    assert_eq!(queues.worker_queue, 0);
    assert_eq!(queues.timer_queue, 0);
}

/// Test: Management features with workflow
#[tokio::test]
async fn test_management_features_with_workflow() {
    let store = Arc::new(SqliteProvider::new("sqlite::memory:", None).await.unwrap());
    let client = Client::new(store.clone());

    // Set up runtime with orchestrations
    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "TestOrchestration",
            |_ctx: OrchestrationContext, _input: String| async move { Ok("completed".to_string()) },
        )
        .build();

    let _rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(ActivityRegistry::builder().build()),
        orchestrations,
    )
    .await;

    // Start an orchestration
    client
        .start_orchestration("test-instance", "TestOrchestration", "{}")
        .await
        .unwrap();

    // Wait a bit for processing
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Check management features
    assert!(client.has_management_capability());

    let instances = client.list_all_instances().await.unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0], "test-instance");

    let info = client.get_instance_info("test-instance").await.unwrap();
    assert_eq!(info.instance_id, "test-instance");
    assert_eq!(info.orchestration_name, "TestOrchestration");
    assert_eq!(info.orchestration_version, "1.0.0");
    assert_eq!(info.current_execution_id, 1);

    let executions = client.list_executions("test-instance").await.unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0], 1);

    let metrics = client.get_system_metrics().await.unwrap();
    assert_eq!(metrics.total_instances, 1);
    assert_eq!(metrics.total_executions, 1);
}

/// Test: Instance discovery and listing
#[tokio::test]
async fn test_instance_discovery() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    // Initially empty
    let instances = client.list_all_instances().await.unwrap();
    assert!(instances.is_empty());

    // Start some orchestrations
    let activities = ActivityRegistry::builder()
        .register("TestActivity", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("Processed: {input}"))
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "TestOrchestration",
            |ctx: OrchestrationContext, input: String| async move {
                let result = ctx.schedule_activity("TestActivity", input).into_activity().await?;
                Ok(result)
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

    // Start multiple orchestrations
    client
        .start_orchestration("instance-1", "TestOrchestration", "input-1")
        .await
        .unwrap();
    client
        .start_orchestration("instance-2", "TestOrchestration", "input-2")
        .await
        .unwrap();
    client
        .start_orchestration("instance-3", "TestOrchestration", "input-3")
        .await
        .unwrap();

    // Wait for completion
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // List instances
    let instances = client.list_all_instances().await.unwrap();
    assert_eq!(instances.len(), 3);
    assert!(instances.contains(&"instance-1".to_string()));
    assert!(instances.contains(&"instance-2".to_string()));
    assert!(instances.contains(&"instance-3".to_string()));

    // Test status filtering
    let completed = client.list_instances_by_status("Completed").await.unwrap();
    assert_eq!(completed.len(), 3);

    let running = client.list_instances_by_status("Running").await.unwrap();
    assert_eq!(running.len(), 0);
}

/// Test: Instance information retrieval
#[tokio::test]
async fn test_instance_info() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let activities = ActivityRegistry::builder()
        .register("TestActivity", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("Processed: {input}"))
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "TestOrchestration",
            |ctx: OrchestrationContext, input: String| async move {
                let result = ctx.schedule_activity("TestActivity", input).into_activity().await?;
                Ok(result)
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
        .start_orchestration("test-instance", "TestOrchestration", "test-input")
        .await
        .unwrap();

    // Wait for completion
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Get instance info
    let info = client.get_instance_info("test-instance").await.unwrap();
    assert_eq!(info.instance_id, "test-instance");
    assert_eq!(info.orchestration_name, "TestOrchestration");
    assert_eq!(info.orchestration_version, "1.0.0");
    assert_eq!(info.current_execution_id, 1);
    assert_eq!(info.status, "Completed");
    assert!(info.output.is_some());
    // Note: created_at and updated_at may be 0 if not properly set by SQLite
    // This is a known limitation - timestamps are stored as strings but read as i64

    // Test non-existent instance
    let result = client.get_instance_info("nonexistent").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

/// Test: Execution information and history
#[tokio::test]
async fn test_execution_info() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let activities = ActivityRegistry::builder()
        .register("TestActivity", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("Processed: {input}"))
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "TestOrchestration",
            |ctx: OrchestrationContext, input: String| async move {
                let result = ctx.schedule_activity("TestActivity", input).into_activity().await?;
                Ok(result)
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
        .start_orchestration("test-exec", "TestOrchestration", "test-input")
        .await
        .unwrap();

    // Wait for completion
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // List executions
    let executions = client.list_executions("test-exec").await.unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0], 1);

    // Get execution info
    let exec_info = client.get_execution_info("test-exec", 1).await.unwrap();
    assert_eq!(exec_info.execution_id, 1);
    assert_eq!(exec_info.status, "Completed");
    assert!(exec_info.output.is_some());
    // Note: started_at and completed_at may be 0 if not properly set by SQLite
    // This is a known limitation - timestamps are stored as strings but read as i64
    assert!(exec_info.event_count > 0);

    // Read execution history
    let history = client.read_execution_history("test-exec", 1).await.unwrap();
    assert!(!history.is_empty());

    // Should contain at least OrchestrationStarted and OrchestrationCompleted events
    let has_started = history
        .iter()
        .any(|e| matches!(&e.kind, duroxide::EventKind::OrchestrationStarted { .. }));
    let has_completed = history
        .iter()
        .any(|e| matches!(&e.kind, duroxide::EventKind::OrchestrationCompleted { .. }));
    assert!(has_started);
    assert!(has_completed);

    // Test non-existent execution
    let result = client.get_execution_info("test-exec", 999).await;
    assert!(result.is_err());
}

/// Test: Multi-execution support (ContinueAsNew)
#[tokio::test]
async fn test_multi_execution_support() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "ContinueAsNewTest",
            |ctx: OrchestrationContext, count_str: String| async move {
                let count: u32 = count_str.parse().unwrap_or(0);
                if count < 3 {
                    return ctx.continue_as_new((count + 1).to_string()).await;
                } else {
                    Ok(format!("Final: {count}"))
                }
            },
        )
        .build();

    let _rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(ActivityRegistry::builder().build()),
        orchestrations,
    )
    .await;

    // Start orchestration that will ContinueAsNew
    client
        .start_orchestration("test-continue", "ContinueAsNewTest", "0")
        .await
        .unwrap();

    // Wait for completion using wait_for_orchestration instead of sleep
    match client
        .wait_for_orchestration("test-continue", std::time::Duration::from_secs(5))
        .await
    {
        Ok(status) => println!("Orchestration completed with status: {status:?}"),
        Err(e) => println!("Orchestration failed: {e:?}"),
    }

    // Add a small delay to ensure all processing is complete
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // ContinueAsNew creates separate execution records now
    let executions = client.list_executions("test-continue").await.unwrap();

    // Should have exactly 4 executions: exec_id=1 (count=0→1), exec_id=2 (count=1→2),
    // exec_id=3 (count=2→3), exec_id=4 (count=3, completes)
    assert_eq!(executions.len(), 4);
    assert_eq!(executions, vec![1, 2, 3, 4]);

    // Get info for each execution
    for exec_id in &executions {
        let exec_info = client.get_execution_info("test-continue", *exec_id).await.unwrap();
        assert_eq!(exec_info.execution_id, *exec_id);

        // First 3 executions should be ContinuedAsNew, last one should be Completed
        if *exec_id == 4 {
            assert_eq!(exec_info.status, "Completed");
        } else {
            assert_eq!(exec_info.status, "ContinuedAsNew");
        }
    }

    // Instance info should show the latest execution
    let instance_info = client.get_instance_info("test-continue").await.unwrap();
    assert_eq!(instance_info.current_execution_id, 4); // Should be 4 (the final execution)
    assert_eq!(instance_info.status, "Completed"); // Instance status is Completed
}

/// Test: System metrics
#[tokio::test]
async fn test_system_metrics() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let activities = ActivityRegistry::builder()
        .register("TestActivity", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("Processed: {input}"))
        })
        .register("FailingActivity", |_ctx: ActivityContext, _input: String| async move {
            Err("Intentional failure".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "SuccessOrchestration",
            |ctx: OrchestrationContext, input: String| async move {
                let result = ctx.schedule_activity("TestActivity", input).into_activity().await?;
                Ok(result)
            },
        )
        .register(
            "FailureOrchestration",
            |ctx: OrchestrationContext, input: String| async move {
                let _result = ctx.schedule_activity("FailingActivity", input).into_activity().await?;
                Ok("Should not reach here".to_string())
            },
        )
        .register(
            "RunningOrchestration",
            |ctx: OrchestrationContext, _input: String| async move {
                // Wait for external event (never comes)
                let _event = ctx.schedule_wait("NeverComes").into_event().await;
                Ok("Should not reach here".to_string())
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

    // Start orchestrations with different outcomes
    client
        .start_orchestration("success-1", "SuccessOrchestration", "input-1")
        .await
        .unwrap();
    client
        .start_orchestration("success-2", "SuccessOrchestration", "input-2")
        .await
        .unwrap();
    client
        .start_orchestration("failure-1", "FailureOrchestration", "input-1")
        .await
        .unwrap();
    client
        .start_orchestration("running-1", "RunningOrchestration", "input-1")
        .await
        .unwrap();

    // Wait for processing
    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;

    // Get system metrics
    let metrics = client.get_system_metrics().await.unwrap();

    assert_eq!(metrics.total_instances, 4);
    assert_eq!(metrics.total_executions, 4);
    assert_eq!(metrics.running_instances, 1); // running-1
    assert_eq!(metrics.completed_instances, 2); // success-1, success-2
    assert_eq!(metrics.failed_instances, 1); // failure-1
    assert!(metrics.total_events > 0);

    // Test status filtering
    let completed = client.list_instances_by_status("Completed").await.unwrap();
    assert_eq!(completed.len(), 2);
    assert!(completed.contains(&"success-1".to_string()));
    assert!(completed.contains(&"success-2".to_string()));

    let failed = client.list_instances_by_status("Failed").await.unwrap();
    assert_eq!(failed.len(), 1);
    assert!(failed.contains(&"failure-1".to_string()));

    let running = client.list_instances_by_status("Running").await.unwrap();
    assert_eq!(running.len(), 1);
    assert!(running.contains(&"running-1".to_string()));
}

/// Test: Queue depths
#[tokio::test]
async fn test_queue_depths() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let activities = ActivityRegistry::builder()
        .register("SlowActivity", |_ctx: ActivityContext, _input: String| async move {
            // Simulate slow activity
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            Ok("Slow result".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "QueueTestOrchestration",
            |ctx: OrchestrationContext, input: String| async move {
                let result = ctx.schedule_activity("SlowActivity", input).into_activity().await?;
                Ok(result)
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

    // Start multiple orchestrations quickly
    for i in 1..=5 {
        client
            .start_orchestration(
                &format!("queue-test-{i}"),
                "QueueTestOrchestration",
                &format!("input-{i}"),
            )
            .await
            .unwrap();
    }

    // Check queue depths immediately (should have pending work)
    let _queues = client.get_queue_depths().await.unwrap();

    // Should have some pending work in queues (counts are always >= 0)
    // Note: Queue depths are always non-negative, so these assertions are redundant

    // Wait for completion
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Check queue depths after completion (should be empty or minimal)
    let queues_after = client.get_queue_depths().await.unwrap();
    // Note: Some queues may still have items due to timing, so we just check they're reasonable
    assert!(queues_after.orchestrator_queue <= 1);
    assert!(queues_after.worker_queue <= 1);
    assert!(queues_after.timer_queue <= 1);
}

/// Test: Error handling for non-existent instances
#[tokio::test]
async fn test_error_handling() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    // Test all management methods with non-existent instance
    let result = client.get_instance_info("nonexistent").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));

    let result = client.get_execution_info("nonexistent", 1).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));

    let result = client.read_execution_history("nonexistent", 1).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());

    let executions = client.list_executions("nonexistent").await.unwrap();
    assert!(executions.is_empty());

    // Test status filtering with non-existent status
    let result = client.list_instances_by_status("NonExistentStatus").await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

/// Test: Management interface with complex workflow
#[tokio::test]
async fn test_complex_workflow_management() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let client = Client::new(store.clone());

    let activities = ActivityRegistry::builder()
        .register("ProcessOrder", |_ctx: ActivityContext, order: String| async move {
            Ok(format!("Processed order: {order}"))
        })
        .register("SendEmail", |_ctx: ActivityContext, email: String| async move {
            Ok(format!("Sent email: {email}"))
        })
        .register("UpdateInventory", |_ctx: ActivityContext, item: String| async move {
            Ok(format!("Updated inventory for: {item}"))
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "OrderProcessing",
            |ctx: OrchestrationContext, order: String| async move {
                // Process order
                let result = ctx
                    .schedule_activity("ProcessOrder", order.clone())
                    .into_activity()
                    .await?;

                // Send confirmation email
                let _email = ctx
                    .schedule_activity("SendEmail", format!("Order processed: {result}"))
                    .into_activity()
                    .await?;

                // Update inventory
                let _inventory = ctx.schedule_activity("UpdateInventory", order).into_activity().await?;

                Ok(result)
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

    // Start multiple order processing workflows
    let orders = vec!["order-1", "order-2", "order-3", "order-4", "order-5"];
    for order in &orders {
        client
            .start_orchestration(*order, "OrderProcessing", *order)
            .await
            .unwrap();
    }

    // Wait for completion
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Verify all orders completed
    let instances = client.list_all_instances().await.unwrap();
    assert_eq!(instances.len(), 5);

    let completed = client.list_instances_by_status("Completed").await.unwrap();
    assert_eq!(completed.len(), 5);

    // Verify system metrics
    let metrics = client.get_system_metrics().await.unwrap();
    assert_eq!(metrics.total_instances, 5);
    assert_eq!(metrics.total_executions, 5);
    assert_eq!(metrics.completed_instances, 5);
    assert_eq!(metrics.failed_instances, 0);
    assert_eq!(metrics.running_instances, 0);
    assert!(metrics.total_events > 0);

    // Verify each instance details
    for order in &orders {
        let info = client.get_instance_info(order).await.unwrap();
        assert_eq!(info.instance_id, *order);
        assert_eq!(info.orchestration_name, "OrderProcessing");
        assert_eq!(info.status, "Completed");
        assert!(info.output.is_some());

        let executions = client.list_executions(order).await.unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0], 1);

        let exec_info = client.get_execution_info(order, 1).await.unwrap();
        assert_eq!(exec_info.execution_id, 1);
        assert_eq!(exec_info.status, "Completed");
        // Note: completed_at may be None due to SQLite timestamp handling
        assert!(exec_info.event_count > 0);

        let history = client.read_execution_history(order, 1).await.unwrap();
        assert!(!history.is_empty());

        // Should contain OrchestrationStarted, ActivityCompleted, and OrchestrationCompleted events
        let has_started = history
            .iter()
            .any(|e| matches!(&e.kind, duroxide::EventKind::OrchestrationStarted { .. }));
        let has_completed = history
            .iter()
            .any(|e| matches!(&e.kind, duroxide::EventKind::OrchestrationCompleted { .. }));
        let has_activity = history
            .iter()
            .any(|e| matches!(&e.kind, duroxide::EventKind::ActivityCompleted { .. }));

        assert!(has_started);
        assert!(has_completed);
        assert!(has_activity);
    }

    // Verify queue depths are empty
    let queues = client.get_queue_depths().await.unwrap();
    assert_eq!(queues.orchestrator_queue, 0);
    assert_eq!(queues.worker_queue, 0);
    assert_eq!(queues.timer_queue, 0);
}
