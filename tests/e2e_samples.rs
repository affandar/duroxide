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
use futures::{FutureExt, pin_mut, select};
use serde::{Serialize, Deserialize};
mod common;

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

/// ContinueAsNew sample: roll over input across executions until a condition is met.
///
/// Highlights:
/// - Use `ctx.continue_as_new(new_input)` to terminate current execution and start a new one
/// - Provider keeps all execution histories; latest execution holds the final result
#[tokio::test]
async fn sample_continue_as_new_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();
    let orch = |ctx: OrchestrationContext, input: String| async move {
        let n: u32 = input.parse().unwrap_or(0);
        if n < 3 {
            ctx.trace_info(format!("CAN sample n={n} -> continue"));
            ctx.continue_as_new((n + 1).to_string());
            Ok(String::new())
        } else {
            Ok(format!("final:{n}"))
        }
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register("CanSample", orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    // Initial handle finishes after ContinueAsNew; final result is available after latest execution completes
    let h = rt.clone().start_orchestration("inst-sample-can", "CanSample", "0").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "");
    // Poll until final completion
    use rust_dtf::OrchestrationStatus;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut final_out = None;
    while std::time::Instant::now() < deadline {
        match rt.get_orchestration_status("inst-sample-can").await {
            OrchestrationStatus::Completed { output } => { final_out = Some(output); break; }
            OrchestrationStatus::Failed { error } => panic!("failed: {error}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    assert_eq!(final_out.unwrap(), "final:3");
    // Check executions exist
    let execs = store.list_executions("inst-sample-can").await;
    assert_eq!(execs, vec![1,2,3,4]);
    rt.shutdown().await;
}


// Typed samples

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AddReq { a: i32, b: i32 }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AddRes { sum: i32 }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Ack { ok: bool }

/// Typed activity + typed orchestration: Add two numbers and return a struct
#[tokio::test]
async fn sample_typed_activity_and_orchestration_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register_typed::<AddReq, AddRes, _, _>("Add", |req| async move { Ok(AddRes { sum: req.a + req.b }) })
        .build();

    let orchestration = |ctx: OrchestrationContext, req: AddReq| async move {
        let out: AddRes = ctx
            .schedule_activity_typed::<AddReq, AddRes>("Add", &req)
            .into_activity_typed::<AddRes>()
            .await?;
        Ok(out)
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<AddReq, AddRes, _, _>("Adder", orchestration)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let handle = rt.clone().start_orchestration_typed::<AddReq, AddRes>("inst-typed-add", "Adder", AddReq { a: 2, b: 3 }).await.unwrap();
    let (_hist, out) = handle.await.unwrap();
    assert_eq!(out.unwrap(), AddRes { sum: 5 });
    rt.shutdown().await;
}

/// Typed external event sample: await Ack { ok } from an event
#[tokio::test]
async fn sample_typed_event_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder().build();
    let orch = |ctx: OrchestrationContext, _in: ()| async move {
        let ack: Ack = ctx.schedule_wait_typed::<Ack>("Ready").into_event_typed::<Ack>().await;
        Ok::<_, String>(serde_json::to_string(&ack).unwrap())
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<(), String, _, _>("WaitAck", orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activity_registry), orchestration_registry).await;
    let store_for_wait = store.clone();
    let rt_c = rt.clone();
    tokio::spawn(async move {
        let _ = common::wait_for_subscription(store_for_wait, "inst-typed-ack", "Ready", 1000).await;
        // Raise typed event by serializing payload
        let payload = serde_json::to_string(&Ack { ok: true }).unwrap();
        rt_c.raise_event("inst-typed-ack", "Ready", payload).await;
    });
    let h = rt.clone().start_orchestration_typed::<(), String>("inst-typed-ack", "WaitAck", ()).await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), serde_json::to_string(&Ack { ok: true }).unwrap());
    rt.shutdown().await;
}

