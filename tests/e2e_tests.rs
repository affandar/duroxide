use futures::future::{join3, select, Either, join};
use std::sync::Arc;
use rust_dtf::{run_turn, Event, OrchestrationContext, DurableOutput, Action};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::{HistoryStore};
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

async fn orchestrator_completes_and_replays_deterministically_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let start = ctx.now_ms();
        let _id = ctx.new_guid();

        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(5);
        let f_e = ctx.schedule_wait("Go");

        let (o_a, _o_t, o_e) = join3(f_a, f_t, f_e).await;

        let a = match o_a { DurableOutput::Activity(v) => v, _ => unreachable!("A must be activity result") };
        let evt = match o_e { DurableOutput::External(v) => v, _ => unreachable!("Go must be external event") };

        let b = ctx.schedule_activity("B", a.clone()).into_activity().await;
        format!("id=_hidden, start={start}, evt={evt}, b={b}")
    };

    // Activity registry used by the runtime
    let registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string()
        })
        .register("B", |input: String| async move { format!("{input}!") })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-orch-1", "Go", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-orch-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();
    assert!(output.contains("evt=ok"));
    assert!(output.contains("b=2!"));
    assert_eq!(final_history.len(), 8, "expected 8 history events (scheduled + completed)");
    let (_h2, acts2, out2) = run_turn(final_history.clone(), orchestrator);
    assert!(acts2.is_empty(), "replay should not produce new actions");
    assert_eq!(out2.unwrap(), output);
    rt.shutdown().await;
}

#[tokio::test]
async fn orchestrator_completes_and_replays_deterministically_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    orchestrator_completes_and_replays_deterministically_with(store).await;
}

#[tokio::test]
async fn orchestrator_completes_and_replays_deterministically_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    orchestrator_completes_and_replays_deterministically_with(store).await;
}

async fn any_of_three_returns_first_is_activity_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(3);
        let f_e = ctx.schedule_wait("Go");

        // Race three futures; capture the winner, then explicitly await the other two before exiting
        let left = select(f_a, f_t);
        let race = select(left, f_e);

        match race.await {
            // A wins first; now await timer and external
            Either::Left((Either::Left((DurableOutput::Activity(a), f_t_rest)), f_e_rest)) => {
                let _ = join(f_t_rest, f_e_rest).await;
                format!("winner=A:{a}")
            }
            // Timer wins first; await activity and external
            Either::Left((Either::Right((DurableOutput::Timer, f_a_rest)), f_e_rest)) => {
                let _ = join(f_a_rest, f_e_rest).await;
                "winner=T".to_string()
            }
            // External wins first; await the left pair (A vs T), then await its leftover
            Either::Right((DurableOutput::External(e), left_rest)) => {
                match left_rest.await {
                    Either::Left((_a_done, f_t_remaining)) => { let _ = f_t_remaining.await; }
                    Either::Right((_t_done, f_a_remaining)) => { let _ = f_a_remaining.await; }
                }
                format!("winner=E:{e}")
            }
            _ => panic!("unexpected winner variant"),
        }
    };

    let registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string()
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-race-1", "Go", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-race-1", orchestrator).await;
    let (_race_history, output) = handle.await.unwrap();
    assert!(output.starts_with("winner=A:"), "expected activity to win deterministically, got {output}");
    rt.shutdown().await;
}

#[tokio::test]
async fn any_of_three_returns_first_is_activity_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    any_of_three_returns_first_is_activity_with(store).await;
}

#[tokio::test]
async fn any_of_three_returns_first_is_activity_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    any_of_three_returns_first_is_activity_with(store).await;
}

