use duroxide::providers::Provider;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Action, Event, OrchestrationContext, OrchestrationRegistry, run_turn};
use std::sync::Arc;

// Helper to create runtime with registries for tests
#[allow(dead_code)]
async fn create_test_runtime(activity_registry: ActivityRegistry) -> Arc<runtime::Runtime> {
    // Create a minimal orchestration registry for basic tests
    let orchestration_registry = OrchestrationRegistry::builder().build();
    runtime::Runtime::start(Arc::new(activity_registry), orchestration_registry).await
}

// 1) Single-turn emission: ensure exactly one action per scheduled future and matching schedule event recorded.
#[test]
fn action_emission_single_turn() {
    // Await the scheduled activity once so it is polled and records its action, then remain pending
    let orchestrator = |ctx: OrchestrationContext| async move {
        let _ = ctx.schedule_activity("A", "1").into_activity().await;
        unreachable!()
    };

    let history: Vec<Event> = Vec::new();
    let (hist_after, actions, _logs, out) = run_turn(history, orchestrator);
    assert!(out.is_none(), "must not complete in first turn");
    assert_eq!(actions.len(), 1, "exactly one action expected");
    match &actions[0] {
        Action::CallActivity { name, input, .. } => {
            assert_eq!(name, "A");
            assert_eq!(input, "1");
        }
        _ => panic!("unexpected action kind"),
    }
    // History should already contain ActivityScheduled
    assert!(matches!(hist_after[0], Event::ActivityScheduled { .. }));
}

// 2) Correlation: out-of-order completion in history still resolves the correct future by id.
#[test]
fn correlation_out_of_order_completion() {
    // Prepare history: schedule A(1), then an unrelated timer, then the activity completion
    let history = vec![
        Event::ActivityScheduled {
            id: 1,
            name: "A".into(),
            input: "1".into(),
            execution_id: 1,
        },
        Event::TimerFired { id: 42, fire_at_ms: 0 },
        Event::ActivityCompleted {
            id: 1,
            result: "ok".into(),
        },
    ];

    let orchestrator = |ctx: OrchestrationContext| async move { ctx.schedule_activity("A", "1").into_activity().await };

    let (_hist_after, actions, _logs, out) = run_turn(history, orchestrator);
    assert!(
        actions.is_empty(),
        "should resolve from existing completion, no new actions"
    );
    assert_eq!(out.unwrap(), Ok("ok".to_string()));
}

// 3) Deterministic replay on a tiny flow (activity only)
#[tokio::test]
async fn deterministic_replay_activity_only() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let a = ctx.schedule_activity("A", "2").into_activity().await.unwrap();
        format!("a={a}")
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            Ok(input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string())
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TestOrchestration", move |ctx, _input| async move {
            Ok(orchestrator(ctx).await)
        })
        .build();

    let store = SqliteProvider::new_in_memory().await.unwrap();
    let store = Arc::new(store) as Arc<dyn Provider>;
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());
    client.start_orchestration("inst-unit-1", "TestOrchestration", "").await.unwrap();

    let status = client
        .wait_for_orchestration("inst-unit-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();

    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        _ => panic!("Expected completed status"),
    };
    assert_eq!(output, "a=3");

    let final_history = client.get_execution_history("inst-unit-1", 1).await;

    // Replay must produce same output and no new actions
    let (_h2, acts2, _logs2, out2) = run_turn(final_history.clone(), orchestrator);
    assert!(acts2.is_empty());
    assert_eq!(out2.unwrap(), output);
    rt.shutdown().await;
}

// Provider admin APIs moved to provider-local tests; runtime tests should use runtime-only APIs.

