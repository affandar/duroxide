use std::sync::Arc as StdArc;
use std::time::Duration;

mod common;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Event, OrchestrationContext, OrchestrationRegistry};

async fn wait_for_history<F>(
    store: StdArc<dyn duroxide::providers::HistoryStore>,
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
async fn dispatcher_enqueues_timer_schedule_then_completes() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let store_dyn = store.clone() as StdArc<dyn duroxide::providers::HistoryStore>;

    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(50).into_timer().await;
        Ok("ok".to_string())
    };
    let reg = OrchestrationRegistry::builder().register("OneTimer", orch).build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::DuroxideRuntime::start_with_store(store_dyn.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::DuroxideClient::new(store_dyn.clone());

    let inst = "inst-disp-timer";
    let _h = client.start_orchestration(inst, "OneTimer", "").await.unwrap();

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
async fn dispatcher_enqueues_start_orchestration_to_orch_queue() {
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;
    let store_dyn = store.clone() as StdArc<dyn duroxide::providers::HistoryStore>;

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
    let rt = runtime::DuroxideRuntime::start_with_store(store_dyn.clone(), StdArc::new(acts), reg).await;
    let client = duroxide::DuroxideClient::new(store_dyn.clone());

    let _h = client.start_orchestration("inst-parent", "Parent", "").await.unwrap();

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
