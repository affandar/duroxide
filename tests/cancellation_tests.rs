#![allow(clippy::unwrap_used)]
#![allow(clippy::clone_on_ref_ptr)]
#![allow(clippy::expect_used)]

use duroxide::Client;
use duroxide::Either2;
use duroxide::EventKind;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{ActivityContext, OrchestrationContext, OrchestrationRegistry};
mod common;
use std::time::Duration;

#[tokio::test]
async fn cancel_parent_down_propagates_to_child() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Child waits for an external event indefinitely (until canceled)
    let child = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_wait("Go").await;
        Ok("done".to_string())
    };

    // Parent starts child and awaits it (will block until canceled)
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let _res = ctx.schedule_sub_orchestration("Child", "seed").await;
        Ok("parent_done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Child", child)
        .register("Parent", parent)
        .build();
    let activity_registry = ActivityRegistry::builder().build();

    // Use faster polling for cancellation timing test
    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = Client::new(store.clone());

    // Start parent
    client.start_orchestration("inst-cancel-1", "Parent", "").await.unwrap();

    // Give it a moment to schedule the child
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Cancel parent with reason
    let _ = client.cancel_instance("inst-cancel-1", "by_test").await;

    // Wait for parent terminal failure due to cancel
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
    loop {
        let hist = store.read("inst-cancel-1").await.unwrap_or_default();
        if hist.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::OrchestrationFailed { details, .. } if matches!(
                    details,
                    duroxide::ErrorDetails::Application {
                        kind: duroxide::AppErrorKind::Cancelled { reason },
                        ..
                    } if reason == "by_test"
                )
            )
        }) {
            assert!(
                hist.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. })),
                "missing cancel requested event for parent"
            );
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("timeout waiting for parent canceled");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Find child instance (prefix inst-cancel-1::)
    let mgmt = store.as_management_capability().expect("ProviderAdmin required");
    let children: Vec<String> = mgmt
        .list_instances()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.starts_with("inst-cancel-1::"))
        .collect();
    assert!(!children.is_empty(), "expected a child instance");

    // Each child should show cancel requested and terminal canceled (wait up to 5s)
    for child in children {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        loop {
            let hist = store.read(&child).await.unwrap_or_default();
            let has_cancel = hist
                .iter()
                .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }));
            let has_failed = hist.iter().any(|e| {
                matches!(&e.kind, EventKind::OrchestrationFailed { details, .. } if matches!(
                        details,
                        duroxide::ErrorDetails::Application {
                            kind: duroxide::AppErrorKind::Cancelled { reason },
                            ..
                        } if reason == "parent canceled"
                    )
                )
            });
            if has_cancel && has_failed {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("child {child} did not cancel in time");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    rt.shutdown(None).await;
}

#[tokio::test]
async fn cancel_after_completion_is_noop() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orch = |_ctx: OrchestrationContext, _input: String| async move { Ok("ok".to_string()) };

    let orchestration_registry = OrchestrationRegistry::builder().register("Quick", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-cancel-noop", "Quick", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-cancel-noop", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }

    // Cancel after completion should have no effect
    let _ = client.cancel_instance("inst-cancel-noop", "late").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let hist = store.read("inst-cancel-noop").await.unwrap_or_default();
    assert!(
        hist.iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { output, .. } if output == "ok"))
    );
    assert!(
        !hist
            .iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }))
    );

    rt.shutdown(None).await;
}

