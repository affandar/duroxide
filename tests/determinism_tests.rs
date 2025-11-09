use duroxide::providers::Provider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Action, ActivityContext, DurableOutput, Event, OrchestrationContext, OrchestrationRegistry, run_turn};
use std::sync::Arc as StdArc;
use std::sync::Arc;
mod common;

async fn orchestration_completes_and_replays_deterministically_with(store: StdArc<dyn Provider>) {
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let start: u64 = 0; // deterministic logical start for test
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(50); // Longer timer for reliability

        let outputs = ctx.join(vec![f_a, f_t]).await;

        // Extract outputs by type since join returns in history order
        let mut o_a = None;

        for output in outputs {
            match output {
                DurableOutput::Activity(_) => o_a = Some(output),
                DurableOutput::Timer => {} // ignore timer
                _ => {}
            }
        }

        let o_a = o_a.expect("Activity result not found");

        let a = match o_a {
            DurableOutput::Activity(v) => v.unwrap(),
            _ => unreachable!("A must be activity result"),
        };

        let b = ctx.schedule_activity("B", a.clone()).into_activity().await.unwrap();
        Ok(format!("id=_hidden, start={start}, a={a}, b={b}"))
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |_ctx: ActivityContext, input: String| async move {
            Ok(input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string())
        })
        .register("B", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("B({input})"))
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("DeterministicTest", orchestration)
        .build();

    let _rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    let client = duroxide::Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("deterministic-instance", "DeterministicTest", "")
        .await
        .unwrap();

    // Wait for completion
    let status = client
        .wait_for_orchestration("deterministic-instance", std::time::Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(
        status,
        duroxide::runtime::OrchestrationStatus::Completed { .. }
    ));

    // Get the result
    let result = match status {
        duroxide::runtime::OrchestrationStatus::Completed { output } => output,
        _ => panic!("Expected completed status"),
    };

    // Verify deterministic result
    assert_eq!(result, "id=_hidden, start=0, a=2, b=B(2)");

    // Test replay determinism by restarting with same store
    let orchestration2 = |ctx: OrchestrationContext, _input: String| async move {
        let start: u64 = 0;
        let f_a = ctx.schedule_activity("A", "1");
        let f_t = ctx.schedule_timer(50); // Longer timer for reliability

        let outputs = ctx.join(vec![f_a, f_t]).await;

        let mut o_a = None;

        for output in outputs {
            match output {
                DurableOutput::Activity(_) => o_a = Some(output),
                DurableOutput::Timer => {}
                _ => {}
            }
        }

        let o_a = o_a.expect("Activity result not found");

        let a = match o_a {
            DurableOutput::Activity(v) => v.unwrap(),
            _ => unreachable!("A must be activity result"),
        };

        let b = ctx.schedule_activity("B", a.clone()).into_activity().await.unwrap();
        Ok(format!("id=_hidden, start={start}, a={a}, b={b}"))
    };

    let activity_registry2 = ActivityRegistry::builder()
        .register("A", |_ctx: ActivityContext, input: String| async move {
            Ok(input.parse::<i32>().unwrap_or(0).saturating_add(1).to_string())
        })
        .register("B", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("B({input})"))
        })
        .build();

    let orchestration_registry2 = OrchestrationRegistry::builder()
        .register("DeterministicTest", orchestration2)
        .build();

    let _rt2 =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry2), orchestration_registry2)
            .await;

    let client2 = duroxide::Client::new(store.clone());

    // Start orchestration with same instance ID
    client2
        .start_orchestration("deterministic-instance", "DeterministicTest", "")
        .await
        .unwrap();

    // Wait for completion
    let status2 = client2
        .wait_for_orchestration("deterministic-instance", std::time::Duration::from_secs(10))
        .await
        .unwrap();

    assert!(matches!(
        status2,
        duroxide::runtime::OrchestrationStatus::Completed { .. }
    ));

    // Verify same result
    let result2 = match status2 {
        duroxide::runtime::OrchestrationStatus::Completed { output } => output,
        _ => panic!("Expected completed status"),
    };

    assert_eq!(result2, result); // Should be identical
}

#[tokio::test]
async fn test_deterministic_replay_with_sqlite() {
    let store = StdArc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    orchestration_completes_and_replays_deterministically_with(store).await;
}

#[tokio::test]
async fn test_trace_deterministic_in_history() {
    let activities = ActivityRegistry::builder()
        .register("GetValue", |_ctx: ActivityContext, _: String| async move {
            Ok("test".to_string())
        })
        .build();

    let orch = |ctx: OrchestrationContext, _: String| async move {
        ctx.trace_info("Test trace message");
        let result = ctx.schedule_activity("GetValue", "").into_activity().await?;
        ctx.trace_warn("Warning message");
        ctx.trace_error("Error message");
        Ok(result)
    };

    let history_store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let orchestration_registry = OrchestrationRegistry::builder().register("test_orch", orch).build();
    let rt =
        runtime::Runtime::start_with_store(history_store.clone(), Arc::new(activities), orchestration_registry).await;
    let client = duroxide::Client::new(history_store.clone());

    // Start orchestration
    client.start_orchestration("instance-2", "test_orch", "").await.unwrap();

    // Wait for completion
    let status = client
        .wait_for_orchestration("instance-2", std::time::Duration::from_millis(1000))
        .await
        .unwrap();
    assert!(matches!(
        status,
        duroxide::runtime::OrchestrationStatus::Completed { .. }
    ));

    // Check history contains trace system calls
    let history = history_store.read("instance-2").await.unwrap_or_default();
    let trace_events: Vec<_> = history
        .iter()
        .filter(|e| matches!(e, duroxide::Event::SystemCall { op, .. } if op.starts_with("trace:")))
        .collect();

    assert_eq!(trace_events.len(), 3, "Expected 3 trace events (info, warn, error)");

    // Verify trace events are in correct order
    let trace_ops: Vec<&str> = trace_events
        .iter()
        .map(|e| match e {
            duroxide::Event::SystemCall { op, .. } => op.as_str(),
            _ => unreachable!(),
        })
        .collect();

    assert_eq!(
        trace_ops,
        vec![
            "trace:INFO:Test trace message",
            "trace:WARN:Warning message",
            "trace:ERROR:Error message"
        ]
    );

    // Shutdown runtime to clean up background tasks
    rt.shutdown(None).await;
}

