use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::providers::{QueueKind, WorkItem};
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;
mod common;

// Simulate crash windows by interleaving dequeue and persistence.
// We approximate by injecting duplicates around the same window; idempotence + peek-lock should ensure correctness.

#[tokio::test]
async fn crash_after_dequeue_before_append_completion_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        // Wait for external then complete with payload
        let v = ctx.schedule_wait("Evt").into_event().await;
        Ok(v)
    };
    let orchestration_registry = OrchestrationRegistry::builder().register("WaitEvt", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    // Start orchestration and wait for subscription
    let inst = "inst-crash-before-append";
    let _h = rt.clone().start_orchestration(inst, "WaitEvt", "").await.unwrap();
    assert!(common::wait_for_subscription(store.clone(), inst, "Evt", 2_000).await);

    // Enqueue the external work item
    let wi = WorkItem::ExternalRaised {
        instance: inst.to_string(),
        name: "Evt".to_string(),
        data: "ok".to_string(),
    };
    let _ = store.enqueue_work(QueueKind::Orchestrator, wi.clone()).await;
    // Simulate crash-before-append by enqueuing duplicate before runtime gets to append
    let _ = store.enqueue_work(QueueKind::Orchestrator, wi.clone()).await;

    // Wait for completion, ensure a single ExternalEvent recorded
    let ok = common::wait_for_history(
        store.clone(),
        inst,
        |h| {
            h.iter()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "ok"))
        },
        5_000,
    )
    .await;
    assert!(ok, "timeout waiting for completion");
    let hist = store.read(inst).await;
    let evs: Vec<&Event> = hist
        .iter()
        .filter(|e| matches!(e, Event::ExternalEvent { .. }))
        .collect();
    assert_eq!(evs.len(), 1);

    rt.shutdown().await;
}

#[tokio::test]
async fn crash_after_append_before_ack_timer_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(50).into_timer().await;
        Ok("t".to_string())
    };
    let orchestration_registry = OrchestrationRegistry::builder().register("OneTimer", orch).build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    let inst = "inst-crash-after-append";
    let _h = rt.clone().start_orchestration(inst, "OneTimer", "").await.unwrap();

    assert!(
        common::wait_for_history(
            store.clone(),
            inst,
            |h| h.iter().any(|e| matches!(e, Event::TimerCreated { .. })),
            2_000
        )
        .await
    );
    // Get timer id
    let (id, fire_at_ms) = {
        let hist = store.read(inst).await;
        let mut t_id = 0u64;
        let mut t_fire = 0u64;
        for e in hist.iter() {
            if let Event::TimerCreated { id, fire_at_ms } = e {
                t_id = *id;
                t_fire = *fire_at_ms;
                break;
            }
        }
        (t_id, t_fire)
    };

    // Inject duplicate TimerFired simulating a crash after append-before-ack
    let wi = WorkItem::TimerFired {
        instance: inst.to_string(),
        id,
        fire_at_ms,
    };
    let _ = store.enqueue_work(QueueKind::Orchestrator, wi.clone()).await;
    let _ = store.enqueue_work(QueueKind::Orchestrator, wi.clone()).await;

    let ok = common::wait_for_history(
        store.clone(),
        inst,
        |h| {
            h.iter()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "t"))
        },
        5_000,
    )
    .await;
    assert!(ok, "timeout waiting for completion");
    let hist = store.read(inst).await;
    let fired: Vec<&Event> = hist.iter().filter(|e| matches!(e, Event::TimerFired { .. })).collect();
    assert_eq!(fired.len(), 1);

    rt.shutdown().await;
}
