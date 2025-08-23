use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationRegistry};
use std::sync::Arc as StdArc;

#[tokio::test]
async fn unknown_orchestration_fails_gracefully_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // No orchestrations registered
    let orchestration_registry = OrchestrationRegistry::builder().build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    let h = rt
        .clone()
        .start_orchestration("inst-unknown-1", "DoesNotExist", "")
        .await
        .unwrap();
    let (hist, out) = h.await.unwrap();
    assert!(matches!(out, Err(e) if e == "unregistered:DoesNotExist"));
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::OrchestrationFailed { error } if error == "unregistered:DoesNotExist"))
    );

    // Store history should also include the failed terminal
    let persisted = store.read("inst-unknown-1").await;
    assert!(
        persisted
            .iter()
            .any(|e| matches!(e, Event::OrchestrationFailed { error } if error == "unregistered:DoesNotExist"))
    );

    rt.shutdown().await;
}