#[tokio::test]
async fn runtime_duplicate_orchestration_deduped_single_execution() {
    // Start runtime and attempt to start the same instance twice concurrently
    let activity_registry = ActivityRegistry::builder().build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TestOrch", |ctx, _| async move {
            // Slow a bit to allow duplicate enqueue to happen
            ctx.schedule_timer(20).into_timer().await;
            Ok("ok".to_string())
        })
        .build();

    let store = SqliteProvider::new_in_memory().await.unwrap();
    let store = Arc::new(store) as Arc<dyn Provider>;
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let inst = "dup-orch";

    let client = duroxide::Client::new(store.clone());
    // Fire two start requests for the same instance
    let _h1 = client.start_orchestration(inst, "TestOrch", "").await.unwrap();
    let _h2 = client.start_orchestration(inst, "TestOrch", "").await.unwrap();

    // Both should resolve to the same single execution/result
    match client
        .wait_for_orchestration(inst, std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Check history
    let hist1 = client.get_execution_history(inst, 1).await;

    // Ensure there is only one terminal event in history
    let term_count = hist1
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::OrchestrationCompleted { .. } | Event::OrchestrationFailed { .. }
            )
        })
        .count();
    assert_eq!(term_count, 1, "should have exactly one terminal event");

    // Both handles observed the same execution: only one start/terminal
    let started_count = hist1
        .iter()
        .filter(|e| matches!(e, Event::OrchestrationStarted { .. }))
        .count();
    assert_eq!(started_count, 1);

    rt.shutdown().await;
}

#[tokio::test]
async fn orchestration_descriptor_root_and_child() {
    // Root orchestrations
    let activity_registry = ActivityRegistry::builder().build();
    let parent = |ctx: OrchestrationContext, _| async move {
        let _ = ctx
            .schedule_sub_orchestration("ChildDsc", "x")
            .into_sub_orchestration()
            .await;
        Ok("done".into())
    };
    let child = |_ctx: OrchestrationContext, _input: String| async move { Ok("child".into()) };
    let reg = OrchestrationRegistry::builder()
        .register("ParentDsc", parent)
        .register("ChildDsc", child)
        .build();
    let store = SqliteProvider::new_in_memory().await.unwrap();
    let store = Arc::new(store) as Arc<dyn Provider>;
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), reg).await;
    let client = duroxide::Client::new(store.clone());
    let _h = client.start_orchestration("inst-desc", "ParentDsc", "seed").await.unwrap();
    // wait for completion
    let _ = client
        .wait_for_orchestration("inst-desc", std::time::Duration::from_secs(2))
        .await;
    // Root descriptor
    let d = rt.get_orchestration_descriptor("inst-desc").await.unwrap();
    assert_eq!(d.name, "ParentDsc");
    assert!(!d.version.is_empty());
    assert!(d.parent_instance.is_none());
    assert!(d.parent_id.is_none());
    // Child descriptor
    let dchild = rt.get_orchestration_descriptor("inst-desc::sub::1").await.unwrap();
    assert_eq!(dchild.name, "ChildDsc");
    assert!(dchild.version.len() > 0);
    assert_eq!(dchild.parent_instance.as_deref(), Some("inst-desc"));
    assert_eq!(dchild.parent_id, Some(1));
    rt.shutdown().await;
}

