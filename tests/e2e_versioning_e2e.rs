use std::sync::Arc as StdArc;
use semver::Version;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry, Event};
use rust_dtf::runtime::{self, activity::ActivityRegistry};
use rust_dtf::providers::HistoryStore;

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
    ).await;

    let h = rt.clone().start_orchestration("inst-vlatest", "OrderFlow", "X").await.unwrap();
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
    reg.set_version_policy("OrderFlow", rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap())).await;

    let activities = ActivityRegistry::builder().build();
    let rt = runtime::Runtime::start_with_store(
        StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default()),
        StdArc::new(activities),
        reg,
    ).await;

    let h = rt.clone().start_orchestration("inst-vpin", "OrderFlow", "Y").await.unwrap();
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
        let res = ctx.schedule_sub_orchestration("ChildFlow", input).into_sub_orchestration().await.unwrap();
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
    ).await;

    // Default latest for child = 1.1.0
    let h1 = rt.clone().start_orchestration("inst-child-latest", "ParentFlow", "Z").await.unwrap();
    let (_hist1, out1) = h1.await.unwrap();
    assert_eq!(out1.unwrap(), "c1.1:Z");

    // Pin child to 1.0.0 via policy
    reg.set_version_policy("ChildFlow", rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap())).await;
    let h2 = rt.clone().start_orchestration("inst-child-pinned", "ParentFlow", "Q").await.unwrap();
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
        let res = ctx.schedule_sub_orchestration("Child", input).into_sub_orchestration().await.unwrap();
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
    let h = rt.clone().start_orchestration("inst-parent-child-upgrade", "Parent", "inp").await.unwrap();
    let (hist, out) = h.await.unwrap();
    assert_eq!(out.unwrap(), "cv1.1:inp");
    assert!(matches!(hist.last().unwrap(), Event::OrchestrationCompleted { .. }));
    // History should include SubOrchestrationCompleted
    assert!(hist.iter().any(|e| matches!(e, Event::SubOrchestrationCompleted { .. })));
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
        .set_policy("Upgrader", rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap()))
        .build();
    let activities = ActivityRegistry::builder().build();
    let store = StdArc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default());
    let rt = runtime::Runtime::start_with_store(store.clone(), StdArc::new(activities), reg).await;

    let h = rt.clone().start_orchestration("inst-can-upgrade", "Upgrader", "seed").await.unwrap();
    let (hist, out) = h.await.unwrap();
    // Initial handle resolves at continue-as-new boundary (empty string)
    assert_eq!(out.unwrap(), "");
    assert!(hist.iter().any(|e| matches!(e, Event::OrchestrationContinuedAsNew { .. })));

    // Poll history until final completion appears with expected output
    let mut got = None;
    for _ in 0..100u32 {
        let hcur = store.read("inst-can-upgrade").await;
        if let Some(Event::OrchestrationCompleted { output }) = hcur.last() {
            got = Some(output.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(got.as_deref(), Some("v2_done:from_v1_to_v2"));
    rt.shutdown().await;
}


