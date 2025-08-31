use rust_dtf::providers::in_memory::InMemoryHistoryStore;
use rust_dtf::runtime::registry::ActivityRegistry;
use rust_dtf::runtime::{self};
use rust_dtf::{Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;

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
    let rt =
        runtime::Runtime::start_with_store(StdArc::new(InMemoryHistoryStore::default()), StdArc::new(acts), reg).await;
    let h = rt
        .clone()
        .start_orchestration_versioned("i1", "S", "1.0.0", "")
        .await
        .unwrap();
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
        .register_versioned("T", "2.0.0", move |ctx, s| async move {
            let _: i32 = serde_json::from_str(&s).unwrap_or_default();
            v2(ctx, 0).await.map(|n| n.to_string())
        })
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt =
        runtime::Runtime::start_with_store(StdArc::new(InMemoryHistoryStore::default()), StdArc::new(acts), reg).await;
    let h = rt
        .clone()
        .start_orchestration_versioned_typed::<i32, i32>("i2", "T", "1.0.0", 0)
        .await
        .unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), 1);
    rt.shutdown().await;
}

#[tokio::test]
async fn sub_orchestration_versioned_explicit_and_policy() {
    let child_v1 = |_: OrchestrationContext, _in: String| async move { Ok("c1".to_string()) };
    let child_v2 = |_: OrchestrationContext, _in: String| async move { Ok("c2".to_string()) };
    let parent_explicit = |ctx: OrchestrationContext, _in: String| async move {
        let a = ctx
            .schedule_sub_orchestration_versioned("C", Some("1.0.0".to_string()), "A")
            .into_sub_orchestration()
            .await
            .unwrap();
        Ok(a)
    };
    let parent_policy = |ctx: OrchestrationContext, _in: String| async move {
        let b = ctx
            .schedule_sub_orchestration("C", "B")
            .into_sub_orchestration()
            .await
            .unwrap();
        Ok(b)
    };
    let reg = OrchestrationRegistry::builder()
        .register("P1", parent_explicit)
        .register("P2", parent_policy)
        .register("C", child_v1)
        .register_versioned("C", "2.0.0", child_v2)
        .build();
    let acts = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()),
        StdArc::new(acts),
        reg,
    )
    .await;
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
    let (_hh, out_child) = rt
        .clone()
        .start_orchestration("i4::child-1", "Leaf", "")
        .await
        .unwrap()
        .await
        .unwrap();
    assert_eq!(out_child.unwrap(), "l2");
    rt.shutdown().await;
}

#[tokio::test]
async fn continue_as_new_versioned_typed_explicit() {
    let v1 = |ctx: OrchestrationContext, _in: String| async move {
        ctx.continue_as_new_versioned("2.0.0", "payload");
        Ok(String::new())
    };
    let v2 = |_ctx: OrchestrationContext, _input: String| async move { Ok("done".to_string()) };
    let reg = OrchestrationRegistry::builder()
        .register("Up", v1)
        .register_versioned("Up", "2.0.0", v2)
        .build();
    let rt = runtime::Runtime::start_with_store(
        StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()),
        StdArc::new(ActivityRegistry::builder().build()),
        reg,
    )
    .await;
    let h = rt.clone().start_orchestration("i5", "Up", "").await.unwrap();
    let (_hist, _out) = h.await.unwrap();
    // Use wait helper instead of polling
    match rt
        .wait_for_orchestration("i5", std::time::Duration::from_secs(3))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output.as_str(), "done"),
        runtime::OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
        _ => unreachable!(),
    }
    rt.shutdown().await;
}
// merged sections below reuse imports from the top of this file

#[tokio::test]
async fn start_uses_latest_version() {
    // v1 returns "v1:", v1.1 returns "v1.1:"
    let v1 = |_: OrchestrationContext, input: String| async move { Ok(format!("v1:{input}")) };
    let v11 = |_: OrchestrationContext, input: String| async move { Ok(format!("v1.1:{input}")) };

    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", v1)
        .register_versioned("OrderFlow", "1.1.0", v11)
        .build();

    let activities = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()),
        StdArc::new(activities),
        reg,
    )
    .await;

    let h = rt
        .clone()
        .start_orchestration("inst-vlatest", "OrderFlow", "X")
        .await
        .unwrap();
    let (hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "v1.1:X");
    assert!(matches!(hist.last().unwrap(), Event::OrchestrationCompleted { .. }));
    rt.shutdown().await;
}

