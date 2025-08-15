use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;

#[tokio::test]
async fn sample_hello_world_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path()));

    let registry = ActivityRegistry::builder()
        .register_result("Hello", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
        rust_dtf::durable_info!(ctx, "hello_world result={} ", res);
        res
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-hello-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "Hello, Rust!");
    rt.shutdown().await;
}

#[tokio::test]
async fn sample_basic_control_flow_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path()));

    let registry = ActivityRegistry::builder()
        .register_result("GetFlag", |_input: String| async move { Ok("yes".to_string()) })
        .register_result("SayYes", |_in: String| async move { Ok("picked_yes".to_string()) })
        .register_result("SayNo", |_in: String| async move { Ok("picked_no".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let flag = ctx.schedule_activity("GetFlag", "").into_activity().await.unwrap();
        rust_dtf::durable_info!(ctx, "control_flow flag decided = {}", flag);
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
async fn sample_loop_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path()));

    let registry = ActivityRegistry::builder()
        .register("Append", |input: String| async move { format!("{input}x") })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let mut acc = String::from("start");
        for i in 0..3 {
            acc = ctx.schedule_activity("Append", acc).into_activity().await.unwrap();
            rust_dtf::durable_info!(ctx, "loop iteration {} completed acc={}", i, acc);
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
async fn sample_error_handling_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path()));

    let registry = ActivityRegistry::builder()
        .register_result("Fragile", |input: String| async move {
            if input == "bad" { Err("boom".to_string()) } else { Ok("ok".to_string()) }
        })
        .register_result("Recover", |_input: String| async move { Ok("recovered".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        match ctx.schedule_activity("Fragile", "bad").into_activity().await {
            Ok(v) => {
                rust_dtf::durable_info!(ctx, "fragile succeeded value={}", v);
                v
            },
            Err(e) => {
                rust_dtf::durable_warn!(ctx, "fragile failed error={}", e);
                let rec = ctx.schedule_activity("Recover", "").into_activity().await.unwrap();
                if rec != "recovered" {
                    rust_dtf::durable_error!(ctx, "unexpected recovery value={}", rec);
                }
                rec
            },
        }
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-err-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "recovered");
    rt.shutdown().await;
}