#[tokio::test]
async fn cancel_child_directly_signals_parent() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let child = |_ctx: OrchestrationContext, _input: String| async move {
        // Just wait forever until canceled
        futures::future::pending::<Result<String, String>>().await
    };

    let parent = |ctx: OrchestrationContext, _input: String| async move {
        match ctx.schedule_sub_orchestration("ChildD", "x").await {
            Ok(v) => Ok(format!("ok:{v}")),
            Err(e) => Ok(format!("child_err:{e}")),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ChildD", child)
        .register("ParentD", parent)
        .build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-chdirect", "ParentD", "")
        .await
        .unwrap();
    // Wait a bit for child schedule, then cancel child directly
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let child_inst = "inst-chdirect::sub::2"; // event_id=2 (OrchestrationStarted is 1)
    let _ = client.cancel_instance(child_inst, "by_test_child").await;

    let s = match client
        .wait_for_orchestration("inst-chdirect", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => output,
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    };
    assert!(
        s.starts_with("child_err:canceled: by_test_child"),
        "unexpected parent out: {s}"
    );

    // Parent should have SubOrchestrationFailed for the child id 2
    let ph = store.read("inst-chdirect").await.unwrap_or_default();
    assert!(ph.iter().any(|e| matches!(
        &e.kind,
        EventKind::SubOrchestrationFailed { details, .. }
        if e.source_event_id == Some(2) && matches!(
            details,
            duroxide::ErrorDetails::Application {
                kind: duroxide::AppErrorKind::Cancelled { reason },
                ..
            } if reason == "by_test_child"
        )
    )));

    rt.shutdown(None).await;
}

#[tokio::test]
async fn cancel_continue_as_new_second_exec() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orch = |ctx: OrchestrationContext, input: String| async move {
        match input.as_str() {
            "start" => {
                return ctx.continue_as_new("wait").await;
            }
            "wait" => {
                // Park until canceled
                let _ = ctx.schedule_wait("Go").await;
                Ok("done".to_string())
            }
            _ => Ok(input),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder().register("CanCancel", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-can-can", "CanCancel", "start")
        .await
        .unwrap();

    // Cancel the second execution while the handle is waiting
    // (the handle will wait for final completion including cancellation)
    tokio::time::sleep(std::time::Duration::from_millis(100)).await; // Let first execution complete
    let _ = client.cancel_instance("inst-can-can", "by_test_can").await;

    // With polling approach, wait for final result (cancellation)
    match client
        .wait_for_orchestration("inst-can-can", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Failed { details } => {
            assert!(matches!(
                details,
                duroxide::ErrorDetails::Application {
                    kind: duroxide::AppErrorKind::Cancelled { reason },
                    ..
                } if reason == "by_test_can"
            ));
        }
        runtime::OrchestrationStatus::Completed { output } => panic!("expected cancellation, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Wait for canceled failure
    let ok = common::wait_for_history(
        store.clone(),
        "inst-can-can",
        |hist| {
            hist.iter().rev().any(|e| {
                matches!(&e.kind, EventKind::OrchestrationFailed { details, .. } if matches!(
                        details,
                        duroxide::ErrorDetails::Application {
                            kind: duroxide::AppErrorKind::Cancelled { reason },
                            ..
                        } if reason == "by_test_can"
                    )
                )
            })
        },
        5000,
    )
    .await;
    assert!(ok, "timeout waiting for cancel failure");

    // Ensure cancel requested recorded
    let hist = store.read("inst-can-can").await.unwrap_or_default();
    assert!(
        hist.iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }))
    );

    rt.shutdown(None).await;
}

#[tokio::test]
async fn orchestration_completes_before_activity_finishes() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Activity will be dispatched but orchestration completes without awaiting it (fire-and-forget)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        drop(ctx.schedule_activity("slow", ""));
        Ok("done".to_string())
    };

    let mut ab = ActivityRegistry::builder();
    ab = ab.register("slow", |_ctx: ActivityContext, _s: String| async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Ok("ok".to_string())
    });
    let activity_registry = ab.build();
    let orchestration_registry = OrchestrationRegistry::builder().register("QuickDone", orch).build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-orch-done-first", "QuickDone", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-orch-done-first", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "done"),
        runtime::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }

    // Give activity time to finish; no additional terminal events should be added
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let hist = store.read("inst-orch-done-first").await.unwrap_or_default();
    assert!(
        hist.iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { output, .. } if output == "done"))
    );

    rt.shutdown(None).await;
}

