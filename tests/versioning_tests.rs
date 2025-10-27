use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{Client, Event, OrchestrationContext, OrchestrationRegistry};
use std::sync::Arc as StdArc;

#[tokio::test]
async fn runtime_start_versioned_string_uses_explicit_version() {
    let v1 = |_: OrchestrationContext, _in: String| async move { Ok("v1".to_string()) };
    let v2 = |_: OrchestrationContext, _in: String| async move { Ok("v2".to_string()) };
    let reg = OrchestrationRegistry::builder()
        .register("S", v1)
        .register_versioned("S", "2.0.0", v2)
        .set_policy("S", duroxide::runtime::VersionPolicy::Latest)
        .build();
    let acts = ActivityRegistry::builder().build();
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration_versioned("i1", "S", "1.0.0", "")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("i1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v1"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration_versioned_typed::<i32>("i2", "T", "1.0.0", 0)
        .await
        .unwrap();

    match client
        .wait_for_orchestration_typed::<i32>("i2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        Ok(result) => assert_eq!(result, 1),
        Err(error) => panic!("orchestration failed: {error}"),
    }
    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());
    client.start_orchestration("i3-1", "P1", "").await.unwrap();
    match client
        .wait_for_orchestration("i3-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "c1"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    client.start_orchestration("i3-2", "P2", "").await.unwrap();
    match client
        .wait_for_orchestration("i3-2", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "c2"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(acts), reg).await;
    let client = Client::new(store.clone());
    client.start_orchestration("i4", "Parent", "").await.unwrap();

    match client
        .wait_for_orchestration("i4", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    // Start the detached child directly to observe its versioned output
    client.start_orchestration("i4::child-1", "Leaf", "").await.unwrap();

    let child_status = client
        .wait_for_orchestration("i4::child-1", std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let out_child = match child_status {
        duroxide::OrchestrationStatus::Completed { output } => output,
        duroxide::OrchestrationStatus::Failed { error } => panic!("child orchestration failed: {error}"),
        _ => panic!("unexpected child orchestration status"),
    };
    assert_eq!(out_child, "l2");
    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt =
        runtime::Runtime::start_with_store(store.clone(), StdArc::new(ActivityRegistry::builder().build()), reg).await;
    let client = Client::new(store.clone());
    client.start_orchestration("i5", "Up", "").await.unwrap();
    // Use wait helper instead of polling
    match client
        .wait_for_orchestration("i5", std::time::Duration::from_secs(3))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output.as_str(), "done"),
        runtime::OrchestrationStatus::Failed { error } => panic!("unexpected failure: {error}"),
        _ => unreachable!(),
    }
    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;
    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-vlatest", "OrderFlow", "X")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-vlatest", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v1.1:X"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for completion event
    let hist = client.read_execution_history("inst-vlatest", 1).await.unwrap();
    assert!(matches!(hist.last().unwrap(), Event::OrchestrationCompleted { .. }));
    rt.shutdown(None).await;
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
        duroxide::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
    )
    .await;

    let activities = ActivityRegistry::builder().build();
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;
    let client = Client::new(store.clone());

    client.start_orchestration("inst-vpin", "OrderFlow", "Y").await.unwrap();

    match client
        .wait_for_orchestration("inst-vpin", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v1:Y"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }
    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg.clone()).await;
    let client = Client::new(store.clone());

    // Default latest for child = 1.1.0
    client
        .start_orchestration("inst-child-latest", "ParentFlow", "Z")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-child-latest", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "c1.1:Z"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Pin child to 1.0.0 via policy
    reg.set_version_policy(
        "ChildFlow",
        duroxide::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
    )
    .await;
    client
        .start_orchestration("inst-child-pinned", "ParentFlow", "Q")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-child-pinned", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "c1:Q"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    rt.shutdown(None).await;
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
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;
    let client = Client::new(store.clone());

    // Start new parent after both child versions registered => latest child (1.1.0) should be used
    client
        .start_orchestration("inst-parent-child-upgrade", "Parent", "inp")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-parent-child-upgrade", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "cv1.1:inp"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // Check history for completion event
    let hist = client
        .read_execution_history("inst-parent-child-upgrade", 1)
        .await
        .unwrap();
    assert!(matches!(hist.last().unwrap(), Event::OrchestrationCompleted { .. }));
    // History should include SubOrchestrationCompleted
    assert!(
        hist.iter()
            .any(|e| matches!(e, Event::SubOrchestrationCompleted { .. }))
    );
    rt.shutdown(None).await;
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
            duroxide::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
        )
        .build();
    let activities = ActivityRegistry::builder().build();
    let store = StdArc::new(SqliteProvider::new_in_memory().await.unwrap());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;
    let client = duroxide::Client::new(store.clone());

    client
        .start_orchestration("inst-can-upgrade", "Upgrader", "seed")
        .await
        .unwrap();

    // With polling approach, wait for final completion
    match client
        .wait_for_orchestration("inst-can-upgrade", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        runtime::OrchestrationStatus::Completed { output } => assert_eq!(output, "v2_done:from_v1_to_v2"),
        runtime::OrchestrationStatus::Failed { error } => panic!("orchestration failed: {error}"),
        _ => panic!("unexpected orchestration status"),
    }

    // History contains the final execution's events
    let hist = client.read_execution_history("inst-can-upgrade", 1).await.unwrap();
    assert!(!hist.is_empty(), "Expected non-empty history");

    // Verify terminal status is also correct
    match client
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
    rt.shutdown(None).await;
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
async fn policy_exact_pins_resolve_handler() {
    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo()) // 1.0.0
        .register_versioned("OrderFlow", "1.1.0", handler_echo())
        .build();
    // Default Latest -> 1.1.0
    let (v_latest, _h) = reg.resolve_handler("OrderFlow").await.expect("resolve latest");
    assert_eq!(v_latest, Version::parse("1.1.0").unwrap());
    // Pin to 1.0.0
    reg.set_version_policy(
        "OrderFlow",
        duroxide::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()),
    )
    .await;
    let (v_pinned, _h) = reg.resolve_handler("OrderFlow").await.expect("resolve pinned");
    assert_eq!(v_pinned, Version::parse("1.0.0").unwrap());
    // Unpin back to Latest
    reg.unpin("OrderFlow").await;
    let (v_unpinned, _h) = reg.resolve_handler("OrderFlow").await.expect("resolve unpinned");
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
fn resolve_handler_exact_missing_returns_none() {
    let reg = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo())
        .build();
    assert!(
        reg.resolve_handler_exact("OrderFlow", &Version::parse("9.9.9").unwrap())
            .is_none()
    );
    assert!(
        reg.resolve_handler_exact("Missing", &Version::parse("1.0.0").unwrap())
            .is_none()
    );
}
