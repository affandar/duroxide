use futures::future::{select, Either};
use std::sync::Arc;
use rust_dtf::{Event, OrchestrationContext};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

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
async fn wait_external_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    wait_external_completes_with(store).await;
}

async fn race_external_vs_timer_ordering_with(store: StdArc<dyn HistoryStore>) {
    let orchestrator = |ctx: OrchestrationContext| async move {
        let race = select(ctx.schedule_timer(10), ctx.schedule_wait("Race"));
        match race.await {
            Either::Left((_t, _e)) => "timer".to_string(),
            Either::Right((_e, _t)) => "external".to_string(),
        }
    };

    let registry = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry.clone())).await;
    let rt_clone = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
async fn race_external_vs_timer_ordering_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    race_external_vs_timer_ordering_with(store).await;
}

async fn race_event_vs_timer_event_wins_with(store: StdArc<dyn HistoryStore>) {
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

    // With batching, timer may win select even if external event is delivered first; assert consistent history ordering
    assert_eq!(output, "external");
    let idx_e = final_history.iter().position(|e| matches!(e, Event::ExternalEvent { .. })).unwrap();
    if let Some(idx_t) = final_history.iter().position(|e| matches!(e, Event::TimerFired { .. })) {
        assert!(idx_e < idx_t, "expected external before timer: {final_history:#?}");
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn race_event_vs_timer_event_wins_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    race_event_vs_timer_event_wins_with(store).await;
}


