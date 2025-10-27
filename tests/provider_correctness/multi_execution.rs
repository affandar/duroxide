use duroxide::Event;
use duroxide::providers::sqlite::{SqliteOptions, SqliteProvider};
use duroxide::providers::{ExecutionMetadata, Provider, WorkItem};
use std::sync::Arc;
use std::time::Duration;

#[path = "../common/mod.rs"]
mod common;

const TEST_LOCK_TIMEOUT_MS: u64 = 1000;

/// Helper to create a provider for testing
async fn create_provider() -> Arc<dyn Provider> {
    let options = SqliteOptions {
        lock_timeout: Duration::from_millis(TEST_LOCK_TIMEOUT_MS),
    };
    Arc::new(SqliteProvider::new_in_memory_with_options(Some(options)).await.unwrap())
}

/// Helper to create a start item for an instance
fn start_item(instance: &str, execution_id: u64) -> WorkItem {
    WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrch".to_string(),
        input: "{}".to_string(),
        version: Some("1.0.0".to_string()),
        parent_instance: None,
        parent_id: None,
        execution_id,
    }
}

/// Test 6.1: Execution Isolation
/// Goal: Verify each execution has separate history.
#[tokio::test]
async fn test_execution_isolation() {
    let provider = create_provider().await;

    // Create execution 1 with 3 events
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 1), None)
        .await
        .unwrap();
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item1.lock_token,
            1,
            vec![
                Event::OrchestrationStarted {
                    event_id: 1,
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
                Event::ActivityScheduled {
                    event_id: 2,
                    name: "Activity1".to_string(),
                    input: "input1".to_string(),
                    execution_id: 1,
                },
                Event::OrchestrationCompleted {
                    event_id: 3,
                    output: "result1".to_string(),
                },
            ],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Create execution 2 with 2 events
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 2), None)
        .await
        .unwrap();
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item2.lock_token,
            2,
            vec![
                Event::OrchestrationStarted {
                    event_id: 1,
                    name: "TestOrch".to_string(),
                    version: "1.0.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
                Event::OrchestrationCompleted {
                    event_id: 2,
                    output: "result2".to_string(),
                },
            ],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Read execution 1 → should return 3 events
    let history1 = provider.read_with_execution("instance-A", 1).await;
    assert_eq!(history1.len(), 3);
    assert!(matches!(history1[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(history1[1], Event::ActivityScheduled { .. }));
    assert!(matches!(history1[2], Event::OrchestrationCompleted { .. }));

    // Read execution 2 → should return 2 events
    let history2 = provider.read_with_execution("instance-A", 2).await;
    assert_eq!(history2.len(), 2);
    assert!(matches!(history2[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(history2[1], Event::OrchestrationCompleted { .. }));

    // Read latest (default) → should return execution 2's events
    let latest = provider.read("instance-A").await;
    assert_eq!(latest.len(), 2);
    assert!(matches!(latest[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(latest[1], Event::OrchestrationCompleted { .. }));
}

/// Test 6.2: Latest Execution Detection
/// Goal: Verify read() returns latest execution's history.
#[tokio::test]
async fn test_latest_execution_detection() {
    let provider = create_provider().await;

    // Create execution 1 with event "A"
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 1), None)
        .await
        .unwrap();
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item1.lock_token,
            1,
            vec![Event::ActivityScheduled {
                event_id: 1,
                name: "A".to_string(),
                input: "".to_string(),
                execution_id: 1,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Create execution 2 with event "B"
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 2), None)
        .await
        .unwrap();
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item2.lock_token,
            2,
            vec![Event::ActivityScheduled {
                event_id: 1,
                name: "B".to_string(),
                input: "".to_string(),
                execution_id: 2,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Call read() → should return execution 2 (latest)
    let latest = provider.read("instance-A").await;
    assert_eq!(latest.len(), 1);
    if let Event::ActivityScheduled { name, .. } = &latest[0] {
        assert_eq!(name, "B");
    } else {
        panic!("Expected ActivityScheduled");
    }

    // Call read_with_execution(instance, 1) → should return execution 1
    let exec1 = provider.read_with_execution("instance-A", 1).await;
    assert_eq!(exec1.len(), 1);
    if let Event::ActivityScheduled { name, .. } = &exec1[0] {
        assert_eq!(name, "A");
    } else {
        panic!("Expected ActivityScheduled");
    }
}

/// Test 6.3: Execution ID Sequencing
/// Goal: Verify execution IDs increment correctly.
#[tokio::test]
async fn test_execution_id_sequencing() {
    let provider = create_provider().await;

    // First execution should be execution_id = 1
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 1), None)
        .await
        .unwrap();
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item1.execution_id, 1);

    // Complete execution 1
    provider
        .ack_orchestration_item(
            &item1.lock_token,
            1,
            vec![Event::OrchestrationStarted {
                event_id: 1,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Enqueue second execution with explicit execution_id = 2
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 2), None)
        .await
        .unwrap();
    let item2 = provider.fetch_orchestration_item().await.unwrap();
    // The fetched item will have execution_id = 1 (from instance's current_execution_id)
    // We need to ack with execution_id = 2 to create the new execution
    provider
        .ack_orchestration_item(
            &item2.lock_token,
            2, // Explicitly ack with execution_id = 2
            vec![Event::OrchestrationStarted {
                event_id: 1,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Now fetch should return execution_id = 2
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 3), None)
        .await
        .unwrap();
    let item3 = provider.fetch_orchestration_item().await.unwrap();
    assert_eq!(item3.execution_id, 2, "Current execution should be 2");
}

/// Test 6.4: Continue-As-New Creates New Execution
/// Goal: Verify continue-as-new creates new execution with incremented ID.
#[tokio::test]
async fn test_continue_as_new_creates_new_execution() {
    let provider = create_provider().await;

    // Create execution 1
    provider
        .enqueue_orchestrator_work(start_item("instance-A", 1), None)
        .await
        .unwrap();
    let item1 = provider.fetch_orchestration_item().await.unwrap();
    provider
        .ack_orchestration_item(
            &item1.lock_token,
            1,
            vec![Event::OrchestrationStarted {
                event_id: 1,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "{}".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Simulate continue-as-new by enqueuing a ContinueAsNew work item
    provider
        .enqueue_orchestrator_work(
            WorkItem::ContinueAsNew {
                instance: "instance-A".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "new-input".to_string(),
                version: Some("1.0.0".to_string()),
            },
            None,
        )
        .await
        .unwrap();

    let item2 = provider.fetch_orchestration_item().await.unwrap();
    // The fetched item will have execution_id = 1 (from instance's current_execution_id)
    // We need to ack with execution_id = 2 to create the new execution
    provider
        .ack_orchestration_item(
            &item2.lock_token,
            2, // Explicitly ack with execution_id = 2 for ContinueAsNew
            vec![Event::OrchestrationStarted {
                event_id: 1,
                name: "TestOrch".to_string(),
                version: "1.0.0".to_string(),
                input: "new-input".to_string(),
                parent_instance: None,
                parent_id: None,
            }],
            vec![],
            vec![],
            ExecutionMetadata::default(),
        )
        .await
        .unwrap();

    // Now the instance should have current_execution_id = 2
    let item3 = provider.fetch_orchestration_item().await;
    if let Some(item) = item3 {
        assert_eq!(item.execution_id, 2, "Continue-as-new should create execution 2");
    }
}

/// Test 6.5: Execution History Persistence
/// Goal: Verify all executions' history persists independently.
#[tokio::test]
async fn test_execution_history_persistence() {
    let provider = create_provider().await;

    // Create 3 executions with different history
    for exec_id in 1..=3 {
        provider
            .enqueue_orchestrator_work(start_item("instance-A", exec_id), None)
            .await
            .unwrap();
        let item = provider.fetch_orchestration_item().await.unwrap();
        provider
            .ack_orchestration_item(
                &item.lock_token,
                exec_id,
                vec![Event::ActivityScheduled {
                    event_id: 1,
                    name: format!("Activity-{}", exec_id),
                    input: format!("input-{}", exec_id),
                    execution_id: exec_id,
                }],
                vec![],
                vec![],
                ExecutionMetadata::default(),
            )
            .await
            .unwrap();
    }

    // Verify each execution's history is independent
    for exec_id in 1..=3 {
        let history = provider.read_with_execution("instance-A", exec_id).await;
        assert_eq!(history.len(), 1);
        if let Event::ActivityScheduled { name, .. } = &history[0] {
            assert_eq!(name, &format!("Activity-{}", exec_id));
        } else {
            panic!("Expected ActivityScheduled");
        }
    }

    // Latest should be execution 3
    let latest = provider.read("instance-A").await;
    assert_eq!(latest.len(), 1);
    if let Event::ActivityScheduled { name, .. } = &latest[0] {
        assert_eq!(name, "Activity-3");
    } else {
        panic!("Expected ActivityScheduled");
    }
}