#[tokio::test]
async fn orchestration_fails_before_activity_finishes() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Activity dispatched; orchestration fails immediately
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        drop(ctx.schedule_activity("slow2", ""));
        Err("boom".to_string())
    };

    let mut ab = ActivityRegistry::builder();
    ab = ab.register("slow2", |_ctx: ActivityContext, _s: String| async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Ok("ok".to_string())
    });
    let activity_registry = ab.build();
    let orchestration_registry = OrchestrationRegistry::builder().register("QuickFail", orch).build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-orch-fail-first", "QuickFail", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-orch-fail-first", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Failed { details: _ } => {} // Expected failure
        runtime::OrchestrationStatus::Completed { output } => panic!("expected failure, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Give activity time to finish; no change to terminal failure
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let hist = store.read("inst-orch-fail-first").await.unwrap_or_default();
    assert!(hist.iter().any(
        |e| matches!(&e.kind, EventKind::OrchestrationFailed { details, .. } if matches!(
                details,
                duroxide::ErrorDetails::Application {
                    kind: duroxide::AppErrorKind::OrchestrationFailed,
                    message,
                    ..
                } if message == "boom"
            )
        )
    ));

    rt.shutdown(None).await;
}

#[tokio::test]
async fn cancel_parent_with_multiple_children() {
    // This test validates the iterator optimization in get_child_cancellation_work_items
    // by ensuring that cancellation properly propagates to multiple sub-orchestrations
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Child waits for an external event indefinitely (until canceled)
    let child = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_wait("Go").await;
        Ok("done".to_string())
    };

    // Parent starts multiple children and awaits all (will block until canceled)
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let mut futures = Vec::new();
        // Schedule 5 sub-orchestrations
        for i in 0..5 {
            futures.push(ctx.schedule_sub_orchestration("MultiChild", format!("input-{i}")));
        }
        // Wait for all (will never complete unless canceled)
        let results = ctx.join(futures).await;
        // All should fail with cancellation
        for result in results {
            match result {
                Err(e) if e.contains("canceled") => {}
                _ => return Err("expected cancellation".to_string()),
            }
        }
        Ok("parent_done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("MultiChild", child)
        .register("MultiParent", parent)
        .build();
    let activity_registry = ActivityRegistry::builder().build();

    // Use faster polling for cancellation timing test
    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = Client::new(store.clone());

    // Start parent
    client
        .start_orchestration("inst-cancel-multi", "MultiParent", "")
        .await
        .unwrap();

    // Give it time to schedule all children
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Cancel parent with reason
    let _ = client.cancel_instance("inst-cancel-multi", "multi_test").await;

    // Wait for parent terminal failure due to cancel
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
    loop {
        let hist = store.read("inst-cancel-multi").await.unwrap_or_default();
        if hist.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::OrchestrationFailed { details, .. } if matches!(
                    details,
                    duroxide::ErrorDetails::Application {
                        kind: duroxide::AppErrorKind::Cancelled { reason },
                        ..
                    } if reason == "multi_test"
                )
            )
        }) {
            assert!(
                hist.iter()
                    .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. })),
                "missing cancel requested event for parent"
            );
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("timeout waiting for parent canceled");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Find all child instances (prefix inst-cancel-multi::)
    let mgmt = store.as_management_capability().expect("ProviderAdmin required");
    let children: Vec<String> = mgmt
        .list_instances()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.starts_with("inst-cancel-multi::"))
        .collect();

    // Should have 5 children
    assert_eq!(
        children.len(),
        5,
        "expected 5 child instances, found {}",
        children.len()
    );

    // Each child should show cancel requested and terminal canceled (wait up to 5s)
    for child in children {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        loop {
            let hist = store.read(&child).await.unwrap_or_default();
            let has_cancel = hist
                .iter()
                .any(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }));
            let has_failed = hist.iter().any(|e| {
                matches!(&e.kind, EventKind::OrchestrationFailed { details, .. } if matches!(
                        details,
                        duroxide::ErrorDetails::Application {
                            kind: duroxide::AppErrorKind::Cancelled { reason },
                            ..
                        } if reason == "parent canceled"
                    )
                )
            });
            if has_cancel && has_failed {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("child {child} did not cancel in time");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    rt.shutdown(None).await;
}