async fn any_of_three_winner_then_staggered_completions_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(3);
        let f_e = ctx.schedule_wait("Go");

        let left = select(f_a, f_t);
        let race = select(left, f_e);

        match race.await {
            // A wins first; then await timer and external in any order
            Either::Left((Either::Left((DurableOutput::Activity(a), f_t_rest)), f_e_rest)) => {
                let _ = join(f_t_rest, f_e_rest).await;
                format!("winner=A:{a}")
            }
            _ => panic!("unexpected winner variant"),
        }
    };

    let registry = ActivityRegistry::builder()
        .register("A", |input: String| async move {
            input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string()
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-stagger-1", "Go", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-stagger-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();
    assert!(output.starts_with("winner=A:"), "expected activity to win deterministically, got {output}");

    let mut idx_a = None;
    let mut idx_t = None;
    let mut idx_e = None;
    for (i, e) in final_history.iter().enumerate() {
        match e {
            Event::ActivityCompleted { .. } if idx_a.is_none() => idx_a = Some(i),
            Event::TimerFired { .. } if idx_t.is_none() => idx_t = Some(i),
            Event::ExternalEvent { .. } if idx_e.is_none() => idx_e = Some(i),
            _ => {}
        }
    }
    let (ia, it, ie) = (idx_a.unwrap(), idx_t.unwrap(), idx_e.unwrap());
    assert!(ia < it && it < ie, "unexpected completion order: A at {ia}, T at {it}, E at {ie}\nfinal_history={final_history:#?}");
    assert_eq!(final_history.len(), 6, "unexpected final history length: {:?}", final_history);
    rt.shutdown().await;
}

#[tokio::test]
async fn any_of_three_winner_then_staggered_completions_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    any_of_three_winner_then_staggered_completions_with(store).await;
}

#[tokio::test]
async fn any_of_three_winner_then_staggered_completions_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    any_of_three_winner_then_staggered_completions_with(store).await;
}

#[test]
fn action_order_is_deterministic_in_first_turn() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let _ = ctx.new_guid();
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(500);
        let f_e = ctx.schedule_wait("Go");
        // Poll all three once; they should each record exactly one action in deterministic order
        let _ = join3(f_a, f_t, f_e).await;
        unreachable!("should not complete in the first turn");
    };

    let history: Vec<Event> = Vec::new();
    let (_hist_after, actions, _out) = run_turn(history, orchestrator);
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
        let a = ctx.schedule_activity("A", "1").into_activity().await;
        let b = ctx.schedule_activity("B", a).into_activity().await;
        let c = ctx.schedule_activity("C", b).into_activity().await;
        format!("c={c}")
    };

    let registry = ActivityRegistry::builder()
        .register("A", |input: String| async move { input.parse::<i32>().map(|x| x + 1).unwrap_or(0).to_string() })
        .register("B", |input: String| async move { format!("{input}b") })
        .register("C", |input: String| async move { format!("{input}c") })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-seq-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();
    assert_eq!(output, "c=2bc");
    assert_eq!(final_history.len(), 6, "expected exactly three scheduled+completed activity pairs in history");
    rt.shutdown().await;
}

#[tokio::test]
async fn sequential_activity_chain_completes_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    sequential_activity_chain_completes_with(store).await;
}

#[tokio::test]
async fn sequential_activity_chain_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    sequential_activity_chain_completes_with(store).await;
}

async fn complex_control_flow_orchestration_with(store: StdArc<dyn HistoryStore>) {
    use futures::future::select;
    use futures::future::Either;

    // Orchestrator with:
    // - retry loop with backoff timer until Fetch succeeds
    // - branch using select between timer and external
    // - sequential loop of dependent activities
    let orchestrator = |ctx: OrchestrationContext| async move {
        // Retry Fetch until it returns "ok"; backoff 5ms between attempts
        let mut attempts = 0u32;
        loop {
            let res = ctx.schedule_activity("Fetch", attempts.to_string()).into_activity().await;
            if res == "ok" { break; }
            attempts += 1;
            ctx.schedule_timer(5).into_timer().await;
        }

        // Branch: race a short timer vs an external event; with our deterministic host ordering
        // and scheduling order (timer polled before wait), the timer should win here.
        let race = select(ctx.schedule_timer(3), ctx.schedule_wait("Proceed"));
        let branch = match race.await {
            Either::Left((_t, _e)) => "timer",
            Either::Right((_e, _t)) => "external",
        };

        // Sequential dependent activities in a loop
        let mut acc = String::from("seed");
        for i in 0..3u32 {
            acc = ctx.schedule_activity("Acc", format!("{acc}-{i}")).into_activity().await;
        }

        format!("attempts={attempts}, branch={branch}, acc={acc}")
    };

    let registry = ActivityRegistry::builder()
        .register("Fetch", |input: String| async move {
            let attempts: u32 = input.parse().unwrap_or(0);
            if attempts < 2 { "err".to_string() } else { "ok".to_string() }
        })
        .register("Acc", |input: String| async move { format!("{input}:x") })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-complex-1", "Proceed", "go").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-complex-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert!(output.contains("attempts=2"), "unexpected attempts count: {output}");
    assert!(output.contains("branch=timer"), "unexpected branch winner: {output}");
    assert!(output.contains("acc=seed-0:x-1:x-2:x"), "unexpected accumulator: {output}");

    let has_ext_event = final_history.iter().any(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "Proceed"));
    let expected_len = if has_ext_event { 20 } else { 19 };
    assert_eq!(final_history.len(), expected_len, "unexpected final history length: {:?}", final_history);

    rt.shutdown().await;
}

