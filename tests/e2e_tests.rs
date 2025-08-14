use futures::future::{join3, select, Either, join};
use std::sync::Arc;
use rust_dtf::{run_turn, Event, OrchestrationContext, DurableOutput, Action};
use rust_dtf::runtime::{self, activity::ActivityRegistry};

#[tokio::test]
async fn orchestrator_completes_and_replays_deterministically() {
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

    // Shared runtime
    let rt = runtime::Runtime::start(Arc::new(registry)).await;

    // Drive to completion via message-driven runtime; raise external "Go" shortly after
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

    // Replay to verify determinism
    let (_h2, acts2, out2) = run_turn(final_history.clone(), orchestrator);
    assert!(acts2.is_empty(), "replay should not produce new actions");
    assert_eq!(out2.unwrap(), output);

    rt.shutdown().await;
}

#[tokio::test]
async fn any_of_three_returns_first_is_activity() {
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

    let rt = runtime::Runtime::start(Arc::new(registry)).await;
    // Raise external slightly later to allow activity/timer to contend
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
async fn any_of_three_winner_then_staggered_completions() {
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

    let rt = runtime::Runtime::start(Arc::new(registry)).await;
    // Stagger external arrival to be last
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-stagger-1", "Go", "ok").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-stagger-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();
    assert!(output.starts_with("winner=A:"), "expected activity to win deterministically, got {output}");

    // Verify completion ordering across turns: ActivityCompleted before TimerFired before ExternalEvent
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

    // Still 6 events total: 3 scheduled/subscribed + 3 completions
    assert_eq!(final_history.len(), 6, "unexpected final history length: {:?}", final_history);
    rt.shutdown().await;
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

#[tokio::test]
async fn sequential_activity_chain_completes() {
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

    let rt = runtime::Runtime::start(Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-seq-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();
    assert_eq!(output, "c=2bc");
    assert_eq!(final_history.len(), 6, "expected exactly three scheduled+completed activity pairs in history");
    rt.shutdown().await;
}

#[tokio::test]
async fn complex_control_flow_orchestration() {
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

    let rt = runtime::Runtime::start(Arc::new(registry)).await;
    // Raise the Proceed external, but timers should win given shorter delay
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-complex-1", "Proceed", "go").await;
    });
    let handle = rt.clone().spawn_instance_to_completion("inst-complex-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    // Expectations:
    // - Fetch attempts = 2 backoffs (3rd attempt succeeds), so attempts==2 when we break
    // - The branch should be "timer" given we schedule timer before wait and host appends Timer before External
    // - Acc runs 3 times, each appending ":x"; starting from "seed" and 3 iterations with suffix numbers
    assert!(output.contains("attempts=2"), "unexpected attempts count: {output}");
    assert!(output.contains("branch=timer"), "unexpected branch winner: {output}");
    assert!(output.contains("acc=seed-0:x-1:x-2:x"), "unexpected accumulator: {output}");

    // History counts with correlated scheduling in message-driven runtime:
    // - Fetch called 3 times -> 3 ActivityScheduled + 3 ActivityCompleted = 6
    // - 2 backoff timers (100ms each) -> 2 TimerCreated + 2 TimerFired = 4
    // - Branch: one 50ms timer scheduled and external subscribed -> 1 TimerCreated + 1 TimerFired + 1 ExternalSubscribed = 3
    //   The ExternalEvent("Proceed") may or may not be appended before completion depending on timing (±1 event)
    // - Acc called 3 times -> 3 ActivityScheduled + 3 ActivityCompleted = 6
    // Total expected = 19 or 20
    let has_ext_event = final_history.iter().any(|e| matches!(e, Event::ExternalEvent { name, .. } if name == "Proceed"));
    let expected_len = if has_ext_event { 20 } else { 19 };
    assert_eq!(final_history.len(), expected_len, "unexpected final history length: {:?}", final_history);

    rt.shutdown().await;
}


#[tokio::test]
async fn wait_external_completes() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let data = ctx.schedule_wait("Only").into_event().await;
        format!("only={data}")
    };

    // No activities needed
    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start(Arc::new(registry)).await;

    // Raise external shortly after start
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        rt_clone.raise_event("inst-wait-1", "Only", "payload").await;
    });

    let handle = rt.clone().spawn_instance_to_completion("inst-wait-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "only=payload");
    // Expect exactly subscription + event
    assert!(matches!(final_history[0], Event::ExternalSubscribed { .. }));
    assert!(matches!(final_history[1], Event::ExternalEvent { .. }));
    assert_eq!(final_history.len(), 2);

    rt.shutdown().await;
}