#[tokio::test]
async fn policy_exact_pins_start() {
    let v1 = |_: OrchestrationContext, input: String| async move { Ok(format!("v1:{input}")) };
    let v11 = |_: OrchestrationContext, input: String| async move { Ok(format!("v1.1:{input}")) };

    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", v1)
        .register_versioned("OrderFlow", "1.1.0", v11)
        .build();
    // Pin new starts to 1.0.0
    reg.set_version_policy(
        "OrderFlow",
        rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
    )
    .await;

    let activities = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()),
        StdArc::new(activities),
        reg,
    )
    .await;

    let h = rt
        .clone()
        .start_orchestration("inst-vpin", "OrderFlow", "Y")
        .await
        .unwrap();
    let (_hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "v1:Y");
    rt.shutdown().await;
}

#[tokio::test]
async fn sub_orchestration_uses_latest_by_default_and_pinned_when_set() {
    // Child versions
    let child_v1 = |_: OrchestrationContext, input: String| async move { Ok(format!("c1:{input}")) };
    let child_v11 = |_: OrchestrationContext, input: String| async move { Ok(format!("c1.1:{input}")) };
    // Parent: call child and return its output
    let parent = |ctx: OrchestrationContext, input: String| async move {
        let res = ctx
            .schedule_sub_orchestration("ChildFlow", input)
            .into_sub_orchestration()
            .await
            .unwrap();
        Ok(res)
    };

    let reg = OrchestrationRegistry::builder()
        .register("ParentFlow", parent)
        .register("ChildFlow", child_v1)
        .register_versioned("ChildFlow", "1.1.0", child_v11)
        .build();

    let activities = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()),
        StdArc::new(activities),
        reg.clone(),
    )
    .await;

    // Default latest for child = 1.1.0
    let h1 = rt
        .clone()
        .start_orchestration("inst-child-latest", "ParentFlow", "Z")
        .await
        .unwrap();
    let (_hist1, out1) = h1.await.unwrap();
    assert_eq!(out1.unwrap(), "c1.1:Z");

    // Pin child to 1.0.0 via policy
    reg.set_version_policy(
        "ChildFlow",
        rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
    )
    .await;
    let h2 = rt
        .clone()
        .start_orchestration("inst-child-pinned", "ParentFlow", "Q")
        .await
        .unwrap();
    let (_hist2, out2) = h2.await.unwrap();
    assert_eq!(out2.unwrap(), "c1:Q");

    rt.shutdown().await;
}

#[tokio::test]
async fn parent_calls_child_upgrade_child_and_verify_latest_used() {
    // Child v1 and v1.1
    let child_v1 = |_: OrchestrationContext, input: String| async move { Ok(format!("cv1:{input}")) };
    let child_v11 = |_: OrchestrationContext, input: String| async move { Ok(format!("cv1.1:{input}")) };
    // Parent calls child and returns result
    let parent = |ctx: OrchestrationContext, input: String| async move {
        let res = ctx
            .schedule_sub_orchestration("Child", input)
            .into_sub_orchestration()
            .await
            .unwrap();
        Ok(res)
    };

    let reg = OrchestrationRegistry::builder()
        .register("Parent", parent)
        .register("Child", child_v1)
        .register_versioned("Child", "1.1.0", child_v11)
        .build();
    let activities = ActivityRegistry::builder().build();
    let store = StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;

    // Start new parent after both child versions registered => latest child (1.1.0) should be used
    let h = rt
        .clone()
        .start_orchestration("inst-parent-child-upgrade", "Parent", "inp")
        .await
        .unwrap();
    let (hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "cv1.1:inp");
    assert!(matches!(hist.last().unwrap(), Event::OrchestrationCompleted { .. }));
    // History should include SubOrchestrationCompleted
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::SubOrchestrationCompleted { .. }))
    );
    rt.shutdown().await;
}

