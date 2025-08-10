use futures::future::join3;
use rust_dtf_experiment::{run_turn, Action, Event, Executor, OrchestrationContext, UnifiedOutput};

#[test]
fn orchestrator_completes_and_replays_deterministically() {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let start = ctx.now_ms();
        let _id = ctx.new_guid();

        let f_a = ctx.call_unified("A", "1");
        let f_t = ctx.timer_unified(500);
        let f_e = ctx.wait_unified("Go");

        let (o_a, _o_t, o_e) = join3(f_a, f_t, f_e).await;

        let a = match o_a { UnifiedOutput::Activity(v) => v, _ => unreachable!("A must be activity result") };
        let evt = match o_e { UnifiedOutput::External(v) => v, _ => unreachable!("Go must be external event") };

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


