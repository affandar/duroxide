use duroxide::providers::Provider;
use futures::future::{Either, select};
use std::sync::Arc;
mod common;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self, RuntimeOptions};
use duroxide::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;

// Helper for timing-sensitive race tests
fn fast_runtime_options() -> RuntimeOptions {
    RuntimeOptions {
        dispatcher_idle_sleep_ms: 10,
        ..Default::default()
    }
}

async fn wait_external_completes_with(store: StdArc<dyn Provider>) {
    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let data = ctx.schedule_wait("Only").into_event().await;
        Ok(format!("only={data}"))
    };

    let activity_registry = ActivityRegistry::builder().build();
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("WaitExternal", orchestrator)
        .build();

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        fast_runtime_options(),
    )
    .await;
    let store_for_wait = store.clone();
    let client_for_event = duroxide::Client::new(store.clone());
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-wait-1", "Only", 1000).await;
        let _ = client_for_event.raise_event("inst-wait-1", "Only", "payload").await;
    });
    let client = duroxide::Client::new(store.clone());
    duroxide::Client::new(store.clone())
        .start_orchestration("inst-wait-1", "WaitExternal", "")
        .await
        .unwrap();

    match duroxide::Client::new(store.clone())
        .wait_for_orchestration("inst-wait-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "only=payload"),
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for expected events
    let final_history = client.read_execution_history("inst-wait-1", 1).await.unwrap();
    // First event is OrchestrationStarted; then subscription, event, and terminal completion
    assert!(matches!(final_history[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(final_history[1], Event::ExternalSubscribed { .. }));
    assert!(matches!(final_history[2], Event::ExternalEvent { .. }));
    assert!(matches!(
        final_history.last().unwrap(),
        Event::OrchestrationCompleted { .. }
    ));
    assert_eq!(final_history.len(), 4);

    rt.shutdown(None).await;
}

#[tokio::test]
async fn wait_external_completes_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    wait_external_completes_with(store).await;
}

async fn race_external_vs_timer_ordering_with(store: StdArc<dyn Provider>) {
    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let race = select(ctx.schedule_timer(10), ctx.schedule_wait("Race"));
        match race.await {
            Either::Left((_t, _e)) => Ok("timer".to_string()),
            Either::Right((_e, _t)) => Ok("external".to_string()),
        }
    };

    let activity_registry = ActivityRegistry::builder().build();
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("RaceOrchestration", orchestrator)
        .build();

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        fast_runtime_options(),
    )
    .await;
    let store_for_wait = store.clone();
    let client_for_event = duroxide::Client::new(store.clone());
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-race-order-1", "Race", 1000).await;
        // Post-subscription delay to allow timer(10ms) to win deterministically
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let _ = client_for_event.raise_event("inst-race-order-1", "Race", "ok").await;
    });
    let client = duroxide::Client::new(store.clone());
    duroxide::Client::new(store.clone())
        .start_orchestration("inst-race-order-1", "RaceOrchestration", "")
        .await
        .unwrap();

    match duroxide::Client::new(store.clone())
        .wait_for_orchestration("inst-race-order-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => assert_eq!(output, "timer"),
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for expected events
    let final_history = client.read_execution_history("inst-race-order-1", 1).await.unwrap();
    let idx_t = final_history
        .iter()
        .position(|e| matches!(e, Event::TimerFired { .. }))
        .unwrap();
    if let Some(idx_e) = final_history
        .iter()
        .position(|e| matches!(e, Event::ExternalEvent { .. }))
    {
        assert!(
            idx_t < idx_e,
            "expected timer to fire before external: {final_history:#?}"
        );
    }

    rt.shutdown(None).await;
}

#[tokio::test]
async fn race_external_vs_timer_ordering_fs() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    race_external_vs_timer_ordering_with(store).await;
}

async fn race_event_vs_timer_event_wins_with(store: StdArc<dyn Provider>) {
    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        // Subscribe first to ensure we can receive the event deterministically
        let ev = ctx.schedule_wait("Race").into_event();
        let t = ctx.schedule_timer(100).into_timer();
        let race = select(ev, t);
        match race.await {
            Either::Left((data, _)) => Ok(data),
            Either::Right((_, _)) => Ok("timer".to_string()),
        }
    };

    let activity_registry = ActivityRegistry::builder().build();
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("RaceEventVsTimer", orchestrator)
        .build();

    let rt = runtime::Runtime::start_with_options(
        store.clone(),
        Arc::new(activity_registry),
        orchestration_registry,
        fast_runtime_options(),
    )
    .await;
    let store_for_wait = store.clone();
    let client_for_event = duroxide::Client::new(store.clone());
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-race-order-2", "Race", 1000).await;
        let _ = client_for_event.raise_event("inst-race-order-2", "Race", "ok").await;
    });
    let client = duroxide::Client::new(store.clone());
    duroxide::Client::new(store.clone())
        .start_orchestration("inst-race-order-2", "RaceEventVsTimer", "")
        .await
        .unwrap();

    let output = match duroxide::Client::new(store.clone())
        .wait_for_orchestration("inst-race-order-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        _ => panic!("unexpected orchestration status"),
    };

    // With batching, timer may win select even if external event is delivered first; assert consistent history ordering
    assert_eq!(output, "ok");

    // Check history for expected events
    let final_history = client.read_execution_history("inst-race-order-2", 1).await.unwrap();
    let idx_e = final_history
        .iter()
        .position(|e| matches!(e, Event::ExternalEvent { .. }))
        .unwrap();
    if let Some(idx_t) = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })) {
        assert!(idx_e < idx_t, "expected external before timer: {final_history:#?}");
    }

    rt.shutdown(None).await;
}

#[tokio::test]
async fn race_event_vs_timer_event_wins_fs() {
    eprintln!("START: race_event_vs_timer_event_wins_fs");
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    race_event_vs_timer_event_wins_with(store).await;
}
