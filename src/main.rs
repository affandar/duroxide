use rust_dtf_experiment::{Action, Event, Executor, OrchestrationContext, run_turn};

async fn my_orchestrator(ctx: OrchestrationContext) -> String {
    let start = ctx.now_ms();
    let id = ctx.new_guid();

    let a = ctx.call_activity("A", "1").await;
    ctx.timer(500).await;
    let evt = ctx.wait_external("Go").await;
    let b = ctx.call_activity("B", a.clone()).await;

    format!("id={id}, start={start}, evt={evt}, b={b}")
}

fn host_execute_actions(actions: Vec<Action>, history: &mut Vec<Event>) {
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
}

fn main() {
    let history: Vec<Event> = Vec::new();
    let (final_history, output) = Executor::drive_to_completion(history, |ctx| my_orchestrator(ctx), host_execute_actions);
    println!("COMPLETED: {output}");

    println!("\nReplaying to verify determinism...");
    let (h2, acts2, out2) = run_turn(final_history.clone(), |ctx| my_orchestrator(ctx));
    assert!(acts2.is_empty());
    println!("REPLAY COMPLETED: {}", out2.unwrap());
    assert_eq!(h2.len(), final_history.len());
}


