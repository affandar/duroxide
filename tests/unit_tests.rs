use rust_dtf::{run_turn, OrchestrationContext, Event, Action, OrchestrationRegistry};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::{HistoryStore};
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;

// Helper to create runtime with registries for tests
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
    match &actions[0] { Action::CallActivity { name, input, .. } => {
        assert_eq!(name, "A"); assert_eq!(input, "1");
    }, _ => panic!("unexpected action kind") }
    // History should already contain ActivityScheduled
    assert!(matches!(hist_after[0], Event::ActivityScheduled { .. }));
}

// 2) Correlation: out-of-order completion in history still resolves the correct future by id.
#[test]
fn correlation_out_of_order_completion() {
    // Prepare history: schedule A(1), then an unrelated timer, then the activity completion
    let history = vec![
        Event::ActivityScheduled { id: 1, name: "A".into(), input: "1".into() },
        Event::TimerFired { id: 42, fire_at_ms: 0 },
        Event::ActivityCompleted { id: 1, result: "ok".into() },
    ];

    let orchestrator = |ctx: OrchestrationContext| async move {
        ctx.schedule_activity("A", "1").into_activity().await
    };

    let (_hist_after, actions, _logs, out) = run_turn(history, orchestrator);
    assert!(actions.is_empty(), "should resolve from existing completion, no new actions");
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
            input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string()
        })
        .build();
    
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TestOrchestration", orchestrator)
        .build();
    
    let rt = runtime::Runtime::start(Arc::new(activity_registry), orchestration_registry).await;
    let h = rt.clone().spawn_instance_to_completion("inst-unit-1", "TestOrchestration").await;
    let (final_history, output) = h.await.unwrap();
    assert_eq!(output, "a=3");

    // Replay must produce same output and no new actions
    let (_h2, acts2, _logs2, out2) = run_turn(final_history.clone(), orchestrator);
    assert!(acts2.is_empty());
    assert_eq!(out2.unwrap(), output);
    rt.shutdown().await;
}

// 4) HistoryStore admin APIs (in-memory)
#[tokio::test]
async fn history_store_admin_apis() {
    let tmp = tempfile::tempdir().unwrap();
    let store = FsHistoryStore::new(tmp.path(), true);
    store.create_instance("i1").await.unwrap();
    store.create_instance("i2").await.unwrap();
    store.append("i1", vec![Event::TimerCreated { id: 1, fire_at_ms: 10 }]).await.unwrap();
    store.append("i2", vec![Event::ExternalSubscribed { id: 1, name: "Go".into() }]).await.unwrap();
    let instances = store.list_instances().await;
    assert!(instances.contains(&"i1".into()) && instances.contains(&"i2".into()));
    let dump = store.dump_all_pretty().await;
    assert!(dump.contains("instance=i1") && dump.contains("instance=i2"));
    store.reset().await;
    assert!(store.list_instances().await.is_empty());
}

#[tokio::test]
async fn providers_create_remove_and_duplicate_checks() {
    // In-memory provider create/remove
    let mem = InMemoryHistoryStore::default();
    mem.create_instance("dup").await.unwrap();
    // Duplicate create should error
    assert!(mem.create_instance("dup").await.is_err());
    // Append to non-existent should fail
    assert!(mem.append("missing", vec![]).await.is_err());
    // Remove existing ok; remove missing should error
    mem.remove_instance("dup").await.unwrap();
    assert!(mem.remove_instance("dup").await.is_err());

    // Filesystem provider create/remove
    let tmp = tempfile::tempdir().unwrap();
    let fs = rust_dtf::providers::fs::FsHistoryStore::new(tmp.path(), true);
    fs.create_instance("i1").await.unwrap();
    assert!(fs.create_instance("i1").await.is_err());
    assert!(fs.append("missing", vec![]).await.is_err());
    fs.remove_instance("i1").await.unwrap();
    assert!(fs.remove_instance("i1").await.is_err());
}

#[tokio::test]
async fn runtime_duplicate_orchestration_errors() {
    // Start runtime and attempt to start the same instance twice concurrently
    let activity_registry = ActivityRegistry::builder().build();
    
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("TestOrch1", |ctx| async move {
            ctx.schedule_timer(10).into_timer().await;
            "ok".to_string()
        })
        .register("TestOrch2", |ctx| async move {
            ctx.schedule_timer(1).into_timer().await;
            "nope".to_string()
        })
        .build();
    
    let rt = runtime::Runtime::start(Arc::new(activity_registry), orchestration_registry).await;
    let inst = "dup-orch";

    let h1 = rt.clone().spawn_instance_to_completion(inst, "TestOrch1").await;

    // Second start should fail (panic inside task) due to runtime active-instance guard
    let h2 = rt.clone().spawn_instance_to_completion(inst, "TestOrch2").await;
    let res = h2.await;
    assert!(res.is_err(), "expected duplicate start to panic in task");

    let (_hist, out) = h1.await.unwrap();
    assert_eq!(out, "ok");
    rt.shutdown().await;
}


