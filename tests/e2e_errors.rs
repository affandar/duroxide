use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

fn parse_activity_result(s: &Result<String, String>) -> Result<String, String> { s.clone() }

async fn error_handling_compensation_on_ship_failure_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
        .register_result("Debit", |input: String| async move { if input == "fail" { Err("insufficient".to_string()) } else { Ok(format!("debited:{input}")) } })
        .register_result("Ship", |input: String| async move { if input == "fail_ship" { Err("courier_down".to_string()) } else { Ok("shipped".to_string()) } })
        .register_result("Credit", |input: String| async move { Ok(format!("credited:{input}")) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let deb = ctx.schedule_activity("Debit", "ok").into_activity().await;
        let deb = parse_activity_result(&deb);
        match deb {
            Err(e) => return format!("debit_failed:{e}"),
            Ok(deb_val) => {
                let ship = ctx.schedule_activity("Ship", "fail_ship").into_activity().await;
                match parse_activity_result(&ship) {
                    Ok(_) => return "ok".to_string(),
                    Err(_) => {
                        let cred = ctx.schedule_activity("Credit", deb_val).into_activity().await.unwrap();
                        return format!("rolled_back:{cred}");
                    }
                }
            }
        }
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-err-ship-1", orchestration).await;
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
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    error_handling_compensation_on_ship_failure_with(store).await;
}

async fn error_handling_success_path_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
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

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-err-ok-1", orchestration).await;
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
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    error_handling_success_path_with(store).await;
}

async fn error_handling_early_debit_failure_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
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

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-err-debit-1", orchestration).await;
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
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    error_handling_early_debit_failure_with(store).await;
}


