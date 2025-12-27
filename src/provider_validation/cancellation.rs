use crate::provider_validation::{Event, EventKind, ExecutionMetadata, start_item};
use crate::provider_validations::ProviderFactory;
use crate::providers::{ExecutionState, WorkItem};
use std::time::Duration;

/// Test: Fetch Returns Running State for Active Orchestration
/// Goal: Verify that fetch_work_item returns ExecutionState::Running when orchestration is active.
pub async fn test_fetch_returns_running_state_for_active_orchestration<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: fetch returns Running for active orchestration");
    let provider = factory.create_provider().await;

    // 1. Create an active orchestration instance
    provider
        .enqueue_for_orchestrator(start_item("inst-running"), None)
        .await
        .unwrap();

    // Ack start to create instance
    let (_item, token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    let metadata = ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        status: Some("Running".to_string()),
        ..Default::default()
    };

    // Enqueue an activity
    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-running".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider
        .ack_orchestration_item(
            &token,
            1,
            vec![Event::with_event_id(
                1,
                "inst-running".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![activity_item],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // 2. Fetch the activity work item
    let result = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap();

    match result {
        Some((_, _, _, state)) => {
            assert_eq!(state, ExecutionState::Running, "Expected ExecutionState::Running");
        }
        None => panic!("Expected to fetch work item"),
    }

    tracing::info!("✓ Test passed: fetch returns Running for active orchestration");
}

/// Test: Fetch Returns Terminal State when Orchestration Completed
/// Goal: Verify that fetch_work_item returns ExecutionState::Terminal when orchestration is completed.
pub async fn test_fetch_returns_terminal_state_when_orchestration_completed<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: fetch returns Terminal for completed orchestration");
    let provider = factory.create_provider().await;

    // 1. Create an active orchestration instance
    provider
        .enqueue_for_orchestrator(start_item("inst-completed"), None)
        .await
        .unwrap();
    let (_item, token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    // Enqueue activity but also COMPLETE the orchestration
    let metadata = ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        status: Some("Completed".to_string()), // Terminal state
        output: Some("done".to_string()),
        ..Default::default()
    };

    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-completed".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider
        .ack_orchestration_item(
            &token,
            1,
            vec![
                Event::with_event_id(
                    1,
                    "inst-completed".to_string(),
                    1,
                    None,
                    EventKind::OrchestrationStarted {
                        name: "TestOrch".to_string(),
                        version: "1.0".to_string(),
                        input: "{}".to_string(),
                        parent_instance: None,
                        parent_id: None,
                    },
                ),
                Event::with_event_id(
                    2,
                    "inst-completed".to_string(),
                    1,
                    None,
                    EventKind::OrchestrationCompleted {
                        output: "done".to_string(),
                    },
                ),
            ],
            vec![activity_item],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // 2. Fetch the activity work item
    let result = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap();

    match result {
        Some((_, _, _, state)) => match state {
            ExecutionState::Terminal { status } => {
                assert_eq!(status, "Completed", "Expected status 'Completed'");
            }
            _ => panic!("Expected ExecutionState::Terminal, got {:?}", state),
        },
        None => panic!("Expected to fetch work item"),
    }

    tracing::info!("✓ Test passed: fetch returns Terminal for completed orchestration");
}

/// Test: Fetch Returns Terminal State when Orchestration Failed
/// Goal: Verify that fetch_work_item returns ExecutionState::Terminal when orchestration is failed.
pub async fn test_fetch_returns_terminal_state_when_orchestration_failed<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: fetch returns Terminal for failed orchestration");
    let provider = factory.create_provider().await;

    // 1. Create an active orchestration instance
    provider
        .enqueue_for_orchestrator(start_item("inst-failed"), None)
        .await
        .unwrap();
    let (_item, token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    // Enqueue activity but also FAIL the orchestration
    let metadata = ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        status: Some("Failed".to_string()), // Terminal state
        ..Default::default()
    };

    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-failed".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider
        .ack_orchestration_item(
            &token,
            1,
            vec![
                Event::with_event_id(
                    1,
                    "inst-failed".to_string(),
                    1,
                    None,
                    EventKind::OrchestrationStarted {
                        name: "TestOrch".to_string(),
                        version: "1.0".to_string(),
                        input: "{}".to_string(),
                        parent_instance: None,
                        parent_id: None,
                    },
                ),
                Event::with_event_id(
                    2,
                    "inst-failed".to_string(),
                    1,
                    None,
                    EventKind::OrchestrationFailed {
                        details: crate::ErrorDetails::Application {
                            kind: crate::AppErrorKind::OrchestrationFailed,
                            message: "boom".to_string(),
                            retryable: false,
                        },
                    },
                ),
            ],
            vec![activity_item],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // 2. Fetch the activity work item
    let result = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap();

    match result {
        Some((_, _, _, state)) => match state {
            ExecutionState::Terminal { status } => {
                assert_eq!(status, "Failed", "Expected status 'Failed'");
            }
            _ => panic!("Expected ExecutionState::Terminal, got {:?}", state),
        },
        None => panic!("Expected to fetch work item"),
    }

    tracing::info!("✓ Test passed: fetch returns Terminal for failed orchestration");
}

/// Test: Fetch Returns Terminal State when Orchestration ContinuedAsNew
/// Goal: Verify that fetch_work_item returns ExecutionState::Terminal when orchestration continued as new.
pub async fn test_fetch_returns_terminal_state_when_orchestration_continued_as_new<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: fetch returns Terminal for ContinuedAsNew orchestration");
    let provider = factory.create_provider().await;

    // 1. Create an active orchestration instance
    provider
        .enqueue_for_orchestrator(start_item("inst-can"), None)
        .await
        .unwrap();
    let (_item, token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    // Enqueue activity but also ContinueAsNew
    let metadata = ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        status: Some("ContinuedAsNew".to_string()), // Terminal state for THIS execution
        output: Some("new-input".to_string()),
        ..Default::default()
    };

    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-can".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider
        .ack_orchestration_item(
            &token,
            1,
            vec![
                Event::with_event_id(
                    1,
                    "inst-can".to_string(),
                    1,
                    None,
                    EventKind::OrchestrationStarted {
                        name: "TestOrch".to_string(),
                        version: "1.0".to_string(),
                        input: "{}".to_string(),
                        parent_instance: None,
                        parent_id: None,
                    },
                ),
                Event::with_event_id(
                    2,
                    "inst-can".to_string(),
                    1,
                    None,
                    EventKind::OrchestrationContinuedAsNew {
                        input: "new-input".to_string(),
                    },
                ),
            ],
            vec![activity_item],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // 2. Fetch the activity work item
    let result = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap();

    match result {
        Some((_, _, _, state)) => match state {
            ExecutionState::Terminal { status } => {
                assert_eq!(status, "ContinuedAsNew", "Expected status 'ContinuedAsNew'");
            }
            _ => panic!("Expected ExecutionState::Terminal, got {:?}", state),
        },
        None => panic!("Expected to fetch work item"),
    }

    tracing::info!("✓ Test passed: fetch returns Terminal for ContinuedAsNew orchestration");
}

/// Test: Fetch Returns Missing State when Instance Deleted
/// Goal: Verify that fetch_work_item returns ExecutionState::Missing when instance is gone.
pub async fn test_fetch_returns_missing_state_when_instance_deleted<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: fetch returns Missing for deleted instance");
    let provider = factory.create_provider().await;

    // 1. Enqueue an activity directly (simulating a leftover message)
    // We don't create the instance, so it is "missing"
    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-missing".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider.enqueue_for_worker(activity_item).await.unwrap();

    // 2. Fetch the activity work item
    let result = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap();

    match result {
        Some((_, _, _, state)) => {
            assert_eq!(state, ExecutionState::Missing, "Expected ExecutionState::Missing");
        }
        None => panic!("Expected to fetch work item"),
    }

    tracing::info!("✓ Test passed: fetch returns Missing for deleted instance");
}

/// Test: Renew Returns Running when Orchestration Active
/// Goal: Verify that renew_work_item_lock returns ExecutionState::Running when orchestration is active.
pub async fn test_renew_returns_running_when_orchestration_active<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: renew returns Running for active orchestration");
    let provider = factory.create_provider().await;

    // 1. Create active instance and activity
    provider
        .enqueue_for_orchestrator(start_item("inst-renew-run"), None)
        .await
        .unwrap();
    let (_item, token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    let metadata = ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        status: Some("Running".to_string()),
        ..Default::default()
    };

    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-renew-run".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider
        .ack_orchestration_item(
            &token,
            1,
            vec![Event::with_event_id(
                1,
                "inst-renew-run".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![activity_item],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // 2. Fetch activity to get lock token
    let (_, lock_token, _, _) = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    // 3. Renew lock
    let state = provider
        .renew_work_item_lock(&lock_token, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(state, ExecutionState::Running, "Expected ExecutionState::Running");

    tracing::info!("✓ Test passed: renew returns Running for active orchestration");
}

/// Test: Renew Returns Terminal when Orchestration Completed
/// Goal: Verify that renew_work_item_lock returns ExecutionState::Terminal when orchestration is completed.
pub async fn test_renew_returns_terminal_when_orchestration_completed<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: renew returns Terminal for completed orchestration");
    let provider = factory.create_provider().await;

    // 1. Create active instance and activity
    provider
        .enqueue_for_orchestrator(start_item("inst-renew-term"), None)
        .await
        .unwrap();
    let (_item, token, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    let metadata = ExecutionMetadata {
        orchestration_name: Some("TestOrch".to_string()),
        status: Some("Running".to_string()),
        ..Default::default()
    };

    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-renew-term".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };

    provider
        .ack_orchestration_item(
            &token,
            1,
            vec![Event::with_event_id(
                1,
                "inst-renew-term".to_string(),
                1,
                None,
                EventKind::OrchestrationStarted {
                    name: "TestOrch".to_string(),
                    version: "1.0".to_string(),
                    input: "{}".to_string(),
                    parent_instance: None,
                    parent_id: None,
                },
            )],
            vec![activity_item],
            vec![],
            metadata,
        )
        .await
        .unwrap();

    // 2. Fetch activity to get lock token
    let (_, lock_token, _, _) = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    // 3. Complete the orchestration (simulate another turn)
    // We need to fetch the orchestration item again (it's not in queue, so we need to trigger it or just update DB directly?
    // Since we can't easily update DB directly in generic test, we'll simulate it by enqueuing a new message to trigger a turn)

    // Enqueue a dummy external event to trigger a turn
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-renew-term".to_string(),
                name: "Trigger".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    let (_item2, token2, _) = provider
        .fetch_orchestration_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    let metadata2 = ExecutionMetadata {
        status: Some("Completed".to_string()),
        output: Some("done".to_string()),
        ..Default::default()
    };

    provider
        .ack_orchestration_item(
            &token2,
            1,
            vec![Event::with_event_id(
                2,
                "inst-renew-term".to_string(),
                1,
                None,
                EventKind::OrchestrationCompleted {
                    output: "done".to_string(),
                },
            )],
            vec![],
            vec![],
            metadata2,
        )
        .await
        .unwrap();

    // 4. Renew lock - should now see Terminal
    let state = provider
        .renew_work_item_lock(&lock_token, Duration::from_secs(30))
        .await
        .unwrap();
    match state {
        ExecutionState::Terminal { status } => {
            assert_eq!(status, "Completed", "Expected status 'Completed'");
        }
        _ => panic!("Expected ExecutionState::Terminal, got {:?}", state),
    }

    tracing::info!("✓ Test passed: renew returns Terminal for completed orchestration");
}

/// Test: Renew Returns Missing when Instance Deleted
/// Goal: Verify that renew_work_item_lock returns ExecutionState::Missing when instance is deleted.
/// Note: This test is hard to implement generically because "deleting an instance" isn't a standard Provider method.
/// We'll skip this one for generic validation unless we add a delete_instance method to Provider.
/// Instead, we can test "Missing" by having an activity for a non-existent instance, fetching it, and renewing it.
pub async fn test_renew_returns_missing_when_instance_deleted<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: renew returns Missing for missing instance");
    let provider = factory.create_provider().await;

    // 1. Enqueue activity for missing instance
    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-renew-missing".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };
    provider.enqueue_for_worker(activity_item).await.unwrap();

    // 2. Fetch activity to get lock token
    let (_, lock_token, _, state) = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, ExecutionState::Missing);

    // 3. Renew lock
    let state = provider
        .renew_work_item_lock(&lock_token, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(state, ExecutionState::Missing, "Expected ExecutionState::Missing");

    tracing::info!("✓ Test passed: renew returns Missing for missing instance");
}

/// Test: Ack Work Item None Deletes Without Enqueue
/// Goal: Verify that ack_work_item(token, None) deletes the item but enqueues nothing.
pub async fn test_ack_work_item_none_deletes_without_enqueue<F: ProviderFactory>(factory: &F) {
    tracing::info!("→ Testing cancellation: ack(None) deletes without enqueue");
    let provider = factory.create_provider().await;

    // 1. Enqueue activity
    let activity_item = WorkItem::ActivityExecute {
        instance: "inst-ack-none".to_string(),
        execution_id: 1,
        id: 1,
        name: "TestActivity".to_string(),
        input: "{}".to_string(),
    };
    provider.enqueue_for_worker(activity_item).await.unwrap();

    // 2. Fetch activity
    let (_, lock_token, _, _) = provider
        .fetch_work_item(Duration::from_secs(30), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();

    // 3. Ack with None
    provider.ack_work_item(&lock_token, None).await.unwrap();

    // 4. Verify worker queue is empty
    let result = provider
        .fetch_work_item(Duration::from_millis(100), Duration::ZERO)
        .await
        .unwrap();
    assert!(result.is_none(), "Worker queue should be empty");

    // 5. Verify orchestrator queue is empty (no completion enqueued)
    // We can check this by trying to fetch orchestration item.
    // Since we didn't create the instance, fetch_orchestration_item might return None anyway.
    // But if a completion WAS enqueued, it would be there (even if instance is missing, the message exists).
    // However, fetch_orchestration_item usually requires instance lock.
    // A better check might be get_queue_depths if available, or just relying on fetch returning None.

    // Let's try to fetch orchestration item. If a completion was enqueued, it would be visible.
    let orch_result = provider
        .fetch_orchestration_item(Duration::from_millis(100), Duration::ZERO)
        .await
        .unwrap();
    assert!(orch_result.is_none(), "Orchestrator queue should be empty");

    tracing::info!("✓ Test passed: ack(None) deletes without enqueue");
}