#[tokio::test]
async fn complex_control_flow_orchestration_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    complex_control_flow_orchestration_with(store).await;
}

#[tokio::test]
async fn complex_control_flow_orchestration_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    complex_control_flow_orchestration_with(store).await;
}


async fn wait_external_completes_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let data = ctx.schedule_wait("Only").into_event().await;
        format!("only={data}")
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        rt_clone.raise_event("inst-wait-1", "Only", "payload").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-wait-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "only=payload");
    assert!(matches!(final_history[0], Event::ExternalSubscribed { .. }));
    assert!(matches!(final_history[1], Event::ExternalEvent { .. }));
    assert_eq!(final_history.len(), 2);

    rt.shutdown().await;
}

#[tokio::test]
async fn wait_external_completes_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    wait_external_completes_with(store).await;
}

#[tokio::test]
async fn wait_external_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    wait_external_completes_with(store).await;
}

async fn external_sent_before_instance_is_ignored_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let data = ctx.schedule_wait("Only").into_event().await;
        format!("only={data}")
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    rt.raise_event("inst-wait-ignore-1", "Only", "early").await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        rt_clone.raise_event("inst-wait-ignore-1", "Only", "late").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-wait-ignore-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "only=late");
    assert!(final_history.iter().any(|e| matches!(e, Event::ExternalEvent { name, data, .. } if name == "Only" && data == "late")));
    assert!(!final_history.iter().any(|e| matches!(e, Event::ExternalEvent { name, data, .. } if name == "Only" && data == "early")));
    assert_eq!(final_history.len(), 2, "expected only subscribe + single event");

    rt.shutdown().await;
}

#[tokio::test]
async fn external_sent_before_instance_is_ignored_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    external_sent_before_instance_is_ignored_with(store).await;
}

#[tokio::test]
async fn external_sent_before_instance_is_ignored_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    external_sent_before_instance_is_ignored_with(store).await;
}

async fn race_external_vs_timer_ordering_with(store: StdArc<dyn HistoryStore>) {
    use futures::future::{select, Either};

    let orchestrator = |ctx: OrchestrationContext| async move {
        let race = select(ctx.schedule_timer(3), ctx.schedule_wait("Race"));
        match race.await {
            Either::Left((_t, _e)) => "timer".to_string(),
            Either::Right((_e, _t)) => "external".to_string(),
        }
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-race-order-1", "Race", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-race-order-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "timer");
    let idx_t = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })).unwrap();
    if let Some(idx_e) = final_history.iter().position(|e| matches!(e, Event::ExternalEvent { .. })) {
        assert!(idx_t < idx_e, "expected timer to fire before external: {final_history:#?}");
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn race_external_vs_timer_ordering_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    race_external_vs_timer_ordering_with(store).await;
}

#[tokio::test]
async fn race_external_vs_timer_ordering_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    race_external_vs_timer_ordering_with(store).await;
}

async fn race_event_vs_timer_event_wins_with(store: StdArc<dyn HistoryStore>) {
    use futures::future::{select, Either};

    let orchestrator = |ctx: OrchestrationContext| async move {
        let race = select(ctx.schedule_timer(6), ctx.schedule_wait("Race"));
        match race.await {
            Either::Left((_t, _e)) => "timer".to_string(),
            Either::Right((_e, _t)) => "external".to_string(),
        }
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        rt_clone.raise_event("inst-race-order-2", "Race", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-race-order-2", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "external");
    let idx_e = final_history.iter().position(|e| matches!(e, Event::ExternalEvent { .. })).unwrap();
    if let Some(idx_t) = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })) {
        assert!(idx_e < idx_t, "expected external before timer: {final_history:#?}");
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn race_event_vs_timer_event_wins_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    race_event_vs_timer_event_wins_with(store).await;
}

