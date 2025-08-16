use std::sync::Arc;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

fn parse_activity_result(s: &Result<String, String>) -> Result<String, String> { s.clone() }

async fn error_handling_compensation_on_ship_failure_with(store: StdArc<dyn HistoryStore>) {
    let activity_registry = ActivityRegistry::builder()
        .register_result("Debit", |input: String| async move { if input == "fail" { Err("insufficient".to_string()) } else { Ok(format!("debited:{input}")) } })
        .register_result("Ship", |input: String| async move { if input == "fail_ship" { Err("courier_down".to_string()) } else { Ok("shipped".to_string()) } })
        .register_result("Credit", |input: String| async move { Ok(format!("credited:{input}")) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let deb = ctx.schedule_activity("Debit", "ok").into_activity().await;
        let deb = parse_activity_result(&deb);
        match deb {
            Err(e) => format!("debit_failed:{e}"),
            Ok(deb_val) => {
                let ship = ctx.schedule_activity("Ship", "fail_ship").into_activity().await;
                match parse_activity_result(&ship) {
                    Ok(_) => "ok".to_string(),
                    Err(_) => {
                        let cred = ctx.schedule_activity("Credit", deb_val).into_activity().await.unwrap();
                        format!("rolled_back:{cred}")
                    }
                }
            }
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandlingCompensation", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-err-ship-1", "ErrorHandlingCompensation").await;
    let (_hist, out) = handle.await.unwrap();
    assert!(out.starts_with("rolled_back:credited:"));
    rt.shutdown().await;
}

#[tokio::test]
async fn error_handling_compensation_on_ship_failure_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    error_handling_compensation_on_ship_failure_with(store).await;
}

#[tokio::test]
async fn error_handling_compensation_on_ship_failure_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    error_handling_compensation_on_ship_failure_with(store).await;
}

async fn error_handling_success_path_with(store: StdArc<dyn HistoryStore>) {
    let activity_registry = ActivityRegistry::builder()
        .register_result("Debit", |input: String| async move { Ok(format!("debited:{input}")) })
        .register_result("Ship", |_input: String| async move { Ok("shipped".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let deb = ctx.schedule_activity("Debit", "ok").into_activity().await;
        parse_activity_result(&deb).unwrap();
        let ship = ctx.schedule_activity("Ship", "ok").into_activity().await;
        parse_activity_result(&ship).unwrap();
        "ok".to_string()
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandlingSuccess", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-err-ok-1", "ErrorHandlingSuccess").await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "ok");
    rt.shutdown().await;
}

#[tokio::test]
async fn error_handling_success_path_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    error_handling_success_path_with(store).await;
}

#[tokio::test]
async fn error_handling_success_path_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    error_handling_success_path_with(store).await;
}