/// Mixed string and typed activities with typed orchestration, showcasing select on typed+string
#[tokio::test]
async fn sample_mixed_string_and_typed_typed_orch_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // String activity: returns uppercased string
    // Typed activity: Add two numbers
    let activity_registry = ActivityRegistry::builder()
        .register("Upper", |input: String| async move { Ok(input.to_uppercase()) })
        .register_typed::<AddReq, AddRes, _, _>("Add", |req| async move { Ok(AddRes { sum: req.a + req.b }) })
        .build();

    // Typed orchestrator input/output
    let orch = |ctx: OrchestrationContext, req: AddReq| async move {
        // Kick off a typed activity and a string activity, race them with select!
        let f_typed = ctx
            .schedule_activity_typed::<AddReq, AddRes>("Add", &req)
            .into_activity_typed::<AddRes>()
            .map(|r| r.map(|v| format!("sum={}", v.sum)))
            .fuse();
        let f_str = ctx
            .schedule_activity("Upper", "hello")
            .into_activity()
            .map(|r| r.map(|v| format!("up={v}")))
            .fuse();
        pin_mut!(f_typed, f_str);
        let first = select! {
            a = f_typed => a,
            b = f_str => b,
        };
        // Return whichever completed first
        Ok::<_, String>(first.unwrap())
    };
    let orchestration_registry = OrchestrationRegistry::builder()
        .register_typed::<AddReq, String, _, _>("MixedTypedOrch", orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orchestration_registry).await;
    let h = rt.clone().start_orchestration_typed::<AddReq, String>("inst-mixed-typed", "MixedTypedOrch", AddReq { a: 1, b: 2 }).await.unwrap();
    let (_hist, out) = h.await.unwrap();
    let s = out.unwrap();
    assert!(s == "sum=3" || s == "up=HELLO");
    rt.shutdown().await;
}

/// Mixed string and typed activities with string orchestration, showcasing select on typed+string
#[tokio::test]
async fn sample_mixed_string_and_typed_string_orch_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let activity_registry = ActivityRegistry::builder()
        .register("Upper", |input: String| async move { Ok(input.to_uppercase()) })
        .register_typed::<AddReq, AddRes, _, _>("Add", |req| async move { Ok(AddRes { sum: req.a + req.b }) })
        .build();

    // String orchestrator mixes typed and string activity calls
    let orch = |ctx: OrchestrationContext, _in: String| async move {
        let f_typed = ctx
            .schedule_activity_typed::<AddReq, AddRes>("Add", &AddReq { a: 5, b: 7 })
            .into_activity_typed::<AddRes>()
            .map(|r| r.map(|v| format!("sum={}", v.sum)))
            .fuse();
        let f_str = ctx
            .schedule_activity("Upper", "race")
            .into_activity()
            .map(|r| r.map(|v| format!("up={v}")))
            .fuse();
        pin_mut!(f_typed, f_str);
        let first = select! {
            a = f_typed => a,
            b = f_str => b,
        };
        Ok::<_, String>(first.unwrap())
    };
    let orch_reg = OrchestrationRegistry::builder()
        .register("MixedStringOrch", orch)
        .build();

    let rt = runtime::Runtime::start_with_store(store, Arc::new(activity_registry), orch_reg).await;
    let h = rt.clone().start_orchestration("inst-mixed-string", "MixedStringOrch", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    let s = out.unwrap();
    assert!(s == "sum=12" || s == "up=RACE");
    rt.shutdown().await;
}

/// Versioning: default latest vs pinned exact on start
///
/// Highlights:
/// - Register two versions of the same orchestration using semver (1.0.0 and 2.0.0)
/// - Default policy (Latest) picks the highest on new starts
/// - Changing policy to Exact pins new starts to a specific version
#[tokio::test]
async fn sample_versioning_start_latest_vs_exact_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // Two versions: return a string indicating which version executed
    let v1 = |_: OrchestrationContext, _in: String| async move { Ok("v1".to_string()) };
    let v2 = |_: OrchestrationContext, _in: String| async move { Ok("v2".to_string()) };

    let reg = OrchestrationRegistry::builder()
        // Default registration is 1.0.0
        .register("Versioned", v1)
        // Add a later version 2.0.0
        .register_versioned("Versioned", "2.0.0", v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(acts), reg.clone()).await;

    // With default policy (Latest), a new start should run v2
    let h_latest = rt.clone().start_orchestration("inst-vers-latest", "Versioned", "").await.unwrap();
    let (_hist_l, out_l) = h_latest.await.unwrap();
    assert_eq!(out_l.unwrap(), "v2");

    // Pin new starts to 1.0.0 via policy, verify it runs v1
    reg.set_version_policy("Versioned", rust_dtf::runtime::VersionPolicy::Exact(semver::Version::parse("1.0.0").unwrap())).await;
    let h_exact = rt.clone().start_orchestration("inst-vers-exact", "Versioned", "").await.unwrap();
    let (_hist_e, out_e) = h_exact.await.unwrap();
    assert_eq!(out_e.unwrap(), "v1");

    rt.shutdown().await;
}