#[tokio::test]
async fn external_sent_before_instance_is_ignored() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let data = ctx.schedule_wait("Only").into_event().await;
        format!("only={data}")
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start(Arc::new(registry)).await;

    // Send event before instance has registered; should be ignored
    rt.raise_event("inst-wait-ignore-1", "Only", "early").await;

    // After a short delay, send the real event and then start the instance
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
        rt_clone.raise_event("inst-wait-ignore-1", "Only", "late").await;
    });

    let handle = rt.clone().spawn_instance_to_completion("inst-wait-ignore-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    // Should not have delivered the early event; only the later one is observed
    assert_eq!(output, "only=late");
    assert!(final_history.iter().any(|e| matches!(e, Event::ExternalEvent { name, data, .. } if name == "Only" && data == "late")));
    assert!(!final_history.iter().any(|e| matches!(e, Event::ExternalEvent { name, data, .. } if name == "Only" && data == "early")));
    assert_eq!(final_history.len(), 2, "expected only subscribe + single event");

    rt.shutdown().await;
}

#[tokio::test]
async fn race_external_vs_timer_ordering() {
    use futures::future::{select, Either};

    let orchestrator = |ctx: OrchestrationContext| async move {
        let race = select(ctx.schedule_timer(3), ctx.schedule_wait("Race"));
        match race.await {
            Either::Left((_t, _e)) => "timer".to_string(),
            Either::Right((_e, _t)) => "external".to_string(),
        }
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start(Arc::new(registry)).await;

    // Raise external after timer should have fired, so timer wins
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        rt_clone.raise_event("inst-race-order-1", "Race", "ok").await;
    });

    let handle = rt.clone().spawn_instance_to_completion("inst-race-order-1", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "timer");
    // Ensure TimerFired occurs and external is either absent or after timer
    let idx_t = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })).unwrap();
    if let Some(idx_e) = final_history.iter().position(|e| matches!(e, Event::ExternalEvent { .. })) {
        assert!(idx_t < idx_e, "expected timer to fire before external: {final_history:#?}");
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn race_event_vs_timer_event_wins() {
    use futures::future::{select, Either};

    let orchestrator = |ctx: OrchestrationContext| async move {
        let race = select(ctx.schedule_timer(6), ctx.schedule_wait("Race"));
        match race.await {
            Either::Left((_t, _e)) => "timer".to_string(),
            Either::Right((_e, _t)) => "external".to_string(),
        }
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start(Arc::new(registry)).await;

    // Raise external before timer can fire so external wins
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        rt_clone.raise_event("inst-race-order-2", "Race", "ok").await;
    });

    let handle = rt.clone().spawn_instance_to_completion("inst-race-order-2", orchestrator).await;
    let (final_history, output) = handle.await.unwrap();

    assert_eq!(output, "external");
    // If both present, ensure external event is before timer fired (or timer may be absent)
    let idx_e = final_history.iter().position(|e| matches!(e, Event::ExternalEvent { .. })).unwrap();
    if let Some(idx_t) = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })) {
        assert!(idx_e < idx_t, "expected external before timer: {final_history:#?}");
    }

    rt.shutdown().await;
}


