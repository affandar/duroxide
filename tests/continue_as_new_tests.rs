use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
mod common;

// Basic ContinueAsNew loop: rolls input across executions and finally completes.
#[tokio::test]
async fn continue_as_new_multiexec_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Orchestrator: if n < 2 then ContinueAsNew with n+1, else complete
    let counter = |ctx: OrchestrationContext, input: String| async move {
        let n: u32 = input.parse().unwrap_or(0);
        if n < 2 {
            ctx.trace_info(format!("counter exec n={n} -> continue as new"));
            ctx.continue_as_new((n + 1).to_string());
            Ok(String::new())
        } else {
            ctx.trace_info(format!("counter exec n={n} -> complete"));
            Ok(format!("done:{n}"))
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder().register("Counter", counter).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // The initial start handle will resolve when the first execution continues-as-new.
    let _h = rt
        .clone()
        .start_orchestration("inst-can-1", "Counter", "0")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-can-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "done:2"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Verify final completion is also in history
    let ok = common::wait_for_history(
        store.clone(),
        "inst-can-1",
        |hist| {
            hist.iter()
                .rev()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "done:2"))
        },
        1_000,
    )
    .await;
    assert!(ok, "timeout waiting for completion");

    // Verify multi-execution histories exist: execs 1,2 continued-as-new; exec 3 completed
    let execs = store.list_executions("inst-can-1").await;
    assert_eq!(execs, vec![1, 2, 3]);

    // read() must reflect the latest execution's history
    let latest = *execs.last().unwrap();
    let latest_hist = store.read_with_execution("inst-can-1", latest).await;
    let current_hist = store.read("inst-can-1").await;
    assert_eq!(current_hist, latest_hist);

    let e1 = store.read_with_execution("inst-can-1", 1).await;
    assert!(
        e1.iter()
            .any(|e| matches!(e, Event::OrchestrationStarted { input, .. } if input == "0"))
    );
    assert!(
        e1.iter()
            .any(|e| matches!(e, Event::OrchestrationContinuedAsNew { input } if input == "1"))
    );
    assert!(!e1.iter().any(|e| matches!(e, Event::OrchestrationCompleted { .. })));

    let e2 = store.read_with_execution("inst-can-1", 2).await;
    assert!(
        e2.iter()
            .any(|e| matches!(e, Event::OrchestrationStarted { input, .. } if input == "1"))
    );
    assert!(
        e2.iter()
            .any(|e| matches!(e, Event::OrchestrationContinuedAsNew { input } if input == "2"))
    );
    assert!(!e2.iter().any(|e| matches!(e, Event::OrchestrationCompleted { .. })));

    let e3 = store.read_with_execution("inst-can-1", 3).await;
    assert!(
        e3.iter()
            .any(|e| matches!(e, Event::OrchestrationStarted { input, .. } if input == "2"))
    );
    assert!(
        e3.iter()
            .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "done:2"))
    );

    rt.shutdown().await;
}

