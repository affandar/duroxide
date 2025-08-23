use std::sync::Arc as StdArc;
use rust_dtf::{OrchestrationRegistry, OrchestrationContext};
use rust_dtf::runtime::{self};
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::providers::in_memory::InMemoryHistoryStore;

#[tokio::test]
async fn runtime_start_versioned_string_uses_explicit_version() {
    let v1 = |_: OrchestrationContext, _in: String| async move { Ok("v1".to_string()) };
    let v2 = |_: OrchestrationContext, _in: String| async move { Ok("v2".to_string()) };
    let reg = OrchestrationRegistry::builder()
        .register("S", v1)
        .register_versioned("S", "2.0.0", v2)
        .set_policy("S", rust_dtf::runtime::VersionPolicy::Latest)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(StdArc::new(InMemoryHistoryStore::default()), StdArc::new(acts), reg).await;
    let h = rt.clone().start_orchestration_versioned("i1", "S", "1.0.0", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "v1");
    rt.shutdown().await;
}

#[tokio::test]
async fn runtime_start_versioned_typed_uses_explicit_version() {
    let v1 = |_: OrchestrationContext, _in: i32| async move { Ok::<i32, String>(1) };
    let v2 = |_: OrchestrationContext, _in: i32| async move { Ok::<i32, String>(2) };
    let reg = OrchestrationRegistry::builder()
        .register_typed::<i32, i32, _, _>("T", v1)
        .register_versioned("T", "2.0.0", move |ctx, s| {
            async move { let _ : i32 = serde_json::from_str(&s).unwrap_or_default(); v2(ctx, 0).await.map(|n| n.to_string()) }
        })
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(StdArc::new(InMemoryHistoryStore::default()), StdArc::new(acts), reg).await;
    let h = rt.clone().start_orchestration_versioned_typed::<i32, i32>("i2", "T", "1.0.0", 0).await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), 1);
    rt.shutdown().await;
}

#[tokio::test]
async fn sub_orchestration_versioned_explicit_and_policy() {
    let child_v1 = |_: OrchestrationContext, _in: String| async move { Ok("c1".to_string()) };
    let child_v2 = |_: OrchestrationContext, _in: String| async move { Ok("c2".to_string()) };
    let parent_explicit = |ctx: OrchestrationContext, _in: String| async move {
        let a = ctx.schedule_sub_orchestration_versioned("C", Some("1.0.0".to_string()), "A").into_sub_orchestration().await.unwrap();
        Ok(a)
    };
    let parent_policy = |ctx: OrchestrationContext, _in: String| async move {
        let b = ctx.schedule_sub_orchestration("C", "B").into_sub_orchestration().await.unwrap();
        Ok(b)
    };
    let reg = OrchestrationRegistry::builder()
        .register("P1", parent_explicit)
        .register("P2", parent_policy)
        .register("C", child_v1)
        .register_versioned("C", "2.0.0", child_v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()), StdArc::new(acts), reg).await;
    let h1 = rt.clone().start_orchestration("i3-1", "P1", "").await.unwrap();
    let (_hist1, out1) = h1.await.unwrap();
    assert_eq!(out1.unwrap(), "c1");
    let h2 = rt.clone().start_orchestration("i3-2", "P2", "").await.unwrap();
    let (_hist2, out2) = h2.await.unwrap();
    assert_eq!(out2.unwrap(), "c2");
    rt.shutdown().await;
}

#[tokio::test]
async fn detached_versioned_uses_policy_latest() {
    let leaf_v1 = |_: OrchestrationContext, _in: String| async move { Ok("l1".to_string()) };
    let leaf_v2 = |_: OrchestrationContext, _in: String| async move { Ok("l2".to_string()) };
    let parent = |ctx: OrchestrationContext, _in: String| async move {
        // Detached start uses registry policy (latest) for version
        ctx.schedule_orchestration("Leaf", "child-1", "");
        Ok("ok".to_string())
    };
    let reg = OrchestrationRegistry::builder()
        .register("Parent", parent)
        .register("Leaf", leaf_v1)
        .register_versioned("Leaf", "2.0.0", leaf_v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let store = StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let h = rt.clone().start_orchestration("i4", "Parent", "").await.unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "ok");
    // Start the detached child directly to observe its versioned output
    let (_hh, out_child) = rt.clone().start_orchestration("i4::child-1", "Leaf", "").await.unwrap().await.unwrap();
    assert_eq!(out_child.unwrap(), "l2");
    rt.shutdown().await;
}

#[tokio::test]
async fn continue_as_new_versioned_typed_explicit() {
    let v1 = |ctx: OrchestrationContext, _in: String| async move { ctx.continue_as_new_versioned("2.0.0", "payload"); Ok(String::new()) };
    let v2 = |_ctx: OrchestrationContext, _input: String| async move { Ok("done".to_string()) };
    let reg = OrchestrationRegistry::builder()
        .register("Up", v1)
        .register_versioned("Up", "2.0.0", v2)
        .build();
    let rt = runtime::Runtime::start_with_store(StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()), StdArc::new(ActivityRegistry::builder().build()), reg).await;
    let h = rt.clone().start_orchestration("i5", "Up", "").await.unwrap();
    let (_hist, _out) = h.await.unwrap();
    // Use wait helper instead of polling
    match rt.wait_for_orchestration("i5", std::time::Duration::from_secs(3)).await.unwrap() {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output.as_str(), "done"),
        runtime::OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
        _ => unreachable!(),
    }
    rt.shutdown().await;
}