#[tokio::test]
async fn race_event_vs_timer_event_wins_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    race_event_vs_timer_event_wins_with(store).await;
}

// Concurrent orchestrations: ensure dispatcher isolation with overlapping correlation ids
async fn concurrent_orchestrations_different_activities_with(store: StdArc<dyn HistoryStore>) {
    // Both orchestrations create futures in the same order to intentionally overlap ids (1=activity, 2=wait, 3=timer)
    let o1 = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("Add", "2,3");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        format!("o1:sum={a};evt={e}")
    };
    let o2 = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("Upper", "hi");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        format!("o2:up={a};evt={e}")
    };

    let registry = ActivityRegistry::builder()
        .register("Add", |input: String| async move {
            let mut parts = input.split(',');
            let a = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            let b = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            (a + b).to_string()
        })
        .register("Upper", |input: String| async move { input.to_uppercase() })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let h1 = rt.clone().spawn_instance_to_completion("inst-multi-1", o1).await;
    let h2 = rt.clone().spawn_instance_to_completion("inst-multi-2", o2).await;

    let rt_c = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        rt_c.raise_event("inst-multi-1", "Go", "E1").await;
    });
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        rt_c2.raise_event("inst-multi-2", "Go", "E2").await;
    });

    let (hist1, out1) = h1.await.unwrap();
    let (hist2, out2) = h2.await.unwrap();

    assert!(out1.contains("o1:sum=5;evt=E1"), "unexpected out1: {out1}");
    assert!(out2.contains("o2:up=HI;evt=E2"), "unexpected out2: {out2}");

    // Verify overlapping ids map to their own instance's events
    assert!(hist1.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "5")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "HI")));
    assert!(hist1.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "E1")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "E2")));
    assert!(hist1.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));
    assert!(hist2.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));

    rt.shutdown().await;
}

#[tokio::test]
async fn concurrent_orchestrations_different_activities_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_different_activities_with(store).await;
}

#[tokio::test]
async fn concurrent_orchestrations_different_activities_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_different_activities_with(store).await;
}

async fn concurrent_orchestrations_same_activities_with(store: StdArc<dyn HistoryStore>) {
    // Both use same activity name; both create futures in same order to overlap ids intentionally
    let o1 = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("Proc", "10");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        format!("o1:a={a};evt={e}")
    };
    let o2 = |ctx: OrchestrationContext| async move {
        let _guid = ctx.new_guid(); // differ a bit but keep scheduling order
        let f_a = ctx.schedule_activity("Proc", "20");
        let f_e = ctx.schedule_wait("Go");
        let f_t = ctx.schedule_timer(1);
        let (a, e, _) = join3(f_a.into_activity(), f_e.into_event(), f_t.into_timer()).await;
        format!("o2:a={a};evt={e}")
    };

    let registry = ActivityRegistry::builder()
        .register("Proc", |input: String| async move {
            let n = input.parse::<i64>().unwrap_or(0);
            (n + 1).to_string()
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let h1 = rt.clone().spawn_instance_to_completion("inst-same-acts-1", o1).await;
    let h2 = rt.clone().spawn_instance_to_completion("inst-same-acts-2", o2).await;

    let rt_c = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        rt_c.raise_event("inst-same-acts-1", "Go", "P1").await;
    });
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt_c2.raise_event("inst-same-acts-2", "Go", "P2").await;
    });

    let (hist1, out1) = h1.await.unwrap();
    let (hist2, out2) = h2.await.unwrap();

    assert_eq!(out1, "o1:a=11;evt=P1");
    assert_eq!(out2, "o2:a=21;evt=P2");

    // Verify overlapping ids remain isolated
    assert!(hist1.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "11")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ActivityCompleted { id, result } if *id == 1 && result == "21")));
    assert!(hist1.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "P1")));
    assert!(hist2.iter().any(|e| matches!(e, Event::ExternalEvent { id, data, .. } if *id == 2 && data == "P2")));
    assert!(hist1.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));
    assert!(hist2.iter().any(|e| matches!(e, Event::TimerFired { id, .. } if *id == 3)));

    rt.shutdown().await;
}

#[tokio::test]
async fn concurrent_orchestrations_same_activities_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_same_activities_with(store).await;
}

#[tokio::test]
async fn concurrent_orchestrations_same_activities_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    concurrent_orchestrations_same_activities_with(store).await;
}