// External events are dispatched to the most recent execution.
#[tokio::test]
async fn continue_as_new_event_routes_to_latest_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Orchestrator: first execution continues immediately; second waits for "Go" then completes with payload
    let orch = |ctx: OrchestrationContext, input: String| async move {
        match input.as_str() {
            "start" => {
                ctx.trace_info("first exec -> continue".to_string());
                ctx.continue_as_new("wait");
                Ok(String::new())
            }
            "wait" => {
                ctx.trace_info("second exec -> subscribe and wait".to_string());
                let v = ctx.schedule_wait("Go").into_event().await;
                Ok(v)
            }
            _ => Ok(input),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder().register("EvtCAN", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // Raise event after the second execution subscribes
    let store_for_wait = store.clone();
    let rt_c = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-can-evt", "Go", 2_000).await;
        rt_c.raise_event("inst-can-evt", "Go", "ok").await;
    });

    let _h = rt
        .clone()
        .start_orchestration("inst-can-evt", "EvtCAN", "start")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-can-evt", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Verify final completion is also in history
    let ok2 = common::wait_for_history(
        store.clone(),
        "inst-can-evt",
        |hist| {
            hist.iter()
                .rev()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "ok"))
        },
        1_000,
    )
    .await;
    assert!(ok2, "timeout waiting for completion");

    // Exec1 should not contain ExternalEvent; Exec2 should
    let e1 = store.read_with_execution("inst-can-evt", 1).await;
    assert!(
        e1.iter()
            .any(|e| matches!(e, Event::OrchestrationContinuedAsNew { input } if input == "wait"))
    );
    assert!(
        !e1.iter()
            .any(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "Go"))
    );

    let e2 = store.read_with_execution("inst-can-evt", 2).await;
    assert!(
        e2.iter()
            .any(|e| matches!(e, Event::ExternalSubscribed { name, .. } if name == "Go"))
    );
    assert!(
        e2.iter()
            .any(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "Go"))
    );

    // read() must reflect the latest execution's history
    let execs = store.list_executions("inst-can-evt").await;
    let latest = *execs.last().unwrap();
    let latest_hist = store.read_with_execution("inst-can-evt", latest).await;
    let current_hist = store.read("inst-can-evt").await;
    assert_eq!(current_hist, latest_hist);

    rt.shutdown().await;
}

// External events sent before the new execution's subscription are dropped; after subscribing, events are delivered.
#[tokio::test]
async fn continue_as_new_event_drop_then_process_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Orchestrator: first execution continues; second waits for Go twice (second send expected to deliver)
    let orch = |ctx: OrchestrationContext, input: String| async move {
        match input.as_str() {
            "start" => {
                ctx.trace_info("first exec -> continue".to_string());
                ctx.continue_as_new("wait");
                Ok(String::new())
            }
            "wait" => {
                ctx.trace_info("second exec -> subscribe and wait".to_string());
                let v = ctx.schedule_wait("Go").into_event().await;
                Ok(v)
            }
            _ => Ok(input),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("EvtDropThenProcess", orch)
        .build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // Start orchestrator
    let rt_c1 = rt.clone();
    tokio::spawn(async move {
        // Intentionally send too early to new execution (before subscription)
        // We wait a bit to ensure CAN happens but before subscription is recorded.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        rt_c1.raise_event("inst-can-evt-drop", "Go", "early").await;
    });

    // After subscription exists, send again
    let store_for_wait = store.clone();
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-can-evt-drop", "Go", 2_000).await;
        rt_c2.raise_event("inst-can-evt-drop", "Go", "late").await;
    });

    let _h = rt
        .clone()
        .start_orchestration("inst-can-evt-drop", "EvtDropThenProcess", "start")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-can-evt-drop", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "late"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Verify final completion is also in history
    let ok = common::wait_for_history(
        store.clone(),
        "inst-can-evt-drop",
        |hist| {
            hist.iter()
                .rev()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "late"))
        },
        1_000,
    )
    .await;
    assert!(ok, "timeout waiting for completion");

    // Exec2 should have ExternalSubscribed and ExternalEvent for Go; payload should be 'late'
    let e2 = store.read_with_execution("inst-can-evt-drop", 2).await;
    assert!(
        e2.iter()
            .any(|e| matches!(e, Event::ExternalSubscribed { name, .. } if name == "Go"))
    );
    assert!(
        e2.iter()
            .any(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "Go"))
    );

    // Exec1 must not have ExternalEvent
    let e1 = store.read_with_execution("inst-can-evt-drop", 1).await;
    assert!(
        !e1.iter()
            .any(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "Go"))
    );

    rt.shutdown().await;
}

