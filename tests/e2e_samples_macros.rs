//! End-to-end samples with macros: start here to learn the macro API by example.
//!
//! Each test demonstrates a common orchestration pattern using
//! `OrchestrationContext` and the macro-based auto-discovery system.

mod common;

#[cfg(feature = "macros")]
mod macros_tests {
use duroxide::runtime::{self};
use duroxide::{Client, OrchestrationContext};
// Note: Some imports removed as they're not used in all tests
use crate::common;

// Import macros directly
use duroxide_macros::*;

/// Hello World: define one activity and call it from an orchestrator using macros.
///
/// Highlights:
/// - Use #[activity] attribute for automatic registration
/// - Use #[orchestration] attribute for automatic registration
/// - Use schedule!() macro for type-safe activity calls
/// - Auto-discovery with Runtime::builder()
#[tokio::test]
async fn sample_hello_world_macros() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activity using macro
    #[activity("tests::e2e_samples_macros::macros_tests::hello")]
    async fn hello(input: String) -> Result<String, String> {
        Ok(format!("Hello, {input}!"))
    }

    // Define orchestration using macro
    #[orchestration("tests::e2e_samples_macros::macros_tests::hello_world")]
    async fn hello_world(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("hello_world started");
        let res = ctx.schedule_activity("tests::e2e_samples_macros::macros_tests::hello", "Rust").into_activity().await?;
        ctx.trace_info(format!("hello_world result={res} "));
        let res1 = ctx.schedule_activity("tests::e2e_samples_macros::macros_tests::hello", input).into_activity().await?;
        ctx.trace_info(format!("hello_world result={res1} "));
        Ok(res1)
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());
    client
        .start_orchestration("inst-sample-hello-1", "tests::e2e_samples_macros::macros_tests::hello_world", "World")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-sample-hello-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Hello, World!");
        }
        duroxide::OrchestrationStatus::Failed { error } => {
            panic!("Orchestration failed: {}", error);
        }
        duroxide::OrchestrationStatus::Running => {
            panic!("Orchestration still running");
        }
        duroxide::OrchestrationStatus::NotFound => {
            panic!("Orchestration not found");
        }
    }

    rt.shutdown().await;
}

/// Simple orchestration using macros
///
/// Highlights:
/// - Basic orchestration with macros
/// - Auto-discovery
#[tokio::test]
async fn sample_simple_orchestration_macros() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define orchestration using macro
    #[orchestration("tests::e2e_samples_macros::macros_tests::simple_orchestration")]
    async fn simple_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Starting simple orchestration");
        ctx.trace_info(format!("Input: {}", input));
        Ok(format!("Processed: {}", input))
    }

    // Auto-discovery!
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    client
        .start_orchestration("inst-simple-1", "tests::e2e_samples_macros::macros_tests::simple_orchestration", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-simple-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Processed: TestInput");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test duplicate detection
#[tokio::test]
#[should_panic(expected = "Duplicate activity name found")]
async fn test_duplicate_detection() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activities with duplicate names
    #[activity("duplicate_test::activity")]
    async fn activity1(input: String) -> Result<String, String> {
        Ok(format!("Activity 1: {}", input))
    }

    #[activity("duplicate_test::activity")] // Same name!
    async fn activity2(input: String) -> Result<String, String> {
        Ok(format!("Activity 2: {}", input))
    }

    // This should panic when discovering activities
    runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();
}

