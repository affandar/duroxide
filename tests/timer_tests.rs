use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::providers::HistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
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

    let h = rt.clone().start_orchestration("inst-one", "OneTimer", "").await.unwrap();
    let (hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "done");
    assert!(hist.iter().any(|e| matches!(e, Event::TimerCreated { .. })));
    assert!(hist.iter().any(|e| matches!(e, Event::TimerFired { .. })));
    rt.shutdown().await;
}

#[tokio::test]
async fn multiple_timers_ordering_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let t1 = ctx.schedule_timer(10).into_timer();
        let t2 = ctx.schedule_timer(20).into_timer();
        let _ = futures::future::join(t1, t2).await;
        Ok("ok".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("TwoTimers", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;

    let h = rt.clone().start_orchestration("inst-two", "TwoTimers", "").await.unwrap();
    let (hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "ok");
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
    assert!(common::wait_for_history(store.clone(), inst, |h| {
        h.iter().any(|e| matches!(e, Event::TimerCreated { .. }))
    }, 2_000).await);

    // Inject duplicate TimerFired for same id
    let (id, fire_at) = {
        let hist = store.read(inst).await;
        hist.iter()
            .find_map(|e| match e {
                Event::TimerCreated { id, fire_at_ms } => Some((*id, *fire_at_ms)),
                _ => None,
            })
            .unwrap()
    };
    let wi = rust_dtf::providers::WorkItem::TimerFired {
        instance: inst.to_string(),
        execution_id: 1,
        id,
        fire_at_ms: fire_at,
    };
    let _ = store.enqueue_work(rust_dtf::providers::QueueKind::Orchestrator, wi.clone()).await;
    let _ = store.enqueue_work(rust_dtf::providers::QueueKind::Orchestrator, wi.clone()).await;

    assert!(common::wait_for_history(store.clone(), inst, |h| {
        let fired: Vec<&Event> = h.iter().filter(|e| matches!(e, Event::TimerFired { .. })).collect();
        fired.len() == 1
    }, 5_000).await);

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
    let h = rt.clone().start_orchestration("inst-delay", "DelayTimer", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    let elapsed_ms = start.elapsed().as_millis() as u64;
    assert_eq!(out.unwrap(), "ok");
    // Allow some jitter, but ensure at least ~1.2s elapsed
    assert!(elapsed_ms >= 1_200, "expected >=1200ms, got {elapsed_ms}ms");
    rt.shutdown().await;
}