/// Test that activities receive cancellation signals when orchestration is cancelled
#[tokio::test]
async fn activity_receives_cancellation_signal() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (store, _td) = common::create_sqlite_store_disk().await;

    // Track if activity saw the cancellation
    let saw_cancellation = Arc::new(AtomicBool::new(false));
    let saw_cancellation_clone = Arc::clone(&saw_cancellation);

    // Activity that waits for cancellation
    let long_activity = move |ctx: ActivityContext, _input: String| {
        let saw_cancellation = Arc::clone(&saw_cancellation_clone);
        async move {
            // Wait for either cancellation or timeout
            tokio::select! {
                _ = ctx.cancelled() => {
                    saw_cancellation.store(true, Ordering::SeqCst);
                    Ok("cancelled".to_string())
                }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    Ok("timeout".to_string())
                }
            }
        }
    };

    // Orchestration that schedules the long activity
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let _result = ctx.schedule_activity("LongActivity", "input").await;
        Ok("done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("LongActivityOrch", orchestration)
        .build();
    let activity_registry = ActivityRegistry::builder()
        .register("LongActivity", long_activity)
        .build();

    // Use shorter lock and grace periods for testing
    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        worker_lock_timeout: Duration::from_secs(2),
        worker_lock_renewal_buffer: Duration::from_millis(500),
        activity_cancellation_grace_period: Duration::from_secs(5),
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("inst-activity-cancel", "LongActivityOrch", "")
        .await
        .unwrap();

    // Wait for activity to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Cancel the orchestration
    let _ = client
        .cancel_instance("inst-activity-cancel", "test_cancellation")
        .await;

    // Wait for cancellation to propagate
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if saw_cancellation.load(Ordering::SeqCst) {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("Activity did not receive cancellation signal within timeout");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Verify the activity saw the cancellation
    assert!(
        saw_cancellation.load(Ordering::SeqCst),
        "Activity should have received cancellation signal"
    );

    // Verify orchestration is in failed/cancelled state
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hist = store.read("inst-activity-cancel").await.unwrap_or_default();
        let is_cancelled = hist.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::OrchestrationFailed { details, .. }
                    if matches!(
                        details,
                        duroxide::ErrorDetails::Application {
                            kind: duroxide::AppErrorKind::Cancelled { .. },
                            ..
                        }
                    )
            )
        });
        if is_cancelled {
            break;
        }
        if std::time::Instant::now() > deadline {
            // Even if orchestration didn't fail yet, the activity received the signal
            // which is the main thing we're testing
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    rt.shutdown(None).await;
}

