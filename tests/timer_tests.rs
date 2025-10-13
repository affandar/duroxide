use duroxide::providers::Provider;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Client, Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
use tempfile::TempDir;

mod common;

/// Helper to create a SQLite store for testing
async fn create_sqlite_store() -> (StdArc<dyn Provider>, TempDir) {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = StdArc::new(SqliteProvider::new(&db_url).await.unwrap()) as StdArc<dyn Provider>;
    (store, td)
}

#[tokio::test]
async fn single_timer_fires() {
    let (store, _td) = create_sqlite_store().await;

    const TIMER_MS: u64 = 50;
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(TIMER_MS).into_timer().await;
        Ok("done".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("OneTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());

    let start = std::time::Instant::now();
    client.start_orchestration("inst-one", "OneTimer", "").await.unwrap();

    let status = client
        .wait_for_orchestration("inst-one", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let elapsed = start.elapsed().as_millis() as u64;

    // Verify timer took at least TIMER_MS
    assert!(
        elapsed >= TIMER_MS,
        "Timer fired too early: expected >={TIMER_MS}ms, got {elapsed}ms"
    );

    match status {
        duroxide::OrchestrationStatus::Completed { output } => {
            println!("Orchestration completed: {output}");
        }
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };

    let hist = client.read_execution_history("inst-one", 1).await.unwrap();
    assert!(hist.iter().any(|e| matches!(e, Event::TimerCreated { .. })));
    assert!(hist.iter().any(|e| matches!(e, Event::TimerFired { .. })));
    rt.shutdown().await;
}

#[tokio::test]
async fn multiple_timers_ordering() {
    let (store, _td) = create_sqlite_store().await;

    const TIMER1_MS: u64 = 100;
    const TIMER2_MS: u64 = 200;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        let t1 = ctx.schedule_timer(TIMER1_MS);
        let t2 = ctx.schedule_timer(TIMER2_MS);
        let _ = ctx.join(vec![t1, t2]).await;
        Ok("ok".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("TwoTimers", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());

    let start = std::time::Instant::now();
    client.start_orchestration("inst-two", "TwoTimers", "").await.unwrap();

    let status = client
        .wait_for_orchestration("inst-two", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let elapsed = start.elapsed().as_millis() as u64;

    // Should wait for the longer timer
    assert!(elapsed >= TIMER2_MS, "Expected >={TIMER2_MS}ms, got {elapsed}ms");
    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(output, "ok");

    let hist = client.read_execution_history("inst-two", 1).await.unwrap();
    // Verify two fired with increasing fire_at_ms
    let fired: Vec<(u64, u64)> = hist
        .iter()
        .filter_map(|e| match e {
            Event::TimerFired {
                source_event_id,
                fire_at_ms,
                ..
            } => Some((*source_event_id, *fire_at_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 2);
    assert!(fired[0].1 <= fired[1].1);
    rt.shutdown().await;
}

#[tokio::test]
async fn timer_deduplication() {
    let (store, _td) = create_sqlite_store().await;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(30).into_timer().await;
        Ok("t".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("DedupTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());

    let inst = "inst-dedup";
    let _ = client.start_orchestration(inst, "DedupTimer", "").await.unwrap();
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
                    event_id,
                    fire_at_ms,
                    execution_id: _,
                } => Some((*event_id, *fire_at_ms)),
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
async fn sub_second_timer_precision() {
    let (store, _td) = create_sqlite_store().await;

    const TIMER_MS: u64 = 250;
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(TIMER_MS).into_timer().await;
        Ok("done".to_string())
    };

    let reg = OrchestrationRegistry::builder()
        .register("SubSecondTimer", orch)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());

    let start = std::time::Instant::now();
    client
        .start_orchestration("inst-subsec", "SubSecondTimer", "")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("inst-subsec", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let _output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        _ => panic!("unexpected orchestration status"),
    };

    // The 250ms timer should take at least 250ms
    assert!(elapsed_ms >= TIMER_MS, "expected >={TIMER_MS}ms, got {elapsed_ms}ms");
    // Allow some overhead but not too much
    assert!(
        elapsed_ms <= TIMER_MS + 200,
        "expected <={} ms, got {elapsed_ms}ms",
        TIMER_MS + 200
    );

    rt.shutdown().await;
}

#[tokio::test]
async fn timer_wall_clock_delay() {
    let (store, _td) = create_sqlite_store().await;

    const TIMER_MS: u64 = 1_500;
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(TIMER_MS).into_timer().await;
        Ok("ok".to_string())
    };

    let reg = OrchestrationRegistry::builder().register("DelayTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());

    let start = std::time::Instant::now();
    client
        .start_orchestration("inst-delay", "DelayTimer", "")
        .await
        .unwrap();

    let status = client
        .wait_for_orchestration("inst-delay", std::time::Duration::from_secs(10))
        .await
        .unwrap();
    let elapsed_ms = start.elapsed().as_millis() as u64;
    println!("Elapsed time: {elapsed_ms}ms");

    let output = match status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(output, "ok");
    // Timer should take at least TIMER_MS
    assert!(elapsed_ms >= TIMER_MS, "expected >={TIMER_MS}ms, got {elapsed_ms}ms");
    // Allow some overhead but not too much (500ms overhead max)
    assert!(
        elapsed_ms <= TIMER_MS + 500,
        "expected <={} ms, got {elapsed_ms}ms",
        TIMER_MS + 500
    );
    rt.shutdown().await;
}
