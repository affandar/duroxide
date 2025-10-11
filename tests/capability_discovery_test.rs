use duroxide::providers::sqlite::SqliteProvider;
use duroxide::{Client, OrchestrationContext, OrchestrationRegistry};
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use std::sync::Arc;

#[tokio::test]
async fn test_capability_discovery() {
    let store = Arc::new(SqliteProvider::new("sqlite::memory:").await.unwrap());
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

#[tokio::test]
async fn test_management_features_with_workflow() {
    let store = Arc::new(SqliteProvider::new("sqlite::memory:").await.unwrap());
    let client = Client::new(store.clone());

    // Set up runtime with orchestrations
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestOrchestration", |_ctx: OrchestrationContext, _input: String| async move {
            Ok("completed".to_string())
        })
        .build();

    let _rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(ActivityRegistry::builder().build()),
        orchestrations
    ).await;

    // Start an orchestration
    client.start_orchestration("test-instance", "TestOrchestration", "{}").await.unwrap();

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

    let executions = client.list_executions("test-instance").await;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0], 1);

    let metrics = client.get_system_metrics().await.unwrap();
    assert_eq!(metrics.total_instances, 1);
    assert_eq!(metrics.total_executions, 1);
}
