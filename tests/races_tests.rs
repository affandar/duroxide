use futures::future::{Either, select};
use std::sync::Arc;
mod common;
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;

async fn wait_external_completes_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext, _input: String| async move {
        let data = ctx.schedule_wait("Only").into_event().await;
        Ok(format!("only={data}"))
    };

    let activity_registry = ActivityRegistry::builder().build();
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("WaitExternal", orchestrator)
        .build();

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let store_for_wait = store.clone();
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-wait-1", "Only", 1000).await;
        rt_clone.raise_event("inst-wait-1", "Only", "payload").await;
    });
    let _handle = rt.clone().start_orchestration("inst-wait-1", "WaitExternal", "").await.unwrap();
    
    match rt
        .wait_for_orchestration("inst-wait-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "only=payload"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    
    // Check history for expected events
    let final_history = rt.get_execution_history("inst-wait-1", 1).await;
    // First event is OrchestrationStarted; then subscription, event, and terminal completion
    assert!(matches!(final_history[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(final_history[1], Event::ExternalSubscribed { .. }));
    assert!(matches!(final_history[2], Event::ExternalEvent { .. }));
    assert!(matches!(
        final_history.last().unwrap(),
        Event::OrchestrationCompleted { .. }
    ));
    assert_eq!(final_history.len(), 4);

    rt.shutdown().await;
}

#[tokio::test]
async fn wait_external_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    wait_external_completes_with(store).await;
}

async fn race_external_vs_timer_ordering_with(store: StdArc<dyn HistoryStore>) {
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

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let store_for_wait = store.clone();
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-race-order-1", "Race", 1000).await;
        // Post-subscription delay to allow timer(10ms) to win deterministically
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        rt_clone.raise_event("inst-race-order-1", "Race", "ok").await;
    });
    let _handle = rt
        .clone()
        .start_orchestration("inst-race-order-1", "RaceOrchestration", "")
        .await
        .unwrap();
    
    match rt
        .wait_for_orchestration("inst-race-order-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "timer"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    
    // Check history for expected events
    let final_history = rt.get_execution_history("inst-race-order-1", 1).await;
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

    rt.shutdown().await;
}

#[tokio::test]
async fn race_external_vs_timer_ordering_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    race_external_vs_timer_ordering_with(store).await;
}

async fn race_event_vs_timer_event_wins_with(store: StdArc<dyn HistoryStore>) {
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

    let rt =
        runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let store_for_wait = store.clone();
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-race-order-2", "Race", 1000).await;
        rt_clone.raise_event("inst-race-order-2", "Race", "ok").await;
    });
    let _handle = rt
        .clone()
        .start_orchestration("inst-race-order-2", "RaceEventVsTimer", "")
        .await
        .unwrap();
    
    let output = match rt
        .wait_for_orchestration("inst-race-order-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => output,
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    
    // With batching, timer may win select even if external event is delivered first; assert consistent history ordering
    assert_eq!(output, "ok");
    
    // Check history for expected events
    let final_history = rt.get_execution_history("inst-race-order-2", 1).await;
    let idx_e = final_history
        .iter()
        .position(|e| matches!(e, Event::ExternalEvent { .. }))
        .unwrap();
    if let Some(idx_t) = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })) {
        assert!(idx_e < idx_t, "expected external before timer: {final_history:#?}");
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn race_event_vs_timer_event_wins_fs() {
    eprintln!("START: race_event_vs_timer_event_wins_fs");
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    race_event_vs_timer_event_wins_with(store).await;
}