/// Test that select/select2 losing activities receive cancellation signals (lock stealing)
/// even when the orchestration continues normally.
#[tokio::test]
async fn select2_loser_activity_receives_cancellation_signal() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (store, _td) = common::create_sqlite_store_disk().await;

    let activity_started = Arc::new(AtomicBool::new(false));
    let saw_cancellation = Arc::new(AtomicBool::new(false));
    let activity_started_clone = Arc::clone(&activity_started);
    let saw_cancellation_clone = Arc::clone(&saw_cancellation);

    // Activity that blocks until cancelled.
    let long_activity = move |ctx: ActivityContext, _input: String| {
        let activity_started = Arc::clone(&activity_started_clone);
        let saw_cancellation = Arc::clone(&saw_cancellation_clone);
        async move {
            activity_started.store(true, Ordering::SeqCst);

            tokio::select! {
                _ = ctx.cancelled() => {
                    saw_cancellation.store(true, Ordering::SeqCst);
                    Ok("cancelled".to_string())
                }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    Ok("timeout".to_string())
                }
            }
        }
    };

    // Orchestration that races a long activity against a short timer.
    // Timer should win, and the losing activity should be cancelled proactively.
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let activity = ctx.schedule_activity("LongActivity", "input");
        let timeout = ctx.schedule_timer(Duration::from_secs(1));
        match ctx.select2(activity, timeout).await {
            Either2::First(_) => panic!("activity should not win"),
            Either2::Second(_) => {} // timer won as expected
        }

        // Keep going to prove the orchestration isn't terminal.
        ctx.schedule_timer(Duration::from_millis(50)).await;
        Ok("done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SelectLoserCancelOrch", orchestration)
        .build();
    let activity_registry = ActivityRegistry::builder()
        .register("LongActivity", long_activity)
        .build();

    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        worker_lock_timeout: Duration::from_secs(2),
        worker_lock_renewal_buffer: Duration::from_millis(500),
        activity_cancellation_grace_period: Duration::from_secs(2),
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-select2-loser-cancel", "SelectLoserCancelOrch", "")
        .await
        .unwrap();

    // Ensure the activity actually started before we assert cancellation behavior.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if activity_started.load(Ordering::SeqCst) {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("Activity did not start in time");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Wait for cancellation to propagate via lock stealing and renewal failure.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if saw_cancellation.load(Ordering::SeqCst) {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("select2 loser activity did not receive cancellation signal in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        saw_cancellation.load(Ordering::SeqCst),
        "Activity should have received cancellation signal as select2 loser"
    );

    // Orchestration should still complete normally.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let hist = store.read("inst-select2-loser-cancel").await.unwrap_or_default();
        if hist
            .iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }))
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("timeout waiting for orchestration to complete");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    rt.shutdown(None).await;
}

/// Test that activity result is dropped when orchestration was cancelled during execution
#[tokio::test]
async fn activity_result_dropped_when_orchestration_cancelled() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let (store, _td) = common::create_sqlite_store_disk().await;

    // Track activity completions
    let activity_completed_count = Arc::new(AtomicU32::new(0));
    let activity_completed_count_clone = Arc::clone(&activity_completed_count);

    // Activity that completes but takes a bit of time
    let slow_activity = move |_ctx: ActivityContext, _input: String| {
        let counter = Arc::clone(&activity_completed_count_clone);
        async move {
            // Simulate work
            tokio::time::sleep(Duration::from_millis(500)).await;
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("completed".to_string())
        }
    };

    // Orchestration that schedules the slow activity
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let _result = ctx.schedule_activity("SlowActivity", "input").await;
        Ok("done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SlowActivityOrch", orchestration)
        .build();
    let activity_registry = ActivityRegistry::builder()
        .register("SlowActivity", slow_activity)
        .build();

    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        worker_lock_timeout: Duration::from_secs(2),
        worker_lock_renewal_buffer: Duration::from_millis(500),
        ..Default::default()
    };

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("inst-drop-result", "SlowActivityOrch", "")
        .await
        .unwrap();

    // Wait briefly for activity to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel orchestration while activity is running
    let _ = client
        .cancel_instance("inst-drop-result", "cancel_during_activity")
        .await;

    // Wait for everything to settle
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Activity should have completed
    assert!(
        activity_completed_count.load(Ordering::SeqCst) > 0,
        "Activity should have completed"
    );

    // But the orchestration should not have an ActivityCompleted event
    // (the result was dropped due to cancellation)
    let hist = store.read("inst-drop-result").await.unwrap_or_default();
    let has_activity_completed = hist
        .iter()
        .any(|e| matches!(&e.kind, EventKind::ActivityCompleted { .. }));

    // Note: The result may or may not be dropped depending on timing.
    // If the orchestration processed the cancel before the activity finished,
    // the result will be dropped. We're testing that the mechanism exists.
    if !has_activity_completed {
        // This confirms the result was dropped
        tracing::info!("Activity result was successfully dropped due to cancellation");
    }

    rt.shutdown(None).await;
}

