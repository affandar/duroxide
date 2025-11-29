use crate::provider_validation::{Event, EventKind, ExecutionMetadata, start_item};
use crate::provider_validations::ProviderFactory;
use std::time::Duration;

/// Test management: list_instances returns all instance IDs
pub async fn test_list_instances<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: list_instances returns all instance IDs");
    let provider = factory.create_provider().await;
    let mgmt = provider
        .as_management_capability()
        .expect("Provider should implement ProviderAdmin");

    // Create a few instances
    for i in 0..3 {
        crate::provider_validation::create_instance(&*provider, &format!("mgmt-inst-{}", i))
            .await
            .unwrap();
    }

    // List all instances
    let instances = mgmt.list_instances().await.unwrap();
    assert!(instances.len() >= 3, "Should list all created instances");
    for i in 0..3 {
        assert!(
            instances.contains(&format!("mgmt-inst-{}", i)),
            "Should include instance mgmt-inst-{}",
            i
        );
    }
    tracing::info!("✓ Test passed: list_instances verified");
}

/// Test management: list_instances_by_status filters correctly
pub async fn test_list_instances_by_status<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: list_instances_by_status filters correctly");
    let provider = factory.create_provider().await;
    let mgmt = provider.as_management_capability().unwrap();

    // Create instance and complete it
    provider
        .enqueue_for_orchestrator(start_item("mgmt-completed"), None)
        .await
        .unwrap();
    let item = provider
        .fetch_orchestration_item(Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();

    // Ack with Completed status
    provider
        .ack_orchestration_item(
            &item.lock_token,
            1,
            vec![Event::with_event_id(
                1,
                "mgmt-completed".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![],
            vec![],
            ExecutionMetadata {
                status: Some("Completed".to_string()),
                output: Some("done".to_string()),
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
            },
        )
        .await
        .unwrap();

    // Query by status
    let completed = mgmt.list_instances_by_status("Completed").await.unwrap();
    assert!(
        completed.contains(&"mgmt-completed".to_string()),
        "Should list completed instance"
    );
    tracing::info!("✓ Test passed: list_instances_by_status verified");
}

/// Test management: list_executions returns all execution IDs
pub async fn test_list_executions<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: list_executions returns all execution IDs");
    let provider = factory.create_provider().await;
    let mgmt = provider.as_management_capability().unwrap();

    // Create instance with first execution
    provider
        .enqueue_for_orchestrator(start_item("mgmt-multi-exec"), None)
        .await
        .unwrap();
    let item = provider
        .fetch_orchestration_item(Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    provider
        .ack_orchestration_item(
            &item.lock_token,
            1,
            vec![Event::with_event_id(
                1,
                "mgmt-multi-exec".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // List executions
    let executions = mgmt.list_executions("mgmt-multi-exec").await.unwrap();
    assert!(executions.contains(&1), "Should list execution 1");
    tracing::info!("✓ Test passed: list_executions verified");
}

/// Test management: get_instance_info returns metadata
pub async fn test_get_instance_info<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: get_instance_info returns metadata");
    let provider = factory.create_provider().await;
    let mgmt = provider.as_management_capability().unwrap();

    // Create and complete instance
    provider
        .enqueue_for_orchestrator(start_item("mgmt-info"), None)
        .await
        .unwrap();
    let item = provider
        .fetch_orchestration_item(Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    provider
        .ack_orchestration_item(
            &item.lock_token,
            1,
            vec![Event::with_event_id(
                1,
                "mgmt-info".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "InfoOrch".to_string(),
                    version: "2.0.0".to_string(),
                    input: "test".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("InfoOrch".to_string()),
                orchestration_version: Some("2.0.0".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Get instance info
    let info = mgmt.get_instance_info("mgmt-info").await.unwrap();
    assert_eq!(info.instance_id, "mgmt-info");
    assert_eq!(info.orchestration_name, "InfoOrch");
    assert_eq!(info.orchestration_version, "2.0.0");
    assert_eq!(info.current_execution_id, 1);
    tracing::info!("✓ Test passed: get_instance_info verified");
}

/// Test management: get_execution_info returns execution metadata
pub async fn test_get_execution_info<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: get_execution_info returns execution metadata");
    let provider = factory.create_provider().await;
    let mgmt = provider.as_management_capability().unwrap();

    // Create instance
    provider
        .enqueue_for_orchestrator(start_item("mgmt-exec-info"), None)
        .await
        .unwrap();
    let item = provider
        .fetch_orchestration_item(Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    provider
        .ack_orchestration_item(
            &item.lock_token,
            1,
            vec![Event::with_event_id(
                1,
                "mgmt-purge".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![],
            vec![],
            ExecutionMetadata {
                status: Some("Completed".to_string()),
                output: Some("result".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Get execution info
    let info = mgmt.get_execution_info("mgmt-exec-info", 1).await.unwrap();
    assert_eq!(info.execution_id, 1);
    assert_eq!(info.status, "Completed");
    assert_eq!(info.output, Some("result".to_string()));
    tracing::info!("✓ Test passed: get_execution_info verified");
}

/// Test management: get_system_metrics returns accurate counts
pub async fn test_get_system_metrics<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: get_system_metrics returns accurate counts");
    let provider = factory.create_provider().await;
    let mgmt = provider.as_management_capability().unwrap();

    // Get baseline metrics
    let metrics = mgmt.get_system_metrics().await.unwrap();
    let baseline_instances = metrics.total_instances;

    // Create new instance
    provider
        .enqueue_for_orchestrator(start_item("mgmt-metrics"), None)
        .await
        .unwrap();
    let item = provider
        .fetch_orchestration_item(Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    provider
        .ack_orchestration_item(
            &item.lock_token,
            1,
            vec![Event::with_event_id(
                1,
                "mgmt-cancel".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Metrics should reflect new instance
    let updated_metrics = mgmt.get_system_metrics().await.unwrap();
    assert!(
        updated_metrics.total_instances > baseline_instances,
        "total_instances should increase"
    );
    assert!(updated_metrics.total_events > 0, "total_events should be > 0");
    tracing::info!("✓ Test passed: get_system_metrics verified");
}

/// Test management: get_queue_depths returns current queue sizes
pub async fn test_get_queue_depths<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing management: get_queue_depths returns current queue sizes");
    let provider = factory.create_provider().await;
    let mgmt = provider.as_management_capability().unwrap();

    // Get baseline
    let depths = mgmt.get_queue_depths().await.unwrap();
    let baseline_orch = depths.orchestrator_queue;

    // Enqueue work
    provider
        .enqueue_for_orchestrator(start_item("mgmt-queue"), None)
        .await
        .unwrap();

    // Queue depth should increase
    let updated_depths = mgmt.get_queue_depths().await.unwrap();
    assert!(
        updated_depths.orchestrator_queue > baseline_orch,
        "orchestrator_queue should increase"
    );
    tracing::info!("✓ Test passed: get_queue_depths verified");
}
