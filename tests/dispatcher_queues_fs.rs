use std::sync::Arc as StdArc;
use std::time::Duration;

use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};

async fn wait_for_history<F>(
    store: StdArc<dyn rust_dtf::providers::HistoryStore>,
    instance: &str,
    pred: F,
    timeout_ms: u64,
) -> bool
where
    F: Fn(&[Event]) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        let h = store.read(instance).await;
        if pred(&h) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn dispatcher_enqueues_timer_schedule_then_completes_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true));
    let store_dyn = store.clone() as StdArc<dyn rust_dtf::providers::HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(50).into_timer().await;
        Ok("ok".to_string())
    };
    let reg = OrchestrationRegistry::builder().register("OneTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store_dyn.clone(), StdArc::new(acts), reg).await;

    let inst = "inst-disp-timer";
    let _h = rt.clone().start_orchestration(inst, "OneTimer", "").await.unwrap();

    // Orchestration should complete.
    let ok = wait_for_history(
        store_dyn.clone(),
        inst,
        |h| {
            h.iter()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "ok"))
        },
        5_000,
    )
    .await;
    assert!(ok, "timeout waiting for completion");

    rt.shutdown().await;
}

#[tokio::test]
async fn dispatcher_enqueues_start_orchestration_to_orch_queue_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true));
    let store_dyn = store.clone() as StdArc<dyn rust_dtf::providers::HistoryStore>;

    let acts = ActivityRegistry::builder().build();
    let child = |_: OrchestrationContext, input: String| async move { Ok(input) };
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_orchestration("Child", "W1", "A");
        Ok("scheduled".to_string())
    };
    let reg = OrchestrationRegistry::builder()
        .register("Child", child)
        .register("Parent", parent)
        .build();
    let rt = runtime::Runtime::start_with_store(store_dyn.clone(), StdArc::new(acts), reg).await;

    let _h = rt
        .clone()
        .start_orchestration("inst-parent", "Parent", "")
        .await
        .unwrap();

    // Child should complete with input "A".
    let ok = wait_for_history(
        store_dyn.clone(),
        "W1",
        |h| {
            h.iter()
                .any(|e| matches!(e, Event::OrchestrationCompleted { output } if output == "A"))
        },
        5_000,
    )
    .await;
    assert!(ok, "timeout waiting for child completion");

    rt.shutdown().await;
}
