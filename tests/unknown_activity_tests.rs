use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::Arc as StdArc;

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

    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    // Start both orchestrations concurrently
    rt.clone()
        .start_orchestration("inst-missing-1", "UsesMissing", "")
        .await
        .unwrap();
    rt.clone()
        .start_orchestration("inst-healthy-1", "Healthy", "yo")
        .await
        .unwrap();

    // Wait for both and assert expected outcomes
    let status_ok = rt
        .wait_for_orchestration("inst-healthy-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let out_ok = match status_ok {
        rust_dtf::OrchestrationStatus::Completed { output } => output,
        rust_dtf::OrchestrationStatus::Failed { error } => panic!("healthy orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(out_ok, "healthy:yo");

    let hist_ok = rt.get_execution_history("inst-healthy-1", 1).await;
    assert!(
        !hist_ok.iter().any(|e| matches!(e, Event::ActivityFailed { .. })),
        "healthy orchestration should not see failures"
    );
    assert!(matches!(hist_ok.last().unwrap(), Event::OrchestrationCompleted { .. }));

    let status_fail = rt
        .wait_for_orchestration("inst-missing-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let error_fail = match status_fail {
        rust_dtf::OrchestrationStatus::Failed { error } => error,
        rust_dtf::OrchestrationStatus::Completed { output } => panic!("expected failure, got success: {output}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(error_fail, "unregistered:Missing");

    let hist_fail = rt.get_execution_history("inst-missing-1", 1).await;
    assert!(
        hist_fail
            .iter()
            .any(|e| matches!(e, Event::ActivityFailed { error, .. } if error == "unregistered:Missing"))
    );
    assert!(
        matches!(hist_fail.last().unwrap(), Event::OrchestrationFailed { error } if error == "unregistered:Missing")
    );

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

    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(activity_registry), orchestration_registry).await;

    rt.clone()
        .start_orchestration("inst-missing-im", "UsesMissing", "")
        .await
        .unwrap();
    rt.clone()
        .start_orchestration("inst-healthy-im", "Healthy", "yo")
        .await
        .unwrap();

    let status_ok = rt
        .wait_for_orchestration("inst-healthy-im", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let out_ok = match status_ok {
        rust_dtf::OrchestrationStatus::Completed { output } => output,
        rust_dtf::OrchestrationStatus::Failed { error } => panic!("healthy orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(out_ok, "healthy:yo");

    let status_fail = rt
        .wait_for_orchestration("inst-missing-im", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let error_fail = match status_fail {
        rust_dtf::OrchestrationStatus::Failed { error } => error,
        rust_dtf::OrchestrationStatus::Completed { output } => panic!("expected failure, got success: {output}"),
        _ => panic!("unexpected orchestration status"),
    };
    assert_eq!(error_fail, "unregistered:Missing");

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