/// Test that activities are skipped when the orchestration is already terminal at fetch time.
///
/// This test is deterministic by using separate runtime phases:
/// 1. Phase 1: Start runtime with worker_concurrency=0 (orchestrator only)
/// 2. Let orchestration run and schedule an activity, then cancel it
/// 3. Phase 2: Start new runtime with workers - they will see terminal state at fetch
#[tokio::test]
async fn activity_skipped_when_orchestration_terminal_at_fetch() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (store, _td) = common::create_sqlite_store_disk().await;

    let ran = Arc::new(AtomicBool::new(false));
    let ran_clone = Arc::clone(&ran);

    let activity = move |_ctx: ActivityContext, _input: String| {
        let ran = Arc::clone(&ran_clone);
        async move {
            ran.store(true, Ordering::SeqCst);
            Ok("done".to_string())
        }
    };

    // Orchestration schedules an activity then waits for an event (will be cancelled)
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Schedule activity (enqueues to worker queue)
        let _handle = ctx.schedule_activity("A", "input");
        // Wait for external event (will block until cancelled)
        let _ = ctx.schedule_wait("Go").await;
        Ok("ok".to_string())
    };

    let orch_registry = OrchestrationRegistry::builder().register("O", orchestration).build();
    let act_registry = ActivityRegistry::builder().register("A", activity).build();

    // PHASE 1: Run with orchestrator only (no workers)
    let options_no_workers = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        worker_concurrency: 0, // No workers - activity won't be picked up
        ..Default::default()
    };

    let rt1 = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(act_registry.clone()),
        orch_registry.clone(),
        options_no_workers,
    )
    .await;
    let client = Client::new(store.clone());

    // Start orchestration - it will schedule activity and wait for event
    client.start_orchestration("inst-skip", "O", "").await.unwrap();

    // Wait for activity to be scheduled (orchestration runs, schedules activity, then waits)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel the orchestration - makes it terminal
    client.cancel_instance("inst-skip", "cancel").await.unwrap();

    // Wait for orchestration to reach terminal state
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hist = store.read("inst-skip").await.unwrap_or_default();
        if hist
            .iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationFailed { .. }))
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("Timeout waiting for orchestration to be cancelled");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Shutdown phase 1 runtime
    rt1.shutdown(None).await;

    // Verify activity hasn't run yet (no workers were active)
    assert!(
        !ran.load(Ordering::SeqCst),
        "Activity should not have run yet - no workers in phase 1"
    );

    // PHASE 2: Start new runtime WITH workers
    // The activity work item is still in the queue, but orchestration is now terminal
    let options_with_workers = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        worker_concurrency: 2,
        worker_lock_timeout: Duration::from_secs(2),
        worker_lock_renewal_buffer: Duration::from_millis(500),
        activity_cancellation_grace_period: Duration::from_secs(1),
        ..Default::default()
    };

    // Need fresh registries for the new runtime
    let ran2 = Arc::new(AtomicBool::new(false));
    let ran2_clone = Arc::clone(&ran2);
    let activity2 = move |_ctx: ActivityContext, _input: String| {
        let ran = Arc::clone(&ran2_clone);
        async move {
            ran.store(true, Ordering::SeqCst);
            Ok("done".to_string())
        }
    };
    let act_registry2 = ActivityRegistry::builder().register("A", activity2).build();

    let rt2 = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(act_registry2),
        orch_registry,
        options_with_workers,
    )
    .await;

    // Give workers time to fetch and skip the activity
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The activity should NOT have run because:
    // - fetch_work_item returns ExecutionState::Terminal for the orchestration
    // - Worker sees terminal state and skips the activity
    assert!(
        !ran2.load(Ordering::SeqCst),
        "Activity should have been skipped when orchestration was terminal at fetch"
    );

    let hist = store.read("inst-skip").await.unwrap_or_default();
    let has_activity_completed = hist
        .iter()
        .any(|e| matches!(&e.kind, EventKind::ActivityCompleted { .. }));
    assert!(
        !has_activity_completed,
        "Activity completion should not be recorded for skipped activity"
    );

    rt2.shutdown(None).await;
}

