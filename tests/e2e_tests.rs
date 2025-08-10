use futures::future::{join3, select, Either};
use futures::FutureExt;
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

        let b = ctx.call_activity("B", a.clone()).await;
        format!("id=_hidden, start={start}, evt={evt}, b={b}")
    };

    let history: Vec<Event> = Vec::new();
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        for a in actions {
            match a {
                Action::CallActivity { name, input } => {
                    let result = match name.as_str() {
                        "A" => {
                            let x: i32 = input.parse().unwrap_or(0);
                            (x + 1).to_string()
                        }
                        "B" => format!("{input}!"),
                        _ => format!("echo:{input}"),
                    };
                    history.push(Event::ActivityResult { name, input, result });
                }
                Action::CreateTimer { delay_ms } => {
                    let last_time = history
                        .iter()
                        .rev()
                        .find_map(|e| match e { Event::TimerFired { fire_at_ms } => Some(*fire_at_ms), _ => None })
                        .unwrap_or(0);
                    history.push(Event::TimerFired { fire_at_ms: last_time + delay_ms });
                }
                Action::WaitExternal { name } => {
                    history.push(Event::ExternalEvent { name, data: "ok".to_string() });
                }
            }
        }
    };

    // Drive to completion
    let (final_history, output) = Executor::drive_to_completion(history.clone(), orchestrator, host_execute_actions);
    assert!(output.contains("evt=ok"));
    assert!(output.contains("b=2!"));
    assert_eq!(final_history.len(), 4, "expected 4 history events");

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

        // Race three futures; winner should be the Activity result given our deterministic scheduling and host
        let left = select(f_a.boxed_local(), f_t.boxed_local());
        let race = select(left, f_e.boxed_local());

        match race.await {
            Either::Left((Either::Left((DurableOutput::Activity(a), _other)), _third)) => format!("winner=A:{a}"),
            Either::Left((Either::Right((DurableOutput::Timer, _other)), _third)) => "winner=T".to_string(),
            Either::Right((DurableOutput::External(e), _left)) => format!("winner=E:{e}"),
            _ => panic!("unexpected winner variant"),
        }
    };

    let history: Vec<Event> = Vec::new();
    let host_execute_actions = |actions: Vec<Action>, history: &mut Vec<Event>| {
        for a in actions {
            match a {
                Action::CallActivity { name, input } => {
                    let result = match name.as_str() {
                        "A" => {
                            let x: i32 = input.parse().unwrap_or(0);
                            (x + 1).to_string()
                        }
                        _ => format!("echo:{input}"),
                    };
                    history.push(Event::ActivityResult { name, input, result });
                }
                Action::CreateTimer { delay_ms } => {
                    let last_time = history
                        .iter()
                        .rev()
                        .find_map(|e| match e { Event::TimerFired { fire_at_ms } => Some(*fire_at_ms), _ => None })
                        .unwrap_or(0);
                    history.push(Event::TimerFired { fire_at_ms: last_time + delay_ms });
                }
                Action::WaitExternal { name } => {
                    history.push(Event::ExternalEvent { name, data: "ok".to_string() });
                }
            }
        }
    };

    let (_final_history, output) = Executor::drive_to_completion(history, orchestrator, host_execute_actions);
    assert!(output.starts_with("winner=A:"), "expected activity to win deterministically, got {output}");
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


