use semver::Version;
use rust_dtf::{OrchestrationContext, OrchestrationRegistry};

fn handler_echo() -> impl Fn(OrchestrationContext, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync + Clone + 'static {
    #[derive(Clone)]
    struct Echo;
    impl Echo {
        fn call(&self, _ctx: OrchestrationContext, input: String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
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
    assert_eq!(vs, vec![
        Version::parse("1.0.0").unwrap(),
        Version::parse("1.1.0").unwrap(),
        Version::parse("2.0.0").unwrap(),
    ]);
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
    reg.set_version_policy("OrderFlow", rust_dtf::runtime::VersionPolicy::Exact(Version::parse("1.0.0").unwrap())).await;
    let (v_pinned, _h) = reg.resolve_for_start("OrderFlow").await.expect("resolve pinned");
    assert_eq!(v_pinned, Version::parse("1.0.0").unwrap());
    // Unpin back to Latest
    reg.unpin("OrderFlow").await;
    let (v_unpinned, _h) = reg.resolve_for_start("OrderFlow").await.expect("resolve unpinned");
    assert_eq!(v_unpinned, Version::parse("1.1.0").unwrap());
}

#[test]
#[should_panic(expected = "duplicate orchestration registration")]
fn duplicate_default_version_panics() {
    let _ = OrchestrationRegistry::builder()
        .register("OrderFlow", handler_echo())
        .register("OrderFlow", handler_echo());
}

#[test]
#[should_panic(expected = "duplicate orchestration registration")]
fn duplicate_specific_version_panics() {
    let _ = OrchestrationRegistry::builder()
        .register_versioned("OrderFlow", "1.2.0", handler_echo())
        .register_versioned("OrderFlow", "1.2.0", handler_echo());
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
    assert!(reg.resolve_exact("OrderFlow", &Version::parse("9.9.9").unwrap()).is_none());
    assert!(reg.resolve_exact("Missing", &Version::parse("1.0.0").unwrap()).is_none());
}


