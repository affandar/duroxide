use std::sync::Arc as StdArc;
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use rust_dtf::runtime::{self};
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;

#[tokio::test]
async fn unknown_activity_is_isolated_from_other_orchestrations_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register only a known-good activity; intentionally omit the one we'll call ("Missing")
    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |input: String| async move { Ok(input) })
        .build();

    // Orchestrator that attempts to call a non-existent activity and propagates the error
    let uses_missing = |ctx: OrchestrationContext, _input: String| async move {
        let r = ctx.schedule_activity("Missing", "x").into_activity().await;
        // Propagate the activity error as orchestration failure
        r.map(|_| "ok".to_string())
    };

    // Healthy orchestrator that should complete successfully
    let healthy = |ctx: OrchestrationContext, input: String| async move {
        let v = ctx.schedule_activity("Echo", input).into_activity().await.unwrap();
        Ok(format!("healthy:{v}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("UsesMissing", uses_missing)
        .register("Healthy", healthy)
        .build();

    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        StdArc::new(activity_registry),
        orchestration_registry,
    ).await;

    // Start both orchestrations concurrently
    let h_fail = rt.clone().start_orchestration("inst-missing-1", "UsesMissing", "").await.unwrap();
    let h_ok = rt.clone().start_orchestration("inst-healthy-1", "Healthy", "yo").await.unwrap();

    // Await both and assert expected outcomes
    let (hist_ok, out_ok) = h_ok.await.unwrap();
    assert_eq!(out_ok.unwrap(), "healthy:yo");
    assert!(!hist_ok.iter().any(|e| matches!(e, Event::ActivityFailed { .. })), "healthy orchestration should not see failures");
    assert!(matches!(hist_ok.last().unwrap(), Event::OrchestrationCompleted { .. }));

    let (hist_fail, out_fail) = h_fail.await.unwrap();
    assert!(matches!(out_fail, Err(e) if e == "unregistered:Missing"));
    assert!(hist_fail.iter().any(|e| matches!(e, Event::ActivityFailed { error, .. } if error == "unregistered:Missing")));
    assert!(matches!(hist_fail.last().unwrap(), Event::OrchestrationFailed { error } if error == "unregistered:Missing"));

    // Status API should reflect isolation as well
    match rt.get_orchestration_status("inst-healthy-1").await {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "healthy:yo"),
        other => panic!("unexpected status for healthy: {other:?}"),
    }
    match rt.get_orchestration_status("inst-missing-1").await {
        OrchestrationStatus::Failed { error } => assert_eq!(error, "unregistered:Missing"),
        other => panic!("unexpected status for missing: {other:?}"),
    }

    rt.shutdown().await;
}

#[tokio::test]
async fn unknown_activity_is_isolated_from_other_orchestrations_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |input: String| async move { Ok(input) })
        .build();

    let uses_missing = |ctx: OrchestrationContext, _input: String| async move {
        let r = ctx.schedule_activity("Missing", "x").into_activity().await;
        r.map(|_| "ok".to_string())
    };
    let healthy = |ctx: OrchestrationContext, input: String| async move {
        let v = ctx.schedule_activity("Echo", input).into_activity().await.unwrap();
        Ok(format!("healthy:{v}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("UsesMissing", uses_missing)
        .register("Healthy", healthy)
        .build();

    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        StdArc::new(activity_registry),
        orchestration_registry,
    ).await;

    let h_fail = rt.clone().start_orchestration("inst-missing-im", "UsesMissing", "").await.unwrap();
    let h_ok = rt.clone().start_orchestration("inst-healthy-im", "Healthy", "yo").await.unwrap();

    let (_hist_ok, out_ok) = h_ok.await.unwrap();
    assert_eq!(out_ok.unwrap(), "healthy:yo");

    let (_hist_fail, out_fail) = h_fail.await.unwrap();
    assert!(matches!(out_fail, Err(e) if e == "unregistered:Missing"));

    match rt.get_orchestration_status("inst-healthy-im").await {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "healthy:yo"),
        other => panic!("unexpected status for healthy: {other:?}"),
    }
    match rt.get_orchestration_status("inst-missing-im").await {
        OrchestrationStatus::Failed { error } => assert_eq!(error, "unregistered:Missing"),
        other => panic!("unexpected status for missing: {other:?}"),
    }

    rt.shutdown().await;
}
