use duroxide::providers::HistoryStore;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Action, DurableOutput, Event, OrchestrationContext, OrchestrationRegistry, run_turn};
use std::sync::Arc;
use std::sync::Arc as StdArc;
mod common;

async fn orchestration_completes_and_replays_deterministically_with(store: StdArc<dyn HistoryStore>) {
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let start: u64 = 0; // deterministic logical start for test
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(5);
        let f_e = ctx.schedule_wait("Go");

        let outputs = ctx.join(vec![f_a, f_t, f_e]).await;

        // Extract outputs by type since join returns in history order
        let mut o_a = None;
        let mut o_e = None;

        for output in outputs {
            match output {
                DurableOutput::Activity(_) => o_a = Some(output),
                DurableOutput::Timer => {} // ignore timer
                DurableOutput::External(_) => o_e = Some(output),
                _ => {}
            }
        }

        let o_a = o_a.expect("Activity result not found");
        let o_e = o_e.expect("External result not found");

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
    let _handle = rt
        .clone()
        .start_orchestration("inst-orch-1", "DeterministicOrchestration", "")
        .await
        .unwrap();

    let output = match rt
        .wait_for_orchestration("inst-orch-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => output,
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };

    assert!(output.contains("evt=ok"));
    assert!(output.contains("b=2!"));

    // Check history for expected events
    let final_history = rt.get_execution_history("inst-orch-1", 1).await;
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
        let outputs = ctx.join(vec![f_a, f_t, f_e]).await;

        // Extract outputs by type since join returns in history order
        let mut o_a = None;
        let mut o_e = None;

        for output in outputs {
            match output {
                DurableOutput::Activity(_) => o_a = Some(output),
                DurableOutput::Timer => {} // ignore timer
                DurableOutput::External(_) => o_e = Some(output),
                _ => {}
            }
        }

        let o_a = o_a.expect("Activity result not found");
        let o_e = o_e.expect("External result not found");
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
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    orchestration_completes_and_replays_deterministically_with(store).await;
}

#[test]
fn action_order_is_deterministic_in_first_turn() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(500);
        let f_e = ctx.schedule_wait("Go");
        let _ = ctx.join(vec![f_a, f_t, f_e]).await;
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
    let _handle = rt
        .clone()
        .start_orchestration("inst-seq-1", "SequentialOrchestration", "")
        .await
        .unwrap();

    match rt
        .wait_for_orchestration("inst-seq-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "c=2bc"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for expected events
    let final_history = rt.get_execution_history("inst-seq-1", 1).await;
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
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    sequential_activity_chain_completes_with(store).await;
}
