//! End-to-end samples: start here to learn the API by example.
//!
//! Each test demonstrates a common orchestration pattern using
//! `OrchestrationContext` and the in-process `Runtime`.
use std::sync::Arc;
use rust_dtf::OrchestrationContext;
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;
use rust_dtf::providers::fs::FsHistoryStore;
use std::sync::Arc as StdArc;
use futures::future::join;

/// Hello World: define one activity and call it from an orchestrator.
///
/// Highlights:
/// - Register an activity in an `ActivityRegistry`
/// - Start the `Runtime` with a provider (filesystem here)
/// - Schedule an activity and await its typed completion
#[tokio::test]
async fn sample_hello_world_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    // Register a simple activity: "Hello" -> format a greeting
    let registry = ActivityRegistry::builder()
        .register_result("Hello", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    // Orchestrator: emit a trace, call Hello twice, return first result
    let orchestration = |ctx: OrchestrationContext| async move {
        ctx.trace_info("hello_world started");
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={res} "));
        let res1 = ctx.schedule_activity("Hello", "Rust123").into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={res1} "));
        res
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-hello-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "Hello, Rust!");
    rt.shutdown().await;
}

/// Basic control flow: branch on a flag returned by an activity.
///
/// Highlights:
/// - Call an activity to fetch a decision
/// - Use standard Rust control flow to drive subsequent activities
#[tokio::test]
async fn sample_basic_control_flow_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    // Register activities that return a flag and branch outcomes
    let registry = ActivityRegistry::builder()
        .register_result("GetFlag", |_input: String| async move { Ok("yes".to_string()) })
        .register_result("SayYes", |_in: String| async move { Ok("picked_yes".to_string()) })
        .register_result("SayNo", |_in: String| async move { Ok("picked_no".to_string()) })
        .build();

    // Orchestrator: get a flag and branch
    let orchestration = |ctx: OrchestrationContext| async move {
        let flag = ctx.schedule_activity("GetFlag", "").into_activity().await.unwrap();
        ctx.trace_info(format!("control_flow flag decided = {flag}"));
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

/// Loops and accumulation: call an activity repeatedly and build up a value.
///
/// Highlights:
/// - Use a for-loop in the orchestrator
/// - Emit replay-safe traces per iteration
#[tokio::test]
async fn sample_loop_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    // Register an activity that appends "x" to its input
    let registry = ActivityRegistry::builder()
        .register("Append", |input: String| async move { format!("{input}x") })
        .build();

    // Orchestrator: loop three times, updating an accumulator
    let orchestration = |ctx: OrchestrationContext| async move {
        ctx.trace_error("loop started");
        let mut acc = String::from("start");
        for i in 0..3 {
            acc = ctx.schedule_activity("Append", acc).into_activity().await.unwrap();
            ctx.trace_info(format!("loop iteration {i} completed acc={acc}"));
        }
        acc
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-sample-loop-1", orchestration).await;
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out, "startxxx");
    rt.shutdown().await;
}

/// Error handling and compensation: recover from a failed activity.
///
/// Highlights:
/// - Activities return `Result<String, String>` and map into `Ok/Err`
/// - On failure, run a compensating activity and log what happened
#[tokio::test]
async fn sample_error_handling_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    // Register a fragile activity that may fail, and a recovery activity
    let registry = ActivityRegistry::builder()
        .register_result("Fragile", |input: String| async move {
            if input == "bad" { Err("boom".to_string()) } else { Ok("ok".to_string()) }
        })
        .register_result("Recover", |_input: String| async move { Ok("recovered".to_string()) })
        .build();

    // Orchestrator: try fragile, on error call Recover
    let orchestration = |ctx: OrchestrationContext| async move {
        match ctx.schedule_activity("Fragile", "bad").into_activity().await {
            Ok(v) => {
                ctx.trace_info(format!("fragile succeeded value={v}"));
                v
            },
            Err(e) => {
                ctx.trace_warn(format!("fragile failed error={e}"));
                let rec = ctx.schedule_activity("Recover", "").into_activity().await.unwrap();
                if rec != "recovered" {
                    ctx.trace_error(format!("unexpected recovery value={rec}"));
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

/// Parallel fan-out/fan-in: run two activities concurrently and join results.
///
/// Highlights:
/// - Use `futures::join` to await multiple `DurableFuture`s concurrently
/// - Deterministic replay ensures join order is stable
#[tokio::test]
async fn dtf_legacy_gabbar_greetings_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    // Register a greeting activity used by both branches
    let registry = ActivityRegistry::builder()
        .register_result("Greetings", |input: String| async move { Ok(format!("Hello, {input}!")) })
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


/// System activities: use built-in activities to get wall-clock time and a new GUID.
///
/// Highlights:
/// - Call `ctx.system_now_ms()` and `ctx.system_new_guid()`
/// - Log and validate basic formatting of results
#[tokio::test]
async fn sample_system_activities_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path())) as StdArc<dyn HistoryStore>;

    let registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext| async move {
        let now = ctx.system_now_ms().await;
        let guid = ctx.system_new_guid().await;
        ctx.trace_info(format!("system now={now}, guid={guid}"));
        format!("n={now},g={guid}")
    };

    let rt = runtime::Runtime::start_with_store(store, Arc::new(registry)).await;
    let handle = rt.clone().spawn_instance_to_completion("inst-system-acts", orchestration).await;
    let (_hist, out) = handle.await.unwrap();

    // Basic assertions
    assert!(out.contains("n=") && out.contains(",g="));
    let parts: Vec<&str> = out.split([',', '=']).collect();
    // parts like ["n", now, "g", guid]
    assert!(parts.len() >= 4);
    let now_val: u128 = parts[1].parse().unwrap_or(0);
    let guid_str = parts[3];
    assert!(now_val > 0);
    assert_eq!(guid_str.len(), 32);
    assert!(guid_str.chars().all(|c| c.is_ascii_hexdigit()));

    rt.shutdown().await;
}


