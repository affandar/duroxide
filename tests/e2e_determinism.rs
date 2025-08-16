use futures::future::{join3};
use std::sync::Arc;
use rust_dtf::{run_turn, Event, OrchestrationContext, DurableOutput, Action, OrchestrationRegistry};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

async fn orchestration_completes_and_replays_deterministically_with(store: StdArc<dyn HistoryStore>) {
    let orchestration = |ctx: OrchestrationContext| async move {
        let start = ctx.now_ms();
        let _id = ctx.new_guid();

        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(5);
        let f_e = ctx.schedule_wait("Go");

        let (o_a, _o_t, o_e) = join3(f_a, f_t, f_e).await;

        let a = match o_a { DurableOutput::Activity(v) => v.unwrap(), _ => unreachable!("A must be activity result") };
        let evt = match o_e { DurableOutput::External(v) => v, _ => unreachable!("Go must be external event") };

        let b = ctx.schedule_activity("B", a.clone()).into_activity().await.unwrap();
        format!("id=_hidden, start={start}, evt={evt}, b={b}")
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |input: String| async move { input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string() })
        .register("B", |input: String| async move { format!("{input}!") })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("DeterministicOrchestration", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-orch-1", "Go", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-orch-1", "DeterministicOrchestration").await;
    let (final_history, output) = handle.await.unwrap();
    assert!(output.contains("evt=ok"));
    assert!(output.contains("b=2!"));
    assert_eq!(final_history.len(), 8, "expected 8 history events (scheduled + completed)");
    let (_h2, acts2, _logs2, out2) = run_turn(final_history.clone(), orchestration);
    assert!(acts2.is_empty(), "replay should not produce new actions");
    assert_eq!(out2.unwrap(), output);
    rt.shutdown().await;
}

#[tokio::test]
async fn orchestration_completes_and_replays_deterministically_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    orchestration_completes_and_replays_deterministically_with(store).await;
}

#[test]
fn action_order_is_deterministic_in_first_turn() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let _ = ctx.new_guid();
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(500);
        let f_e = ctx.schedule_wait("Go");
        let _ = join3(f_a, f_t, f_e).await;
        unreachable!("should not complete in the first turn");
    };

    let history: Vec<Event> = Vec::new();
    let (_hist_after, actions, _logs, _out) = run_turn(history, orchestrator);
    let kinds: Vec<&'static str> = actions
        .iter()
        .map(|a| match a {
            Action::CallActivity { .. } => "CallActivity",
            Action::CreateTimer { .. } => "CreateTimer",
            Action::WaitExternal { .. } => "WaitExternal",
        })
        .collect();
    assert_eq!(kinds, vec!["CallActivity", "CreateTimer", "WaitExternal"], "actions must be recorded in declaration/poll order");
}

async fn sequential_activity_chain_completes_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let a = ctx.schedule_activity("A", "1").into_activity().await.unwrap();
        let b = ctx.schedule_activity("B", a).into_activity().await.unwrap();
        let c = ctx.schedule_activity("C", b).into_activity().await.unwrap();
        format!("c={c}")
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |input: String| async move { input.parse::<i32>().map(|x| x + 1).unwrap_or(0).to_string() })
        .register("B", |input: String| async move { format!("{input}b") })
        .register("C", |input: String| async move { format!("{input}c") })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SequentialOrchestration", orchestrator)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-seq-1", "SequentialOrchestration").await;
    let (final_history, output) = handle.await.unwrap();
    assert_eq!(output, "c=2bc");
    assert_eq!(final_history.len(), 6, "expected exactly three scheduled+completed activity pairs in history");
    rt.shutdown().await;
}

#[tokio::test]
async fn sequential_activity_chain_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    sequential_activity_chain_completes_with(store).await;
}

