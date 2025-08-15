use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

// 1) Hello World: call one activity and return its result
async fn sample_hello_world_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
        .register_result("Hello", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
        res
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-hello-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "Hello, Rust!");
    rt.shutdown().await;
}

#[tokio::test]
async fn sample_hello_world_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    sample_hello_world_with(store).await;
}

#[tokio::test]
async fn sample_hello_world_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    sample_hello_world_with(store).await;
}

// 2) Basic control flow: branch based on an activity result
async fn sample_basic_control_flow_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
        .register_result("GetFlag", |_input: String| async move { Ok("yes".to_string()) })
        .register_result("SayYes", |_in: String| async move { Ok("picked_yes".to_string()) })
        .register_result("SayNo", |_in: String| async move { Ok("picked_no".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let flag = ctx.schedule_activity("GetFlag", "").into_activity().await.unwrap();
        if flag == "yes" {
            ctx.schedule_activity("SayYes", "").into_activity().await.unwrap()
        } else {
            ctx.schedule_activity("SayNo", "").into_activity().await.unwrap()
        }
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-cflow-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "picked_yes");
    rt.shutdown().await;
}

#[tokio::test]
async fn sample_basic_control_flow_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    sample_basic_control_flow_with(store).await;
}

#[tokio::test]
async fn sample_basic_control_flow_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    sample_basic_control_flow_with(store).await;
}

// 3) Loop: accumulate over iterations using an activity
async fn sample_loop_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
        .register("Append", |input: String| async move { format!("{input}x") })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let mut acc = String::from("start");
        for _ in 0..3 {
            acc = ctx.schedule_activity("Append", acc).into_activity().await.unwrap();
        }
        acc
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-loop-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "startxxx");
    rt.shutdown().await;
}

#[tokio::test]
async fn sample_loop_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    sample_loop_with(store).await;
}

#[tokio::test]
async fn sample_loop_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    sample_loop_with(store).await;
}

// 4) Error handling: catch failure and run compensating action
async fn sample_error_handling_with(store: StdArc<dyn HistoryStore>) {
    let registry = ActivityRegistry::builder()
        .register_result("Fragile", |input: String| async move {
            if input == "bad" { Err("boom".to_string()) } else { Ok("ok".to_string()) }
        })
        .register_result("Recover", |_input: String| async move { Ok("recovered".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        match ctx.schedule_activity("Fragile", "bad").into_activity().await {
            Ok(v) => v,
            Err(_e) => ctx.schedule_activity("Recover", "").into_activity().await.unwrap(),
        }
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-err-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "recovered");
    rt.shutdown().await;
}

#[tokio::test]
async fn sample_error_handling_inmem() {
    let store = StdArc::new(InMemoryHistoryStore::default()) as StdArc<dyn HistoryStore>;
    sample_error_handling_with(store).await;
}

#[tokio::test]
async fn sample_error_handling_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;
    sample_error_handling_with(store).await;
}


