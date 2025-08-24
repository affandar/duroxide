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

#[tokio::test]
async fn select_two_externals_history_order_wins_fs() {
    // Force a deterministic repro: subscribe to A then B (and select in A,B order),
    // stop the runtime, enqueue B then A externally, restart and assert B wins.
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let a = ctx.schedule_wait("A").into_event();
        let b = ctx.schedule_wait("B").into_event();
        let which = select(a, b).await;
        match which {
            Either::Left((av, rb)) => { let _ = rb.await; Ok(format!("A:{av}")) }
            Either::Right((bv, ra)) => { let _ = ra.await; Ok(format!("B:{bv}")) }
        }
    };

    let acts = ActivityRegistry::builder().build();
    let reg = OrchestrationRegistry::builder().register("ABSelect", orchestrator).build();
    let rt1 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    let _h = rt1
        .clone()
        .start_orchestration("inst-ab", "ABSelect", "")
        .await
        .unwrap();

    // Wait for both subscriptions to be recorded, then stop the runtime
    assert!(
        common::wait_for_history(store.clone(), "inst-ab", |h| {
            let mut seen_a = false; let mut seen_b = false;
            for e in h.iter() {
                if let Event::ExternalSubscribed { name, .. } = e {
                    if name == "A" { seen_a = true; }
                    if name == "B" { seen_b = true; }
                }
            }
            seen_a && seen_b
        }, 3_000).await,
        "timeout waiting for subscriptions"
    );
    rt1.shutdown().await;

    // Enqueue B then A completions while runtime is down
    let wi_b = rust_dtf::providers::WorkItem::ExternalRaised {
        instance: "inst-ab".to_string(),
        name: "B".to_string(),
        data: "vb".to_string(),
    };
    let wi_a = rust_dtf::providers::WorkItem::ExternalRaised {
        instance: "inst-ab".to_string(),
        name: "A".to_string(),
        data: "va".to_string(),
    };
    let _ = store.enqueue_work(rust_dtf::providers::QueueKind::Orchestrator, wi_b).await;
    let _ = store.enqueue_work(rust_dtf::providers::QueueKind::Orchestrator, wi_a).await;

    // Restart runtime and wait for completion; with current select semantics, this may fail
    let acts2 = ActivityRegistry::builder().build();
    let reg2 = OrchestrationRegistry::builder().register("ABSelect", move |ctx, s| orchestrator(ctx, s)).build();
    let rt2 = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts2), reg2).await;

    // Wait for completion and inspect output
    assert!(
        common::wait_for_history(store.clone(), "inst-ab", |h| {
            h.iter().any(|e| matches!(e, Event::OrchestrationCompleted { .. }))
        }, 5_000).await,
        "timeout waiting for completion"
    );
    let hist = store.read("inst-ab").await;
    let output = match hist.last().unwrap() {
        Event::OrchestrationCompleted { output } => output.clone(),
        _ => String::new(),
    };

    // Also assert B's ExternalEvent appears before A's in history
    let idx_b = hist
        .iter()
        .position(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "B"))
        .expect("missing ExternalEvent B");
    let idx_a = hist
        .iter()
        .position(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "A"))
        .expect("missing ExternalEvent A");
    assert!(idx_b < idx_a, "expected ExternalEvent B before A in history: {hist:#?}");

    // Assert B wins because we enqueued B before A, despite select(A,B)
    assert!(output.starts_with("B:"), "expected B to win, got {output}");
    rt2.shutdown().await;
}
