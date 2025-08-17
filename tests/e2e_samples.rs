//! End-to-end samples: start here to learn the API by example.
//!
//! Each test demonstrates a common orchestration pattern using
//! `OrchestrationContext` and the in-process `Runtime`.
use std::sync::Arc;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};
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
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register a simple activity: "Hello" -> format a greeting
    let activity_registry = ActivityRegistry::builder()
        .register("Hello", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    // Orchestrator: emit a trace, call Hello twice, return result using input
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("hello_world started");
        let res = ctx.schedule_activity("Hello", "Rust").into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={res} "));
        let res1 = ctx.schedule_activity("Hello", input).into_activity().await.unwrap();
        ctx.trace_info(format!("hello_world result={res1} "));
        Ok(res1)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("HelloWorld", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-sample-hello-1", "HelloWorld", "World").await;
    let (_hist, out) = handle.unwrap().await.unwrap();
    assert_eq!(out.unwrap(), "Hello, World!");
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
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register activities that return a flag and branch outcomes
    let activity_registry = ActivityRegistry::builder()
        .register("GetFlag", |_input: String| async move { Ok("yes".to_string()) })
        .register("SayYes", |_in: String| async move { Ok("picked_yes".to_string()) })
        .register("SayNo", |_in: String| async move { Ok("picked_no".to_string()) })
        .build();

    // Orchestrator: get a flag and branch
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let flag = ctx.schedule_activity("GetFlag", "").into_activity().await.unwrap();
        ctx.trace_info(format!("control_flow flag decided = {flag}"));
        if flag == "yes" {
            Ok(ctx.schedule_activity("SayYes", "").into_activity().await.unwrap())
        } else {
            Ok(ctx.schedule_activity("SayNo", "").into_activity().await.unwrap())
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ControlFlow", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-sample-cflow-1", "ControlFlow", "").await;
    let (_hist, out) = handle.unwrap().await.unwrap();
    assert_eq!(out.unwrap(), "picked_yes");
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
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register an activity that appends "x" to its input
    let activity_registry = ActivityRegistry::builder()
        .register("Append", |input: String| async move { Ok(format!("{input}x")) })
        .build();

    // Orchestrator: loop three times, updating an accumulator
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let mut acc = String::from("start");
        for i in 0..3 {
            acc = ctx.schedule_activity("Append", acc).into_activity().await.unwrap();
            ctx.trace_info(format!("loop iteration {i} completed acc={acc}"));
        }
        Ok(acc)
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("LoopOrchestration", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-sample-loop-1", "LoopOrchestration", "").await;
    let (_hist, out) = handle.unwrap().await.unwrap();
    assert_eq!(out.unwrap(), "startxxx");
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
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register a fragile activity that may fail, and a recovery activity
    let activity_registry = ActivityRegistry::builder()
        .register("Fragile", |input: String| async move {
            if input == "bad" { Err("boom".to_string()) } else { Ok("ok".to_string()) }
        })
        .register("Recover", |_input: String| async move { Ok("recovered".to_string()) })
        .build();

    // Orchestrator: try fragile, on error call Recover
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        match ctx.schedule_activity("Fragile", "bad").into_activity().await {
            Ok(v) => {
                ctx.trace_info(format!("fragile succeeded value={v}"));
                Ok(v)
            },
            Err(e) => {
                ctx.trace_warn(format!("fragile failed error={e}"));
                let rec = ctx.schedule_activity("Recover", "").into_activity().await.unwrap();
                if rec != "recovered" {
                    ctx.trace_error(format!("unexpected recovery value={rec}"));
                }
                Ok(rec)
            },
        }
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ErrorHandling", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-sample-err-1", "ErrorHandling", "").await;
    let (_hist, out) = handle.unwrap().await.unwrap();
    assert_eq!(out.unwrap(), "recovered");
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
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Register a greeting activity used by both branches
    let activity_registry = ActivityRegistry::builder()
        .register("Greetings", |input: String| async move { Ok(format!("Hello, {input}!")) })
        .build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        // Schedule two greetings in parallel (DTF AsyncDynamicProxyGreetingsTest equivalent)
        let f1 = ctx.schedule_activity("Greetings", "Gabbar").into_activity();
        let f2 = ctx.schedule_activity("Greetings", "Samba").into_activity();
        let (r1, r2) = join(f1, f2).await;
        Ok(format!("{}, {}", r1.unwrap(), r2.unwrap()))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Greetings", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-dtf-greetings", "Greetings", "").await;
    let (_hist, out) = handle.unwrap().await.unwrap();
    assert_eq!(out.unwrap(), "Hello, Gabbar!, Hello, Samba!");
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
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();

    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        let now = ctx.system_now_ms().await;
        let guid = ctx.system_new_guid().await;
        ctx.trace_info(format!("system now={now}, guid={guid}"));
        Ok(format!("n={now},g={guid}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("SystemActivities", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration("inst-system-acts", "SystemActivities", "").await;
    let (_hist, out) = handle.unwrap().await.unwrap();
    let out = out.unwrap();
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

/// Sample: start an orchestration and poll its status until completion.
#[tokio::test]
async fn sample_status_polling_fs() {
    
    use rust_dtf::OrchestrationStatus;
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();
    let orchestration = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_timer(20).into_timer().await;
        Ok("done".to_string())
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("StatusSample", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let _h = rt.clone().start_orchestration("inst-status-sample", "StatusSample", "").await.unwrap();

    // Poll status until Completed
    loop {
        match rt.get_orchestration_status("inst-status-sample").await {
            OrchestrationStatus::Completed { output } => { assert_eq!(output, "done"); break; }
            OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
            OrchestrationStatus::Running => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            OrchestrationStatus::NotFound => panic!("instance not found"),
        }
    }
    rt.shutdown().await;
}

/// Sub-orchestrations: simple parent/child orchestration.
///
/// Highlights:
/// - Parent calls a child orchestration and awaits its result
/// - Child uses an activity and returns its output
#[tokio::test]
async fn sample_sub_orchestration_basic_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("Upper", |input: String| async move { Ok(input.to_uppercase()) })
        .build();

    let child_upper = |ctx: OrchestrationContext, input: String| async move {
        let up = ctx.schedule_activity("Upper", input).into_activity().await.unwrap();
        Ok(up)
    };
    let parent = |ctx: OrchestrationContext, input: String| async move {
        let r = ctx.schedule_sub_orchestration("ChildUpper", input).into_sub_orchestration().await.unwrap();
        Ok(format!("parent:{r}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ChildUpper", child_upper)
        .register("Parent", parent)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let h = rt.clone().start_orchestration("inst-sub-basic", "Parent", "hi").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "parent:HI");
    rt.shutdown().await;
}

/// Sub-orchestrations: fan-out to multiple children and join.
///
/// Highlights:
/// - Parent starts two child orchestrations in parallel
/// - Uses `futures::join` to await both and aggregates results
#[tokio::test]
async fn sample_sub_orchestration_fanout_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("Add", |input: String| async move {
            let mut it = input.split(',');
            let a = it.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            let b = it.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
            Ok((a + b).to_string())
        })
        .build();

    let child_sum = |ctx: OrchestrationContext, input: String| async move {
        let s = ctx.schedule_activity("Add", input).into_activity().await.unwrap();
        Ok(s)
    };
    let parent = |ctx: OrchestrationContext, _input: String| async move {
        let a = ctx.schedule_sub_orchestration("ChildSum", "1,2").into_sub_orchestration();
        let b = ctx.schedule_sub_orchestration("ChildSum", "3,4").into_sub_orchestration();
        let (ra, rb) = join(a, b).await;
        let total = ra.unwrap().parse::<i64>().unwrap() + rb.unwrap().parse::<i64>().unwrap();
        Ok(format!("total={total}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("ChildSum", child_sum)
        .register("ParentFan", parent)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let h = rt.clone().start_orchestration("inst-sub-fan", "ParentFan", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "total=10");
    rt.shutdown().await;
}

/// Sub-orchestrations: chained (root -> mid -> leaf).
///
/// Highlights:
/// - Root calls Mid; Mid calls Leaf; each returns a transformed value
/// - Demonstrates nested sub-orchestrations
#[tokio::test]
async fn sample_sub_orchestration_chained_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("AppendX", |input: String| async move { Ok(format!("{input}x")) })
        .build();

    let leaf = |ctx: OrchestrationContext, input: String| async move {
        Ok(ctx.schedule_activity("AppendX", input).into_activity().await.unwrap())
    };
    let mid = |ctx: OrchestrationContext, input: String| async move {
        let r = ctx.schedule_sub_orchestration("Leaf", input).into_sub_orchestration().await.unwrap();
        Ok(format!("{r}-mid"))
    };
    let root = |ctx: OrchestrationContext, input: String| async move {
        let r = ctx.schedule_sub_orchestration("Mid", input).into_sub_orchestration().await.unwrap();
        Ok(format!("root:{r}"))
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Leaf", leaf)
        .register("Mid", mid)
        .register("Root", root)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let h = rt.clone().start_orchestration("inst-sub-chain", "Root", "a").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "root:ax-mid");
    rt.shutdown().await;
}

/// Detached orchestration scheduling: start independent orchestrations without awaiting.
///
/// Highlights:
/// - Use `ctx.schedule_orchestration(name, instance, input)` with explicit instance IDs
/// - No parent/child semantics; scheduled orchestrations are independent roots
/// - Verify scheduled instances complete via status polling
#[tokio::test]
async fn sample_detached_orchestration_scheduling_fs() {
    use rust_dtf::OrchestrationStatus;
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("Echo", |input: String| async move { Ok(input) })
        .build();

    let chained = |ctx: OrchestrationContext, input: String| async move {
        ctx.schedule_timer(5).into_timer().await;
        Ok(ctx.schedule_activity("Echo", input).into_activity().await.unwrap())
    };
    let coordinator = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_orchestration("Chained", "W1", "A");
        ctx.schedule_orchestration("Chained", "W2", "B");
        Ok("scheduled".to_string())
    };

    let orchestration_registry = OrchestrationRegistry::builder()
        .register("Chained", chained)
        .register("Coordinator", coordinator)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let h = rt.clone().start_orchestration("CoordinatorRoot", "Coordinator", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "scheduled");

    // The scheduled instances are plain W1/W2 (no prefixing)
    let insts = vec!["W1".to_string(), "W2".to_string()];
    for inst in insts {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match rt.get_orchestration_status(&inst).await {
                OrchestrationStatus::Completed { output } => { assert!(output == "A" || output == "B"); break; }
                OrchestrationStatus::Failed { error } => panic!("scheduled orchestration failed: {error}"),
                OrchestrationStatus::Running | OrchestrationStatus::NotFound => {
                    if std::time::Instant::now() > deadline { panic!("timeout waiting for {inst}"); }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
    }

    rt.shutdown().await;
}