// ============================================================================
// UNIT TESTS FOR DETERMINISTIC BEHAVIOR
// ============================================================================

#[test]
fn action_emission_single_turn() {
    // Await the scheduled activity once so it is polled and records its action, then remain pending
    let orchestrator = |ctx: OrchestrationContext| async move {
        let _ = ctx.schedule_activity("A", "1").into_activity().await;
        unreachable!()
    };

    let history: Vec<Event> = Vec::new();
    let (hist_after, actions, out) = run_turn(history, orchestrator);
    assert!(out.is_none(), "must not complete in first turn");
    assert_eq!(actions.len(), 1, "exactly one action expected");
    match &actions[0] {
        Action::CallActivity { name, input, .. } => {
            assert_eq!(name, "A");
            assert_eq!(input, "1");
        }
        _ => panic!("unexpected action kind"),
    }
    // History should already contain ActivityScheduled
    assert!(matches!(hist_after[0], Event::ActivityScheduled { .. }));
}

#[tokio::test]
async fn deterministic_replay_activity_only() {
    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let a = ctx.schedule_activity("A", "2").into_activity().await.unwrap();
        Ok(a)
    };

    let activity_registry = ActivityRegistry::builder()
        .register("A", |_ctx: ActivityContext, input: String| async move {
            Ok(format!("A({input})"))
        })
        .build();

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ActivityOnly", orchestrator)
        .build();

    let store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );

    let _rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;

    let client = duroxide::Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("activity-only-instance", "ActivityOnly", "")
        .await
        .unwrap();

    // Wait for completion
    let status = client
        .wait_for_orchestration("activity-only-instance", std::time::Duration::from_secs(5))
        .await
        .unwrap();

    assert!(matches!(
        status,
        duroxide::runtime::OrchestrationStatus::Completed { .. }
    ));

    if let duroxide::runtime::OrchestrationStatus::Completed { output } = status {
        assert_eq!(output, "A(2)");
    }
}

#[tokio::test]
async fn test_trace_fire_and_forget() {
    let activities = ActivityRegistry::builder()
        .register("DoWork", |_ctx: ActivityContext, _: String| async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            Ok("done".to_string())
        })
        .build();

    let orch = |ctx: OrchestrationContext, _: String| async move {
        // Traces should not block execution
        ctx.trace_info("Starting work");
        let future1 = ctx.schedule_activity("DoWork", "1");
        ctx.trace_info("Scheduled first activity");
        let future2 = ctx.schedule_activity("DoWork", "2");
        ctx.trace_info("Scheduled second activity");

        // Wait for both activities
        let result1 = future1.into_activity().await?;
        let result2 = future2.into_activity().await?;

        ctx.trace_info("Both activities completed");
        Ok(format!("{result1} {result2}"))
    };

    let history_store = Arc::new(
        duroxide::providers::sqlite::SqliteProvider::new_in_memory()
            .await
            .unwrap(),
    );
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("test_trace_fire_and_forget", orch)
        .build();
    let rt =
        runtime::Runtime::start_with_store(history_store.clone(), Arc::new(activities), orchestration_registry).await;
    let client = duroxide::Client::new(history_store.clone());

    // Start orchestration
    client
        .start_orchestration("instance-trace-fire-and-forget", "test_trace_fire_and_forget", "")
        .await
        .unwrap();

    // Wait for completion
    let status = client
        .wait_for_orchestration("instance-trace-fire-and-forget", std::time::Duration::from_millis(2000))
        .await
        .unwrap();
    assert!(matches!(
        status,
        duroxide::runtime::OrchestrationStatus::Completed { .. }
    ));

    // Check history contains trace system calls
    let history = history_store.read("instance-trace-fire-and-forget").await.unwrap_or_default();
    let trace_events: Vec<_> = history
        .iter()
        .filter(|e| matches!(e, duroxide::Event::SystemCall { op, .. } if op.starts_with("trace:")))
        .collect();

    assert_eq!(
        trace_events.len(),
        4,
        "Expected 4 trace events (3 info traces + potentially others)"
    );

    // Verify trace events are in correct order
    let trace_ops: Vec<&str> = trace_events
        .iter()
        .map(|e| match e {
            duroxide::Event::SystemCall { op, .. } => op.as_str(),
            _ => unreachable!(),
        })
        .collect();

    assert!(trace_ops.contains(&"trace:INFO:Starting work"));
    assert!(trace_ops.contains(&"trace:INFO:Scheduled first activity"));
    assert!(trace_ops.contains(&"trace:INFO:Scheduled second activity"));
    assert!(trace_ops.contains(&"trace:INFO:Both activities completed"));

    drop(rt);
}
