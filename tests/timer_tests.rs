use duroxide::providers::HistoryStore;
use duroxide::providers::fs::FsHistoryStore;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;

mod common;

#[tokio::test]
async fn single_timer_fires_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(50).into_timer().await;
        Ok("done".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("OneTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    rt.clone()
        .start_orchestration("inst-one", "OneTimer", "")
        .await
        .unwrap();

    let status = rt
        .wait_for_orchestration("inst-one", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(output, "done");

    let hist = rt.get_execution_history("inst-one", 1).await;
    assert!(hist.iter().any(|e| matches!(e, Event::TimerCreated { .. })));
    assert!(hist.iter().any(|e| matches!(e, Event::TimerFired { .. })));
    rt.shutdown().await;
}

#[tokio::test]
async fn multiple_timers_ordering_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let t1 = ctx.schedule_timer(10);
        let t2 = ctx.schedule_timer(20);
        let _ = ctx.join(vec![t1, t2]).await;
        Ok("ok".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("TwoTimers", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    rt.clone()
        .start_orchestration("inst-two", "TwoTimers", "")
        .await
        .unwrap();

    let status = rt
        .wait_for_orchestration("inst-two", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(output, "ok");

    let hist = rt.get_execution_history("inst-two", 1).await;
    // Verify two fired with increasing fire_at_ms
    let fired: Vec<(u64, u64)> = hist
        .iter()
        .filter_map(|e| match e {
            Event::TimerFired { id, fire_at_ms } => Some((*id, *fire_at_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 2);
    assert!(fired[0].1 <= fired[1].1);
    rt.shutdown().await;
}

#[tokio::test]
async fn timer_deduplication_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(30).into_timer().await;
        Ok("t".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("DedupTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    let inst = "inst-dedup";
    let _h = rt.clone().start_orchestration(inst, "DedupTimer", "").await.unwrap();
    assert!(
        common::wait_for_history(
            store.clone(),
            inst,
            |h| { h.iter().any(|e| matches!(e, Event::TimerCreated { .. })) },
            2_000
        )
        .await
    );

    // Inject duplicate TimerFired for same id
    let (id, fire_at) = {
        let hist = store.read(inst).await;
        hist.iter()
            .find_map(|e| match e {
                Event::TimerCreated {
                    id,
                    fire_at_ms,
                    execution_id: _,
                } => Some((*id, *fire_at_ms)),
                _ => None,
            })
            .unwrap()
    };
    let wi = duroxide::providers::WorkItem::TimerFired {
        instance: inst.to_string(),
        execution_id: 1,
        id,
        fire_at_ms: fire_at,
    };
    let _ = store.enqueue_orchestrator_work(wi.clone(), None).await;
    let _ = store.enqueue_orchestrator_work(wi.clone(), None).await;

    assert!(
        common::wait_for_history(
            store.clone(),
            inst,
            |h| {
                let fired: Vec<&Event> = h.iter().filter(|e| matches!(e, Event::TimerFired { .. })).collect();
                fired.len() == 1
            },
            5_000
        )
        .await
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn timer_wall_clock_delay_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(1_500).into_timer().await;
        Ok("ok".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("DelayTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    let start = std::time::Instant::now();
    rt.clone()
        .start_orchestration("inst-delay", "DelayTimer", "")
        .await
        .unwrap();

    let status = rt
        .wait_for_orchestration("inst-delay", std::time::Duration::from_secs(10))
        .await
        .unwrap();
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(output, "ok");
    // Allow some jitter, but ensure at least ~1.2s elapsed
    assert!(elapsed_ms >= 1_200, "expected >=1200ms, got {elapsed_ms}ms");
    rt.shutdown().await;
}
