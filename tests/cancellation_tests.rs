use duroxide::Client;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Event, OrchestrationContext, OrchestrationRegistry};
mod common;

#[tokio::test]
async fn cancel_parent_down_propagates_to_child() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Child waits for an external event indefinitely (until canceled)
    let child = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_wait("Go").into_event().await;
        Ok("done".to_string())
    };

    // Parent starts child and awaits it (will block until canceled)
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let _res = ctx
            .schedule_sub_orchestration("Child", "seed")
            .into_sub_orchestration()
            .await;
        Ok("parent_done".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Child", child)
        .register("Parent", parent)
        .build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
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
        let hist = store.read("inst-cancel-1").await;
        if hist
            .iter()
            .any(|e| matches!(e, Event::OrchestrationFailed { error } if error.starts_with("canceled: by_test")))
        {
            assert!(
                hist.iter()
                    .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. })),
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
    let children: Vec<String> = store
        .list_instances()
        .await
        .into_iter()
        .filter(|i| i.starts_with("inst-cancel-1::"))
        .collect();
    assert!(!children.is_empty(), "expected a child instance");

    // Each child should show cancel requested and terminal canceled (wait up to 5s)
    for child in children {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        loop {
            let hist = store.read(&child).await;
            let has_cancel = hist
                .iter()
                .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. }));
            let has_failed = hist.iter().any(
                |e| matches!(e, Event::OrchestrationFailed { error } if error.starts_with("canceled: parent canceled")),
            );
            if has_cancel && has_failed {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("child {} did not cancel in time", child);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    rt.shutdown().await;
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
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Cancel after completion should have no effect
    let _ = client.cancel_instance("inst-cancel-noop", "late").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let hist = store.read("inst-cancel-noop").await;
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "ok"))
    );
    assert!(
        !hist
            .iter()
            .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. }))
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn cancel_child_directly_signals_parent() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let child = |_ctx: OrchestrationContext, _input: String| async move {
        // Just wait forever until canceled
        futures::future::pending::<Result<String, String>>().await
    };

    let parent = |ctx: OrchestrationContext, _input: String| async move {
        match ctx
            .schedule_sub_orchestration("ChildD", "x")
            .into_sub_orchestration()
            .await
        {
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
    let child_inst = "inst-chdirect::sub::1";
    let _ = client.cancel_instance(child_inst, "by_test_child").await;

    let s = match client
        .wait_for_orchestration("inst-chdirect", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => output,
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert!(
        s.starts_with("child_err:canceled: by_test_child"),
        "unexpected parent out: {s}"
    );

    // Parent should have SubOrchestrationFailed for the child id 1
    let ph = store.read("inst-chdirect").await;
    assert!(ph.iter().any(|e| matches!(e, Event::SubOrchestrationFailed { id, error } if *id == 1 && error.starts_with("canceled: by_test_child"))));

    rt.shutdown().await;
}

#[tokio::test]
async fn cancel_continue_as_new_second_exec() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    let orch = |ctx: OrchestrationContext, input: String| async move {
        match input.as_str() {
            "start" => {
                ctx.continue_as_new("wait");
                Ok(String::new())
            }
            "wait" => {
                // Park until canceled
                let _ = ctx.schedule_wait("Go").into_event().await;
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
        runtime::OrchestrationStatus::Failed { error } => {
            assert!(error.starts_with("canceled: by_test_can"));
        }
        runtime::OrchestrationStatus::Completed { output } => panic!("expected cancellation, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Wait for canceled failure
    let ok = common::wait_for_history(
        store.clone(),
        "inst-can-can",
        |hist| {
            hist.iter().rev().any(
                |e| matches!(e, Event::OrchestrationFailed { error } if error.starts_with("canceled: by_test_can")),
            )
        },
        5000,
    )
    .await;
    assert!(ok, "timeout waiting for cancel failure");

    // Ensure cancel requested recorded
    let hist = store.read("inst-can-can").await;
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::OrchestrationCancelRequested { .. }))
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn orchestration_completes_before_activity_finishes() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Activity will be dispatched but orchestration completes without awaiting it (fire-and-forget)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_activity("slow", "");
        Ok("done".to_string())
    };

    let mut ab = ActivityRegistry::builder();
    ab = ab.register("slow", |_s: String| async move {
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
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Give activity time to finish; no additional terminal events should be added
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let hist = store.read("inst-orch-done-first").await;
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "done"))
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn orchestration_fails_before_activity_finishes() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // Activity dispatched; orchestration fails immediately
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let _ = ctx.schedule_activity("slow2", "");
        Err("boom".to_string())
    };

    let mut ab = ActivityRegistry::builder();
    ab = ab.register("slow2", |_s: String| async move {
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
        runtime::OrchestrationStatus::Failed { error: _ } => {} // Expected failure
        runtime::OrchestrationStatus::Completed { output } => panic!("expected failure, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Give activity time to finish; no change to terminal failure
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let hist = store.read("inst-orch-fail-first").await;
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::OrchestrationFailed { error } if error == "boom"))
    );

    rt.shutdown().await;
}
