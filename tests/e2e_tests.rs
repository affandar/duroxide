use futures::future::{join3, select, Either, join};
//
use rust_dtf::{run_turn, Action, Event, Executor, OrchestrationContext, DurableOutput};

#[test]
fn orchestrator_completes_and_replays_deterministically() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let start = ctx.now_ms();
        let _id = ctx.new_guid();

        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(500);
        let f_e = ctx.schedule_wait("Go");

        let (o_a, _o_t, o_e) = join3(f_a, f_t, f_e).await;

        let a = match o_a { DurableOutput::Activity(v) => v, _ => unreachable!("A must be activity result") };
        let evt = match o_e { DurableOutput::External(v) => v, _ => unreachable!("Go must be external event") };

        let b = ctx.schedule_activity("B", a.clone()).into_activity().await;
        format!("id=_hidden, start={start}, evt={evt}, b={b}")
    };

    let history: Vec<Event> = Vec::new();
    // Host processes only actions and appends completion events
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        for a in actions {
            match a {
                Action::CallActivity { id, name, input } => {
                    let result = match name.as_str() {
                        "A" => input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string(),
                        "B" => format!("{input}!"),
                        _ => format!("echo:{input}"),
                    };
                    history.push(Event::ActivityCompleted { id, result });
                }
                Action::CreateTimer { id, .. } => {
                    // Look up created fire_at_ms in history by id
                    let fire_at_ms = history
                        .iter()
                        .rev()
                        .find_map(|e| match e { Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None })
                        .unwrap_or_else(|| 0);
                    history.push(Event::TimerFired { id, fire_at_ms });
                }
                Action::WaitExternal { id, name } => {
                    history.push(Event::ExternalEvent { id, name, data: "ok".to_string() });
                }
            }
        }
    };

    // Drive to completion
    let (final_history, output) = Executor::drive_to_completion(history.clone(), orchestrator, host_execute_actions);
    assert!(output.contains("evt=ok"));
    assert!(output.contains("b=2!"));
    assert_eq!(final_history.len(), 8, "expected 8 history events (scheduled + completed)");

    // Replay to verify determinism
    let (_h2, acts2, out2) = run_turn(final_history.clone(), orchestrator);
    assert!(acts2.is_empty(), "replay should not produce new actions");
    assert_eq!(out2.unwrap(), output);
}


#[test]
fn any_of_three_returns_first_is_activity() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(500);
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

    let history: Vec<Event> = Vec::new();
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        for a in actions {
            match a {
                Action::CallActivity { id, name, input } => {
                    let result = match name.as_str() {
                        "A" => input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string(),
                        _ => format!("echo:{input}"),
                    };
                    history.push(Event::ActivityCompleted { id, result });
                }
                Action::CreateTimer { id, .. } => {
                    let fire_at_ms = history
                        .iter()
                        .rev()
                        .find_map(|e| match e { Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None })
                        .unwrap_or_else(|| 0);
                    history.push(Event::TimerFired { id, fire_at_ms });
                }
                Action::WaitExternal { id, name } => {
                    history.push(Event::ExternalEvent { id, name, data: "ok".to_string() });
                }
            }
        }
    };

    let (_final_history, output) = Executor::drive_to_completion(history, orchestrator, host_execute_actions);
    assert!(output.starts_with("winner=A:"), "expected activity to win deterministically, got {output}");
}

#[test]
fn any_of_three_winner_then_staggered_completions() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(500);
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

    let mut pending_timer: Option<(u64, u64)> = None; // (id, fire_at)
    let mut pending_external: Option<(u64, String)> = None; // (id, name)
    let mut step: u32 = 0;

    let history: Vec<Event> = Vec::new();
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        match step {
            0 => {
                // First iteration: complete activity only; stash timer/external for later
                for a in actions {
                    match a {
                        Action::CallActivity { id, name, input } => {
                            let result = match name.as_str() {
                                "A" => input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string(),
                                _ => format!("echo:{input}"),
                            };
                            history.push(Event::ActivityCompleted { id, result });
                        }
                        Action::CreateTimer { id, .. } => {
                            // Lookup TimerCreated to learn fire time; store to complete later
                            let fire_at_ms = history
                                .iter()
                                .rev()
                                .find_map(|e| match e { Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None })
                                .unwrap_or_else(|| 0);
                            pending_timer = Some((id, fire_at_ms));
                        }
                        Action::WaitExternal { id, name } => {
                            pending_external = Some((id, name));
                        }
                    }
                }
                step = 1;
            }
            1 => {
                // Second iteration: fire the timer if pending
                if let Some((id, fire_at_ms)) = pending_timer.take() {
                    history.push(Event::TimerFired { id, fire_at_ms });
                }
                step = 2;
            }
            2 => {
                // Third iteration: raise the external if pending
                if let Some((id, name)) = pending_external.take() {
                    history.push(Event::ExternalEvent { id, name, data: "ok".to_string() });
                }
                step = 3;
            }
            _ => {}
        }
    };

    let (final_history, output) = Executor::drive_to_completion(history, orchestrator, host_execute_actions);
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