/// Versioning: sub-orchestration explicit version vs default policy
///
/// Highlights:
/// - Parent calls child once with an explicit version and once without
/// - The explicit call uses 1.0.0; the policy (Latest) uses 2.0.0
#[tokio::test]
async fn sample_versioning_sub_orchestration_explicit_vs_policy_fs() {
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    let child_v1 = |_: OrchestrationContext, _in: String| async move { Ok("c1".to_string()) };
    let child_v2 = |_: OrchestrationContext, _in: String| async move { Ok("c2".to_string()) };
    let parent = |ctx: OrchestrationContext, _in: String| async move {
        // Explicit versioned call -> expect c1
        let a = ctx
            .schedule_sub_orchestration_versioned("Child", Some("1.0.0".to_string()), "exp")
            .into_sub_orchestration()
            .await
            .unwrap();
        // Policy-based call (Latest) -> expect c2
        let b = ctx
            .schedule_sub_orchestration("Child", "pol")
            .into_sub_orchestration()
            .await
            .unwrap();
        Ok(format!("{a}-{b}"))
    };

    let reg = OrchestrationRegistry::builder()
        .register("ParentVers", parent)
        .register("Child", child_v1)
        .register_versioned("Child", "2.0.0", child_v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store, Arc::new(acts), reg).await;

    let h = rt.clone().start_orchestration("inst-sub-vers", "ParentVers", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "c1-c2");

    rt.shutdown().await;
}

/// Versioning + ContinueAsNew: safe upgrade of a long-running (infinite) orchestration
///
/// Highlights:
/// - Use `continue_as_new(new_input)` to roll to a fresh execution that picks the default version
///   from the registry policy (Latest by default, or a pinned Exact if set)
/// - Avoids nondeterminism because the new execution starts fresh at the version boundary
/// - Carry forward state via the CAN input, or transform as needed during upgrade
#[tokio::test]
async fn sample_versioning_continue_as_new_upgrade_fs() {
    use rust_dtf::OrchestrationStatus;
    let td = tempfile::tempdir().unwrap();
    let store = StdArc::new(FsHistoryStore::new(td.path(), true)) as StdArc<dyn HistoryStore>;

    // v1: simulate deciding to upgrade at a maintenance boundary (e.g., at the end of a cycle)
    // In a real infinite loop, you'd do some work (timer/activity), then CAN to v2.
    let v1 = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("v1: upgrading via ContinueAsNew (default policy)".to_string());
        // Roll to a fresh execution, marking the payload so we can attribute it to v1 deterministically
        ctx.continue_as_new(format!("v1:{input}"));
        Ok(String::new())
    };
    // v2: represents the upgraded logic. Here we just simulate one step and complete for the sample.
    let v2 = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info(format!("v2: resumed with input={input}"));
        Ok(format!("upgraded:{input}"))
    };

    let reg = OrchestrationRegistry::builder()
        .register("LongRunner", v1)                // implicit 1.0.0
        .register_versioned("LongRunner", "2.0.0", v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(acts), reg).await;

    // Start on v1; the first handle will resolve at the CAN boundary
    // Pin initial start to v1 explicitly to demonstrate upgrade via CAN; default policy remains Latest (v2)
    let h = rt.clone().start_orchestration_versioned("inst-can-upgrade", "LongRunner", "1.0.0", "state" ).await.unwrap();
    let _ = h.await.unwrap();

    // Poll for the new execution (v2) to complete
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match rt.get_orchestration_status("inst-can-upgrade").await {
            OrchestrationStatus::Completed { output } => { assert_eq!(output, "upgraded:v1:state"); break; }
            OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
            _ if std::time::Instant::now() < deadline => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            _ => panic!("timeout waiting for upgraded completion"),
        }
    }

    // Verify two executions exist, exec1 continued-as-new, exec2 completed with v2 output
    let execs = store.list_executions("inst-can-upgrade").await;
    assert_eq!(execs, vec![1, 2]);
    let e1 = store.read_with_execution("inst-can-upgrade", 1).await;
    assert!(e1.iter().any(|e| matches!(e, rust_dtf::Event::OrchestrationContinuedAsNew { .. })));
    // Exec2 must start with the v1-marked payload, proving v1 ran first and handed off via CAN
    let e2 = store.read_with_execution("inst-can-upgrade", 2).await;
    assert!(e2.iter().any(|e| matches!(e, rust_dtf::Event::OrchestrationStarted { input, .. } if input == "v1:state")));

    rt.shutdown().await;
}