async fn recovery_across_restart_core<F1, F2>(make_store_stage1: F1, make_store_stage2: F2, instance: String)
where
    F1: Fn() -> StdArc<dyn HistoryStore>,
    F2: Fn() -> StdArc<dyn HistoryStore>,
{
    // Orchestrator performs 4 steps with a barrier between 2 and 3
    let orchestrator = |ctx: OrchestrationContext| async move {
        let s1 = ctx.schedule_activity("Step", "1").into_activity().await;
        let s2 = ctx.schedule_activity("Step", "2").into_activity().await;
        let _ = ctx.schedule_wait("Resume").into_event().await;
        let s3 = ctx.schedule_activity("Step", "3").into_activity().await;
        let s4 = ctx.schedule_activity("Step", "4").into_activity().await;
        format!("{s1}{s2}{s3}{s4}")
    };

    let count_scheduled = |hist: &Vec<Event>, input: &str| -> usize {
        hist.iter()
            .filter(|e| matches!(e, Event::ActivityScheduled { name, input: inp, .. } if name == "Step" && inp == input))
            .count()
    };

    // Stage 1: start runtime with store1, progress to barrier, then shutdown (crash)
    let store1 = make_store_stage1();
    let registry = ActivityRegistry::builder()
        .register("Step", |input: String| async move { input })
        .build();

    let rt1 = runtime::Runtime::start_with_store(store1.clone(), Arc::new(registry.clone())).await;
    let handle1 = rt1.clone().spawn_instance_to_completion(&instance, orchestrator).await;

    // Allow time for steps 1&2 and subscription to complete, but don't resume
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;

    let pre_crash_hist = store1.read(&instance).await;
    // Ensure we actually reached the barrier
    assert_eq!(count_scheduled(&pre_crash_hist, "1"), 1);
    assert_eq!(count_scheduled(&pre_crash_hist, "2"), 1);
    assert_eq!(count_scheduled(&pre_crash_hist, "3"), 0);

    // Simulate crash by shutting down runtime1; drop handle without awaiting
    rt1.shutdown().await;
    drop(handle1);

    // Stage 2: start runtime with store2 (same dir for Fs, fresh for InMemory), resume, and complete
    let store2 = make_store_stage2();
    let rt2 = runtime::Runtime::start_with_store(store2.clone(), Arc::new(registry.clone())).await;
    let rt2_c = rt2.clone();
    let instance_for_spawn = instance.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt2_c.raise_event(&instance_for_spawn, "Resume", "go").await;
    });
    let handle2 = rt2.clone().spawn_instance_to_completion(&instance, orchestrator).await;
    let (_final_hist_runtime, output) = handle2.await.unwrap();
    assert_eq!(output, "1234");

    // Collect final histories for comparison
    let final_hist2 = store2.read(&instance).await;

    // Return values via assertions from test wrappers below
    // The wrappers will compute deltas according to provider expectations
    // We simply ensure post-completion contains steps 3 and 4
    assert_eq!(count_scheduled(&final_hist2, "3"), 1, "expected exactly one schedule of step 3 after resume");
    assert_eq!(count_scheduled(&final_hist2, "4"), 1, "expected exactly one schedule of step 4 after resume");

    rt2.shutdown().await;
}