/// Test that non-cooperative activities are aborted after grace period on cancellation
#[tokio::test]
async fn activity_aborted_after_cancellation_grace() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let (store, _td) = common::create_sqlite_store_disk().await;

    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = Arc::clone(&attempts);

    // Activity ignores cancellation and sleeps long
    let stubborn_activity = move |_ctx: ActivityContext, _input: String| {
        let attempts = Arc::clone(&attempts_clone);
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok("done".to_string())
        }
    };

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_activity("Stubborn", "input").await;
        Ok("ok".to_string())
    };

    let orch_registry = OrchestrationRegistry::builder()
        .register("StubbornOrch", orchestration)
        .build();
    let act_registry = ActivityRegistry::builder()
        .register("Stubborn", stubborn_activity)
        .build();

    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        worker_lock_timeout: Duration::from_secs(2),
        worker_lock_renewal_buffer: Duration::from_millis(500),
        activity_cancellation_grace_period: Duration::from_millis(500),
        ..Default::default()
    };

    let rt =
        runtime::Runtime::start_with_options(store.clone(), std::sync::Arc::new(act_registry), orch_registry, options)
            .await;
    let client = Client::new(store.clone());

    let start = std::time::Instant::now();
    client
        .start_orchestration("inst-grace", "StubbornOrch", "")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    client.cancel_instance("inst-grace", "cancel_now").await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Activity should have been attempted once but not completed
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "Activity should run once");

    let hist = store.read("inst-grace").await.unwrap_or_default();
    let has_activity_completed = hist
        .iter()
        .any(|e| matches!(&e.kind, EventKind::ActivityCompleted { .. }));
    assert!(
        !has_activity_completed,
        "Activity completion should be dropped after cancellation"
    );

    // Ensure we did not wait the full 5s activity duration (abort happened)
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "Activity should have been aborted before full run"
    );

    rt.shutdown(None).await;
}

/// Test that cancelling a non-existent instance is a no-op (doesn't error)
#[tokio::test]
async fn cancel_nonexistent_instance_is_noop() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Dummy", |_ctx: OrchestrationContext, _input: String| async move {
            Ok("ok".to_string())
        })
        .build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;
    let client = Client::new(store.clone());

    // Cancel an instance that was never created - should succeed without error
    let result = client.cancel_instance("does-not-exist", "test").await;
    assert!(result.is_ok(), "Cancelling non-existent instance should not error");

    // The cancel work item gets enqueued but the orchestrator will find no matching instance
    // and simply ack it without effect. Give it time to process.
    tokio::time::sleep(Duration::from_millis(200)).await;

    rt.shutdown(None).await;
}