async fn error_handling_early_debit_failure_with(store: StdArc<dyn HistoryStore>) {
    let activity_registry = ActivityRegistry::builder()
        .register_result("Debit", |input: String| async move { Err(format!("bad:{input}")) })
        .register_result("Ship", |_input: String| async move { Ok("shipped".to_string()) })
        .register_result("Credit", |_input: String| async move { Ok("credited".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let deb = ctx.schedule_activity("Debit", "fail").into_activity().await;
        match deb {
            Err(e) => format!("debit_failed:{e}"),
            Ok(_) => unreachable!(),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("DebitFailureTest", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-err-debit-1", "DebitFailureTest").await;
    let (_hist, out) = handle.await.unwrap();
    assert!(out.starts_with("debit_failed:"));
    rt.shutdown().await;
}

#[tokio::test]
async fn error_handling_early_debit_failure_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    error_handling_early_debit_failure_with(store).await;
}

#[tokio::test]
async fn error_handling_early_debit_failure_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    error_handling_early_debit_failure_with(store).await;
}

// 5) Unknown activity handler: should fail with unregistered error
async fn unknown_activity_fails_with(store: StdArc<dyn HistoryStore>) {
    let activity_registry = ActivityRegistry::builder().build();
    let orchestration = |ctx: OrchestrationContext| async move {
        match ctx.schedule_activity("Missing", "foo").into_activity().await {
            Ok(v) => format!("unexpected_ok:{v}"),
            Err(e) => format!("err={e}"),
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("MissingActivityTest", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-unknown-act-1", "MissingActivityTest").await;
    let (_hist, out) = handle.await.unwrap();
    assert!(out.starts_with("err=unregistered:Missing"));
    rt.shutdown().await;
}

#[tokio::test]
async fn unknown_activity_fails_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    unknown_activity_fails_with(store).await;
}

// 6) Event after orchestration completion is ignored (no history change)
#[tokio::test]
async fn event_after_completion_is_ignored_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    let activity_registry = ActivityRegistry::builder().build();

    let instance = "inst-post-complete-1";
    // Orchestration: subscribe and exit on first event
    let orchestration = |ctx: OrchestrationContext| async move {
        let _ = ctx.schedule_wait("Once").into_event().await;
        "done".to_string()
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("PostCompleteTest", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let rt_c = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt_c.raise_event(instance, "Once", "go").await;
    });
    let handle = rt.clone().spawn_instance_to_completion(instance, "PostCompleteTest").await;
    let (hist, out) = handle.await.unwrap();
    assert_eq!(out, "done");
    let before = hist.len();

    // Raise another event after completion
    rt.raise_event(instance, "Once", "late").await;
    // Give router a moment
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let hist_after = store.read(instance).await;
    assert_eq!(hist_after.len(), before, "post-completion event must not append history");
    rt.shutdown().await;
}

// 7) Event raised before subscription after instance start is ignored
#[tokio::test]
async fn event_before_subscription_after_start_is_ignored() {
    // Use FS store for consistency
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    let activity_registry = ActivityRegistry::builder().build();
    let orchestration = |ctx: OrchestrationContext| async move {
        ctx.schedule_timer(10).into_timer().await;
        ctx.schedule_wait("Evt").into_event().await
    };
    
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("PreSubscriptionTest", orchestration)
        .build();
    
    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    // Orchestration: delay, then subscribe
    let instance = "inst-pre-sub-drop-1";
    let rt_c1 = rt.clone();
    tokio::spawn(async move {
        // Raise early before subscription exists (timer delays subscription)
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        rt_c1.raise_event(instance, "Evt", "early").await;
    });
    let rt_c2 = rt.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        rt_c2.raise_event(instance, "Evt", "late").await;
    });
    let handle = rt.clone().spawn_instance_to_completion(instance, "PreSubscriptionTest").await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "late");
    rt.shutdown().await;
}

// 8) History cap exceeded triggers a hard error (no truncation) for both providers
async fn history_cap_exceeded_with(store: StdArc<dyn HistoryStore>) {
    let activity_registry = ActivityRegistry::builder()
        .register_result("Noop", |_in: String| async move { Ok(String::new()) })
        .build();

    // Orchestration that schedules more than CAP events.
    // Each activity emits two events (Scheduled + Completed). With CAP=1024, 600 activities exceed.
    let orchestration = |ctx: OrchestrationContext| async move {
        for i in 0..600u32 {
            let _ = ctx.schedule_activity("Noop", format!("{i}")).into_activity().await;
        }
        "done".to_string()
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("HistoryCapTest", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-cap-exceed", "HistoryCapTest").await;
    // Expect the background task to panic due to append error; awaiting should return JoinError
    let res = handle.await;
    assert!(res.is_err(), "expected append failure to propagate as task error");
    rt.shutdown().await;
}

#[tokio::test]
async fn history_cap_exceeded_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    history_cap_exceeded_with(store).await;
}

#[tokio::test]
async fn history_cap_exceeded_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;
    history_cap_exceeded_with(store).await;
}


