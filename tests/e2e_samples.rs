use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;
use futures::future::join;

#[tokio::test]
async fn sample_hello_world_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    let registry = ActivityRegistry::builder()
        .register_result("Hello", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        ctx.trace_info("hello_world started");
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={} ", res));
        let res1 = ctx.schedule_activity("Hello", "Rust123").into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={} ", res1));
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
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    let registry = ActivityRegistry::builder()
        .register_result("GetFlag", |_input: String| async move { Ok("yes".to_string()) })
        .register_result("SayYes", |_in: String| async move { Ok("picked_yes".to_string()) })
        .register_result("SayNo", |_in: String| async move { Ok("picked_no".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let flag = ctx.schedule_activity("GetFlag", "").into_activity().await.unwrap();
        ctx.trace_info(format!("control_flow flag decided = {}", flag));
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
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    let registry = ActivityRegistry::builder()
        .register("Append", |input: String| async move { format!("{input}x") })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        ctx.trace_error("loop started");
        let mut acc = String::from("start");
        for i in 0..3 {
            acc = ctx.schedule_activity("Append", acc).into_activity().await.unwrap();
            ctx.trace_info(format!("loop iteration {} completed acc={}", i, acc));
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
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    let registry = ActivityRegistry::builder()
        .register_result("Fragile", |input: String| async move {
            if input == "bad" { Err("boom".to_string()) } else { Ok("ok".to_string()) }
        })
        .register_result("Recover", |_input: String| async move { Ok("recovered".to_string()) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        match ctx.schedule_activity("Fragile", "bad").into_activity().await {
            Ok(v) => {
                ctx.trace_info(format!("fragile succeeded value={}", v));
                v
            },
            Err(e) => {
                ctx.trace_warn(format!("fragile failed error={}", e));
                let rec = ctx.schedule_activity("Recover", "").into_activity().await.unwrap();
                if rec != "recovered" {
                    ctx.trace_error(format!("unexpected recovery value={}", rec));
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


#[tokio::test]
async fn dtf_legacy_gabbar_greetings_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    let registry = ActivityRegistry::builder()
        .register_result("Greetings", |input: String| async move { Ok(format!("Hello, {}!", input)) })
        .build();

    let orchestration = |ctx: OrchestrationContext| async move {
        // Schedule two greetings in parallel (DTF AsyncDynamicProxyGreetingsTest equivalent)
        let f1 = ctx.schedule_activity("Greetings", "Gabbar").into_activity();
        let f2 = ctx.schedule_activity("Greetings", "Samba").into_activity();
        let (r1, r2) = join(f1, f2).await;
        vec![r1.unwrap(), r2.unwrap()]
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-dtf-greetings", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, vec![
        "Hello, Gabbar!".to_string(),
        "Hello, Samba!".to_string(),
    ]);
    rt.shutdown().await;
}