// An event raised while active but before any subscription exists is warned and dropped; re-raising after subscription succeeds.
#[tokio::test]
async fn event_drop_then_retry_after_subscribe_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.trace_info("subscribe after a short delay".to_string());
        // Introduce a small timer before subscribing to simulate early event arrival
        ctx.schedule_timer(100).into_timer().await;
        let v = ctx.schedule_wait("Data").into_event().await;
        Ok(v)
    };

    let orchestration_registry = OrchestrationRegistry::builder().register("EvtDropRetry", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        std::sync::Arc::new(activity_registry),
        orchestration_registry,
    )
    .await;

    // Send early event before subscription is recorded (instance will be active due to timer)
    let rt_c1 = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        rt_c1.raise_event("inst-drop-retry", "Data", "early").await;
    });

    // Send after subscription
    let store_for_wait = store.clone();
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-drop-retry", "Data", 5_000).await;
        rt_c2.raise_event("inst-drop-retry", "Data", "ok").await;
    });

    let _h = rt
        .clone()
        .start_orchestration("inst-drop-retry", "EvtDropRetry", "x")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-drop-retry", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Ensure only one ExternalEvent recorded, post-subscription
    let e = store.read("inst-drop-retry").await;
    let events: Vec<&Event> = e
        .iter()
        .filter(|ev| matches!(ev, Event::ExternalEvent { name, .. } if name == "Data"))
        .collect();
    assert_eq!(events.len(), 1);

    rt.shutdown().await;
}
// Test: Execution ID validation for ContinueAsNew scenarios
// This test verifies that completions from old executions are properly ignored

// merged section imports are already present at the top of this file
use rust_dtf::providers::WorkItem;

#[tokio::test]
async fn old_execution_completions_are_ignored() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("TestActivity", |_input: String| async move {
            // Activity that completes quickly
            Ok("activity_result".to_string())
        })
        .build();

    // Orchestration that waits for external events (stays active)
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Wait for an external event to keep the orchestration active
        let _result = ctx.schedule_wait("continue_signal").into_event().await;
        Ok("orchestration_complete".to_string())
    };

    let reg = OrchestrationRegistry::builder()
        .register("ExecutionIdTest", orch)
        .build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _handle = rt
        .clone()
        .start_orchestration("inst-exec-test", "ExecutionIdTest", "")
        .await
        .unwrap();

    // Wait for orchestration to start and be waiting for external event
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Inject a completion with OLD execution ID (execution_id=0, but current should be 1)
    let _ = store
        .enqueue_orchestrator_work(

            WorkItem::ActivityCompleted {
                instance: "inst-exec-test".to_string(),
                execution_id: 0, // Old execution ID (current is 1)
                id: 999,         // Some activity ID
                result: "old_execution_result".to_string(),
            },
        )
        .await;

    // Give time for the completion to be processed (and ignored)
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify the completion was ignored - check that it's not in history
    let history = store.read("inst-exec-test").await;
    let has_old_completion = history.iter().any(|e| match e {
        rust_dtf::Event::ActivityCompleted { result, .. } => result == "old_execution_result",
        _ => false,
    });

    assert!(
        !has_old_completion,
        "Old execution completion should be ignored due to execution ID mismatch"
    );

    println!("✓ Old execution completion was properly ignored due to execution ID validation");
}

#[tokio::test]
async fn future_execution_completions_are_ignored() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();

    // Simple orchestration that waits for external events
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let _result = ctx.schedule_wait("test_event").into_event().await;
        Ok("completed".to_string())
    };

    let reg = OrchestrationRegistry::builder()
        .register("FutureExecTest", orch)
        .build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), reg).await;

    // Start the orchestration
    let _handle = rt
        .clone()
        .start_orchestration("inst-future", "FutureExecTest", "")
        .await
        .unwrap();

    // Wait for orchestration to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Inject a completion with a future execution ID (should never happen in practice)
    let _ = store
        .enqueue_orchestrator_work(

            WorkItem::ActivityCompleted {
                instance: "inst-future".to_string(),
                execution_id: 999, // Future execution ID
                id: 1,
                result: "future_completion".to_string(),
            },
        )
        .await;

    // Give some time for processing
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify the completion was ignored
    let history = store.read("inst-future").await;
    let has_future_completion = history.iter().any(|e| match e {
        rust_dtf::Event::ActivityCompleted { result, .. } => result == "future_completion",
        _ => false,
    });

    assert!(!has_future_completion, "Future execution completion should be ignored");

    println!("✓ Future execution completion was properly ignored");
}