#[tokio::test]
async fn orchestration_status_apis() {
    use duroxide::OrchestrationStatus;

    // Registry with two orchestrations: one completes after a short timer, one fails immediately
    let activity_registry = ActivityRegistry::builder().build();
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ShortTimer", |ctx, _| async move {
            ctx.schedule_timer(100).into_timer().await;
            Ok("ok".to_string())
        })
        .register("AlwaysFails", |_ctx, _| async move { Err("boom".to_string()) })
        .build();

    let store = SqliteProvider::new_in_memory().await.unwrap();
    let store = Arc::new(store) as Arc<dyn Provider>;
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let client = duroxide::Client::new(store.clone());

    // NotFound for unknown instance
    let s = client.get_orchestration_status("no-such").await;
    assert!(matches!(s, OrchestrationStatus::NotFound));

    // Start a running orchestration; should be Running after dispatcher processes it
    let inst_running = "inst-status-running";
    let _handle_running = client
        .start_orchestration(inst_running, "ShortTimer", "")
        .await
        .unwrap();
    // Wait a bit for the orchestrator dispatcher to process the queued work item
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let s1 = client.get_orchestration_status(inst_running).await;
    // The orchestration should be running (waiting for timer)
    assert!(
        matches!(s1, OrchestrationStatus::Running),
        "expected Running, got {:?}",
        s1
    );

    // After completion, should be Completed with output
    match client
        .wait_for_orchestration(inst_running, std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    let s2 = client.get_orchestration_status(inst_running).await;
    assert!(matches!(s2, OrchestrationStatus::Completed { .. }));
    if let OrchestrationStatus::Completed { output } = s2 {
        assert_eq!(output, "ok");
    }

    // Failed orchestration
    let inst_fail = "inst-status-fail";
    let _handle_fail = client
        .start_orchestration(inst_fail, "AlwaysFails", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration(inst_fail, std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Failed { error: _ } => {} // Expected failure
        duroxide::OrchestrationStatus::Completed { output } => panic!("expected failure, got: {output}"),
        _ => panic!("unexpected orchestration status"),
    }
    let s3 = client.get_orchestration_status(inst_fail).await;
    assert!(matches!(s3, OrchestrationStatus::Failed { .. }));
    if let OrchestrationStatus::Failed { error } = s3 {
        assert_eq!(error, "boom");
    }

    rt.shutdown().await;
}

// Providers: filesystem multi-execution persistence and latest read() contract
#[tokio::test]
async fn providers_fs_multi_execution_persistence_and_latest_read() {
    let _tmp = tempfile::tempdir().unwrap();
    let fs = SqliteProvider::new_in_memory().await.unwrap();

    // Create execution #1 and append CAN terminal
    fs.create_new_execution("pfs", "O", "0.0.0", "0", None, None)
        .await
        .unwrap();
    fs.append_with_execution(
        "pfs",
        1,
        vec![Event::OrchestrationContinuedAsNew { input: "1".into() }],
    )
    .await
    .unwrap();
    let e1_before = fs.read_with_execution("pfs", 1).await;

    // Create execution #2 via reset_for_continue_as_new; complete it
    let _eid2 = fs
        .create_new_execution("pfs", "O", "0.0.0", "1", None, None)
        .await
        .unwrap();
    fs.append_with_execution("pfs", 2, vec![Event::OrchestrationCompleted { output: "ok".into() }])
        .await
        .unwrap();

    // Execution list must contain both
    let execs = fs.list_executions("pfs").await;
    assert_eq!(execs, vec![1, 2]);

    // Older execution history remains unchanged
    let e1_after = fs.read_with_execution("pfs", 1).await;
    assert_eq!(e1_before, e1_after);

    // Latest read() equals latest execution history
    let latest_hist = fs.read_with_execution("pfs", 2).await;
    let current_hist = fs.read("pfs").await;
    assert_eq!(current_hist, latest_hist);
}

// Providers: in-memory multi-execution persistence and latest read() contract
#[tokio::test]
async fn providers_inmem_multi_execution_persistence_and_latest_read() {
    let mem = SqliteProvider::new_in_memory().await.unwrap();

    mem.create_new_execution("pmem", "O", "0.0.0", "0", None, None)
        .await
        .unwrap();
    mem.append_with_execution(
        "pmem",
        1,
        vec![Event::OrchestrationContinuedAsNew { input: "1".into() }],
    )
    .await
    .unwrap();
    let e1_before = mem.read_with_execution("pmem", 1).await;

    let _eid2 = mem
        .create_new_execution("pmem", "O", "0.0.0", "1", None, None)
        .await
        .unwrap();
    mem.append_with_execution("pmem", 2, vec![Event::OrchestrationCompleted { output: "ok".into() }])
        .await
        .unwrap();

    let execs = mem.list_executions("pmem").await;
    assert_eq!(execs, vec![1, 2]);

    let e1_after = mem.read_with_execution("pmem", 1).await;
    assert_eq!(e1_before, e1_after);

    let latest_hist = mem.read_with_execution("pmem", 2).await;
    let current_hist = mem.read("pmem").await;
    assert_eq!(current_hist, latest_hist);
}
