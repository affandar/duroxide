use futures::future::{join3, select, Either};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Action, DurableOutput, Event, OrchestrationContext, OrchestrationRegistry, run_turn};
use std::sync::Arc;
use std::sync::Arc as StdArc;
mod common;

async fn orchestration_completes_and_replays_deterministically_with(store: StdArc<dyn HistoryStore>) {
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let start: u64 = 0; // deterministic logical start for test
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(5);
        let f_e = ctx.schedule_wait("Go");

        let (o_a, _o_t, o_e) = join3(f_a, f_t, f_e).await;

        let a = match o_a {
            DurableOutput::Activity(v) => v.unwrap(),
            _ => unreachable!("A must be activity result"),
        };
        let evt = match o_e {
            DurableOutput::External(v) => v,
            _ => unreachable!("Go must be external event"),
        };

        let b = ctx.schedule_activity("B", a.clone()).into_activity().await.unwrap();
        Ok(format!("id=_hidden, start={start}, evt={evt}, b={b}"))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            Ok(input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string())
        })
        .register("B", |input: String| async move { Ok(format!("{input}!")) })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("DeterministicOrchestration", orchestration)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let store_for_wait = store.clone();
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        let _ = crate::common::wait_for_subscription(store_for_wait, "inst-orch-1", "Go", 1000).await;
        rt_clone.raise_event("inst-orch-1", "Go", "ok").await;
    });
    let handle = rt
        .clone()
        .start_orchestration("inst-orch-1", "DeterministicOrchestration", "")
        .await;
    let (final_history, output) = handle.unwrap().await.unwrap();
    let output = output.unwrap();
    assert!(output.contains("evt=ok"));
    assert!(output.contains("b=2!"));
    // Includes OrchestrationStarted + 4 schedule/complete pairs + terminal OrchestrationCompleted
    assert_eq!(
        final_history.len(),
        10,
        "expected 10 history events including OrchestrationStarted and terminal event"
    );
    // For replay, provide a 1-arg closure equivalent to the registered orchestrator
    let replay = |ctx: OrchestrationContext| async move {
        let start: u64 = 0; // deterministic logical start for test
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(5);
        let f_e = ctx.schedule_wait("Go");
        let (o_a, _o_t, o_e) = join3(f_a, f_t, f_e).await;
        let a = match o_a {
            DurableOutput::Activity(v) => v.unwrap(),
            _ => unreachable!("A must be activity result"),
        };
        let evt = match o_e {
            DurableOutput::External(v) => v,
            _ => unreachable!("Go must be external event"),
        };
        let b = ctx.schedule_activity("B", a.clone()).into_activity().await.unwrap();
        format!("id=_hidden, start={start}, evt={evt}, b={b}")
    };
    let (_h2, acts2, _logs2, out2) = run_turn(final_history.clone(), replay);
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
            Action::StartOrchestrationDetached { .. } => "StartOrchestrationDetached",
            Action::StartSubOrchestration { .. } => "StartSubOrchestration",
            Action::ContinueAsNew { .. } => "ContinueAsNew",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["CallActivity", "CreateTimer", "WaitExternal"],
        "actions must be recorded in declaration/poll order"
    );
}

#[tokio::test]
async fn activities_same_delay_completion_vs_history_order_fs() {
    // Flaky by design until deterministic selectors are introduced
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let a1 = ctx.schedule_activity("Sleep", "50").into_activity();
        let a2 = ctx.schedule_activity("Sleep", "50").into_activity();
        let res = select(a1, a2).await;
        match res {
            Either::Left((_x, y)) => { let _ = y.await; Ok("first=a1".to_string()) }
            Either::Right((_y, x)) => { let _ = x.await; Ok("first=a2".to_string()) }
        }
    };

    let acts = ActivityRegistry::builder()
        .register("Sleep", |ms: String| async move {
            let d = ms.parse::<u64>().unwrap_or(0);
            tokio::time::sleep(std::time::Duration::from_millis(d)).await;
            Ok(ms)
        })
        .build();
    let reg = OrchestrationRegistry::builder().register("TwoSameActs", orchestrator).build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    let h = rt
        .clone()
        .start_orchestration("inst-two-acts", "TwoSameActs", "")
        .await
        .unwrap();
    let (hist, out) = h.await.unwrap();
    let out = out.unwrap();

    // Identify the two activity ids by ActivityScheduled order for name Sleep
    let mut ids: Vec<u64> = hist
        .iter()
        .filter_map(|e| match e { Event::ActivityScheduled { id, name, .. } if name == "Sleep" => Some(*id), _ => None })
        .collect();
    assert!(ids.len() >= 2, "expected at least two ActivityScheduled events for Sleep");
    ids.truncate(2);
    let (a1_id, a2_id) = (ids[0], ids[1]);

    let idx_a1 = hist.iter().position(|e| matches!(e, Event::ActivityCompleted { id, .. } if *id == a1_id)).unwrap();
    let idx_a2 = hist.iter().position(|e| matches!(e, Event::ActivityCompleted { id, .. } if *id == a2_id)).unwrap();

    if out == "first=a1" {
        assert!(idx_a1 <= idx_a2, "a1 completed first by select, but history shows a2 earlier: {hist:#?}");
    } else {
        assert_eq!(out.as_str(), "first=a2");
        assert!(idx_a2 <= idx_a1, "a2 completed first by select, but history shows a1 earlier: {hist:#?}");
    }

    rt.shutdown().await;
}

async fn sequential_activity_chain_completes_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let a = ctx.schedule_activity("A", "1").into_activity().await.unwrap();
        let b = ctx.schedule_activity("B", a).into_activity().await.unwrap();
        let c = ctx.schedule_activity("C", b).into_activity().await.unwrap();
        Ok(format!("c={c}"))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            Ok(input.parse::<i32>().map(|x| x + 1).unwrap_or(0).to_string())
        })
        .register("B", |input: String| async move { Ok(format!("{input}b")) })
        .register("C", |input: String| async move { Ok(format!("{input}c")) })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SequentialOrchestration", orchestrator)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt
        .clone()
        .start_orchestration("inst-seq-1", "SequentialOrchestration", "")
        .await;
    let (final_history, output) = handle.unwrap().await.unwrap();
    assert_eq!(output.unwrap(), "c=2bc");
    // Includes OrchestrationStarted + 3 schedule/complete pairs + terminal OrchestrationCompleted
    assert_eq!(
        final_history.len(),
        8,
        "expected OrchestrationStarted + three scheduled+completed activity pairs + terminal event in history"
    );
    rt.shutdown().await;
}

#[tokio::test]
async fn sequential_activity_chain_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    sequential_activity_chain_completes_with(store).await;
}