/// Test implicit naming (no explicit names provided)
#[tokio::test]
async fn test_implicit_naming() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activity with implicit name (just function name)
    #[activity("implicit_activity_test")]
    async fn implicit_activity_test(input: String) -> Result<String, String> {
        Ok(format!("Implicit: {}", input))
    }

    // Define orchestration with implicit name
    #[orchestration("implicit_orchestration_test")]
    async fn implicit_orchestration_test(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Implicit orchestration started");
        let result = ctx.schedule_activity("implicit_activity_test", input).into_activity().await?;
        ctx.trace_info(format!("Implicit orchestration result: {}", result));
        Ok(result)
    }

    // Auto-discovery with implicit names
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration using implicit name
    client
        .start_orchestration("inst-implicit-1", "implicit_orchestration_test", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-implicit-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Implicit: TestInput");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test mixed explicit and implicit naming
#[tokio::test]
async fn test_mixed_naming() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Mix of explicit and implicit naming
    #[activity("explicit::activity")]
    async fn explicit_activity(input: String) -> Result<String, String> {
        Ok(format!("Explicit: {}", input))
    }

    #[activity("implicit_activity_mixed")] // Unique name for this test
    async fn implicit_activity_mixed(input: String) -> Result<String, String> {
        Ok(format!("Implicit: {}", input))
    }

    #[orchestration("explicit::orchestration")]
    async fn explicit_orchestration(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Explicit orchestration started");
        
        // Call both explicit and implicit activities
        let explicit_result = ctx.schedule_activity("explicit::activity", format!("{}_explicit", input)).into_activity().await?;
        let implicit_result = ctx.schedule_activity("implicit_activity_mixed", format!("{}_implicit", input)).into_activity().await?;
        
        ctx.trace_info(format!("Explicit result: {}", explicit_result));
        ctx.trace_info(format!("Implicit result: {}", implicit_result));
        
        Ok(format!("{} | {}", explicit_result, implicit_result))
    }

    // Auto-discovery
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration using explicit name
    client
        .start_orchestration("inst-mixed-1", "explicit::orchestration", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-mixed-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Explicit: TestInput_explicit | Implicit: TestInput_implicit");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test namespace isolation with implicit names
#[tokio::test]
async fn test_namespace_isolation_implicit() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Same function names but different implicit namespaces
    #[activity("process_data_isolation")]
    async fn process_data_isolation(input: String) -> Result<String, String> {
        Ok(format!("Processed: {}", input))
    }

    #[orchestration("workflow_isolation")]
    async fn workflow_isolation(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Workflow started");
        let result = ctx.schedule_activity("process_data_isolation", input).into_activity().await?;
        ctx.trace_info(format!("Workflow result: {}", result));
        Ok(result)
    }

    // Auto-discovery
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("inst-isolation-1", "workflow_isolation", "IsolationTest")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-isolation-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Processed: IsolationTest");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test true implicit naming (no explicit names at all)
#[tokio::test]
async fn test_true_implicit_naming() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activity with NO explicit name - uses function name
    #[activity]
    async fn greet_user(input: String) -> Result<String, String> {
        Ok(format!("Hello, {}!", input))
    }

    // Define orchestration with NO explicit name - uses function name
    #[orchestration]
    async fn welcome_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Welcome workflow started");
        let result = ctx.schedule_activity("greet_user", input).into_activity().await?;
        ctx.trace_info(format!("Welcome workflow result: {}", result));
        Ok(result)
    }

    // Auto-discovery with true implicit names
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration using function name
    client
        .start_orchestration("inst-true-implicit-1", "welcome_workflow", "Alice")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-true-implicit-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Hello, Alice!");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test that explicit names are used exactly as provided (no prefixes)
#[tokio::test]
async fn test_explicit_names_exact() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Explicit name - should be used exactly as provided
    #[activity("simple_name")]
    async fn complex_function_name(input: String) -> Result<String, String> {
        Ok(format!("Simple: {}", input))
    }

    // Explicit name with colons - should be used exactly as provided
    #[activity("my::custom::activity")]
    async fn another_function(input: String) -> Result<String, String> {
        Ok(format!("Custom: {}", input))
    }

    #[orchestration("simple_orchestration")]
    async fn complex_orchestration_name(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Simple orchestration started");
        
        // Call both activities using their explicit names
        let simple_result = ctx.schedule_activity("simple_name", format!("{}_simple", input)).into_activity().await?;
        let custom_result = ctx.schedule_activity("my::custom::activity", format!("{}_custom", input)).into_activity().await?;
        
        ctx.trace_info(format!("Simple result: {}", simple_result));
        ctx.trace_info(format!("Custom result: {}", custom_result));
        
        Ok(format!("{} | {}", simple_result, custom_result))
    }

    // Auto-discovery
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration using explicit name
    client
        .start_orchestration("inst-explicit-exact-1", "simple_orchestration", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-explicit-exact-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Simple: TestInput_simple | Custom: TestInput_custom");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test that implicit names use function names exactly (no prefixes)
#[tokio::test]
async fn test_implicit_names_exact() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // No explicit name - should use function name
    #[activity]
    async fn my_activity_function(input: String) -> Result<String, String> {
        Ok(format!("Function: {}", input))
    }

    // No explicit name - should use function name
    #[orchestration]
    async fn my_orchestration_function(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Function orchestration started");
        let result = ctx.schedule_activity("my_activity_function", input).into_activity().await?;
        ctx.trace_info(format!("Function orchestration result: {}", result));
        Ok(result)
    }

    // Auto-discovery
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration using function name
    client
        .start_orchestration("inst-implicit-exact-1", "my_orchestration_function", "TestInput")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-implicit-exact-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Function: TestInput");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test schedule! macro with implicit function names
#[tokio::test]
async fn test_schedule_macro_basic() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activity with implicit name (function name)
    #[activity]
    async fn greet_user(input: String) -> Result<String, String> {
        Ok(format!("Hello, {}!", input))
    }

    // Define orchestration that uses schedule! macro
    #[orchestration]
    async fn welcome_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Welcome workflow started");
        
        // Use schedule! macro with implicit function name
        let result = schedule!(ctx, greet_user(input.clone())).into_activity().await?;
        
        ctx.trace_info(format!("Welcome workflow result: {}", result));
        Ok(result)
    }

    // Auto-discovery
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities()
        .discover_orchestrations()
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start orchestration
    client
        .start_orchestration("inst-schedule-basic-1", "welcome_workflow", "Alice")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-schedule-basic-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Hello, Alice!");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;
}

