use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Event, OrchestrationRegistry};
use std::sync::Arc as StdArc;
mod common;

#[tokio::test]
async fn unknown_orchestration_fails_gracefully() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // No orchestrations registered
    let orchestration_registry = OrchestrationRegistry::builder().build();
    let activity_registry = ActivityRegistry::builder().build();
    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    let client = duroxide::Client::new(store.clone());
    client
        .start_orchestration("inst-unknown-1", "DoesNotExist", "")
        .await
        .unwrap();

    let client = duroxide::Client::new(store.clone());
    let status = client
        .wait_for_orchestration("inst-unknown-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let error = match status {
        duroxide::OrchestrationStatus::Failed { error } => error,
        duroxide::OrchestrationStatus::Completed { output } => panic!("expected failure, got success: {output}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(error, "unregistered:DoesNotExist");

    let hist = client.read_execution_history("inst-unknown-1", 1).await.unwrap();
    
    // History should be well-formed: start with OrchestrationStarted, end with OrchestrationFailed
    assert_eq!(hist.len(), 2);
    assert!(matches!(&hist[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(&hist[1], Event::OrchestrationFailed { error, .. } if error == "unregistered:DoesNotExist"));
    
    // Store history should also include both events
    let persisted = store.read("inst-unknown-1").await;
    assert_eq!(persisted.len(), 2);
    assert!(matches!(&persisted[0], Event::OrchestrationStarted { .. }));
    assert!(matches!(&persisted[1], Event::OrchestrationFailed { error, .. } if error == "unregistered:DoesNotExist"));

    rt.shutdown().await;
}