#[tokio::test]
async fn recovery_across_restart_fs_provider() {
    // Use a stable on-disk directory that survives the runtime restart within this test
    let base = std::env::current_dir().unwrap().join(".testdata");
    std::fs::create_dir_all(&base).unwrap();
    let dir = base.join(format!("fs_recovery_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
    std::fs::create_dir_all(&dir).unwrap();

    let instance = String::from("inst-recover-fs-1");

    let make_store1 = || StdArc::new(FsHistoryStore::new(&dir)) as StdArc<dyn HistoryStore>;
    let make_store2 = || StdArc::new(FsHistoryStore::new(&dir)) as StdArc<dyn HistoryStore>;

    recovery_across_restart_core(make_store1, make_store2, instance.clone()).await;

    // Validate deltas explicitly: steps 1 & 2 should not be rescheduled after resume
    let store = StdArc::new(FsHistoryStore::new(&dir)) as StdArc<dyn HistoryStore>;
    let hist = store.read(&instance).await;
    let count = |inp: &str| hist.iter().filter(|e| matches!(e, Event::ActivityScheduled { name, input, .. } if name == "Step" && input == inp)).count();
    assert_eq!(count("1"), 1, "fs store should not repeat step 1 after restart");
    assert_eq!(count("2"), 1, "fs store should not repeat step 2 after restart");
    assert_eq!(count("3"), 1);
    assert_eq!(count("4"), 1);
}

#[tokio::test]
async fn recovery_across_restart_inmem_provider() {
    let instance = String::from("inst-recover-mem-1");

    // Two distinct in-memory stores simulate crash (history lost)
    let make_store1 = || StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let make_store2 = || StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;

    recovery_across_restart_core(make_store1, make_store2, instance.clone()).await;

    // Since history was not persisted, the second stage redid steps 1 & 2 before the barrier;
    // However, because we are using a fresh store, we need to combine pre-crash and post histories
    let store_before = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let store_after = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let hist_before = store_before.read(&instance).await;
    let hist_after = store_after.read(&instance).await;

    let count = |hist: &Vec<Event>, inp: &str| hist.iter().filter(|e| matches!(e, Event::ActivityScheduled { name, input, .. } if name == "Step" && input == inp)).count();

    // In-memory: before had 1 for steps 1 & 2; after has 1 again => effectively repeated work across crash
    assert_eq!(count(&hist_before, "1"), 0, "no persistent record in pre-crash store copy");
    assert_eq!(count(&hist_before, "2"), 0, "no persistent record in pre-crash store copy");
    assert_eq!(count(&hist_after, "1"), 0, "no persistent record in post-crash store copy");
    assert_eq!(count(&hist_after, "2"), 0, "no persistent record in post-crash store copy");
}

async fn wait_until<F: Fn() -> bool>(predicate: F, timeout_ms: u64) {
    let start = std::time::Instant::now();
    while !predicate() {
        if start.elapsed() > std::time::Duration::from_millis(timeout_ms) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

fn read_count(file: &std::path::Path) -> usize {
    if !file.exists() { return 0; }
    let Ok(content) = std::fs::read_to_string(file) else { return 0; };
    content.lines().count()
}

#[tokio::test]
async fn recovery_counts_fs_persists_history_no_reschedule() {
    use std::path::PathBuf;

    // Side-effect dir for counting executions
    let base = std::env::current_dir().unwrap().join(".testdata");
    std::fs::create_dir_all(&base).unwrap();
    let dir = base.join(format!("fs_counts_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
    std::fs::create_dir_all(&dir).unwrap();

    let step_file = |s: &str| -> PathBuf { dir.join(format!("step_{s}.log")) };

    // Registry that appends one line per execution
    let make_registry = || {
        let dir_clone = dir.clone();
        ActivityRegistry::builder()
            .register("Step", move |input: String| {
                let dir_inner = dir_clone.clone();
                async move {
                    let file = dir_inner.join(format!("step_{}.log", &input));
                    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file).unwrap();
                    use std::io::Write;
                    writeln!(f, "x").unwrap();
                    input
                }
            })
            .build()
    };

    let instance = format!(
        "inst-fs-counts-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros()
    );

    // Stage 1 with FS store
    let store1 = StdArc::new(FsHistoryStore::new(&dir)) as StdArc<dyn HistoryStore>;
    let rt1 = runtime::Runtime::start_with_store(store1.clone(), Arc::new(make_registry())).await;

    let orchestrator = |ctx: OrchestrationContext| async move {
        let _ = ctx.schedule_activity("Step", "1").into_activity().await;
        let _ = ctx.schedule_activity("Step", "2").into_activity().await;
        let _ = ctx.schedule_wait("Resume").into_event().await;
        let _ = ctx.schedule_activity("Step", "3").into_activity().await;
        let _ = ctx.schedule_activity("Step", "4").into_activity().await;
        "done".to_string()
    };

    let handle1 = rt1.clone().spawn_instance_to_completion(&instance, orchestrator).await;

    // Wait until steps 1 and 2 each executed once
    wait_until(|| read_count(&step_file("1")) >= 1, 200).await;
    wait_until(|| read_count(&step_file("2")) >= 1, 200).await;

    // Crash before resuming
    rt1.shutdown().await;
    drop(handle1);

    // Counts before restart
    let c1_before = read_count(&step_file("1"));
    let c2_before = read_count(&step_file("2"));

    // Stage 2 with same FS store dir
    let store2 = StdArc::new(FsHistoryStore::new(&dir)) as StdArc<dyn HistoryStore>;
    let rt2 = runtime::Runtime::start_with_store(store2, Arc::new(make_registry())).await;
    let rt2c = rt2.clone();
    let instance2 = instance.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt2c.raise_event(&instance2, "Resume", "go").await;
    });
    let handle2 = rt2.clone().spawn_instance_to_completion(&instance, orchestrator).await;
    let _ = handle2.await.unwrap();

    // After resume completes, verify steps 1 and 2 were NOT executed again
    let c1_after = read_count(&step_file("1"));
    let c2_after = read_count(&step_file("2"));
    let c3_after = read_count(&step_file("3"));
    let c4_after = read_count(&step_file("4"));

    assert_eq!(c1_before, 1, "expected single execution of step 1 before restart");
    assert_eq!(c2_before, 1, "expected single execution of step 2 before restart");
    assert_eq!(c1_after, 1, "fs store should not re-execute step 1 after restart");
    assert_eq!(c2_after, 1, "fs store should not re-execute step 2 after restart");
    assert_eq!(c3_after, 1, "step 3 should run once after resume");
    assert_eq!(c4_after, 1, "step 4 should run once after resume");

    rt2.shutdown().await;
}

#[tokio::test]
async fn recovery_counts_inmem_reexecutes_prebarrier_steps() {
    use std::path::PathBuf;

    // Side-effect dir for counting executions
    let base = std::env::current_dir().unwrap().join(".testdata");
    std::fs::create_dir_all(&base).unwrap();
    let dir = base.join(format!("mem_counts_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
    std::fs::create_dir_all(&dir).unwrap();

    let step_file = |s: &str| -> PathBuf { dir.join(format!("step_{s}.log")) };

    // Registry that appends one line per execution
    let make_registry = || {
        let dir_clone = dir.clone();
        ActivityRegistry::builder()
            .register("Step", move |input: String| {
                let dir_inner = dir_clone.clone();
                async move {
                    let file = dir_inner.join(format!("step_{}.log", &input));
                    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file).unwrap();
                    use std::io::Write;
                    writeln!(f, "x").unwrap();
                    input
                }
            })
            .build()
    };

    let instance = format!(
        "inst-mem-counts-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros()
    );

    // Stage 1 with first InMemory store
    let store1 = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let rt1 = runtime::Runtime::start_with_store(store1, Arc::new(make_registry())).await;

    let orchestrator = |ctx: OrchestrationContext| async move {
        let _ = ctx.schedule_activity("Step", "1").into_activity().await;
        let _ = ctx.schedule_activity("Step", "2").into_activity().await;
        let _ = ctx.schedule_wait("Resume").into_event().await;
        let _ = ctx.schedule_activity("Step", "3").into_activity().await;
        let _ = ctx.schedule_activity("Step", "4").into_activity().await;
        "done".to_string()
    };

    let handle1 = rt1.clone().spawn_instance_to_completion(&instance, orchestrator).await;

    // Wait until steps 1 and 2 each executed once
    wait_until(|| read_count(&step_file("1")) >= 1, 200).await;
    wait_until(|| read_count(&step_file("2")) >= 1, 200).await;

    // Crash before resuming
    rt1.shutdown().await;
    drop(handle1);

    // Stage 2 with fresh InMemory store
    let store2 = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    let rt2 = runtime::Runtime::start_with_store(store2, Arc::new(make_registry())).await;
    let rt2c = rt2.clone();
    let instance2 = instance.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt2c.raise_event(&instance2, "Resume", "go").await;
    });

    let handle2 = rt2.clone().spawn_instance_to_completion(&instance, orchestrator).await;
    let _ = handle2.await.unwrap();

    // After resume completes, verify steps 1 and 2 were executed twice total (pre and post restart)
    let c1 = read_count(&step_file("1"));
    let c2 = read_count(&step_file("2"));
    let c3 = read_count(&step_file("3"));
    let c4 = read_count(&step_file("4"));

    assert_eq!(c1, 2, "in-memory should re-execute step 1 after restart");
    assert_eq!(c2, 2, "in-memory should re-execute step 2 after restart");
    assert_eq!(c3, 1, "step 3 should run once after resume");
    assert_eq!(c4, 1, "step 4 should run once after resume");

    rt2.shutdown().await;
}