/// Test module-specific discovery
#[tokio::test]
async fn test_module_specific_discovery() {
    // Clear registries before test
    duroxide::__internal::clear_registries();
    
    let (store, _temp_dir) = common::create_sqlite_store_disk().await;

    // Define activities with explicit names in different modules
    #[activity("user::greet")]
    async fn user_greet(input: String) -> Result<String, String> {
        Ok(format!("User greeting: {}", input))
    }

    #[activity("admin::process")]
    async fn admin_process(input: String) -> Result<String, String> {
        Ok(format!("Admin processing: {}", input))
    }

    #[activity("other::function")]
    async fn other_function(input: String) -> Result<String, String> {
        Ok(format!("Other function: {}", input))
    }

    // Define orchestrations with explicit names in different modules
    #[orchestration("user::workflow")]
    async fn user_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("User workflow started");
        // Use direct context call since schedule! macro doesn't support explicit names yet
        let result = ctx.schedule_activity("user::greet", input.clone()).into_activity().await?;
        Ok(result)
    }

    #[orchestration("admin::workflow")]
    async fn admin_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
        ctx.trace_info("Admin workflow started");
        // Use direct context call since schedule! macro doesn't support explicit names yet
        let result = ctx.schedule_activity("admin::process", input.clone()).into_activity().await?;
        Ok(result)
    }

    // Test discovering only user module
    let rt = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities_from_module("user::")
        .discover_orchestrations_from_module("user::")
        .start()
        .await
        .unwrap();

    let client = Client::new(store.clone());

    // Start user orchestration (should work)
    client
        .start_orchestration("inst-user-1", "user::workflow", "Alice")
        .await
        .unwrap();

    match client
        .wait_for_orchestration("inst-user-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "User greeting: Alice");
        }
        _ => panic!("Expected completion"),
    }

    rt.shutdown().await;

    // Test discovering only admin module
    let rt2 = runtime::Runtime::builder()
        .store(store.clone())
        .discover_activities_from_module("admin::")
        .discover_orchestrations_from_module("admin::")
        .start()
        .await
        .unwrap();

    let client2 = Client::new(store.clone());

    // Start admin orchestration (should work)
    client2
        .start_orchestration("inst-admin-1", "admin::workflow", "Bob")
        .await
        .unwrap();

    match client2
        .wait_for_orchestration("inst-admin-1", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "Admin processing: Bob");
        }
        _ => panic!("Expected completion"),
    }

    rt2.shutdown().await;
}

} // mod macros_tests