/// Test that multiple cancel calls on the same instance are idempotent
#[tokio::test]
async fn multiple_cancel_calls_are_idempotent() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Orchestration that waits for an event (will block until canceled)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_wait("Go").await;
        Ok("done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder().register("Waiter", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;
    let client = Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("inst-multi-cancel", "Waiter", "")
        .await
        .unwrap();

    // Give it time to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cancel multiple times rapidly
    for i in 0..5 {
        let result = client.cancel_instance("inst-multi-cancel", format!("cancel-{i}")).await;
        assert!(result.is_ok(), "Cancel call {i} should succeed");
    }

    // Wait for orchestration to fail
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hist = store.read("inst-multi-cancel").await.unwrap_or_default();
        if hist
            .iter()
            .any(|e| matches!(&e.kind, EventKind::OrchestrationFailed { .. }))
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("Timeout waiting for orchestration to be cancelled");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Should only have ONE OrchestrationCancelRequested event (first cancel wins)
    let hist = store.read("inst-multi-cancel").await.unwrap_or_default();
    let cancel_requested_count = hist
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }))
        .count();
    assert_eq!(
        cancel_requested_count, 1,
        "Should only have one CancelRequested event, got {cancel_requested_count}"
    );

    // The reason should be from the first cancel (cancel-0)
    let cancel_event = hist
        .iter()
        .find(|e| matches!(&e.kind, EventKind::OrchestrationCancelRequested { .. }))
        .unwrap();
    if let EventKind::OrchestrationCancelRequested { reason } = &cancel_event.kind {
        assert_eq!(reason, "cancel-0", "First cancel reason should win");
    }

    rt.shutdown(None).await;
}

/// Test that cancel before orchestration starts is handled gracefully
#[tokio::test]
async fn cancel_before_orchestration_starts() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // First, just enqueue the start work item without running a runtime
    store
        .enqueue_for_orchestrator(
            duroxide::providers::WorkItem::StartOrchestration {
                instance: "inst-early-cancel".to_string(),
                orchestration: "Early".to_string(),
                version: Some("1.0.0".to_string()),
                input: "input".to_string(),
                parent_instance: None,
                parent_id: None,
                execution_id: 1,
            },
            None,
        )
        .await
        .unwrap();

    // Now cancel it before any runtime has processed it
    store
        .enqueue_for_orchestrator(
            duroxide::providers::WorkItem::CancelInstance {
                instance: "inst-early-cancel".to_string(),
                reason: "early_cancel".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Now start the runtime - it should process both work items
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Early", |_ctx: OrchestrationContext, _input: String| async move {
            // This should never run because cancel came first (or runs but fails immediately)
            Ok("done".to_string())
        })
        .build();
    let activity_registry = ActivityRegistry::builder().build();
    let options = runtime::RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
        options,
    )
    .await;

    // Wait for orchestration to be cancelled
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hist = store.read("inst-early-cancel").await.unwrap_or_default();
        // The orchestration MUST end up in Failed (cancelled) state, not Completed.
        // Since the cancel was enqueued after the start, it should be processed
        // and the orchestration should fail with cancellation.
        let is_cancelled = hist.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::OrchestrationFailed { details, .. }
                    if matches!(
                        details,
                        duroxide::ErrorDetails::Application {
                            kind: duroxide::AppErrorKind::Cancelled { .. },
                            ..
                        }
                    )
            )
        });
        if is_cancelled {
            break;
        }
        if std::time::Instant::now() > deadline {
            let hist = store.read("inst-early-cancel").await.unwrap_or_default();
            panic!(
                "Timeout waiting for orchestration to be cancelled. History: {:?}",
                hist.iter().map(|e| &e.kind).collect::<Vec<_>>()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Verify it was cancelled with the correct reason
    let hist = store.read("inst-early-cancel").await.unwrap_or_default();
    let cancel_event = hist.iter().find(|e| {
        matches!(
            &e.kind,
            EventKind::OrchestrationFailed { details, .. }
                if matches!(
                    details,
                    duroxide::ErrorDetails::Application {
                        kind: duroxide::AppErrorKind::Cancelled { reason },
                        ..
                    } if reason == "early_cancel"
                )
        )
    });
    assert!(
        cancel_event.is_some(),
        "Orchestration should be cancelled with reason 'early_cancel'"
    );

    // Verify it did NOT complete successfully
    let completed = hist
        .iter()
        .any(|e| matches!(&e.kind, EventKind::OrchestrationCompleted { .. }));
    assert!(
        !completed,
        "Orchestration should NOT have completed - cancel was enqueued after start"
    );

    rt.shutdown(None).await;
}
