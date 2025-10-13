use duroxide::runtime;
use duroxide::runtime::registry::{ActivityRegistry, OrchestrationRegistry};
use duroxide::*;
use std::sync::Arc;

mod common;

/// Test that verifies activity completion reliability after crash between dequeue and enqueue
///
/// Scenario:
/// 1. Orchestration schedules an activity
/// 2. Activity is dequeued from worker queue and executed
/// 3. System crashes after activity execution but before completion is enqueued
/// 4. System restarts
/// 5. Activity should be redelivered and executed again (at-least-once semantics)
#[tokio::test]
async fn activity_reliability_after_crash_before_completion_enqueue() {
    // Use SQLite store for persistence across "crash"
    // Shorten worker lock lease so redelivery happens quickly after restart
    // Safety: setting environment variables in tests is process-wide; we set it before creating the store
    // and use a deterministic value to control lease duration. We restore it at the end of the test.
    let prev_lease = std::env::var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS").ok();
    unsafe {
        std::env::set_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS", "1000");
    }
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Simple orchestration that schedules an activity and waits for completion
    let orch = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("Starting activity reliability test orchestration");

        // Schedule an activity
        let result = ctx.schedule_activity("TestActivity", input).into_activity().await?;

        ctx.trace_info("Activity completed successfully");
        Ok(format!("Activity result: {}", result))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("TestActivity", |input: String| async move {
            println!("Executing TestActivity with input: {}", input);
            // Simulate some work
            Ok(format!("Processed: {}", input))
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ActivityReliabilityTest", orch)
        .build();

    // Phase 1: Start orchestration and let it schedule the activity
    let store1 = store.clone();
    let rt1 = runtime::Runtime::start_with_store(
        store1.clone(),
        Arc::new(activity_registry.clone()),
        orchestration_registry.clone(),
    )
    .await;

    let instance = "inst-activity-reliability";
    let client1 = duroxide::Client::new(store1.clone());
    let _ = client1
        .start_orchestration(instance, "ActivityReliabilityTest", "test-data")
        .await
        .unwrap();

    // Wait for activity to be scheduled
    assert!(
        common::wait_for_history(
            store1.clone(),
            instance,
            |h| h.iter().any(|e| matches!(e, Event::ActivityScheduled { .. })),
            2_000
        )
        .await,
        "Activity should be scheduled"
    );

    // Verify activity hasn't completed yet (we'll simulate crash before completion)
    let hist_before = store1.read(instance).await;
    assert!(
        !hist_before.iter().any(|e| matches!(e, Event::ActivityCompleted { .. })),
        "Activity should not have completed yet"
    );

    // Verify activity was scheduled but not completed
    assert!(
        hist_before
            .iter()
            .any(|e| matches!(e, Event::ActivityScheduled { name, .. } if name == "TestActivity"))
    );
    assert!(
        !hist_before
            .iter()
            .any(|e| matches!(e, Event::ActivityCompleted { result, .. } if result == "Processed: test-data"))
    );

    // Simulate crash by shutting down runtime
    println!("Simulating crash - shutting down runtime before activity completes...");
    rt1.shutdown().await;

    // Small delay to ensure shutdown completes
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Phase 2: "Restart" system with new runtime but same store
    println!("Restarting system...");
    let store2 = store.clone();
    let rt2 =
        runtime::Runtime::start_with_store(store2.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // The runtime should automatically resume the orchestration and reprocess pending activities

    // Wait for orchestration to complete
    let client2 = duroxide::Client::new(store2.clone());
    match client2
        .wait_for_orchestration(instance, std::time::Duration::from_secs(10))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Activity result: Processed: test-data");
            println!("✅ Orchestration completed successfully after restart");
        }
        runtime::OrchestrationStatus::Failed { error } => {
            panic!("Orchestration failed after restart: {}", error);
        }
        status => {
            panic!("Unexpected orchestration status after restart: {:?}", status);
        }
    }

    // Verify the activity actually completed
    let hist_after = store2.read(instance).await;

    // Debug: print all events (can be removed in production)
    println!("History after restart:");
    for (i, event) in hist_after.iter().enumerate() {
        println!("  {}: {:?}", i, event);
    }

    // Should have exactly one ActivityScheduled and one ActivityCompleted for our TestActivity
    let test_activity_scheduled_count = hist_after
        .iter()
        .filter(|e| {
            matches!(e, Event::ActivityScheduled { name, .. } 
                           if name == "TestActivity")
        })
        .count();
    let test_activity_completed_count = hist_after
        .iter()
        .filter(|e| {
            matches!(e, Event::ActivityCompleted { result, .. } 
                           if result == "Processed: test-data")
        })
        .count();

    assert_eq!(
        test_activity_scheduled_count, 1,
        "Should have exactly one TestActivity scheduled event"
    );
    assert_eq!(
        test_activity_completed_count, 1,
        "Should have exactly one TestActivity completed event"
    );

    println!("✅ Activity reliability test passed - activity completed correctly after restart");
    rt2.shutdown().await;

    // Restore environment
    match prev_lease {
        Some(v) => unsafe {
            std::env::set_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS", v);
        },
        None => unsafe {
            std::env::remove_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS");
        },
    }
}

/// Test multiple activities with crash/recovery
///
/// NOTE: This test is currently ignored because the FS provider has a race condition:
/// - Activities can be scheduled and written to history
/// - System crashes before work items are enqueued to worker queue
/// - On restart, history shows scheduled activities but no work items exist
/// - Runtime lacks history-driven reconciliation to detect and fix this inconsistency
///
/// This would be fixed by either:
/// 1. Implementing history-driven reconciliation in the runtime
/// 2. Using a transactional provider like SQLite
#[tokio::test]
async fn multiple_activities_reliability_after_crash() {
    // Use SQLite store for persistence across "crash" - SQLite provides atomicity
    // Shorten worker lock lease so redelivery happens quickly after restart
    // Safety: see note above. We restore it at the end of the test.
    let prev_lease = std::env::var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS").ok();
    unsafe {
        std::env::set_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS", "1000");
    }
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Orchestration with multiple activities
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.trace_info("Starting multi-activity reliability test");

        // Schedule three activities in parallel
        let a1 = ctx.schedule_activity("TestActivity", "task1");
        let a2 = ctx.schedule_activity("TestActivity", "task2");
        let a3 = ctx.schedule_activity("TestActivity", "task3");

        // Wait for all activities
        let results = ctx.join(vec![a1, a2, a3]).await;

        let mut outputs = Vec::new();
        for result in results {
            match result {
                DurableOutput::Activity(Ok(output)) => outputs.push(output),
                DurableOutput::Activity(Err(e)) => return Err(e),
                _ => return Err("Unexpected output type".to_string()),
            }
        }

        Ok(format!("All activities completed: {:?}", outputs))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("TestActivity", |input: String| async move {
            println!("Executing TestActivity with input: {}", input);
            Ok(format!("Processed: {}", input))
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("MultiActivityTest", orch)
        .build();

    // Phase 1: Start and wait for all activities to be scheduled
    let store1 = store.clone();
    let rt1 = runtime::Runtime::start_with_store(
        store1.clone(),
        Arc::new(activity_registry.clone()),
        orchestration_registry.clone(),
    )
    .await;

    let instance = "inst-multi-activity-reliability";
    let client1 = duroxide::Client::new(store1.clone());
    let _ = client1
        .start_orchestration(instance, "MultiActivityTest", "")
        .await
        .unwrap();

    // Wait for all 3 activities to be scheduled
    assert!(
        common::wait_for_history(
            store1.clone(),
            instance,
            |h| h
                .iter()
                .filter(|e| matches!(e, Event::ActivityScheduled { .. }))
                .count()
                >= 3,
            2_000
        )
        .await,
        "All 3 activities should be scheduled"
    );

    // Crash before any activity completes
    let hist_before = store1.read(instance).await;
    assert_eq!(
        hist_before
            .iter()
            .filter(|e| matches!(e, Event::ActivityCompleted { .. }))
            .count(),
        0,
        "No activities should have completed yet"
    );

    println!("Crashing with 3 pending activities...");
    rt1.shutdown().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Phase 2: Restart and verify all activities complete
    println!("Restarting...");
    let store2 = store.clone();
    let rt2 =
        runtime::Runtime::start_with_store(store2.clone(), Arc::new(activity_registry), orchestration_registry).await;

    // Wait for completion
    let client2 = duroxide::Client::new(store2.clone());
    match client2
        .wait_for_orchestration(instance, std::time::Duration::from_secs(20))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            println!("✅ All activities completed after recovery");
            assert!(output.contains("All activities completed"));
        }
        runtime::OrchestrationStatus::Failed { error } => {
            panic!("Orchestration failed: {}", error);
        }
        status => {
            panic!("Unexpected status: {:?}", status);
        }
    }

    // Verify all 3 TestActivity activities completed
    let hist_after = store2.read(instance).await;
    let test_activity_completed_count = hist_after
        .iter()
        .filter(|e| {
            matches!(e, Event::ActivityCompleted { result, .. } 
                           if result.starts_with("Processed: task"))
        })
        .count();

    assert_eq!(
        test_activity_completed_count, 3,
        "All 3 TestActivity activities should have completed"
    );

    rt2.shutdown().await;

    // Restore environment
    match prev_lease {
        Some(v) => unsafe {
            std::env::set_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS", v);
        },
        None => unsafe {
            std::env::remove_var("DUROXIDE_SQLITE_LOCK_TIMEOUT_MS");
        },
    }
}
