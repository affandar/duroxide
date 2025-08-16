use rust_dtf::{run_turn, OrchestrationContext, Event, Action};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::{HistoryStore};
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc;

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
        let out = ctx.schedule_activity("A", "1").into_activity().await;
        out
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

    let registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string()
        })
        .build();
    let rt = runtime::Runtime::start(Arc::new(registry)).await;
    let h = rt.clone().spawn_instance_to_completion("inst-unit-1", orchestrator).await;
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
    let store = FsHistoryStore::new(tmp.path());
    store.append("i1", vec![Event::TimerCreated { id: 1, fire_at_ms: 10 }]).await.unwrap();
    store.append("i2", vec![Event::ExternalSubscribed { id: 1, name: "Go".into() }]).await.unwrap();
    let instances = store.list_instances().await;
    assert!(instances.contains(&"i1".into()) && instances.contains(&"i2".into()));
    let dump = store.dump_all_pretty().await;
    assert!(dump.contains("instance=i1") && dump.contains("instance=i2"));
    store.reset().await;
    assert!(store.list_instances().await.is_empty());
}