#[test]
fn sequential_activity_chain_completes() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let a = ctx.schedule_activity("A", "1").into_activity().await;
        let b = ctx.schedule_activity("B", a).into_activity().await;
        let c = ctx.schedule_activity("C", b).into_activity().await;
        format!("c={c}")
    };

    let history: Vec<Event> = Vec::new();
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        for a in actions {
            match a {
                Action::CallActivity { id, name, input } => {
                    let result = match name.as_str() {
                        // A: parse int, increment
                        "A" => input.parse::<i32>().map(|x| x + 1).unwrap_or(0).to_string(),
                        // B: append a marker
                        "B" => format!("{input}b"),
                        // C: append another marker
                        "C" => format!("{input}c"),
                        _ => format!("echo:{input}"),
                    };
                    history.push(Event::ActivityCompleted { id, result });
                }
                Action::CreateTimer { id, .. } => {
                    let fire_at_ms = history
                        .iter()
                        .rev()
                        .find_map(|e| match e { Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None })
                        .unwrap_or_else(|| 0);
                    history.push(Event::TimerFired { id, fire_at_ms });
                }
                Action::WaitExternal { id, name } => {
                    history.push(Event::ExternalEvent { id, name, data: "ok".to_string() });
                }
            }
        }
    };

    let (final_history, output) = Executor::drive_to_completion(history, orchestrator, host_execute_actions);
    assert_eq!(output, "c=2bc");
    assert_eq!(final_history.len(), 6, "expected exactly three scheduled+completed activity pairs in history");
}

#[test]
fn complex_control_flow_orchestration() {
    use futures::future::select;
    use futures::future::Either;

    // Orchestrator with:
    // - retry loop with backoff timer until Fetch succeeds
    // - branch using select between timer and external
    // - sequential loop of dependent activities
    let orchestrator = |ctx: OrchestrationContext| async move {
        // Retry Fetch until it returns "ok"; backoff 100ms between attempts
        let mut attempts = 0u32;
        loop {
            let res = ctx.schedule_activity("Fetch", attempts.to_string()).into_activity().await;
            if res == "ok" { break; }
            attempts += 1;
            ctx.schedule_timer(100).into_timer().await;
        }

        // Branch: race a short timer vs an external event; with our deterministic host ordering
        // and scheduling order (timer polled before wait), the timer should win here.
        let race = select(ctx.schedule_timer(50), ctx.schedule_wait("Proceed"));
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

    // Host that simulates two transient failures for Fetch, then success.
    let mut fetch_calls = 0u32;
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        for a in actions {
            match a {
                Action::CallActivity { id, name, input } => {
                    let result = match name.as_str() {
                        // First two Fetch calls fail (return "err"), third succeeds ("ok").
                        "Fetch" => {
                            fetch_calls += 1;
                            if fetch_calls < 3 { "err".to_string() } else { "ok".to_string() }
                        }
                        // Acc appends a marker to accumulate deterministic string
                        "Acc" => format!("{input}:x"),
                        _ => format!("echo:{input}"),
                    };
                    history.push(Event::ActivityCompleted { id, result });
                }
                Action::CreateTimer { id, .. } => {
                    let fire_at_ms = history
                        .iter()
                        .rev()
                        .find_map(|e| match e { Event::TimerCreated { id: cid, fire_at_ms } if *cid == id => Some(*fire_at_ms), _ => None })
                        .unwrap_or_else(|| 0);
                    history.push(Event::TimerFired { id, fire_at_ms });
                }
                Action::WaitExternal { id, name } => {
                    history.push(Event::ExternalEvent { id, name, data: "go".to_string() });
                }
            }
        }
    };

    let (final_history, output) = Executor::drive_to_completion(Vec::new(), orchestrator, host_execute_actions);

    // Expectations:
    // - Fetch attempts = 2 backoffs (3rd attempt succeeds), so attempts==2 when we break
    // - The branch should be "timer" given we schedule timer before wait and host appends Timer before External
    // - Acc runs 3 times, each appending ":x"; starting from "seed" and 3 iterations with suffix numbers
    assert!(output.contains("attempts=2"), "unexpected attempts count: {output}");
    assert!(output.contains("branch=timer"), "unexpected branch winner: {output}");
    assert!(output.contains("acc=seed-0:x-1:x-2:x"), "unexpected accumulator: {output}");

    // History counts with correlated scheduling:
    // - Fetch called 3 times -> 3 ActivityScheduled + 3 ActivityCompleted = 6
    // - 2 backoff timers (100ms each) -> 2 TimerCreated + 2 TimerFired = 4
    // - Branch: one 50ms timer and one External -> 1 TimerCreated + 1 TimerFired + 1 ExternalSubscribed + 1 ExternalEvent = 4
    // - Acc called 3 times -> 3 ActivityScheduled + 3 ActivityCompleted = 6
    // Total = 6 + 4 + 4 + 6 = 20 events
    assert_eq!(final_history.len(), 20, "unexpected final history length: {:?}", final_history);
}