#[tokio::test]
async fn continue_as_new_upgrades_version_deterministically() {
    // v1 continues-as-new to v2; v2 completes
    let v1 = |ctx: OrchestrationContext, _input: String| async move {
        // Explicitly upgrade to v2 on CAN
        ctx.continue_as_new_versioned("2.0.0", "from_v1_to_v2");
        Ok(String::new())
    };
    let v2 = |_ctx: OrchestrationContext, input: String| async move { Ok(format!("v2_done:{input}")) };

    let reg = OrchestrationRegistry::builder()
        .register("Upgrader", v1)
        .register_versioned("Upgrader", "2.0.0", v2)
        .set_policy(
            "Upgrader",
            rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
        )
        .build();
    let activities = ActivityRegistry::builder().build();
    let store = StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;

    let h = rt
        .clone()
        .start_orchestration("inst-can-upgrade", "Upgrader", "seed")
        .await
        .unwrap();
    let (hist, out) = h.await.unwrap();
    // With polling approach, handle waits for final completion
    assert_eq!(out.unwrap(), "v2_done:from_v1_to_v2");
    // History contains the final execution's events
    assert!(!hist.is_empty(), "Expected non-empty history");

    // Verify terminal status is also correct
    match rt
        .wait_for_orchestration("inst-can-upgrade", std::time::Duration::from_secs(1))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "v2_done:from_v1_to_v2")
        }
        runtime::OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
        _ => unreachable!(),
    }
    rt.shutdown().await;
}

// imports already present above for merged section
use semver::Version;

fn handler_echo() -> impl Fn(
    OrchestrationContext,
    String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
+ Send
+ Sync
+ Clone
+ 'static {
    #[derive(Clone)]
    struct Echo;
    impl Echo {
        fn call(
            &self,
            _ctx: OrchestrationContext,
            input: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
            Box::pin(async move { Ok(input) })
        }
    }
    let f = Echo;
    move |ctx, input| f.call(ctx, input)
}

#[test]
fn register_default_is_1_0_0_and_list_versions() {
    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo())
        .build();
    let names = reg.list_orchestration_names();
    assert!(names.contains(&"OrderFlow".to_string()));
    let vs = reg.list_orchestration_versions("OrderFlow");
    assert_eq!(vs, vec![Version::parse("1.0.0").unwrap()]);
}

#[test]
fn register_multiple_versions_latest_is_highest() {
    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo()) // 1.0.0
        .register_versioned("OrderFlow", "1.1.0", handler_echo())
        .register_versioned("OrderFlow", "2.0.0", handler_echo())
        .build();
    let mut vs = reg.list_orchestration_versions("OrderFlow");
    vs.sort();
    assert_eq!(
        vs,
        vec![
            Version::parse("1.0.0").unwrap(),
            Version::parse("1.1.0").unwrap(),
            Version::parse("2.0.0").unwrap(),
        ]
    );
}

#[tokio::test]
async fn policy_exact_pins_resolve_for_start() {
    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo()) // 1.0.0
        .register_versioned("OrderFlow", "1.1.0", handler_echo())
        .build();
    // Default Latest -> 1.1.0
    let (v_latest, _h) = reg.resolve_for_start("OrderFlow").await.expect("resolve latest");
    assert_eq!(v_latest, Version::parse("1.1.0").unwrap());
    // Pin to 1.0.0
    reg.set_version_policy(
        "OrderFlow",
        rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
    )
    .await;
    let (v_pinned, _h) = reg.resolve_for_start("OrderFlow").await.expect("resolve pinned");
    assert_eq!(v_pinned, Version::parse("1.0.0").unwrap());
    // Unpin back to Latest
    reg.unpin("OrderFlow").await;
    let (v_unpinned, _h) = reg.resolve_for_start("OrderFlow").await.expect("resolve unpinned");
    assert_eq!(v_unpinned, Version::parse("1.1.0").unwrap());
}

#[test]
fn duplicate_default_version_returns_error() {
    let res = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo())
        .register("OrderFlow", handler_echo())
        .build_result();
    assert!(res.is_err());
    let msg = res.err().unwrap();
    assert!(msg.contains("duplicate orchestration registration"));
}

#[test]
fn duplicate_specific_version_returns_error() {
    let res = OrchestrationRegistry::builder()
        .register_versioned("OrderFlow", "1.2.0", handler_echo())
        .register_versioned("OrderFlow", "1.2.0", handler_echo())
        .build_result();
    assert!(res.is_err());
    let msg = res.err().unwrap();
    assert!(msg.contains("duplicate orchestration registration"));
}

#[test]
#[should_panic(expected = "non-monotonic orchestration version")]
fn non_monotonic_registration_panics() {
    let _ = OrchestrationRegistry::builder()
        .register_versioned("OrderFlow", "2.0.0", handler_echo())
        .register_versioned("OrderFlow", "1.1.0", handler_echo());
}

#[test]
fn resolve_exact_missing_returns_none() {
    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo())
        .build();
    assert!(
        reg.resolve_exact("OrderFlow", &Version::parse("9.9.9").unwrap())
            .is_none()
    );
    assert!(
        reg.resolve_exact("Missing", &Version::parse("1.0.0").unwrap())
            .is_none()
    );
}
